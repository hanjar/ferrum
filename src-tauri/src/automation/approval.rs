//! Approval, resume, pause and delete of automations, and the automation test (Automation
//! design §7.2, §8, §10.3). Tauri-free: `commands.rs` supplies the window, the native dialog and
//! the live probes; everything that DECIDES lives here and takes its confirmation, clock, keys and
//! engine as injected seams (design §13.1), exactly like `hotlist::run_command_in`.
//!
//! The rules this file enforces (SE-1..SE-3):
//!
//! - An approval exists only after the BACKEND showed a native confirmation built from validated,
//!   stored data. No input carries a "confirmed" flag; every input type is `deny_unknown_fields`.
//! - It needs a PASSED test of the same policy hash from this process within 15 minutes.
//! - After the dialog the hash is recomputed inside the write transaction; a changed profile,
//!   automation or expired test means `stale` and nothing is stored.
//! - Automation, approval, runtime row, event and `change_seq` are written in ONE transaction.
//! - Resume re-verifies the stored approval before the dialog (a changed profile answers
//!   `needs_review` WITHOUT showing anything) and again after it.
//! - Pause and delete go in the safe direction and show no native dialog.

use rusqlite::{Connection, TransactionBehavior};

use crate::automation::commands::{CommandError, ConfirmAnswer};
use crate::automation::confirm_text::{self, ApprovalAction, Prompt};
use crate::automation::dpapi::{ApprovalKeys, KeyLoad};
use crate::automation::dto::{
    ApproveResultDto, AutomationDto, AutomationValidateDto, DeleteAutomationDto, PauseMode,
    PauseResultDto, SetEnabledResultDto, StepDto, StepStatus, TestResultDto,
};
use crate::automation::model::{
    self, AutomationDraft, AutomationPolicy, ProfilePolicy, ValidatedAutomation,
};
use crate::automation::policy::{self, HASH_VERSION, MacInput, Verdict};
use crate::automation::store::approvals::{self, ApprovalRecord};
use crate::automation::store::automations::{self, ApprovedAutomation, EventKind};
use crate::automation::store::{Store, StoreError, profiles, runtime};
use crate::automation::test_run::{self, TestDeps, TestLedger, TestRecord};

/// Clock seam: milliseconds since the Unix epoch.
pub type Clock<'a> = &'a dyn Fn() -> i64;

// ── Errors ──

fn invalid_id(what: &str) -> CommandError {
    CommandError::new(
        "invalid_input",
        format!("The {what} is not a valid identifier"),
    )
}

fn require_id(id: &str, what: &str) -> Result<(), CommandError> {
    if model::is_profile_id(id) {
        Ok(())
    } else {
        Err(invalid_id(what))
    }
}

fn automation_missing() -> CommandError {
    CommandError::new("not_found", "The automation does not exist")
}

fn needs_review() -> CommandError {
    CommandError::new(
        "needs_review",
        "The automation needs a review before it can run",
    )
}

fn key_unavailable() -> CommandError {
    CommandError::new(
        "store_unavailable",
        "The approval key is unavailable; nothing was changed",
    )
}

fn busy() -> CommandError {
    CommandError::new("busy", "Another confirmation is open; nothing was changed")
}

fn field_error(errors: &[model::FieldError]) -> CommandError {
    let detail = errors
        .iter()
        .take(5)
        .map(|e| format!("{} ({})", e.field, e.code))
        .collect::<Vec<_>>()
        .join(", ");
    CommandError::new(
        "invalid_input",
        format!("The automation data was refused: {detail}"),
    )
}

// ── Loading ──

/// A profile as the approval flow needs it: its record and parsed policy.
struct ProfileFacts {
    rec: profiles::ProfileRecord,
    policy: ProfilePolicy,
}

fn load_profile(conn: &Connection, id: &str) -> Result<ProfileFacts, CommandError> {
    let rec = profiles::get(conn, id)?
        .ok_or_else(|| CommandError::new("not_found", "The sync profile does not exist"))?;
    let policy = serde_json::from_str(&rec.policy)
        .map_err(|_| CommandError::new("needs_review", "The stored profile is invalid"))?;
    Ok(ProfileFacts { rec, policy })
}

