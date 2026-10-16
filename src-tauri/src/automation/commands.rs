//! Tauri commands for Sync Profiles (Automation design §7.1). This is the only automation file
//! allowed to import the app runtime (design §2, enforced by a source test); every decision
//! lives in Tauri-free `*_in` functions so tests drive confirmations, probes and the clock
//! without a window (design §13.1).
//!
//! IPC rules (design §7): camelCase DTOs, closed-set error codes as `{ code, message }` without
//! user paths, ids validated as lowercase UUID v4 before any lookup, blocking work on
//! `spawn_blocking`, and no command input that names or behaves like a `confirmed` flag (SE-1).

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::automation::confirm_text::{self, Prompt};
use crate::automation::lease::{LeaseEnv, LeaseRegistry, ManualAcquire};
use crate::automation::manual_run;
use crate::automation::model::{
    self, Draft, FieldError, PolicyDraft, ProfilePolicy, RootIdentity, WireCompare,
    WireDeletePolicy, WireMode, WireSide,
};
use crate::automation::store::{Store, StoreError, profiles};
use crate::profile;
use crate::sync_engine::roots::{
    self, FsProbe, READINESS_BUDGET, RootError, RootProbe, RootProbeOutcome, probe_with,
};
use std::sync::Arc as ProbeArc;

// ── State ──

/// Managed state: one handle to the opened automation store. Opening happens once at startup;
/// an unusable store is remembered and every command reports it.
pub struct AutomationState {
    pub store: Arc<Store>,
    /// Manual root leases of this process (design §3.4); the webview only holds their tokens.
    pub leases: Arc<LeaseRegistry>,
}

impl AutomationState {
    pub fn open() -> Self {
        let store = match store_dir() {
            Some(dir) => Store::open(&dir, env!("CARGO_PKG_VERSION")),
            // No resolvable local data folder: the store is unavailable, never relocated.
            None => Store::unavailable(),
        };
        Self {
            store: Arc::new(store),
            leases: Arc::new(LeaseRegistry::new(LeaseEnv::system())),
        }
    }
}

/// `<local data>/automation` (design §4.1) - the only location the store may live in. `None`
/// when the OS cannot name the local app data folder (no silent fallback elsewhere).
pub fn store_dir() -> Option<std::path::PathBuf> {
    profile::resolve_local_data_dir().map(|dir| dir.join("automation"))
}

// ── Error shape ──

/// The only error shape crossing IPC for automation commands: a closed-set code and a fixed,
/// path-free message (design §7, SE-7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    fn invalid(errors: &[FieldError]) -> Self {
        let detail = errors
            .iter()
            .take(5)
            .map(|e| format!("{} ({})", e.field, e.code))
            .collect::<Vec<_>>()
            .join(", ");
        Self::new(
            "invalid_input",
            format!("The profile data was refused: {detail}"),
        )
    }
}

impl From<StoreError> for CommandError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Unavailable => Self::new(
                "store_unavailable",
                "The automation store is unavailable; nothing was changed",
            ),
            StoreError::Untrusted => Self::new(
                "store_untrusted",
                "The automation store location is not trusted",
            ),
            // LC-3: the store schema is newer than this binary.
            StoreError::ReadOnly => {
                Self::new("store_read_only", "Ferrum was updated - restart Ferrum")
            }
            StoreError::NotFound => Self::new("not_found", "The sync profile does not exist"),
            StoreError::NameTaken => Self::new("invalid_input", "This name is already in use"),
            StoreError::Stale => Self::new(
                "stale",
                "This profile was changed elsewhere. Review the current version",
            ),
            StoreError::InUse => Self::new(
                "in_use",
                "The profile is used by an automation - delete or change it first",
            ),
        }
    }
}

