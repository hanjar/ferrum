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
use crate::automation::model::{
    self, Draft, FieldError, PolicyDraft, ProfilePolicy, RootIdentity, WireCompare,
    WireDeletePolicy, WireMode, WireSide,
};
use crate::automation::store::{Store, StoreError, profiles};
use crate::profile;
use crate::sync_engine::roots::{
    FsProbe, READINESS_BUDGET, RootProbe, RootProbeOutcome, probe_with,
};
use std::sync::Arc as ProbeArc;

// ── State ──

/// Managed state: one handle to the opened automation store. Opening happens once at startup;
/// an unusable store is remembered and every command reports it.
pub struct AutomationState {
    pub store: Arc<Store>,
}

impl AutomationState {
    pub fn open() -> Self {
        Self {
            store: Arc::new(Store::open(&store_dir(), env!("CARGO_PKG_VERSION"))),
        }
    }
}

/// `<local data>/automation` (design §4.1) — the only location the store may live in.
pub fn store_dir() -> std::path::PathBuf {
    profile::resolve_local_data_dir().join("automation")
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

// ── Readiness (SP-8) ──

/// Reason strings for [`ReadinessDto::NotReady`], camelCase for the UI.
mod reasons {
    pub const ROOT_MISSING: &str = "rootMissing";
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
/// probe budget. A slower answer means `checking`, never "treat as empty" (SP-8, WA-5.1).
fn root_verdict(
    spec: &model::RootSpec,
    probe: &ProbeArc<dyn RootProbe>,
    budget: Duration,
) -> Result<(), String> {
    match probe_with(probe.clone(), &spec.path, budget) {
        RootProbeOutcome::Ready {
            canonical,
            identity,
        } => {
            if canonical != spec.path || identity != spec.identity {
                Err(reasons::IDENTITY_MISMATCH.into())
            } else {
                Ok(())
            }
        }
        RootProbeOutcome::Checking => Err("checking".into()),
        RootProbeOutcome::Missing | RootProbeOutcome::Unavailable => {
            Err(reasons::ROOT_MISSING.into())
        }
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
    probe: &ProbeArc<dyn RootProbe>,
) -> ProfileSummaryDto {
    ProfileSummaryDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        revision: rec.revision,
        left: policy.left.path.clone(),
        right: policy.right.path.clone(),
        mode_label: model::mode_label(&policy.mode),
        delete_label: model::delete_label(&policy.delete_policy),
        readiness: readiness_dto(policy, probe, READINESS_BUDGET),
        used_by: 0, // P1.1: automations using this profile (SP-9.5).
    }
}

fn dto_of(
    conn: &rusqlite::Connection,
    rec: &profiles::ProfileRecord,
    policy: &ProfilePolicy,
    probe: &ProbeArc<dyn RootProbe>,
) -> Result<ProfileDto, CommandError> {
    let changes = profiles::revisions(conn, &rec.id)?
        .into_iter()
        .map(|r| RevisionChangesDto {
            revision: r.revision,
            changed_ms: r.changed_ms,
            changes: serde_json::from_str(&r.changes).unwrap_or_default(),
        })
        .collect();
    Ok(ProfileDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        revision: rec.revision,
        policy: policy_draft_of(policy),
        identities: IdentitiesDto {
            left: Some(policy.left.identity.clone()),
            right: Some(policy.right.identity.clone()),
        },
        changes,
        created_ms: rec.created_ms,
        updated_ms: rec.updated_ms,
        readiness: readiness_dto(policy, probe, READINESS_BUDGET),
    })
}

// ── Tauri-free command cores (test seam, design §13.1) ──

pub fn list_in(
    store: &Store,
    probe: &ProbeArc<dyn RootProbe>,
) -> Result<Vec<ProfileSummaryDto>, CommandError> {
    store
        .with_read(profiles::list)
        .map_err(CommandError::from)
        .and_then(|records| {
            records
                .iter()
                .map(|rec| match parse_policy(&rec.policy) {
                    Ok(policy) => Ok(summary_of(rec, &policy, probe)),
                    // An invalid stored row is never fixed silently (design §4.4): it shows as a
                    // profile that needs review.
                    Err(_) => Ok(ProfileSummaryDto {
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
                    }),
                })
                .collect()
        })
}

pub fn get_in(
    store: &Store,
    id: &str,
    probe: &ProbeArc<dyn RootProbe>,
) -> Result<ProfileDto, CommandError> {
    require_profile_id(id)?;
    store.with_read(|conn| -> Result<ProfileDto, CommandError> {
        let rec = profiles::get(conn, id)?.ok_or(CommandError::from(StoreError::NotFound))?;
        let policy = parse_policy(&rec.policy)?;
        dto_of(conn, &rec, &policy, probe)
    })
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

/// Input of `sync_profile_save`. `deny_unknown_fields` is part of SE-1: no extra input field
/// (in particular anything like `confirmed`) can ride along.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    pub draft: Draft,
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

/// Save (create or update). Order: validate → stale check → native confirm when needed →
/// store in one transaction with the revision re-checked inside (design §8.2 shape).
pub fn save_in(
    store: &Store,
    req: SaveRequest,
    probe: &ProbeArc<dyn RootProbe>,
    confirm: impl FnOnce(&Prompt) -> bool,
    now_ms: i64,
) -> Result<SaveResultDto, CommandError> {
    if let Some(id) = &req.id {
        require_profile_id(id)?;
    }
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
        let prompt = confirm_text::save_profile_prompt(
            &validated.name,
            existing.as_ref().map(|r| r.revision),
            &validated.policy,
        );
        if !confirm(&prompt) {
            // Cancel = nothing saved (SP-3.3).
            return Ok(SaveResultDto::Canceled);
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
    let profile = store.with_read(|conn| -> Result<ProfileDto, CommandError> {
        let rec = profiles::get(conn, &id)?.ok_or(CommandError::from(StoreError::NotFound))?;
        let policy = parse_policy(&rec.policy)?;
        dto_of(conn, &rec, &policy, probe)
    })?;
    Ok(SaveResultDto::Saved {
        profile: Box::new(profile),
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
fn owner_hwnd(window: &tauri::Window) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(not(windows))]
fn owner_hwnd(_window: &tauri::Window) -> isize {
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

#[tauri::command]
pub async fn sync_profile_save(
    state: State<'_, AutomationState>,
    window: tauri::Window,
    req: SaveRequest,
) -> Result<SaveResultDto, CommandError> {
    let store = state.store.clone();
    // The whole flow (validation probes + the native dialog + the store transaction) runs off
    // the async workers; the dialog is owned by the Ferrum window (SE-2).
    let owner = owner_hwnd(&window);
    let probe: Arc<dyn RootProbe> = Arc::new(FsProbe);
    tauri::async_runtime::spawn_blocking(move || {
        save_in(
            &store,
            req,
            &probe,
            |prompt| crate::native_dialog::confirm(owner, prompt.caption, &prompt.text),
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
            |_| panic!("a plain save must not ask"),
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
            |_| panic!("no confirm needed"),
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
            |_| panic!("stale must not ask"),
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
            |_| panic!("no confirm needed"),
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
            |_| panic!("no confirm needed"),
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
            |_| panic!("no confirm needed"),
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
            |_| panic!("no confirm needed"),
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
                unc_prefix: r"\\nas\photos".into()
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
}