fn parse_automation_policy(json: &str) -> Result<AutomationPolicy, CommandError> {
    serde_json::from_str(json).map_err(|_| needs_review())
}

fn hash_for(
    automation_id: &str,
    profile: &ProfileFacts,
    policy: &AutomationPolicy,
) -> Result<String, CommandError> {
    policy::policy_hash(
        automation_id,
        &profile.rec.id,
        profile.rec.revision,
        &profile.policy,
        policy,
    )
    .ok_or_else(needs_review)
}

/// The DTO of a stored automation, with its verification verdict (AP-6).
pub fn dto_of(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    rec: &automations::AutomationRecord,
) -> Result<AutomationDto, CommandError> {
    let profile = profiles::get(conn, &rec.profile_id)?;
    let policy = parse_automation_policy(&rec.policy)?;
    let review = match policy::verify(conn, keys, &rec.id) {
        Ok(Verdict::Valid) => None,
        Ok(Verdict::NeedsReview(r)) => Some(r.into()),
        // A store failure while verifying never reads as "valid".
        Err(_) => Some(policy::NeedsReview::PolicyChanged.into()),
    };
    Ok(AutomationDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        kind: rec.kind.clone(),
        profile_id: rec.profile_id.clone(),
        profile_name: profile.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
        profile_revision: profile.as_ref().map(|p| p.revision).unwrap_or(0),
        policy,
        enabled: rec.enabled,
        config_rev: rec.config_rev,
        created_ms: rec.created_ms,
        updated_ms: rec.updated_ms,
        review,
    })
}

// ── Validate ──

/// `automation_validate` core: field errors, readiness checks, next runs. No side effects.
pub fn validate_in(
    store: &Store,
    draft: &AutomationDraft,
    probe: &std::sync::Arc<dyn crate::sync_engine::roots::RootProbe>,
    budget: std::time::Duration,
    background_ready: bool,
) -> Result<AutomationValidateDto, CommandError> {
    let facts = if model::is_profile_id(&draft.profile_id) {
        store.with_read(|conn| -> Result<Option<ProfileFacts>, CommandError> {
            match load_profile(conn, &draft.profile_id) {
                Ok(f) => Ok(Some(f)),
                Err(e) if e.code == "not_found" => Ok(None),
                Err(e) => Err(e),
            }
        })?
    } else {
        None
    };
    let Some(profile) = facts else {
        // Unknown or malformed profile: the only honest answer is a field error.
        let mut errors = vec![model::FieldError {
            field: "profileId".into(),
            code: "profile_required".into(),
        }];
        errors.extend(draft_name_errors(draft));
        return Ok(AutomationValidateDto {
            errors,
            checks: Vec::new(),
            next: Vec::new(),
        });
    };
    let errors = match draft.validate(&profile.policy) {
        Ok(_) => Vec::new(),
        Err(e) => e,
    };
    // AP-2.2: readiness block. The live probes run after the store lock is released.
    let mut checks = Vec::new();
    let ready = crate::automation::commands::readiness_dto(&profile.policy, probe, budget);
    checks.push(match ready {
        crate::automation::commands::ReadinessDto::Ready => StepDto {
            id: "profile".into(),
            status: StepStatus::Ok,
            text: "The sync profile is ready.".into(),
        },
        crate::automation::commands::ReadinessDto::Checking => StepDto {
            id: "profile".into(),
            status: StepStatus::Warn,
            text: "The profile's folders are still being checked.".into(),
        },
        crate::automation::commands::ReadinessDto::NotReady { reason } => StepDto {
            id: "profile".into(),
            status: StepStatus::Fail,
            text: format!("The sync profile is not ready ({reason})."),
        },
    });
    checks.push(match (draft.policy.runtime, background_ready) {
        (model::Runtime::App, _) => StepDto {
            id: "runtime".into(),
            status: StepStatus::Ok,
            text: "Runs while Ferrum is open.".into(),
        },
        (model::Runtime::Background, true) => StepDto {
            id: "runtime".into(),
            status: StepStatus::Ok,
            text: "The background runner is ready.".into(),
        },
        (model::Runtime::Background, false) => StepDto {
            id: "runtime".into(),
            status: StepStatus::Fail,
            text: "The background runner is not set up yet.".into(),
        },
    });
    Ok(AutomationValidateDto {
        errors,
        checks,
        // The scheduler (P1.4) fills the next occurrences.
        next: Vec::new(),
    })
}