fn internal() -> CommandError {
    CommandError::new("store_unavailable", "The automation store task failed")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn policy_digest(policy: &ProfilePolicy) -> String {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_vec(policy).unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(json);
    hex::encode(hash.finalize())
}

// ── DTOs (camelCase, fixtures in contract-fixtures/automation/) ──

fn serde_broken() -> CommandError {
    CommandError::new(
        "needs_review",
        "The stored profile is invalid and needs review",
    )
}

/// SP-8 readiness, as the UI renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ReadinessDto {
    Ready,
    Checking,
    NotReady { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummaryDto {
    pub id: String,
    pub name: String,
    pub revision: i64,
    pub left: String,
    pub right: String,
    pub mode_label: String,
    pub delete_label: String,
    pub readiness: ReadinessDto,
    pub used_by: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitiesDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<RootIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<RootIdentity>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindsDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionChangesDto {
    pub revision: i64,
    pub changed_ms: i64,
    pub changes: Vec<model::FieldChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDto {
    pub id: String,
    pub name: String,
    pub revision: i64,
    /// The policy exactly as the editor sends and receives it (design §7.1 `draft` shape).
    pub policy: PolicyDraft,
    pub identities: IdentitiesDto,
    /// Per revision, which policy fields changed with display values (SP-6.3), newest first.
    pub changes: Vec<RevisionChangesDto>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub readiness: ReadinessDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResultDto {
    pub errors: Vec<FieldError>,
    pub identities: IdentitiesDto,
    pub kinds: KindsDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SaveResultDto {
    Saved { profile: Box<ProfileDto> },
    Canceled,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResultDto {
    pub status: String,
}

/// Who is in the way of a refused lease: the kind of holder only (no paths, no names).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseHolderDto {
    /// `manual`, `app`, `runner` or `unknown`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<i64>,
}

/// `sync_lease_acquire` result: `{ leaseId }` or `{ status: 'busy', holder }` (design §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LeaseAcquireDto {
    Acquired(LeaseAcquired),
    Busy(LeaseBusy),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseAcquired {
    pub lease_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseBusy {
    pub status: LeaseBusyStatus,
    pub holder: LeaseHolderDto,
}

/// The literal `"busy"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeaseBusyStatus {
    Busy,
}

/// `sync_lease_release` result: an empty object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseReleaseDto {}

// ── Readiness (SP-8) ──

/// Reason strings for [`ReadinessDto::NotReady`], camelCase for the UI.
mod reasons {
    pub const ROOT_MISSING: &str = "rootMissing";
    /// A network root that cannot be opened now (offline, share gone, no answer in time).
    pub const ROOT_OFFLINE: &str = "rootOffline";
    /// A network root saved while offline: its identity was never verified.
    pub const IDENTITY_UNKNOWN: &str = "identityUnknown";
    pub const IDENTITY_MISMATCH: &str = "identityMismatch";
    pub const RECYCLE_BIN_UNAVAILABLE: &str = "recycleBinUnavailable";
    pub const VERSIONING_ROOT_INVALID: &str = "versioningRootInvalid";
    pub const PROFILE_INVALID: &str = "profileInvalid";
}

fn kind_of(identity: &RootIdentity) -> String {
    match identity {
        RootIdentity::Local { drive, .. } => match drive {
            model::DriveKind::Fixed => "fixed".into(),
            model::DriveKind::Removable => "removable".into(),
        },
        RootIdentity::Network { .. } => "network".into(),
    }
}

/// One live root: still there, still the same folder it was saved for (SP-7.2), within the
/// probe budget. A slower answer means `checking`, never "treat as empty" (SP-8, WA-5.1). A
/// network root that cannot be opened is offline; one saved without verified ids stays not ready.
fn root_verdict(
    spec: &model::RootSpec,
    probe: &ProbeArc<dyn RootProbe>,
    budget: Duration,
) -> Result<(), String> {
    let network = crate::sync_engine::roots::is_unc(&spec.path);
    match probe_with(probe.clone(), &spec.path, budget) {
        RootProbeOutcome::Ready {
            canonical,
            identity,
        } => {
            if matches!(
                &identity,
                RootIdentity::Network {
                    volume_serial: None,
                    ..
                }
            ) {
                return Err(reasons::ROOT_OFFLINE.into());
            }
            if matches!(
                &spec.identity,
                RootIdentity::Network {
                    volume_serial: None,
                    ..
                }
            ) {
                return Err(reasons::IDENTITY_UNKNOWN.into());
            }
            if canonical != spec.path || identity != spec.identity {
                Err(reasons::IDENTITY_MISMATCH.into())
            } else {
                Ok(())
            }
        }
        RootProbeOutcome::Checking => Err("checking".into()),
        RootProbeOutcome::Missing | RootProbeOutcome::Unavailable => Err(if network {
            reasons::ROOT_OFFLINE
        } else {
            reasons::ROOT_MISSING
        }
        .into()),
        RootProbeOutcome::Invalid => Err(reasons::PROFILE_INVALID.into()),
    }
}

/// Readiness of one profile, computed by the backend from live probes and stored identities
/// (SP-8; design §7.1: ≤ 2 s budget per root, slower roots report `checking`).
pub fn readiness_dto(
    policy: &ProfilePolicy,
    probe: &ProbeArc<dyn RootProbe>,
    budget: Duration,
) -> ReadinessDto {
    let mut first_problem: Option<String> = None;
    let mut checking = false;
    for spec in [&policy.left, &policy.right] {
        match root_verdict(spec, probe, budget) {
            Ok(()) => {}
            Err(reason) if reason == "checking" => checking = true,
            Err(reason) => {
                first_problem.get_or_insert(reason);
            }
        }
    }
    // SP-3.1 stays visible after saving: a Recycle Bin policy on a side that is not a local
    // fixed drive keeps the profile "not ready".
    if first_problem.is_none()
        && !checking
        && matches!(policy.delete_policy, model::DeletePolicy::RecycleBin)
    {
        for spec in model::receiving_sides(&policy.mode) {
            let identity = match spec {
                model::Side::Left => &policy.left.identity,
                model::Side::Right => &policy.right.identity,
            };
            if !matches!(
                identity,
                RootIdentity::Local {
                    drive: model::DriveKind::Fixed,
                    ..
                }
            ) {
                first_problem.get_or_insert(reasons::RECYCLE_BIN_UNAVAILABLE.into());
            }
        }
    }
    if first_problem.is_none()
        && !checking
        && let model::DeletePolicy::Versioning { root } = &policy.delete_policy
    {
        // The versioning root must still be a plausible absolute folder; it is created on
        // first use, so existence is not required (SP-3.2).
        if crate::sync_engine::roots::canonical(root).is_err() {
            first_problem.get_or_insert(reasons::VERSIONING_ROOT_INVALID.into());
        }
    }
    if checking {
        return ReadinessDto::Checking;
    }
    match first_problem {
        Some(reason) => ReadinessDto::NotReady { reason },
        None => ReadinessDto::Ready,
    }
}

fn policy_draft_of(policy: &ProfilePolicy) -> PolicyDraft {
    let (mode, from, propagate_deletes) = match &policy.mode {
        model::SyncMode::OneWay { from } => (WireMode::OneWay, Some(side_wire(*from)), None),
        model::SyncMode::Mirror { from } => (WireMode::Mirror, Some(side_wire(*from)), None),
        model::SyncMode::TwoWay { propagate_deletes } => {
            (WireMode::TwoWay, None, Some(*propagate_deletes))
        }
    };
    let (delete_policy, versioning_root) = match &policy.delete_policy {
        model::DeletePolicy::RecycleBin => (WireDeletePolicy::RecycleBin, None),
        model::DeletePolicy::Versioning { root } => {
            (WireDeletePolicy::Versioning, Some(root.clone()))
        }
        model::DeletePolicy::Permanent => (WireDeletePolicy::Permanent, None),
    };
    PolicyDraft {
        left: policy.left.path.clone(),
        right: policy.right.path.clone(),
        mode,
        from,
        propagate_deletes,
        delete_policy,
        versioning_root,
        compare: match policy.compare {
            model::CompareMode::SizeAndDate => WireCompare::SizeAndDate,
            model::CompareMode::Content => WireCompare::Content,
        },
        include: policy.filters.include.clone(),
        exclude: policy.filters.exclude.clone(),
        guard: model::DraftGuard {
            enabled: policy.guard.enabled,
            max_count: policy.guard.max_count,
            max_percent: policy.guard.max_percent,
        },
    }
}

fn side_wire(side: model::Side) -> WireSide {
    match side {
        model::Side::Left => WireSide::Left,
        model::Side::Right => WireSide::Right,
    }
}

fn parse_policy(json: &str) -> Result<ProfilePolicy, CommandError> {
    serde_json::from_str(json).map_err(|_| serde_broken())
}

fn summary_of(
    rec: &profiles::ProfileRecord,
    policy: &ProfilePolicy,
    readiness: ReadinessDto,
) -> ProfileSummaryDto {
    ProfileSummaryDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        revision: rec.revision,
        left: policy.left.path.clone(),
        right: policy.right.path.clone(),
        mode_label: model::mode_label(&policy.mode),
        delete_label: model::delete_label(&policy.delete_policy),
        readiness,
        used_by: 0, // P1.1: automations using this profile (SP-9.5).
    }
}

/// What the store gives for one profile; readiness is added AFTER the store lock is released
/// (a live probe can take seconds and must never block other store users).
struct LoadedProfile {
    rec: profiles::ProfileRecord,
    policy: ProfilePolicy,
    changes: Vec<RevisionChangesDto>,
}

fn load_profile(conn: &rusqlite::Connection, id: &str) -> Result<LoadedProfile, CommandError> {
    let rec = profiles::get(conn, id)?.ok_or(CommandError::from(StoreError::NotFound))?;
    let policy = parse_policy(&rec.policy)?;
    let changes = profiles::revisions(conn, &rec.id)?
        .into_iter()
        .map(|r| RevisionChangesDto {
            revision: r.revision,
            changed_ms: r.changed_ms,
            changes: serde_json::from_str(&r.changes).unwrap_or_default(),
        })
        .collect();
    Ok(LoadedProfile {
        rec,
        policy,
        changes,
    })
}

fn dto_of(loaded: LoadedProfile, probe: &ProbeArc<dyn RootProbe>) -> ProfileDto {
    let readiness = readiness_dto(&loaded.policy, probe, READINESS_BUDGET);
    ProfileDto {
        id: loaded.rec.id.clone(),
        name: loaded.rec.name.clone(),
        revision: loaded.rec.revision,
        policy: policy_draft_of(&loaded.policy),
        identities: IdentitiesDto {
            left: Some(loaded.policy.left.identity.clone()),
            right: Some(loaded.policy.right.identity.clone()),
        },
        changes: loaded.changes,
        created_ms: loaded.rec.created_ms,
        updated_ms: loaded.rec.updated_ms,
        readiness,
    }
}

// ── Tauri-free command cores (test seam, design §13.1) ──

/// Profiles per wave of parallel readiness probes (each probe is bounded by the shared worker
/// cap in `sync_engine::roots`).
const READINESS_WAVE: usize = 8;

pub fn list_in(
    store: &Store,
    probe: &ProbeArc<dyn RootProbe>,
) -> Result<Vec<ProfileSummaryDto>, CommandError> {
    // The store lock is held only for the read; the live probes run after it is released.
    let records = store.with_read(profiles::list)?;
    let mut out = Vec::with_capacity(records.len());
    for wave in records.chunks(READINESS_WAVE) {
        let summaries: Vec<ProfileSummaryDto> = std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|rec| scope.spawn(move || summary_row(rec, probe)))
                .collect();
            handles
                .into_iter()
                .zip(wave)
                .map(|(handle, rec)| handle.join().unwrap_or_else(|_| invalid_summary(rec)))
                .collect()
        });
        out.extend(summaries);
    }
    Ok(out)
}

fn summary_row(
    rec: &profiles::ProfileRecord,
    probe: &ProbeArc<dyn RootProbe>,
) -> ProfileSummaryDto {
    match parse_policy(&rec.policy) {
        Ok(policy) => {
            let readiness = readiness_dto(&policy, probe, READINESS_BUDGET);
            summary_of(rec, &policy, readiness)
        }
        // An invalid stored row is never fixed silently (design §4.4): it shows as a profile
        // that needs review.
        Err(_) => invalid_summary(rec),
    }
}

fn invalid_summary(rec: &profiles::ProfileRecord) -> ProfileSummaryDto {
    ProfileSummaryDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        revision: rec.revision,
        left: String::new(),
        right: String::new(),
        mode_label: String::new(),
        delete_label: String::new(),
        readiness: ReadinessDto::NotReady {
            reason: reasons::PROFILE_INVALID.into(),
        },
        used_by: 0,
    }
}

pub fn get_in(
    store: &Store,
    id: &str,
    probe: &ProbeArc<dyn RootProbe>,
) -> Result<ProfileDto, CommandError> {
    require_profile_id(id)?;
    let loaded = store.with_read(|conn| load_profile(conn, id))?;
    Ok(dto_of(loaded, probe))
}

/// Validate a draft with no side effects (design §7.1). Also reports the identities and drive
/// kinds the backend probed, for the editor's captions.
pub fn validate_in(draft: Draft, probe: &dyn RootProbe) -> ValidateResultDto {
    let mut identities = IdentitiesDto::default();
    let mut kinds = KindsDto::default();
    match draft.validate(probe) {
        Ok(validated) => {
            identities.left = Some(validated.policy.left.identity.clone());
            identities.right = Some(validated.policy.right.identity.clone());
            kinds.left = Some(kind_of(&validated.policy.left.identity));
            kinds.right = Some(kind_of(&validated.policy.right.identity));
            ValidateResultDto {
                errors: Vec::new(),
                identities,
                kinds,
            }
        }
        Err(errors) => ValidateResultDto {
            errors,
            identities,
            kinds,
        },
    }
}

/// The input of a save, assembled from the command's separate IPC parameters
/// (`id`, `expectedRevision`, `draft` - the documented wire shape). `deny_unknown_fields` is
/// part of SE-1 for any caller that deserializes the whole request at once.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    pub draft: Draft,
}

/// The answer of a native confirmation as the command core sees it. `Busy` = another native
/// dialog is open: nothing was shown and nothing is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAnswer {
    Yes,
    No,
    Busy,
}

impl From<bool> for ConfirmAnswer {
    fn from(yes: bool) -> Self {
        if yes { Self::Yes } else { Self::No }
    }
}

impl From<Result<bool, crate::domain::FsError>> for ConfirmAnswer {
    fn from(answer: Result<bool, crate::domain::FsError>) -> Self {
        match answer {
            Ok(yes) => yes.into(),
            Err(e) if e.code == crate::domain::ErrorCode::Busy => Self::Busy,
            Err(_) => Self::No,
        }
    }
}

/// SP-3.3 + SP-5.1: a native confirmation is needed when the policy deletes `permanent` (new
/// profile, or changed to it) and when the delete guard is turned off.
pub fn confirm_needed(is_new: bool, old: Option<&ProfilePolicy>, new: &ProfilePolicy) -> bool {
    let permanent_changed = matches!(new.delete_policy, model::DeletePolicy::Permanent)
        && (is_new
            || old.is_none_or(|o| !matches!(o.delete_policy, model::DeletePolicy::Permanent)));
    let guard_turned_off = !new.guard.enabled && (is_new || old.is_none_or(|o| o.guard.enabled));
    permanent_changed || guard_turned_off
}

/// Save (create or update). Order: id shape → store writable → validate → existence and stale
/// check → native confirm when needed → store in one transaction with the revision re-checked
/// inside (design §8.2 shape). Every refusal that does not depend on the user's answer happens
/// BEFORE the dialog, so nobody is asked about a change that cannot happen.
pub fn save_in<A: Into<ConfirmAnswer>>(
    store: &Store,
    req: SaveRequest,
    probe: &ProbeArc<dyn RootProbe>,
    confirm: impl FnOnce(&Prompt) -> A,
    now_ms: i64,
) -> Result<SaveResultDto, CommandError> {
    if let Some(id) = &req.id {
        require_profile_id(id)?;
    }
    // LC-3: a read-only or unavailable store refuses before any probe or dialog.
    store.check_writable()?;
    let validated = req
        .draft
        .validate(probe.as_ref())
        .map_err(|e| CommandError::invalid(&e))?;
    let existing = store.with_read(
        |conn| -> Result<Option<profiles::ProfileRecord>, StoreError> {
            match &req.id {
                Some(id) => profiles::get(conn, id),
                None => Ok(None),
            }
        },
    )?;
    // An update of a profile that does not exist is refused before any dialog.
    if req.id.is_some() && existing.is_none() {
        return Err(StoreError::NotFound.into());
    }
    // SP-6.2 stale check before the dialog: no native prompt for a dead edit.
    if let Some(rec) = &existing
        && req.expected_revision != Some(rec.revision)
    {
        return Ok(SaveResultDto::Stale);
    }
    let old_policy = match &existing {
        Some(rec) => Some(parse_policy(&rec.policy)?),
        None => None,
    };
    if confirm_needed(existing.is_none(), old_policy.as_ref(), &validated.policy) {
        // The text is built from the validated values only (no control/format characters).
        let prompt = confirm_text::save_profile_prompt(
            &validated.name,
            existing.as_ref().map(|r| r.revision),
            &validated.policy,
        );
        match confirm(&prompt).into() {
            ConfirmAnswer::Yes => {}
            // Cancel = nothing saved (SP-3.3).
            ConfirmAnswer::No => return Ok(SaveResultDto::Canceled),
            ConfirmAnswer::Busy => {
                return Err(CommandError::new(
                    "busy",
                    "Another confirmation is open; nothing was saved",
                ));
            }
        }
    }
    let id = match &req.id {
        Some(id) => id.clone(),
        None => model::new_profile_id().ok_or_else(internal)?,
    };
    let policy_json = serde_json::to_string(&validated.policy).map_err(|_| internal())?;
    let digest = policy_digest(&validated.policy);
    let changes = old_policy
        .as_ref()
        .map(|old| model::field_changes(old, &validated.policy))
        .unwrap_or_default();
    store.with_write(|conn| -> Result<i64, StoreError> {
        match &req.id {
            Some(_) => profiles::update(
                conn,
                &profiles::UpdateProfile {
                    id: id.clone(),
                    // Re-checked inside the transaction: the profile may have changed while
                    // the dialog was open (design §8.2 shape).
                    expected_revision: req.expected_revision.unwrap_or(0),
                    name: validated.name.clone(),
                    policy: policy_json.clone(),
                    digest: digest.clone(),
                    changes: serde_json::to_string(&changes).unwrap_or_else(|_| "[]".into()),
                    policy_changed: !changes.is_empty(),
                    now_ms,
                },
            ),
            None => profiles::create(
                conn,
                &profiles::NewProfile {
                    id: id.clone(),
                    name: validated.name.clone(),
                    policy: policy_json.clone(),
                    digest,
                    now_ms,
                },
            )
            .map(|()| 1),
        }
    })?;
    let loaded = store.with_read(|conn| load_profile(conn, &id))?;
    Ok(SaveResultDto::Saved {
        profile: Box::new(dto_of(loaded, probe)),
    })
}

pub fn delete_in(
    store: &Store,
    id: &str,
    expected_revision: i64,
) -> Result<DeleteResultDto, CommandError> {
    require_profile_id(id)?;
    store.with_write(|conn| profiles::delete(conn, id, expected_revision))?;
    Ok(DeleteResultDto {
        status: "deleted".into(),
    })
}

/// Longest folder path accepted by the lease commands (characters).
const MAX_LEASE_PATH_CHARS: usize = 4096;

/// `sync_lease_acquire` core: canonicalize both roots (real path, mapped drive resolved, design
/// §5.8) and take a manual lease on them. Refusal because the folders are in use is a normal
/// result (`busy`), not an error. The canonicalizer is injected for tests.
pub fn lease_acquire_in(
    store: &Arc<Store>,
    registry: &LeaseRegistry,
    left: &str,
    right: &str,
    canonical: &dyn Fn(&str) -> Result<String, RootError>,
) -> Result<LeaseAcquireDto, CommandError> {
    let bad_path = || {
        CommandError::new(
            "invalid_input",
            "The folder path is not valid for a sync lease",
        )
    };
    for path in [left, right] {
        if path.is_empty() || path.chars().count() > MAX_LEASE_PATH_CHARS {
            return Err(bad_path());
        }
    }
    // Refuse before any lookup when the store cannot be written (read-only newer schema, or
    // unusable): without a lease no manual sync may run.
    store.check_writable()?;
    let roots = [
        canonical(left).map_err(|_| bad_path())?,
        canonical(right).map_err(|_| bad_path())?,
    ];
    match registry.acquire_manual(store, &roots)? {
        ManualAcquire::Acquired { lease_id } => {
            Ok(LeaseAcquireDto::Acquired(LeaseAcquired { lease_id }))
        }
        ManualAcquire::Busy(holder) => Ok(LeaseAcquireDto::Busy(LeaseBusy {
            status: LeaseBusyStatus::Busy,
            holder: LeaseHolderDto {
                kind: holder.kind.as_str().to_string(),
                run_id: holder.run_id,
            },
        })),
        ManualAcquire::TooMany => Err(CommandError::new(
            "lease_busy",
            "Too many folder pairs are being synchronized at once",
        )),
        ManualAcquire::Reloaded => Err(CommandError::new(
            "lease_busy",
            "The page was reloaded while the folders were being locked; try again",
        )),
    }
}

/// The webview (re)loaded its page: whatever JavaScript held manual lease tokens is gone, so
/// every manual lease of this process is released. Wired to the page-load-started event in
/// `lib.rs`. Returns how many leases were released.
pub fn release_manual_leases_on_page_load(state: &AutomationState) -> usize {
    state.leases.release_all()
}

/// Hook variant of [`release_manual_leases_on_page_load`] for the WebView2 UI thread: only
/// drains the registry (no I/O) and hands the guards to a blocking worker, because dropping a
/// guard joins its heartbeat thread and deletes its row (busy timeout up to 5 s per lease).
/// Returns how many leases were handed off.
pub fn release_manual_leases_on_page_load_async(state: &AutomationState) -> usize {
    let guards = state.leases.take_all();
    let n = guards.len();
    if n > 0 {
        tauri::async_runtime::spawn_blocking(move || drop(guards));
    }
    n
}

/// `sync_lease_release` core: idempotent. Only a token this process was given releases
/// anything; any other value does nothing and looks the same (no existence oracle).
pub fn lease_release_in(
    registry: &LeaseRegistry,
    lease_id: &str,
) -> Result<LeaseReleaseDto, CommandError> {
    if !crate::automation::store::leases::is_lease_id(lease_id) {
        return Err(CommandError::new(
            "invalid_input",
            "The lease id is not a valid identifier",
        ));
    }
    registry.release(lease_id);
    Ok(LeaseReleaseDto {})
}

fn require_profile_id(id: &str) -> Result<(), CommandError> {
    if model::is_profile_id(id) {
        Ok(())
    } else {
        Err(CommandError::new(
            "invalid_input",
            "The profile id is not a valid identifier",
        ))
    }
}

// ── Tauri command wrappers ──