fn draft_name_errors(draft: &AutomationDraft) -> Vec<model::FieldError> {
    let name = draft.name.trim();
    let mut errors = Vec::new();
    if name.is_empty() {
        errors.push(model::FieldError {
            field: "name".into(),
            code: "name_required".into(),
        });
    } else if name.chars().count() > model::NAME_MAX_CHARS {
        errors.push(model::FieldError {
            field: "name".into(),
            code: "name_too_long".into(),
        });
    }
    errors
}

// ── Test ──

/// Input of `automation_test`: the draft, plus the id of the automation being edited (the policy
/// hash covers the id, so an edit is tested under its own id).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub draft: AutomationDraft,
}

/// `automation_test` core: validate, run the read-only steps, and remember `(hash, passed, at)`
/// in the ledger. A draft that fails validation is `invalid_input` (the editor validates first).
pub fn test_in(
    store: &Store,
    ledger: &TestLedger,
    req: TestRequest,
    deps: &TestDeps<'_>,
    clock: Clock<'_>,
) -> Result<TestResultDto, CommandError> {
    if let Some(id) = &req.id {
        require_id(id, "automation id")?;
    }
    require_id(&req.draft.profile_id, "profile id")?;
    let (profile, existing) = store.with_read(
        |conn| -> Result<(ProfileFacts, Option<automations::AutomationRecord>), CommandError> {
            let profile = load_profile(conn, &req.draft.profile_id)?;
            let existing = match &req.id {
                Some(id) => Some(automations::get(conn, id)?.ok_or_else(automation_missing)?),
                None => None,
            };
            Ok((profile, existing))
        },
    )?;
    let validated: ValidatedAutomation = req
        .draft
        .validate(&profile.policy)
        .map_err(|e| field_error(&e))?;
    // The hash covers the automation id, so a NEW automation gets its id now; approve takes it
    // from this record and the webview never chooses an id.
    let automation_id = match (&req.id, &existing) {
        (Some(id), Some(_)) => id.clone(),
        _ => model::new_profile_id().ok_or_else(|| {
            CommandError::new("store_unavailable", "Could not create an identifier")
        })?,
    };
    let hash = hash_for(&automation_id, &profile, &validated.policy)?;
    let steps = test_run::run_steps(
        &profile.rec.name,
        profile.rec.revision,
        &profile.policy,
        &validated.policy,
        deps,
    );
    let passed = test_run::passed(&steps);
    let test_id = model::new_profile_id()
        .ok_or_else(|| CommandError::new("store_unavailable", "Could not create an identifier"))?;
    ledger.record(TestRecord {
        test_id: test_id.clone(),
        automation_id: automation_id.clone(),
        policy_hash: hash.clone(),
        passed,
        at_ms: clock(),
    });
    Ok(TestResultDto {
        test_id,
        automation_id,
        policy_hash: hash,
        steps,
        passed,
    })
}

// ── Approve ──

/// Input of `automation_approve`, assembled from the command's separate IPC parameters.
/// `deny_unknown_fields` for any caller that deserializes the whole request at once (SE-1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub expected_config_rev: Option<i64>,
    pub draft: AutomationDraft,
    pub test_id: String,
}

/// The seams of the approval flows.
pub struct Deps<'a> {
    pub keys: &'a dyn ApprovalKeys,
    pub ledger: &'a TestLedger,
    pub clock: Clock<'a>,
}

fn answer_to_cancel(answer: ConfirmAnswer) -> Result<bool, CommandError> {
    match answer {
        ConfirmAnswer::Yes => Ok(true),
        ConfirmAnswer::No => Ok(false),
        ConfirmAnswer::Busy => Err(busy()),
    }
}

/// Outcome of the write transaction of an approval.
enum Commit {
    Done(String),
    Stale,
}