#[cfg(windows)]
fn owner_hwnd<R: tauri::Runtime>(window: &tauri::Window<R>) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(not(windows))]
fn owner_hwnd<R: tauri::Runtime>(_window: &tauri::Window<R>) -> isize {
    0
}

#[tauri::command]
pub async fn sync_profile_list(
    state: State<'_, AutomationState>,
) -> Result<Vec<ProfileSummaryDto>, CommandError> {
    let store = state.store.clone();
    let probe: Arc<dyn RootProbe> = Arc::new(FsProbe);
    tauri::async_runtime::spawn_blocking(move || list_in(&store, &probe))
        .await
        .map_err(|_| internal())?
}

#[tauri::command]
pub async fn sync_profile_get(
    state: State<'_, AutomationState>,
    id: String,
) -> Result<ProfileDto, CommandError> {
    let store = state.store.clone();
    let probe: Arc<dyn RootProbe> = Arc::new(FsProbe);
    tauri::async_runtime::spawn_blocking(move || get_in(&store, &id, &probe))
        .await
        .map_err(|_| internal())?
}

#[tauri::command]
pub async fn sync_profile_validate(draft: Draft) -> Result<ValidateResultDto, CommandError> {
    // No side effects: probes only, no store, no dialog.
    tauri::async_runtime::spawn_blocking(move || validate_in(draft, &FsProbe))
        .await
        .map_err(|_| internal())
}

/// `sync_profile_save` takes the documented top-level IPC arguments `{ id?, expectedRevision?,
/// draft }` (docs/ipc.md; contract fixture `sync-profile-save.request.json` is read by Rust and
/// TypeScript tests). Tauri reads each parameter by its camelCase name; unknown extra keys are
/// ignored and never interpreted, in particular nothing can act as a `confirmed` flag (SE-1).
#[tauri::command]
pub async fn sync_profile_save<R: tauri::Runtime>(
    state: State<'_, AutomationState>,
    window: tauri::Window<R>,
    id: Option<String>,
    expected_revision: Option<i64>,
    draft: Draft,
) -> Result<SaveResultDto, CommandError> {
    let store = state.store.clone();
    let req = SaveRequest {
        id,
        expected_revision,
        draft,
    };
    // The whole flow (validation probes + the native dialog + the store transaction) runs off
    // the async workers; the dialog is owned by the Ferrum window (SE-2).
    let owner = owner_hwnd(&window);
    let probe: Arc<dyn RootProbe> = Arc::new(FsProbe);
    tauri::async_runtime::spawn_blocking(move || {
        save_in(
            &store,
            req,
            &probe,
            |prompt| crate::native_dialog::confirm_or_busy(owner, prompt.caption, &prompt.text),
            now_ms(),
        )
    })
    .await
    .map_err(|_| internal())?
}

#[tauri::command]
pub async fn sync_profile_delete(
    state: State<'_, AutomationState>,
    id: String,
    expected_revision: i64,
) -> Result<DeleteResultDto, CommandError> {
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || delete_in(&store, &id, expected_revision))
        .await
        .map_err(|_| internal())?
}

/// Take a manual lease on a folder pair before a Synchronize Dirs run (design §3.4). The
/// arguments are the two roots as the compare reported them.
#[tauri::command]
pub async fn sync_lease_acquire(
    state: State<'_, AutomationState>,
    left: String,
    right: String,
) -> Result<LeaseAcquireDto, CommandError> {
    let store = state.store.clone();
    let registry = state.leases.clone();
    tauri::async_runtime::spawn_blocking(move || {
        lease_acquire_in(&store, &registry, &left, &right, &|p| roots::canonical(p))
    })
    .await
    .map_err(|_| internal())?
}

/// Release a manual lease by its token. Idempotent.
#[tauri::command]
pub async fn sync_lease_release(
    state: State<'_, AutomationState>,
    lease_id: String,
) -> Result<LeaseReleaseDto, CommandError> {
    let registry = state.leases.clone();
    tauri::async_runtime::spawn_blocking(move || lease_release_in(&registry, &lease_id))
        .await
        .map_err(|_| internal())?
}

/// Whether profile runs may skip content reads with the indexer journal (IX-1): the user's
/// Settings flag, read here too so the webview alone can never turn it on.
fn journal_skip_enabled<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    profile::config_dir(app)
        .map(|d| crate::prefs::load_from(&d).sync_journal_skip)
        .unwrap_or(false)
}

/// Read-only preview of a profile run (engine dry run): the plan summary, held conflicts and the
/// delete guard verdict. Touches nothing and needs no lease. The answer carries a `previewId`
/// that `sync_profile_run` requires.
#[tauri::command]
pub async fn sync_profile_preview<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AutomationState>,
    baselines: State<'_, crate::sync_baseline::SyncBaselineStore>,
    previews: State<'_, manual_run::PreviewBoard>,
    id: String,
) -> Result<manual_run::PreviewDto, CommandError> {
    let store = state.store.clone();
    let baselines = baselines.inner().clone();
    let board = previews.inner().clone();
    let incremental = journal_skip_enabled(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let env = crate::sync_engine::env::EngineEnv::production();
        manual_run::preview_in(
            &store,
            &baselines,
            &board,
            &env,
            &crate::sync_engine::NoSink,
            &id,
            incremental,
            now_ms(),
        )
    })
    .await
    .map_err(|_| internal())?
}

/// Forwards the engine's phase and (throttled) count changes to the webview.
struct RunEvents<R: tauri::Runtime> {
    app: tauri::AppHandle<R>,
    run_id: i64,
}

impl<R: tauri::Runtime> RunEvents<R> {
    fn send(&self, event: manual_run::RunEventDto) {
        use tauri::Emitter;
        let _ = self.app.emit(manual_run::RUN_EVENT, event);
    }
}

impl<R: tauri::Runtime> crate::sync_engine::EngineSink for RunEvents<R> {
    fn phase(&self, phase: crate::sync_engine::Phase) {
        self.send(manual_run::RunEventDto {
            run_id: self.run_id,
            phase: Some(phase),
            counts: None,
            result: None,
        });
    }

    fn counts(&self, counts: &crate::sync_engine::Counts) {
        self.send(manual_run::RunEventDto {
            run_id: self.run_id,
            phase: None,
            counts: Some(*counts),
            result: None,
        });
    }
}

/// Start a manual run of a previewed profile. Returns as soon as the checks passed and the run
/// holds its folder lease; the run itself continues on its own thread and reports through the
/// `sync-profile://run` event (phase, counts, and a final result).
///
/// A run with `permanent` deletes is confirmed here, by a native dialog, per run (SE-1): the
/// webview cannot say "already confirmed".
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn sync_profile_run<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AutomationState>,
    window: tauri::Window<R>,
    baselines: State<'_, crate::sync_baseline::SyncBaselineStore>,
    previews: State<'_, manual_run::PreviewBoard>,
    id: String,
    preview_id: String,
    confirm_overwrite: bool,
) -> Result<manual_run::RunStartedDto, CommandError> {
    let store = state.store.clone();
    let lease_env = state.leases.env().clone();
    let baselines = baselines.inner().clone();
    let board = previews.inner().clone();
    let owner = owner_hwnd(&window);
    let incremental = journal_skip_enabled(&app);
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        let env = crate::sync_engine::env::EngineEnv::production();
        manual_run::start_run_in(
            &store,
            &board,
            &lease_env,
            &env,
            &id,
            &preview_id,
            confirm_overwrite,
            incremental,
            |prompt| crate::native_dialog::confirm_or_busy(owner, prompt.caption, &prompt.text),
            now_ms(),
        )
    })
    .await
    .map_err(|_| internal())??;

    let run_id = prepared.run_id;
    let events = RunEvents {
        app: app.clone(),
        run_id,
    };
    // The lease lives in `prepared` and is released when the run thread ends; if the thread
    // cannot even start, the closure (and the lease) is dropped right here.
    std::thread::Builder::new()
        .name("ferrum-sync-run".into())
        .spawn(move || {
            let env = crate::sync_engine::env::EngineEnv::production();
            let report = prepared.execute(&baselines, &env, &events);
            events.send(manual_run::RunEventDto {
                run_id,
                phase: None,
                counts: None,
                result: Some(manual_run::result_dto(&report)),
            });
        })
        .map_err(|_| CommandError::new("store_unavailable", "The run could not be started"))?;
    Ok(manual_run::RunStartedDto { run_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::model::{DraftGuard, WireCompare, WireDeletePolicy, WireMode, WireSide};
    use crate::automation::store::Store;
    use std::cell::Cell;

    fn draft(name: &str, left: &str, right: &str) -> Draft {
        Draft {
            name: name.into(),
            policy: PolicyDraft {
                left: left.into(),
                right: right.into(),
                mode: WireMode::OneWay,
                from: Some(WireSide::Left),
                propagate_deletes: None,
                delete_policy: WireDeletePolicy::RecycleBin,
                versioning_root: None,
                compare: WireCompare::SizeAndDate,
                include: vec![],
                exclude: vec![],
                guard: DraftGuard {
                    enabled: true,
                    max_count: 50,
                    max_percent: 20,
                },
            },
        }
    }

    fn permanent(mut d: Draft) -> Draft {
        d.policy.delete_policy = WireDeletePolicy::Permanent;
        d
    }

    fn guard_off(mut d: Draft) -> Draft {
        d.policy.guard.enabled = false;
        d
    }

    fn probe() -> Arc<dyn RootProbe> {
        Arc::new(FsProbe)
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        (dir, store)
    }

    fn temp_draft(dir: &tempfile::TempDir, name: &str) -> Draft {
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        draft(name, &left.to_string_lossy(), &right.to_string_lossy())
    }

    /// SE-1 / SP-3.3 (injectable confirm): a permanent save with the answer `false` stores
    /// nothing; with `true` the profile is stored. The webview never gets a say beyond that.
    #[test]
    fn se_1_permanent_save_asks_and_cancels_or_saves() {
        let (dir, store) = store();
        let asked = Cell::new(0);
        // Answer no: canceled, nothing stored.
        let out = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: permanent(temp_draft(&dir, "P")),
            },
            &probe(),
            |prompt| {
                asked.set(asked.get() + 1);
                assert!(prompt.text.starts_with(confirm_text::PERMANENT_WARNING));
                false
            },
            1_000,
        )
        .unwrap();
        assert_eq!(out, SaveResultDto::Canceled);
        assert_eq!(asked.get(), 1);
        assert!(list_in(&store, &probe()).unwrap().is_empty());
        // Answer yes: saved, revision 1, permanent policy recorded with its identity.
        let out = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: permanent(temp_draft(&dir, "P")),
            },
            &probe(),
            |_| {
                asked.set(asked.get() + 1);
                true
            },
            1_000,
        )
        .unwrap();
        let SaveResultDto::Saved { profile } = out else {
            panic!("expected saved");
        };
        assert_eq!(profile.revision, 1);
        assert_eq!(profile.policy.delete_policy, WireDeletePolicy::Permanent);
        assert!(model::is_profile_id(&profile.id));
        assert!(matches!(profile.readiness, ReadinessDto::Ready));
    }

    /// SE-1 / SP-5.1: a save that turns the guard off needs the native confirmation; saves that
    /// need none never call it.
    #[test]
    fn sp_5_1_guard_off_asks_and_plain_saves_never_ask() {
        let (dir, store) = store();
        // First save (guard on, recycle bin): no dialog.
        let first = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: temp_draft(&dir, "P"),
            },
            &probe(),
            |_| -> bool { panic!("a plain save must not ask") },
            1_000,
        )
        .unwrap();
        let SaveResultDto::Saved { profile } = first else {
            panic!("expected saved");
        };
        // Guard off: asked; false → nothing changed.
        let asked = Cell::new(0);
        let out = save_in(
            &store,
            SaveRequest {
                id: Some(profile.id.clone()),
                expected_revision: Some(1),
                draft: guard_off(temp_draft(&dir, "P")),
            },
            &probe(),
            |_| {
                asked.set(1);
                false
            },
            2_000,
        )
        .unwrap();
        assert_eq!(out, SaveResultDto::Canceled);
        assert_eq!(asked.get(), 1);
        assert_eq!(get_in(&store, &profile.id, &probe()).unwrap().revision, 1);
        // true → saved, revision bumped (guard is a policy field, SP-6.1).
        let out = save_in(
            &store,
            SaveRequest {
                id: Some(profile.id.clone()),
                expected_revision: Some(1),
                draft: guard_off(temp_draft(&dir, "P")),
            },
            &probe(),
            |_| true,
            3_000,
        )
        .unwrap();
        let SaveResultDto::Saved { profile } = out else {
            panic!("expected saved");
        };
        assert_eq!(profile.revision, 2);
        assert!(!profile.policy.guard.enabled);
    }

    /// SE-1: no command input can behave like a `confirmed` flag — the request shape refuses
    /// unknown fields.
    #[test]
    fn se_1_no_confirmed_field_is_accepted() {
        let base = serde_json::json!({
            "id": null,
            "expectedRevision": null,
            "draft": draft("n", "C:\\A", "D:\\B"),
        });
        assert!(serde_json::from_value::<SaveRequest>(base.clone()).is_ok());
        for extra in ["confirmed", "approved", "userConfirmed"] {
            let mut sneaky = base.clone();
            sneaky[extra] = serde_json::json!(true);
            assert!(
                serde_json::from_value::<SaveRequest>(sneaky).is_err(),
                "{extra}"
            );
        }
    }

    /// SP-6.2: saving against a stale revision returns `stale` without touching the store or
    /// asking the user anything.
    #[test]
    fn sp_6_2_save_with_wrong_expected_revision_is_stale() {
        let (dir, store) = store();
        let saved = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: temp_draft(&dir, "P"),
            },
            &probe(),
            |_| -> bool { panic!("no confirm needed") },
            1_000,
        )
        .unwrap();
        let SaveResultDto::Saved { profile } = saved else {
            panic!("expected saved");
        };
        let out = save_in(
            &store,
            SaveRequest {
                id: Some(profile.id.clone()),
                expected_revision: Some(99),
                draft: permanent(temp_draft(&dir, "P")),
            },
            &probe(),
            |_| -> bool { panic!("stale must not ask") },
            2_000,
        )
        .unwrap();
        assert_eq!(out, SaveResultDto::Stale);
    }

    /// SP-6.4 call site / SP-9.5 readiness: the in-use check stays the single delete gate.
    #[test]
    fn delete_reports_missing_and_stale_and_deletes() {
        let (dir, store) = store();
        let saved = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: temp_draft(&dir, "P"),
            },
            &probe(),
            |_| -> bool { panic!("no confirm needed") },
            1_000,
        )
        .unwrap();
        let SaveResultDto::Saved { profile } = saved else {
            panic!("expected saved");
        };
        assert_eq!(
            delete_in(&store, &profile.id, 99).unwrap_err(),
            CommandError::from(StoreError::Stale)
        );
        assert_eq!(
            delete_in(&store, &profile.id, 1).unwrap(),
            DeleteResultDto {
                status: "deleted".into()
            }
        );
        assert_eq!(
            delete_in(&store, &profile.id, 1).unwrap_err(),
            CommandError::from(StoreError::NotFound)
        );
    }

    /// SE-7: ids are validated before any lookup; invalid ones never reach the store.
    #[test]
    fn se_7_invalid_ids_are_refused_before_lookup() {
        let (_dir, store) = store();
        let err = get_in(&store, "../etc", &probe()).unwrap_err();
        assert_eq!(err.code, "invalid_input");
        let err = delete_in(&store, "not-a-uuid", 1).unwrap_err();
        assert_eq!(err.code, "invalid_input");
    }

    /// SP-7.1 + SP-7.3: the identity is recorded on save; a folder deleted and re-created at the
    /// same path makes readiness report `identityMismatch` (never "treat as empty").
    #[test]
    fn sp_7_identity_is_recorded_and_a_recreated_folder_is_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        let store = Store::open(&dir.path().join("automation"), "0.2.0");
        let saved = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: draft("P", &left.to_string_lossy(), &right.to_string_lossy()),
            },
            &probe(),
            |_| -> bool { panic!("no confirm needed") },
            1_000,
        )
        .unwrap();
        let SaveResultDto::Saved { profile } = saved else {
            panic!("expected saved");
        };
        let left_identity = profile.identities.left.clone().unwrap();
        let RootIdentity::Local { file_id, .. } = &left_identity else {
            panic!("local identity recorded");
        };
        assert_eq!(file_id.len(), 32);
        assert_eq!(profile.readiness, ReadinessDto::Ready);

        // Delete + recreate the left folder: same path, different folder object.
        std::fs::remove_dir_all(&left).unwrap();
        std::fs::create_dir_all(&left).unwrap();
        let after = get_in(&store, &profile.id, &probe()).unwrap();
        assert_eq!(
            after.readiness,
            ReadinessDto::NotReady {
                reason: "identityMismatch".into()
            }
        );
    }

    /// SP-8: a root that answers slower than the budget reports `checking`, never a false
    /// "ready" or "missing".
    #[test]
    fn sp_8_a_slow_root_reports_checking() {
        struct SlowProbe;
        impl RootProbe for SlowProbe {
            fn canonical(&self, _p: &str) -> Result<String, crate::sync_engine::roots::RootError> {
                std::thread::sleep(Duration::from_millis(50));
                Ok(r"C:\slow".into())
            }
            fn identity(
                &self,
                _c: &str,
            ) -> Result<RootIdentity, crate::sync_engine::roots::RootError> {
                std::thread::sleep(Duration::from_millis(50));
                Ok(RootIdentity::Local {
                    volume_serial: "aa".into(),
                    file_id: "b".repeat(32),
                    fs_name: "NTFS".into(),
                    drive: model::DriveKind::Fixed,
                })
            }
        }
        // A draft still validates (validation has no budget), but readiness with a tiny budget
        // reports checking.
        let policy = ProfilePolicy {
            left: model::RootSpec {
                path: r"C:\slow".into(),
                identity: RootIdentity::Local {
                    volume_serial: "aa".into(),
                    file_id: "b".repeat(32),
                    fs_name: "NTFS".into(),
                    drive: model::DriveKind::Fixed,
                },
            },
            right: model::RootSpec {
                path: r"C:\other".into(),
                identity: RootIdentity::Local {
                    volume_serial: "aa".into(),
                    file_id: "c".repeat(32),
                    fs_name: "NTFS".into(),
                    drive: model::DriveKind::Fixed,
                },
            },
            mode: model::SyncMode::OneWay {
                from: model::Side::Left,
            },
            delete_policy: model::DeletePolicy::RecycleBin,
            compare: model::CompareMode::SizeAndDate,
            filters: model::Filters {
                include: vec![],
                exclude: vec![],
            },
            guard: model::DeleteGuard {
                enabled: true,
                max_count: 50,
                max_percent: 20,
            },
        };
        let probe: Arc<dyn RootProbe> = Arc::new(SlowProbe);
        // Fresh identity differs from the stored one → mismatch (probe budget large enough).
        assert_eq!(
            readiness_dto(&policy, &probe, Duration::from_secs(5)),
            ReadinessDto::NotReady {
                reason: "identityMismatch".into()
            }
        );
        // Budget smaller than the probe's latency → checking.
        assert_eq!(
            readiness_dto(&policy, &probe, Duration::from_millis(1)),
            ReadinessDto::Checking
        );
    }

    /// SP-1.3 at the command level: a name already in use is an inline `invalid_input` with the
    /// `name_taken` field error, and nothing is stored.
    #[test]
    fn sp_1_3_duplicate_names_are_refused_inline() {
        let (dir, store) = store();
        save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: temp_draft(&dir, "Backup"),
            },
            &probe(),
            |_| -> bool { panic!("no confirm needed") },
            1_000,
        )
        .unwrap();
        let err = save_in(
            &store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: temp_draft(&dir, "backup"),
            },
            &probe(),
            |_| -> bool { panic!("no confirm needed") },
            1_000,
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_input");
        assert_eq!(list_in(&store, &probe()).unwrap().len(), 1);
    }

    /// The command error mapping stays inside the closed code set of design §7.
    #[test]
    fn store_errors_map_to_closed_codes_without_paths() {
        assert_eq!(
            CommandError::from(StoreError::ReadOnly).code,
            "store_read_only"
        );
        assert_eq!(
            CommandError::from(StoreError::Untrusted).code,
            "store_untrusted"
        );
        assert_eq!(
            CommandError::from(StoreError::Unavailable).code,
            "store_unavailable"
        );
        assert_eq!(CommandError::from(StoreError::NotFound).code, "not_found");
        assert_eq!(CommandError::from(StoreError::Stale).code, "stale");
        assert_eq!(CommandError::from(StoreError::InUse).code, "in_use");
        assert_eq!(
            CommandError::from(StoreError::NameTaken).code,
            "invalid_input"
        );
        for e in [
            CommandError::from(StoreError::Unavailable),
            CommandError::new("invalid_input", "x"),
        ] {
            assert!(
                !e.message.contains('\\'),
                "no paths in messages: {}",
                e.message
            );
        }
    }

    /// The contract fixtures under `contract-fixtures/automation/` are the wire truth: the Rust
    /// DTOs parse them strictly (the same files the vitest suite asserts on).
    #[test]
    fn contract_fixtures_parse_into_the_dtos() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../contract-fixtures/automation/"
        );
        let read = |name: &str| {
            std::fs::read_to_string(format!("{dir}{name}"))
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        };
        let summaries: Vec<ProfileSummaryDto> =
            serde_json::from_str(&read("sync-profile-list.json")).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].readiness, ReadinessDto::Ready);
        assert_eq!(
            summaries[1].readiness,
            ReadinessDto::NotReady {
                reason: "identityMismatch".into()
            }
        );
        let profile: ProfileDto = serde_json::from_str(&read("sync-profile-get.json")).unwrap();
        assert_eq!(profile.revision, 4);
        assert_eq!(profile.policy.mode, WireMode::Mirror);
        assert_eq!(
            profile.identities.right,
            Some(RootIdentity::Network {
                unc_prefix: r"\\nas\photos".into(),
                volume_serial: None,
                file_id: None,
            })
        );
        assert_eq!(profile.changes.len(), 2);
        assert_eq!(profile.changes[0].changes[0].field, "rightRoot");
        let validated: ValidateResultDto =
            serde_json::from_str(&read("sync-profile-validate.json")).unwrap();
        assert!(validated.errors.is_empty());
        assert_eq!(validated.kinds.left.as_deref(), Some("fixed"));
        let saved: SaveResultDto = serde_json::from_str(&read("sync-profile-save.json")).unwrap();
        let SaveResultDto::Saved { profile } = saved else {
            panic!("saved fixture");
        };
        assert_eq!(profile.revision, 1);
        let deleted: DeleteResultDto =
            serde_json::from_str(&read("sync-profile-delete.json")).unwrap();
        assert_eq!(deleted.status, "deleted");
    }

    // ── Rework 1 (review findings P1-1, P2-1 ... P2-4, P3) ──

    fn busy() -> Result<bool, crate::domain::FsError> {
        Err(crate::domain::FsError::new(
            crate::domain::ErrorCode::Busy,
            "Konfirmasi lain masih terbuka",
        ))
    }

    fn create_request(d: Draft) -> SaveRequest {
        SaveRequest {
            id: None,
            expected_revision: None,
            draft: d,
        }
    }

    fn saved(out: SaveResultDto) -> ProfileDto {
        match out {
            SaveResultDto::Saved { profile } => *profile,
            other => panic!("expected saved, got {other:?}"),
        }
    }

    /// A store whose schema is newer than this binary (LC-3), opened read-only.
    fn newer_schema_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path(), "0.2.0");
        {
            let conn = rusqlite::Connection::open(dir.path().join("automation.db")).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        let store = Store::open(dir.path(), "0.2.0");
        (dir, store)
    }

    /// P2-4: another native dialog is open - the save answers `busy`, shows nothing and stores
    /// nothing.
    #[test]
    fn p2_4_a_busy_dialog_saves_nothing() {
        let (dir, store) = store();
        let d = permanent(temp_draft(&dir, "Nightly"));
        let err = save_in(&store, create_request(d), &probe(), |_| busy(), 1_000).unwrap_err();
        assert_eq!(err.code, "busy");
        assert!(list_in(&store, &probe()).unwrap().is_empty());
    }

    /// LC-3 at the command level: a newer-schema store refuses saves BEFORE any dialog and
    /// refuses deletes; reads keep working.
    #[test]
    fn lc_3_a_read_only_store_refuses_commands_before_any_dialog() {
        let (dir, store) = newer_schema_store();
        let asked = Cell::new(0);
        for d in [
            permanent(temp_draft(&dir, "A")),
            guard_off(temp_draft(&dir, "B")),
            temp_draft(&dir, "C"),
        ] {
            let err = save_in(
                &store,
                create_request(d),
                &probe(),
                |_| {
                    asked.set(asked.get() + 1);
                    true
                },
                1_000,
            )
            .unwrap_err();
            assert_eq!(err.code, "store_read_only");
        }
        assert_eq!(asked.get(), 0, "no native dialog for a refused save");
        let id = "3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f";
        assert_eq!(
            delete_in(&store, id, 1).unwrap_err().code,
            "store_read_only"
        );
        assert!(list_in(&store, &probe()).unwrap().is_empty());
    }

    /// An update of a profile that does not exist is refused before the dialog.
    #[test]
    fn an_unknown_profile_is_not_found_before_any_dialog() {
        let (dir, store) = store();
        let asked = Cell::new(0);
        let err = save_in(
            &store,
            SaveRequest {
                id: Some("3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f".into()),
                expected_revision: Some(1),
                draft: permanent(temp_draft(&dir, "Ghost")),
            },
            &probe(),
            |_| {
                asked.set(asked.get() + 1);
                true
            },
            1_000,
        )
        .unwrap_err();
        assert_eq!(err.code, "not_found");
        assert_eq!(asked.get(), 0);
    }

    /// A store that could not be set up (no local data folder) reports `store_unavailable`
    /// everywhere and never asks.
    #[test]
    fn an_unavailable_store_reports_store_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::unavailable();
        assert_eq!(
            list_in(&store, &probe()).unwrap_err().code,
            "store_unavailable"
        );
        let err = save_in(
            &store,
            create_request(permanent(temp_draft(&dir, "X"))),
            &probe(),
            |_| -> bool { panic!("must not ask") },
            1_000,
        )
        .unwrap_err();
        assert_eq!(err.code, "store_unavailable");
    }

    /// SP-6.1 end to end through `save_in`: a rename keeps the revision, a policy change bumps
    /// it; and the SP-6.4 dependants hook runs for the policy change only (P3 review item 4).
    #[test]
    fn sp_6_1_rename_keeps_the_revision_and_does_not_run_the_dependants_hook() {
        let (dir, store) = store();
        let d = temp_draft(&dir, "Photos");
        let first =
            saved(save_in(&store, create_request(d.clone()), &probe(), |_| true, 1_000).unwrap());
        assert_eq!(first.revision, 1);
        profiles::reset_dependants_hook_calls();

        let mut renamed = d.clone();
        renamed.name = "Photos renamed".into();
        let after_rename = saved(
            save_in(
                &store,
                SaveRequest {
                    id: Some(first.id.clone()),
                    expected_revision: Some(1),
                    draft: renamed.clone(),
                },
                &probe(),
                |_| true,
                2_000,
            )
            .unwrap(),
        );
        assert_eq!(after_rename.revision, 1, "rename is not a policy change");
        assert_eq!(after_rename.name, "Photos renamed");
        assert!(
            after_rename.changes.is_empty(),
            "no policy revision recorded"
        );
        assert_eq!(profiles::dependants_hook_calls(), 0);

        let mut changed = renamed;
        changed.policy.exclude = vec!["*.tmp".into()];
        let after_change = saved(
            save_in(
                &store,
                SaveRequest {
                    id: Some(first.id.clone()),
                    expected_revision: Some(1),
                    draft: changed,
                },
                &probe(),
                |_| true,
                3_000,
            )
            .unwrap(),
        );
        assert_eq!(after_change.revision, 2);
        assert_eq!(profiles::dependants_hook_calls(), 1);
    }

    /// P3 item 5 (e): a slow live probe never holds the store's mutex. While `get_in` waits on a
    /// slow root, another store user is served at once.
    #[test]
    fn a_slow_readiness_probe_does_not_hold_the_store_lock() {
        struct SlowProbe;
        impl RootProbe for SlowProbe {
            fn canonical(
                &self,
                path: &str,
            ) -> Result<String, crate::sync_engine::roots::RootError> {
                std::thread::sleep(Duration::from_millis(700));
                crate::sync_engine::roots::canonical(path)
            }
            fn identity(
                &self,
                canonical: &str,
            ) -> Result<RootIdentity, crate::sync_engine::roots::RootError> {
                crate::sync_engine::roots::identity(canonical)
            }
        }
        let (dir, store) = store();
        let d = temp_draft(&dir, "Slow");
        let profile = saved(save_in(&store, create_request(d), &probe(), |_| true, 1_000).unwrap());
        let slow: ProbeArc<dyn RootProbe> = Arc::new(SlowProbe);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| get_in(&store, &profile.id, &slow));
            std::thread::sleep(Duration::from_millis(150));
            let started = std::time::Instant::now();
            store
                .with_read(profiles::list)
                .expect("store readable while a probe runs");
            assert!(
                started.elapsed() < Duration::from_millis(400),
                "the store was blocked for {:?}",
                started.elapsed()
            );
            assert!(reader.join().unwrap().is_ok());
        });
    }

    /// P2-1: a UNC root that cannot be opened is accepted at save time (the share may be offline)
    /// but recorded as UNVERIFIED, and the profile is honestly not ready (offline).
    #[test]
    fn p2_1_an_unreachable_unc_root_saves_but_is_not_ready() {
        let (dir, store) = store();
        let left = dir.path().join("left");
        std::fs::create_dir_all(&left).unwrap();
        let d = draft(
            "Remote",
            &left.to_string_lossy(),
            r"\\localhost\no-such-share-ferrum-p03\data",
        );
        let profile = saved(save_in(&store, create_request(d), &probe(), |_| true, 1_000).unwrap());
        assert_eq!(
            profile.identities.right,
            Some(RootIdentity::Network {
                unc_prefix: r"\\localhost\no-such-share-ferrum-p03".into(),
                volume_serial: None,
                file_id: None,
            })
        );
        assert_eq!(
            profile.readiness,
            ReadinessDto::NotReady {
                reason: "rootOffline".into()
            }
        );
        let listed = list_in(&store, &probe()).unwrap();
        assert_eq!(
            listed[0].readiness,
            ReadinessDto::NotReady {
                reason: "rootOffline".into()
            }
        );
    }

    /// The readiness verdicts for network roots, driven by an injected probe: a live share that
    /// answers with ids is ready; one that cannot be opened is offline; a profile saved while
    /// the share was offline stays not ready even when it answers now (identity never verified);
    /// different ids are a mismatch.
    #[test]
    fn network_readiness_verdicts() {
        struct NetProbe(Option<(&'static str, &'static str)>);
        impl RootProbe for NetProbe {
            fn canonical(
                &self,
                path: &str,
            ) -> Result<String, crate::sync_engine::roots::RootError> {
                Ok(path.to_string())
            }
            fn identity(
                &self,
                canonical: &str,
            ) -> Result<RootIdentity, crate::sync_engine::roots::RootError> {
                Ok(RootIdentity::Network {
                    unc_prefix: canonical.to_lowercase(),
                    volume_serial: self.0.map(|(v, _)| v.to_string()),
                    file_id: self.0.map(|(_, f)| f.to_string()),
                })
            }
        }
        let net = |ids: Option<(&str, &str)>| RootSpec {
            path: r"\\nas\data".into(),
            identity: RootIdentity::Network {
                unc_prefix: r"\\nas\data".into(),
                volume_serial: ids.map(|(v, _)| v.to_string()),
                file_id: ids.map(|(_, f)| f.to_string()),
            },
        };
        let live = |ids| -> ProbeArc<dyn RootProbe> { Arc::new(NetProbe(ids)) };
        let b = Duration::from_secs(2);
        let ids = Some(("0000000a", "bb"));
        let right_only =
            |spec: &RootSpec, probe: &ProbeArc<dyn RootProbe>| root_verdict(spec, probe, b);
        assert_eq!(right_only(&net(ids), &live(ids)), Ok(()));
        assert_eq!(
            right_only(&net(ids), &live(None)),
            Err("rootOffline".to_string())
        );
        assert_eq!(
            right_only(&net(None), &live(ids)),
            Err("identityUnknown".to_string())
        );
        assert_eq!(
            right_only(&net(ids), &live(Some(("0000000a", "cc")))),
            Err("identityMismatch".to_string())
        );
    }

    use crate::automation::model::RootSpec;

    fn versioning_draft(dir: &tempfile::TempDir, versioning_root: &std::path::Path) -> Draft {
        let mut d = temp_draft(dir, "Versioned");
        d.policy.mode = WireMode::Mirror;
        d.policy.delete_policy = WireDeletePolicy::Versioning;
        d.policy.versioning_root = Some(versioning_root.to_string_lossy().into_owned());
        d
    }

    fn field_codes(draft: Draft) -> Vec<(String, String)> {
        validate_in(draft, &FsProbe)
            .errors
            .into_iter()
            .map(|e| (e.field, e.code))
            .collect()
    }

    fn junction(link: &std::path::Path, target: &std::path::Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// P2-2: a versioning root reached through `..` is refused outright.
    #[test]
    fn p2_2_a_versioning_root_with_dot_dot_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        let tricky = dir
            .path()
            .join("other")
            .join("..")
            .join("left")
            .join(".versions");
        let codes = field_codes(versioning_draft(&dir, &tricky));
        assert!(
            codes.contains(&("versioningRoot".into(), "versioning_root_invalid".into())),
            "{codes:?}"
        );
    }

    /// P2-2: a junction on a parent of a not-yet-existing versioning root is resolved to its real
    /// target, so a versioning root that really sits inside a sync root is refused.
    #[cfg(windows)]
    #[test]
    fn p2_2_a_junction_on_the_parent_cannot_hide_a_versioning_root_inside_a_sync_root() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        std::fs::create_dir_all(&left).unwrap();
        let alias = dir.path().join("alias");
        assert!(junction(&alias, &left), "junction created");
        let via_alias = alias.join(".versions");
        let codes = field_codes(versioning_draft(&dir, &via_alias));
        assert!(
            codes.contains(&("versioningRoot".into(), "versioning_root_overlap".into())),
            "{codes:?}"
        );
    }

    /// SP-3.2: a versioning root equal to, inside, or containing a sync root is refused; a
    /// sibling folder that does not exist yet is accepted.
    #[test]
    fn sp_3_2_versioning_root_overlap_forms() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        for bad in [left.clone(), left.join("v"), dir.path().to_path_buf()] {
            let codes = field_codes(versioning_draft(&dir, &bad));
            assert!(
                codes.contains(&("versioningRoot".into(), "versioning_root_overlap".into())),
                "{bad:?}: {codes:?}"
            );
        }
        let ok = dir.path().join("versions").join("sub");
        assert!(field_codes(versioning_draft(&dir, &ok)).is_empty());
    }

    /// P2-3: control and format (bidi) characters are refused in the name, in filters and in
    /// every path (SE-2, threat T4).
    #[test]
    fn p2_3_format_characters_are_refused_everywhere_text_reaches_a_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let bidi = "\u{202E}";
        let mut d = temp_draft(&dir, &format!("Nightly{bidi}tnenamreP"));
        assert!(field_codes(d.clone()).contains(&("name".into(), "name_control".into())));
        d = temp_draft(&dir, "ok");
        d.policy.exclude = vec![format!("*.tmp{bidi}")];
        assert!(field_codes(d.clone()).contains(&("exclude".into(), "filter_control".into())));
        d = temp_draft(&dir, "ok");
        d.policy.include = vec!["a\u{200E}b".into()];
        assert!(field_codes(d.clone()).contains(&("include".into(), "filter_control".into())));
        d = temp_draft(&dir, "ok");
        d.policy.right = format!("{}{bidi}", d.policy.right);
        assert!(field_codes(d.clone()).contains(&("right".into(), "root_invalid".into())));
        let versions = dir.path().join(format!("v{bidi}x"));
        let codes = field_codes(versioning_draft(&dir, &versions));
        assert!(
            codes.contains(&("versioningRoot".into(), "versioning_root_invalid".into())),
            "{codes:?}"
        );
    }

    /// The same checks hold for the real path resolution: a folder whose real name carries a
    /// format character can never become a root, even when typed without it.
    #[test]
    fn p2_3_the_prompt_is_built_from_validated_values_only() {
        let (dir, store) = store();
        let bidi = "\u{202E}";
        let err = save_in(
            &store,
            create_request(permanent(temp_draft(&dir, &format!("N{bidi}x")))),
            &probe(),
            |_| -> bool { panic!("a refused draft never reaches the dialog") },
            1_000,
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_input");
    }

    // ── IPC contract (P1-1): request fixtures shared with the TypeScript tests. ──

    mod ipc {
        //! The wire contract of the request side (P1-1). The webview calls a command with a flat
        //! object `{ id, expectedRevision, draft }`; Tauri hands each Rust parameter the value
        //! under its camelCase name. The Rust wrappers and the TypeScript wrappers are both held
        //! to the same request fixtures: TypeScript asserts it sends exactly the fixture, and
        //! here every fixture key must be a parameter of the command (and the other way round),
        //! and every value must deserialize into that parameter's type.
        use super::*;

        const SOURCE: &str = include_str!("commands.rs");

        fn fixture(name: &str) -> serde_json::Value {
            let dir = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../contract-fixtures/automation/"
            );
            let text = std::fs::read_to_string(format!("{dir}{name}"))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            serde_json::from_str(&text).unwrap()
        }

        fn camel(snake: &str) -> String {
            let mut out = String::new();
            let mut upper = false;
            for c in snake.chars() {
                if c == '_' {
                    upper = true;
                } else if upper {
                    out.push(c.to_ascii_uppercase());
                    upper = false;
                } else {
                    out.push(c);
                }
            }
            out
        }

        /// The IPC argument keys of a command: its parameters minus the injected ones (state,
        /// window, app handle), camelCased the way Tauri names them.
        fn wire_keys(command: &str) -> Vec<String> {
            let start = SOURCE
                .find(&format!("pub async fn {command}"))
                .unwrap_or_else(|| panic!("{command} is defined"));
            let open = start + SOURCE[start..].find('(').unwrap();
            let mut depth = 0i32;
            let mut end = open;
            for (i, c) in SOURCE[open..].char_indices() {
                match c {
                    '(' | '<' => depth += 1,
                    ')' | '>' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            let params = &SOURCE[open + 1..end];
            let mut keys = Vec::new();
            let mut depth = 0i32;
            let mut current = String::new();
            let mut parts = Vec::new();
            for c in params.chars() {
                match c {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth -= 1,
                    _ => {}
                }
                if c == ',' && depth == 0 {
                    parts.push(std::mem::take(&mut current));
                } else {
                    current.push(c);
                }
            }
            parts.push(current);
            for part in parts {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let (name, ty) = part.split_once(':').expect("name: Type");
                let ty = ty.trim();
                if ty.starts_with("State<")
                    || ty.starts_with("tauri::Window")
                    || ty.starts_with("tauri::AppHandle")
                {
                    continue;
                }
                keys.push(camel(name.trim()));
            }
            keys.sort();
            keys
        }

        fn fixture_keys(value: &serde_json::Value) -> Vec<String> {
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        }

        #[test]
        fn every_request_fixture_names_exactly_the_command_parameters() {
            for (command, name) in [
                ("sync_profile_get", "sync-profile-get.request.json"),
                (
                    "sync_profile_validate",
                    "sync-profile-validate.request.json",
                ),
                ("sync_profile_save", "sync-profile-save.request.json"),
                ("sync_profile_save", "sync-profile-save-update.request.json"),
                ("sync_profile_delete", "sync-profile-delete.request.json"),
                ("sync_profile_preview", "sync-profile-preview.request.json"),
                ("sync_profile_run", "sync-profile-run.request.json"),
            ] {
                assert_eq!(
                    wire_keys(command),
                    fixture_keys(&fixture(name)),
                    "{command} vs {name}"
                );
            }
            // No argument at all for the list command.
            assert!(wire_keys("sync_profile_list").is_empty());
        }

        /// The old nested shape (`{ req: { ... } }`) is exactly what P1-1 was: it must no longer
        /// match the command parameters.
        #[test]
        fn the_nested_req_shape_does_not_match_the_command() {
            let nested = serde_json::json!({ "req": fixture("sync-profile-save.request.json") });
            assert_ne!(wire_keys("sync_profile_save"), fixture_keys(&nested));
            assert!(!wire_keys("sync_profile_save").contains(&"req".to_string()));
        }

        #[test]
        fn fixture_values_deserialize_into_the_parameter_types() {
            for name in [
                "sync-profile-save.request.json",
                "sync-profile-save-update.request.json",
            ] {
                let body = fixture(name);
                let id: Option<String> = serde_json::from_value(body["id"].clone()).unwrap();
                let rev: Option<i64> =
                    serde_json::from_value(body["expectedRevision"].clone()).unwrap();
                let draft: Draft = serde_json::from_value(body["draft"].clone()).unwrap();
                assert_eq!(draft.name, "Projects backup");
                assert_eq!(
                    id.is_some(),
                    rev.is_some(),
                    "{name}: id and revision go together"
                );
            }
            let get = fixture("sync-profile-get.request.json");
            let _: String = serde_json::from_value(get["id"].clone()).unwrap();
            let del = fixture("sync-profile-delete.request.json");
            let _: String = serde_json::from_value(del["id"].clone()).unwrap();
            let _: i64 = serde_json::from_value(del["expectedRevision"].clone()).unwrap();
            let validate = fixture("sync-profile-validate.request.json");
            let _: Draft = serde_json::from_value(validate["draft"].clone()).unwrap();
            let preview = fixture("sync-profile-preview.request.json");
            let id: String = serde_json::from_value(preview["id"].clone()).unwrap();
            assert!(model::is_profile_id(&id));
            let run = fixture("sync-profile-run.request.json");
            let _: String = serde_json::from_value(run["id"].clone()).unwrap();
            let pid: String = serde_json::from_value(run["previewId"].clone()).unwrap();
            assert!(model::is_profile_id(&pid));
            let _: bool = serde_json::from_value(run["confirmOverwrite"].clone()).unwrap();
        }

        /// SE-1: the run command's only boolean is the overwrite confirmation of the webview's
        /// own dialog; nothing in its parameters can stand in for the native permanent-delete
        /// confirmation.
        #[test]
        fn the_run_command_has_no_native_confirmation_parameter() {
            let keys = wire_keys("sync_profile_run");
            assert_eq!(keys, ["confirmOverwrite", "id", "previewId"]);
            assert!(wire_keys("sync_profile_preview") == ["id"]);
            for k in keys {
                assert!(!k.to_lowercase().contains("permanent"));
                assert!(!k.to_lowercase().contains("native"));
            }
        }

        /// The create fixture drives the real core with real folders and saves; the update fixture
        /// addresses a profile that is not there and gets `not_found`.
        #[test]
        fn the_save_fixtures_drive_the_core() {
            let (dir, store) = store();
            let mut body = fixture("sync-profile-save.request.json");
            let temp = temp_draft(&dir, "Projects backup");
            body["draft"]["left"] = temp.policy.left.clone().into();
            body["draft"]["right"] = temp.policy.right.clone().into();
            let req = SaveRequest {
                id: serde_json::from_value(body["id"].clone()).unwrap(),
                expected_revision: serde_json::from_value(body["expectedRevision"].clone())
                    .unwrap(),
                draft: serde_json::from_value(body["draft"].clone()).unwrap(),
            };
            let profile = saved(save_in(&store, req, &probe(), |_| true, 1_000).unwrap());
            assert_eq!(profile.name, "Projects backup");
            assert_eq!(profile.revision, 1);

            let update = fixture("sync-profile-save-update.request.json");
            let req = SaveRequest {
                id: serde_json::from_value(update["id"].clone()).unwrap(),
                expected_revision: serde_json::from_value(update["expectedRevision"].clone())
                    .unwrap(),
                draft: serde_json::from_value(body["draft"].clone()).unwrap(),
            };
            let err = save_in(&store, req, &probe(), |_| true, 2_000).unwrap_err();
            assert_eq!(err.code, "not_found");
        }
    }
}

#[cfg(test)]
#[path = "commands_lease_tests.rs"]
mod lease_tests;