/// `automation_approve` core (design §8.2).
pub fn approve_in<A: Into<ConfirmAnswer>>(
    store: &Store,
    req: ApproveRequest,
    deps: &Deps<'_>,
    confirm: impl FnOnce(&Prompt) -> A,
) -> Result<ApproveResultDto, CommandError> {
    if let Some(id) = &req.id {
        require_id(id, "automation id")?;
    }
    require_id(&req.test_id, "test id")?;
    require_id(&req.draft.profile_id, "profile id")?;
    // LC-3: nobody is asked about a change a read-only or unusable store would refuse.
    store.check_writable()?;

    // Everything that does not depend on the user's answer is decided BEFORE the dialog.
    let (profile, existing) = store.with_read(
        |conn| -> Result<(ProfileFacts, Option<automations::AutomationRecord>), CommandError> {
            let profile = load_profile(conn, &req.draft.profile_id)?;
            let existing = match &req.id {
                Some(id) => Some(automations::get(conn, id)?.ok_or_else(automation_missing)?),
                None => None,
            };
            Ok((profile, existing))
        },
    )?;
    let validated = req
        .draft
        .validate(&profile.policy)
        .map_err(|e| field_error(&e))?;
    if let Some(row) = &existing
        && req.expected_config_rev != Some(row.config_rev)
    {
        return Ok(ApproveResultDto::Stale);
    }
    // SE-3.2: a passed test of this exact hash, from this process, within 15 minutes.
    let Some(record) = deps.ledger.find(&req.test_id, (deps.clock)()) else {
        return Ok(ApproveResultDto::Stale);
    };
    if !record.passed {
        return Ok(ApproveResultDto::Stale);
    }
    // Existing automation: the test must have been for it. New automation: the id comes from the
    // test record and must still be unused.
    let automation_id = match &req.id {
        Some(id) if *id == record.automation_id => id.clone(),
        Some(_) => return Ok(ApproveResultDto::Stale),
        None => {
            let taken = store.with_read(|conn| automations::get(conn, &record.automation_id))?;
            if taken.is_some() {
                return Ok(ApproveResultDto::Stale);
            }
            record.automation_id.clone()
        }
    };
    let shown_hash = hash_for(&automation_id, &profile, &validated.policy)?;
    if shown_hash != record.policy_hash {
        return Ok(ApproveResultDto::Stale);
    }
    // The key before the dialog (FER-524, review P1.1 #3):
    //  - usable: used as it is;
    //  - missing: created now, so a machine where DPAPI is refused tells the user before they
    //    answer (nothing is lost by it: no approval could verify without a key anyway);
    //  - unusable (another user, machine or entropy, a damaged blob): NOT touched yet. Replacing
    //    it invalidates every earlier approval, so it happens only after the user said Yes
    //    below; a Cancel leaves the old blob exactly where it is;
    //  - unreadable (I/O problem): refused, never rotated.
    let mut key = match deps.keys.load() {
        KeyLoad::Key(key) => Some(key),
        KeyLoad::Missing => Some(
            deps.keys
                .create_if_missing()
                .map_err(|_| key_unavailable())?,
        ),
        KeyLoad::Unusable => None,
        KeyLoad::Unreadable => return Err(key_unavailable()),
    };
    // A key that was just created invalidates every earlier approval: say so on each automation,
    // whatever the user answers next (FER-522 rework 1).
    if deps.keys.take_rotation_notice() {
        note_key_rotation(store, deps.clock)?;
    }

    let action = if existing.is_some() {
        ApprovalAction::Reapprove
    } else {
        ApprovalAction::Enable
    };
    // A re-approval names the automation by its STORED name; a rename is stated separately.
    let shown_name = existing
        .as_ref()
        .map_or(validated.name.as_str(), |r| r.name.as_str());
    let prompt = confirm_text::approval_prompt(
        action,
        shown_name,
        existing.as_ref().map(|_| validated.name.as_str()),
        &profile.rec.name,
        profile.rec.revision,
        &profile.policy,
        &validated.policy,
    );
    if !answer_to_cancel(confirm(&prompt).into())? {
        // Cancel = nothing written (AP-5).
        return Ok(ApproveResultDto::Canceled);
    }

    // The user said Yes. Only now may an unusable key be replaced (it is backed up first).
    let key = match key.take() {
        Some(key) => key,
        None => {
            let replaced = deps.keys.load_or_create().map_err(|_| key_unavailable())?;
            if deps.keys.take_rotation_notice() {
                note_key_rotation(store, deps.clock)?;
            }
            replaced
        }
    };

    // After the dialog: recompute everything inside the write transaction (design §8.2).
    let policy_json = serde_json::to_string(&validated.policy)
        .map_err(|_| CommandError::new("store_unavailable", "Could not encode the automation"))?;
    let commit = store.with_write(|conn| -> Result<Commit, CommandError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Unavailable)?;
        let now = (deps.clock)();
        // The profile may have been saved while the dialog was open.
        let fresh = match load_profile(&tx, &validated.profile_id) {
            Ok(p) => p,
            Err(e) if e.code == "not_found" => return Ok(Commit::Stale),
            Err(e) => return Err(e),
        };
        let Ok(fresh_hash) = hash_for(&automation_id, &fresh, &validated.policy) else {
            return Ok(Commit::Stale);
        };
        if fresh_hash != shown_hash {
            return Ok(Commit::Stale);
        }
        // The automation may have been saved by another connection meanwhile.
        let current = automations::get(&tx, &automation_id)?;
        let new_rev = match (&req.id, &current) {
            (Some(_), Some(row)) if Some(row.config_rev) == req.expected_config_rev => {
                row.config_rev + 1
            }
            (None, None) => 1,
            _ => return Ok(Commit::Stale),
        };
        // The test must still be fresh when the answer arrives (SE-3.2).
        if deps.ledger.find(&req.test_id, now).is_none() {
            return Ok(Commit::Stale);
        }
        let kind = validated.policy.trigger.kind();
        automations::upsert_approved(
            &tx,
            &ApprovedAutomation {
                id: &automation_id,
                name: &validated.name,
                kind,
                profile_id: &fresh.rec.id,
                policy: &policy_json,
                config_rev: new_rev,
                now_ms: now,
            },
        )?;
        let input = MacInput {
            hash_version: i64::from(HASH_VERSION),
            policy_hash: &fresh_hash,
            automation_id: &automation_id,
            profile_id: &fresh.rec.id,
            profile_revision: fresh.rec.revision,
            config_rev: new_rev,
            approved_ms: now,
        };
        let mac = policy::compute_mac(&key, &input).ok_or_else(key_unavailable)?;
        approvals::put(
            &tx,
            &ApprovalRecord {
                automation_id: automation_id.clone(),
                hash_version: i64::from(HASH_VERSION),
                policy_hash: fresh_hash.clone(),
                profile_id: fresh.rec.id.clone(),
                profile_revision: fresh.rec.revision,
                config_rev: new_rev,
                approved_ms: now,
                mac,
            },
        )?;
        // An edited or re-approved automation starts from a clean slate: no queued entry from the
        // old configuration, no countdown. An attention hold stays until a recheck or retry resolves it.
        let removed = automations::take_queue(&tx, &automation_id)?;
        crate::automation::host::requeue_siblings(&tx, deps.keys, &removed, now)?;
        let trigger_state = match &validated.policy.trigger {
            model::Trigger::Schedule(_) => "scheduled",
            model::Trigger::Watch(_) => "watching",
        };
        let host_class = match validated.policy.runtime {
            model::Runtime::App => "app",
            model::Runtime::Background => "background",
        };
        automations::reset_runtime(&tx, &automation_id, host_class, trigger_state, now)?;
        let verb = if existing.is_some() {
            "Re-approved"
        } else {
            "Approved"
        };
        automations::add_event(
            &tx,
            &automation_id,
            now,
            EventKind::Ok,
            &format!(
                "{verb} \"{}\" rev {} with the reviewed policies \u{b7} enabled",
                validated.name, fresh.rec.revision
            ),
        )?;
        automations::bump_change_seq(&tx)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(Commit::Done(automation_id.clone()))
    })?;
    match commit {
        Commit::Stale => Ok(ApproveResultDto::Stale),
        Commit::Done(id) => {
            deps.ledger.consume(&req.test_id);
            let dto = store.with_read(|conn| -> Result<AutomationDto, CommandError> {
                let row = automations::get(conn, &id)?.ok_or_else(automation_missing)?;
                dto_of(conn, deps.keys, &row)
            })?;
            Ok(ApproveResultDto::Approved {
                automation: Box::new(dto),
            })
        }
    }
}

/// One `warn` event per automation: the approval key was made again, so earlier approvals no
/// longer verify (Needs review) and must be approved again.
pub(crate) fn note_key_rotation(store: &Store, clock: Clock<'_>) -> Result<(), CommandError> {
    store.with_write(|conn| -> Result<(), CommandError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Unavailable)?;
        let now = clock();
        for row in automations::list(&tx)? {
            automations::add_event(
                &tx,
                &row.id,
                now,
                EventKind::Warn,
                "The approval key was created again \u{2014} review and approve this automation again",
            )?;
        }
        automations::bump_change_seq(&tx)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(())
    })
}

// ── Enable / pause / delete ──

/// `automation_set_enabled` core. Enabling is a RESUME: the stored approval must verify before
/// the dialog (a changed profile answers `needs_review` without showing anything) and again after
/// it, against the very policy the dialog showed. Disabling goes in the safe direction without a
/// dialog; with a run in progress it answers `needsChoice` so the UI can ask finish-or-stop.
pub fn set_enabled_in<A: Into<ConfirmAnswer>>(
    store: &Store,
    id: &str,
    enabled: bool,
    deps: &Deps<'_>,
    confirm: impl FnOnce(&Prompt) -> A,
) -> Result<SetEnabledResultDto, CommandError> {
    require_id(id, "automation id")?;
    store.check_writable()?;
    if !enabled {
        return disable(store, deps.keys, id, deps.clock, None);
    }
    struct Shown {
        rec: automations::AutomationRecord,
        policy: AutomationPolicy,
        profile: ProfileFacts,
        hash: String,
    }
    let shown = store.with_read(|conn| -> Result<Option<Shown>, CommandError> {
        let rec = automations::get(conn, id)?.ok_or_else(automation_missing)?;
        if rec.enabled {
            return Ok(None);
        }
        // Valid approval first: a profile that changed since means `needs_review`, no dialog.
        match policy::verify(conn, deps.keys, id)? {
            Verdict::Valid => {}
            Verdict::NeedsReview(_) => return Err(needs_review()),
        }
        let policy = parse_automation_policy(&rec.policy)?;
        let profile = load_profile(conn, &rec.profile_id)?;
        let hash = hash_for(&rec.id, &profile, &policy)?;
        Ok(Some(Shown {
            rec,
            policy,
            profile,
            hash,
        }))
    })?;
    // Already enabled: nothing to ask, nothing to change.
    let Some(shown) = shown else {
        return Ok(SetEnabledResultDto::Enabled);
    };
    let prompt = confirm_text::approval_prompt(
        ApprovalAction::Resume,
        &shown.rec.name,
        None,
        &shown.profile.rec.name,
        shown.profile.rec.revision,
        &shown.profile.policy,
        &shown.policy,
    );
    if !answer_to_cancel(confirm(&prompt).into())? {
        return Ok(SetEnabledResultDto::Canceled);
    }
    store.with_write(|conn| -> Result<SetEnabledResultDto, CommandError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Unavailable)?;
        let now = (deps.clock)();
        // Verify again, inside the transaction, and require that it is still the policy the user
        // saw (another connection may have re-approved or edited while the dialog was open).
        let row = automations::get(&tx, id)?.ok_or_else(automation_missing)?;
        if !matches!(policy::verify(&tx, deps.keys, id)?, Verdict::Valid) {
            return Err(needs_review());
        }
        let policy = parse_automation_policy(&row.policy)?;
        let profile = load_profile(&tx, &row.profile_id)?;
        if row.config_rev != shown.rec.config_rev
            || hash_for(&row.id, &profile, &policy)? != shown.hash
        {
            return Err(CommandError::new(
                "stale",
                "The automation changed while the confirmation was open",
            ));
        }
        automations::set_enabled(&tx, id, true, now)?;
        let trigger_state = match &policy.trigger {
            model::Trigger::Schedule(_) => "scheduled",
            model::Trigger::Watch(_) => "watching",
        };
        let host_class = match policy.runtime {
            model::Runtime::App => "app",
            model::Runtime::Background => "background",
        };
        automations::reset_runtime(&tx, id, host_class, trigger_state, now)?;
        automations::add_event(
            &tx,
            id,
            now,
            EventKind::Ok,
            &format!("Resumed \"{}\" rev {}", row.name, profile.rec.revision),
        )?;
        automations::bump_change_seq(&tx)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(SetEnabledResultDto::Enabled)
    })
}

/// Safe direction shared by `set_enabled(false)` and `pause`. `forced` = the user already chose
/// finish/stop (pause), so a running run does not ask again.
fn disable(
    store: &Store,
    keys: &dyn ApprovalKeys,
    id: &str,
    clock: Clock<'_>,
    forced: Option<PauseMode>,
) -> Result<SetEnabledResultDto, CommandError> {
    store.with_write(|conn| -> Result<SetEnabledResultDto, CommandError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Unavailable)?;
        let row = automations::get(&tx, id)?.ok_or_else(automation_missing)?;
        if forced.is_none() && automations::has_running_run(&tx, id)? {
            return Ok(SetEnabledResultDto::NeedsChoice);
        }
        if !row.enabled {
            return Ok(SetEnabledResultDto::Paused);
        }
        let now = clock();
        automations::set_enabled(&tx, id, false, now)?;
        // Queued entries are cancelled; the run part (finish or stop) is a control for the host
        // (`host::request::pause_run_part`). Triggers of other automations that were merged into
        // this one's entry move to a healthy sibling (design §9.5).
        let removed = automations::take_queue(&tx, id)?;
        crate::automation::host::requeue_siblings(&tx, keys, &removed, now)?;
        // RQ-8.2: pausing also drops the pending rerun.
        runtime::set_pending_rerun(&tx, id, false, now)?;
        let text = match forced {
            Some(PauseMode::Stop) => "Paused \u{b7} the current run is stopped",
            Some(PauseMode::Finish) => "Paused \u{b7} the current run finishes first",
            None => "Paused",
        };
        automations::add_event(&tx, id, now, EventKind::Mute, text)?;
        automations::bump_change_seq(&tx)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(SetEnabledResultDto::Paused)
    })
}

/// `automation_pause` core (config part): no native dialog, safe direction.
pub fn pause_in(
    store: &Store,
    keys: &dyn ApprovalKeys,
    id: &str,
    mode: PauseMode,
    clock: Clock<'_>,
) -> Result<PauseResultDto, CommandError> {
    require_id(id, "automation id")?;
    store.check_writable()?;
    disable(store, keys, id, clock, Some(mode))?;
    Ok(PauseResultDto::Paused)
}

/// `automation_delete` core: webview confirmation only. Queued entries, the approval and the
/// runtime row go with the automation.
pub fn delete_in(
    store: &Store,
    keys: &dyn ApprovalKeys,
    id: &str,
    clock: Clock<'_>,
) -> Result<DeleteAutomationDto, CommandError> {
    require_id(id, "automation id")?;
    store.check_writable()?;
    store.with_write(|conn| -> Result<(), CommandError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Unavailable)?;
        // Triggers of other automations merged into this one's entry move to a sibling.
        let removed = automations::take_queue(&tx, id)?;
        if !automations::delete(&tx, id)? {
            return Err(automation_missing());
        }
        crate::automation::host::requeue_siblings(&tx, keys, &removed, clock())?;
        automations::bump_change_seq(&tx)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(())
    })?;
    Ok(DeleteAutomationDto::Deleted)
}

/// Whether the approval key currently exists and decrypts (diagnostics; `automation_list` shows
/// `keyUnavailable` per automation through [`policy::verify`]).
pub fn key_is_usable(keys: &dyn ApprovalKeys) -> bool {
    matches!(keys.load(), KeyLoad::Key(_))
}

#[cfg(test)]
mod tests;
