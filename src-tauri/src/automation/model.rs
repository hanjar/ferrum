//! Sync Profile model (Automation design §4.4): stored types, the IPC draft shape (design §7.1)
//! and validation. Validation is fail-closed: an invalid draft never reaches the store, an
//! invalid stored policy is reported as a profile needing review, never fixed silently.
//!
//! Tauri-free by design (the runner links this module).

use serde::{Deserialize, Serialize};

use crate::hotlist::model::is_format_char;
use crate::sync_engine::roots::{RootError, RootProbe, overlap};
use crate::sync_engine::write_guard::{FerrumGuard, WriteGuard};

/// Control characters and Unicode format (bidi, zero-width) characters can reorder or hide text
/// in a native dialog (SE-2, threat T4): they are refused in names, filters and paths.
fn has_unsafe_char(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || is_format_char(c))
}

// ── Limits (requirements SP-1.3, SP-1.4, SP-4.2, SP-5.1) ──

pub const NAME_MAX_CHARS: usize = 80;
pub const ROOT_MAX_CHARS: usize = 1024;
pub const MAX_FILTERS: usize = 64;
pub const MAX_FILTER_CHARS: usize = 260;
pub const GUARD_MAX_COUNT: u32 = 100_000;
/// Highest percent threshold that can still trip: the engine stops a run only when the deletes
/// exceed BOTH the count and the percent of the side's files, and deletes can never exceed 100 %
/// of them, so `100` would be a guard that never fires. Validation refuses it (FER-522 rework 1).
pub const GUARD_MAX_PERCENT: u8 = 99;

// ── Stored model (§4.4, serde camelCase, deny_unknown_fields) ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriveKind {
    Fixed,
    Removable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RootIdentity {
    #[serde(rename_all = "camelCase")]
    Local {
        volume_serial: String,
        file_id: String,
        fs_name: String,
        drive: DriveKind,
    },
    /// A UNC root. The ids are recorded when the share could be opened at save time; both absent
    /// means the identity is UNKNOWN (offline share): the profile is never Ready on that basis.
    #[serde(rename_all = "camelCase")]
    Network {
        unc_prefix: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        volume_serial: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
    pub fn field(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum SyncMode {
    #[serde(rename_all = "camelCase")]
    OneWay { from: Side },
    #[serde(rename_all = "camelCase")]
    Mirror { from: Side },
    #[serde(rename_all = "camelCase")]
    TwoWay { propagate_deletes: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum DeletePolicy {
    RecycleBin,
    #[serde(rename_all = "camelCase")]
    Versioning {
        root: String,
    },
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompareMode {
    SizeAndDate,
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filters {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteGuard {
    pub enabled: bool,
    pub max_count: u32,
    pub max_percent: u8,
}

impl DeleteGuard {
    /// Whether the guard can ever stop a run. `enabled` with a percent of 100 (or more) or a zero
    /// count never trips, so it is the same as OFF and is treated as off wherever the user must
    /// confirm turning protection off (SE-1, SP-5.1): a profile stored before validation refused
    /// such values cannot slip past the native dialog.
    pub fn effective(&self) -> bool {
        self.enabled && self.max_count > 0 && self.max_percent < 100
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootSpec {
    /// Canonical display path (design §5.8).
    pub path: String,
    pub identity: RootIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfilePolicy {
    pub left: RootSpec,
    pub right: RootSpec,
    pub mode: SyncMode,
    pub delete_policy: DeletePolicy,
    pub compare: CompareMode,
    pub filters: Filters,
    pub guard: DeleteGuard,
}

// ── Draft shape crossing IPC (design §7.1) ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireMode {
    OneWay,
    Mirror,
    TwoWay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireDeletePolicy {
    RecycleBin,
    Versioning,
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireCompare {
    SizeAndDate,
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftGuard {
    pub enabled: bool,
    pub max_count: u32,
    pub max_percent: u8,
}

/// The policy part of a draft, exactly as the editor sends and receives it. `deny_unknown_fields`
/// is the SE-1 backstop: an input carrying extra fields (for example something behaving like a
/// `confirmed` flag) is refused, never interpreted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyDraft {
    pub left: String,
    pub right: String,
    pub mode: WireMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<WireSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_deletes: Option<bool>,
    pub delete_policy: WireDeletePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning_root: Option<String>,
    pub compare: WireCompare,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub guard: DraftGuard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Draft {
    pub name: String,
    #[serde(flatten)]
    pub policy: PolicyDraft,
}

fn side_of(s: WireSide) -> Side {
    match s {
        WireSide::Left => Side::Left,
        WireSide::Right => Side::Right,
    }
}

fn fe(field: &str, code: &'static str) -> FieldError {
    FieldError {
        field: field.to_string(),
        code: code.to_string(),
    }
}

// ── Validation ──

/// One rejected field: where and why. Codes are a stable, closed vocabulary; they never carry
/// user paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    pub field: String,
    pub code: String,
}

/// A draft that passed every check, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProfile {
    /// Trimmed display name (SP-1.3).
    pub name: String,
    pub policy: ProfilePolicy,
}

/// Probe outcome for one root: canonical path, recorded identity and the drive class used for the
/// Recycle Bin check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideFacts {
    pub canonical: String,
    pub identity: RootIdentity,
    pub kind: SideKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideKind {
    Fixed,
    Removable,
    Network,
}

impl Draft {
    /// Validate the whole draft (SP-1.3–SP-1.5, SP-3.1, SP-3.2, SP-4.2, SP-5.1 ranges, SP-7.1).
    /// All errors are collected so the editor can show them inline in one pass.
    pub fn validate(&self, probe: &dyn RootProbe) -> Result<ValidatedProfile, Vec<FieldError>> {
        let mut errors: Vec<FieldError> = Vec::new();

        // SP-1.3 name.
        let name = self.name.trim();
        if name.is_empty() {
            errors.push(fe("name", "name_required"));
        } else if name.chars().count() > NAME_MAX_CHARS {
            errors.push(fe("name", "name_too_long"));
        }
        if has_unsafe_char(&self.name) {
            errors.push(fe("name", "name_control"));
        }

        // SP-1.4 path forms + existence + SP-7.1 identity.
        let left = root_facts(&self.policy.left, "left", probe, &mut errors);
        let right = root_facts(&self.policy.right, "right", probe, &mut errors);

        // SP-1.5 overlap after canonicalization (same folder, junction alias, case, mapped drive).
        if let (Some(l), Some(r)) = (&left, &right) {
            if l.canonical.eq_ignore_ascii_case(&r.canonical) {
                errors.push(fe("right", "roots_identical"));
            } else if overlap(&l.canonical, &r.canonical) {
                errors.push(fe("right", "roots_nested"));
            }
        }

        // SP-2 mode + side / propagation interplay.
        let mut mode = match self.policy.mode {
            WireMode::OneWay | WireMode::Mirror => match self.policy.from {
                Some(from) => {
                    let from = side_of(from);
                    if matches!(self.policy.mode, WireMode::OneWay) {
                        SyncMode::OneWay { from }
                    } else {
                        SyncMode::Mirror { from }
                    }
                }
                None => {
                    errors.push(fe("from", "side_required"));
                    SyncMode::OneWay { from: Side::Left }
                }
            },
            WireMode::TwoWay => {
                if self.policy.from.is_some() {
                    errors.push(fe("from", "side_not_allowed"));
                }
                SyncMode::TwoWay {
                    propagate_deletes: self.policy.propagate_deletes.unwrap_or(false),
                }
            }
        };
        if self.policy.propagate_deletes.is_some() && !matches!(mode, SyncMode::TwoWay { .. }) {
            errors.push(fe("propagateDeletes", "propagate_not_allowed"));
            mode = SyncMode::TwoWay {
                propagate_deletes: false,
            };
        }

        // SP-3 delete policy.
        let delete_policy = match self.policy.delete_policy {
            WireDeletePolicy::RecycleBin => {
                reject_versioning_root(&self.policy.versioning_root, &mut errors);
                // SP-3.1: every side that can receive deletes must be a local fixed drive.
                for side in receiving_sides(&mode) {
                    let facts = match side {
                        Side::Left => &left,
                        Side::Right => &right,
                    };
                    if facts.as_ref().is_some_and(|f| f.kind != SideKind::Fixed) {
                        errors.push(fe(side.field(), "recycle_bin_unavailable"));
                    }
                }
                DeletePolicy::RecycleBin
            }
            WireDeletePolicy::Versioning => {
                match versioning_facts(self, probe, &left, &right, &mut errors) {
                    Some(root) => DeletePolicy::Versioning { root },
                    None => DeletePolicy::Versioning {
                        root: String::new(),
                    },
                }
            }
            WireDeletePolicy::Permanent => {
                reject_versioning_root(&self.policy.versioning_root, &mut errors);
                DeletePolicy::Permanent
            }
        };

        // FER-562: no root in Ferrum's own folders or a Windows Startup folder; a root that holds
        // one is refused when the profile writes to it.
        // No file system access here (roots are already canonical), so a slow share cannot
        // hold the editor.
        {
            let writable = written_sides(&mode);
            let mut asked: Vec<(&'static str, String, bool)> = Vec::new();
            for (side, facts) in [(Side::Left, &left), (Side::Right, &right)] {
                if let Some(f) = facts {
                    asked.push((side.field(), f.canonical.clone(), writable.contains(&side)));
                }
            }
            if let DeletePolicy::Versioning { root } = &delete_policy
                && !root.is_empty()
            {
                asked.push(("versioningRoot", root.clone(), true));
            }
            for (field, path, w) in &asked {
                if let Some(v) = FerrumGuard.root_violation(path, *w) {
                    errors.push(fe(field, v.field_code()));
                }
            }
        }

        // SP-4.2 filter limits.
        for (field, list) in [
            ("include", &self.policy.include),
            ("exclude", &self.policy.exclude),
        ] {
            if list.len() > MAX_FILTERS {
                errors.push(fe(field, "filters_too_many"));
            }
            for pattern in list {
                if pattern.chars().count() > MAX_FILTER_CHARS {
                    errors.push(fe(field, "filter_too_long"));
                    break;
                }
                if has_unsafe_char(pattern) {
                    errors.push(fe(field, "filter_control"));
                    break;
                }
            }
        }

        // SP-5.1 guard ranges.
        if self.policy.guard.max_count == 0 || self.policy.guard.max_count > GUARD_MAX_COUNT {
            errors.push(fe("guard", "guard_count_range"));
        }
        if self.policy.guard.max_percent == 0 || self.policy.guard.max_percent > GUARD_MAX_PERCENT {
            errors.push(fe("guard", "guard_percent_range"));
        }

        let (Some(left), Some(right)) = (left, right) else {
            return Err(errors);
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(ValidatedProfile {
            name: name.to_string(),
            policy: ProfilePolicy {
                left: RootSpec {
                    path: left.canonical,
                    identity: left.identity,
                },
                right: RootSpec {
                    path: right.canonical,
                    identity: right.identity,
                },
                mode,
                delete_policy,
                compare: match self.policy.compare {
                    WireCompare::SizeAndDate => CompareMode::SizeAndDate,
                    WireCompare::Content => CompareMode::Content,
                },
                filters: Filters {
                    include: cleaned_patterns(&self.policy.include),
                    exclude: cleaned_patterns(&self.policy.exclude),
                },
                guard: DeleteGuard {
                    enabled: self.policy.guard.enabled,
                    max_count: self.policy.guard.max_count,
                    max_percent: self.policy.guard.max_percent,
                },
            },
        })
    }
}

fn root_facts(
    path: &str,
    field: &str,
    probe: &dyn RootProbe,
    errors: &mut Vec<FieldError>,
) -> Option<SideFacts> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        errors.push(fe(field, "root_required"));
        return None;
    }
    if trimmed.chars().count() > ROOT_MAX_CHARS {
        errors.push(fe(field, "root_too_long"));
        return None;
    }
    if has_unsafe_char(trimmed) {
        errors.push(fe(field, "root_invalid"));
        return None;
    }
    let canonical = match probe.canonical(trimmed) {
        Ok(c) => c,
        Err(RootError::Invalid) => {
            errors.push(fe(field, "root_invalid"));
            return None;
        }
        Err(_) => {
            errors.push(fe(field, "root_unavailable"));
            return None;
        }
    };
    let identity = match probe.identity(&canonical) {
        Ok(id) => id,
        Err(RootError::NotFound) => {
            errors.push(fe(field, "root_missing"));
            return None;
        }
        Err(_) => {
            errors.push(fe(field, "root_unavailable"));
            return None;
        }
    };
    let kind = match &identity {
        RootIdentity::Local { drive, .. } => match drive {
            DriveKind::Fixed => SideKind::Fixed,
            DriveKind::Removable => SideKind::Removable,
        },
        RootIdentity::Network { .. } => SideKind::Network,
    };
    Some(SideFacts {
        canonical,
        identity,
        kind,
    })
}

/// SP-3.2: the versioning root must be an absolute local or UNC folder that neither sits inside a
/// sync root nor contains one. It need not exist yet (created on first use).
fn versioning_facts(
    draft: &Draft,
    probe: &dyn RootProbe,
    left: &Option<SideFacts>,
    right: &Option<SideFacts>,
    errors: &mut Vec<FieldError>,
) -> Option<String> {
    let Some(raw) = draft
        .policy
        .versioning_root
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        errors.push(fe("versioningRoot", "versioning_root_required"));
        return None;
    };
    if raw.chars().count() > ROOT_MAX_CHARS {
        errors.push(fe("versioningRoot", "root_too_long"));
        return None;
    }
    if has_unsafe_char(raw) {
        errors.push(fe("versioningRoot", "versioning_root_invalid"));
        return None;
    }
    let canonical = match probe.canonical(raw) {
        Ok(c) => c,
        Err(_) => {
            errors.push(fe("versioningRoot", "versioning_root_invalid"));
            return None;
        }
    };
    for facts in [left, right] {
        if let Some(f) = facts
            && (f.canonical.eq_ignore_ascii_case(&canonical) || overlap(&f.canonical, &canonical))
        {
            errors.push(fe("versioningRoot", "versioning_root_overlap"));
            return None;
        }
    }
    Some(canonical)
}

fn reject_versioning_root(value: &Option<String>, errors: &mut Vec<FieldError>) {
    if value
        .as_deref()
        .map(str::trim)
        .is_some_and(|v| !v.is_empty())
    {
        errors.push(fe("versioningRoot", "versioning_root_not_allowed"));
    }
}

/// Sides a run can write to (copy, overwrite or delete), given the mode (FER-562): the target of
/// a one-way or mirror profile, both sides of a two-way one.
pub fn written_sides(mode: &SyncMode) -> Vec<Side> {
    match mode {
        SyncMode::OneWay { from } | SyncMode::Mirror { from } => vec![from.opposite()],
        SyncMode::TwoWay { .. } => vec![Side::Left, Side::Right],
    }
}

/// Sides a run could delete on, given the mode (SP-3.1).
pub fn receiving_sides(mode: &SyncMode) -> Vec<Side> {
    match mode {
        SyncMode::OneWay { .. } => Vec::new(),
        SyncMode::Mirror { from } => vec![from.opposite()],
        SyncMode::TwoWay {
            propagate_deletes: true,
        } => vec![Side::Left, Side::Right],
        SyncMode::TwoWay {
            propagate_deletes: false,
        } => Vec::new(),
    }
}

fn cleaned_patterns(list: &[String]) -> Vec<String> {
    list.iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

// ── Display values (revision changes list SP-6.3, prompt §10.3, list DTO) ──

/// Bounded display form of one field value: at most `max` characters with an ellipsis in the
/// middle, so a long path or pattern can never blow up a dialog or a changes row.
pub fn shorten_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let head_s: String = s.chars().take(head).collect();
    let tail_s: String = s.chars().skip(n - tail).collect();
    format!("{head_s}…{tail_s}")
}

pub fn mode_label(mode: &SyncMode) -> String {
    match mode {
        SyncMode::OneWay { from: Side::Left } => "Update left → right".into(),
        SyncMode::OneWay { from: Side::Right } => "Update right → left".into(),
        SyncMode::Mirror { from: Side::Left } => "Mirror left → right".into(),
        SyncMode::Mirror { from: Side::Right } => "Mirror right → left".into(),
        SyncMode::TwoWay {
            propagate_deletes: true,
        } => "Two-way · deletions propagate".into(),
        SyncMode::TwoWay {
            propagate_deletes: false,
        } => "Two-way".into(),
    }
}

pub fn delete_label(policy: &DeletePolicy) -> String {
    match policy {
        DeletePolicy::RecycleBin => "Recycle Bin".into(),
        DeletePolicy::Versioning { root } => {
            format!("Versioning · {}", shorten_middle(root, 80))
        }
        DeletePolicy::Permanent => "Permanent".into(),
    }
}

fn identity_display(identity: &RootIdentity) -> String {
    match identity {
        RootIdentity::Local {
            volume_serial,
            file_id,
            ..
        } => format!("vol {volume_serial} · id {}", shorten_middle(file_id, 12)),
        RootIdentity::Network {
            unc_prefix,
            volume_serial: Some(serial),
            file_id: Some(file_id),
        } => format!(
            "{unc_prefix} · vol {serial} · id {}",
            shorten_middle(file_id, 12)
        ),
        RootIdentity::Network { unc_prefix, .. } => format!("{unc_prefix} · unverified"),
    }
}

fn filters_display(filters: &Filters) -> String {
    format!(
        "{} exclude · {} include",
        filters.exclude.len(),
        filters.include.len()
    )
}

fn guard_display(guard: &DeleteGuard) -> String {
    if guard.enabled {
        format!("on · {} · {}%", guard.max_count, guard.max_percent)
    } else {
        "off".into()
    }
}

/// One changed policy field with its old and new display values (SP-6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub field: String,
    pub from: String,
    pub to: String,
}

/// Which policy fields changed between two stored revisions (SP-6.1, SP-6.3). Renames are not a
/// policy change and never appear here.
pub fn field_changes(old: &ProfilePolicy, new: &ProfilePolicy) -> Vec<FieldChange> {
    let mut out: Vec<FieldChange> = Vec::new();
    let mut push = |field: &str, from: String, to: String| {
        if from != to {
            out.push(FieldChange {
                field: field.to_string(),
                from,
                to,
            });
        }
    };
    for (field, old_root, new_root) in [
        ("leftRoot", &old.left, &new.left),
        ("rightRoot", &old.right, &new.right),
    ] {
        if old_root.path != new_root.path {
            push(field, old_root.path.clone(), new_root.path.clone());
        } else if old_root.identity != new_root.identity {
            push(
                &format!("{}Identity", field.trim_end_matches("Root")),
                identity_display(&old_root.identity),
                identity_display(&new_root.identity),
            );
        }
    }
    if old.mode != new.mode {
        push("mode", mode_label(&old.mode), mode_label(&new.mode));
    }
    if old.delete_policy != new.delete_policy {
        push(
            "deletePolicy",
            delete_label(&old.delete_policy),
            delete_label(&new.delete_policy),
        );
    }
    if old.compare != new.compare {
        let label = |c: &CompareMode| match c {
            CompareMode::SizeAndDate => "Size and date",
            CompareMode::Content => "Content",
        };
        push(
            "compare",
            label(&old.compare).into(),
            label(&new.compare).into(),
        );
    }
    if old.filters != new.filters {
        push(
            "filters",
            filters_display(&old.filters),
            filters_display(&new.filters),
        );
    }
    if old.guard != new.guard {
        push(
            "guard",
            guard_display(&old.guard),
            guard_display(&new.guard),
        );
    }
    out
}

// ── Profile ids: lowercase UUID v4 (SE-7) ──

/// Strict lowercase UUID v4 check (regex `^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`).
pub fn is_profile_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 || b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *c != b'-' {
                    return false;
                }
            }
            14 => {
                if *c != b'4' {
                    return false;
                }
            }
            19 => {
                if !matches!(c, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() || c.is_ascii_uppercase() {
                    return false;
                }
            }
        }
    }
    true
}

/// A fresh lowercase UUID v4 for a new profile. `None` = the OS entropy source failed (fail
/// closed: no id, no save).
#[must_use]
pub fn new_profile_id() -> Option<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).ok()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        h[0..4].join(""),
        h[4..6].join(""),
        h[6..8].join(""),
        h[8..10].join(""),
        h[10..16].join("")
    ))
}

// ── Automation model (design §4.4, P1.1) ──
//
// The stored policy of one automation and its IPC draft are the same shape: the wire type IS
// the stored type, validated after deserialization. `deny_unknown_fields` everywhere is the SE-1
// backstop - nothing behaving like a `confirmed` flag can ride along.

/// Retries of a transient error per run (RQ-7.1): `0..=MAX_RETRIES`, default [`DEFAULT_RETRIES`].
pub const MAX_RETRIES: u8 = 3;
pub const DEFAULT_RETRIES: u8 = 2;
/// Quiet period of a watch trigger, seconds (WA-1).
pub const IDLE_SECS_RANGE: std::ops::RangeInclusive<u32> = 2..=3600;
/// Longest wait for a folder that never goes quiet, seconds (WA-1.4, design Q3).
pub const MAX_WAIT_SECS_RANGE: std::ops::RangeInclusive<u32> = 60..=86_400;
pub const DEFAULT_MAX_WAIT_SECS: u32 = 900;
/// Longest IANA zone id accepted (`America/Argentina/ComodRivadavia` is 32).
pub const TZ_MAX_CHARS: usize = 64;

/// Where an automation runs (RT-1): only while Ferrum is open, or in the background runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Runtime {
    App,
    Background,
}

/// What to do about a schedule slot that passed while nothing was running (SC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MissedPolicy {
    Skip,
    RunOnce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Freq {
    /// One run on `date` (`YYYY-MM-DD`, the zone's calendar).
    Once {
        date: String,
    },
    Daily {},
    /// Weekdays `0 = Sunday .. 6`, sorted and unique.
    Weekly {
        days: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleTrigger {
    pub freq: Freq,
    /// `HH:MM`, 24 hours.
    pub time: String,
    /// IANA zone id (syntax checked here; the scheduler resolves it).
    pub tz: String,
    pub missed: MissedPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WatchTrigger {
    pub watch_left: bool,
    pub watch_right: bool,
    pub on_changes: bool,
    pub on_available: bool,
    pub idle_secs: u32,
    pub max_wait_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Trigger {
    Schedule(ScheduleTrigger),
    Watch(WatchTrigger),
}

impl Trigger {
    /// The `kind` column of `automations`.
    pub fn kind(&self) -> &'static str {
        match self {
            Trigger::Schedule(_) => "schedule",
            Trigger::Watch(_) => "watch",
        }
    }
}

/// Optional run conditions (OP-1); all off by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Conditions {
    #[serde(default)]
    pub skip_on_battery: bool,
    #[serde(default)]
    pub skip_on_metered: bool,
}

fn default_retries() -> u8 {
    DEFAULT_RETRIES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationPolicy {
    pub trigger: Trigger,
    pub runtime: Runtime,
    #[serde(default = "default_retries")]
    pub retries: u8,
    #[serde(default)]
    pub conditions: Conditions,
}

/// The IPC draft of an automation (design §7.2 `draft`): a name, the sync profile it runs and
/// the automation policy. The profile id is an id the backend looks up; the webview never sends
/// profile contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationDraft {
    pub name: String,
    pub profile_id: String,
    pub policy: AutomationPolicy,
}

/// A draft that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAutomation {
    /// Trimmed display name (AP-2.1: 1..=80).
    pub name: String,
    pub profile_id: String,
    pub policy: AutomationPolicy,
}

/// `YYYY-MM-DD` as a real calendar date (leap years honoured).
fn is_calendar_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = &s[r];
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (Some(y), Some(m), Some(d)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    if !(2000..=2200).contains(&y) || !(1..=12).contains(&m) || d == 0 {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    d <= days
}

/// `HH:MM`, 24 hours, exactly two digits each.
fn is_clock_time(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return false;
    }
    let num = |a: u8, c: u8| -> Option<u32> {
        (a.is_ascii_digit() && c.is_ascii_digit())
            .then(|| u32::from(a - b'0') * 10 + u32::from(c - b'0'))
    };
    matches!((num(b[0], b[1]), num(b[3], b[4])), (Some(h), Some(m)) if h < 24 && m < 60)
}

/// IANA zone id syntax: `UTC`, `Asia/Jakarta`, `America/Argentina/Buenos_Aires`, `Etc/GMT+7`.
/// Characters `A-Za-z0-9_+-` per component, components separated by `/`, none empty. Whether the
/// zone EXISTS is the scheduler's question (it owns the time zone data).
fn is_zone_syntax(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= TZ_MAX_CHARS
        && s.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
        })
}

impl AutomationPolicy {
    /// Validate the policy against the profile it will run (AP-2.1, SC-1, WA-2, SE-7). All
    /// errors are collected so the editor can list them (AP-2.4). Field paths use dots
    /// (`trigger.time`); codes are a closed snake_case vocabulary and never carry user text.
    pub fn validate(&self, profile: &ProfilePolicy) -> Vec<FieldError> {
        let mut errors: Vec<FieldError> = Vec::new();
        if self.retries > MAX_RETRIES {
            errors.push(fe("retries", "retries_out_of_range"));
        }
        match &self.trigger {
            Trigger::Schedule(s) => {
                if !is_clock_time(&s.time) {
                    errors.push(fe("trigger.time", "time_invalid"));
                }
                if !is_zone_syntax(&s.tz) {
                    errors.push(fe("trigger.tz", "tz_invalid"));
                }
                match &s.freq {
                    Freq::Once { date } => {
                        if !is_calendar_date(date) {
                            errors.push(fe("trigger.date", "date_invalid"));
                        }
                    }
                    Freq::Daily {} => {}
                    Freq::Weekly { days } => {
                        let sorted_unique = days.windows(2).all(|w| w[0] < w[1]);
                        if days.is_empty() {
                            errors.push(fe("trigger.days", "days_required"));
                        } else if days.iter().any(|d| *d > 6) || !sorted_unique {
                            errors.push(fe("trigger.days", "days_invalid"));
                        }
                    }
                }
            }
            Trigger::Watch(w) => {
                if !w.watch_left && !w.watch_right {
                    errors.push(fe("trigger.watch", "watch_root_required"));
                }
                if !w.on_changes && !w.on_available {
                    errors.push(fe("trigger.on", "watch_event_required"));
                }
                if !IDLE_SECS_RANGE.contains(&w.idle_secs) {
                    errors.push(fe("trigger.idleSecs", "idle_out_of_range"));
                }
                if !MAX_WAIT_SECS_RANGE.contains(&w.max_wait_secs) {
                    errors.push(fe("trigger.maxWaitSecs", "max_wait_out_of_range"));
                }
                // WA-2.3 / design Q2: only local drives have native change notifications.
                for (watched, spec, field) in [
                    (w.watch_left, &profile.left, "trigger.watchLeft"),
                    (w.watch_right, &profile.right, "trigger.watchRight"),
                ] {
                    if watched && matches!(spec.identity, RootIdentity::Network { .. }) {
                        errors.push(fe(field, "watch_unsupported_root"));
                    }
                }
            }
        }
        errors
    }
}

impl AutomationDraft {
    /// Validate name, profile id shape and policy (the profile itself is looked up by the
    /// caller; its policy is needed for the trigger checks).
    pub fn validate(
        &self,
        profile: &ProfilePolicy,
    ) -> Result<ValidatedAutomation, Vec<FieldError>> {
        let mut errors: Vec<FieldError> = Vec::new();
        let name = self.name.trim();
        if name.is_empty() {
            errors.push(fe("name", "name_required"));
        } else if name.chars().count() > NAME_MAX_CHARS {
            errors.push(fe("name", "name_too_long"));
        }
        if has_unsafe_char(&self.name) {
            errors.push(fe("name", "name_control"));
        }
        if !is_profile_id(&self.profile_id) {
            errors.push(fe("profileId", "profile_required"));
        }
        errors.extend(self.policy.validate(profile));
        if errors.is_empty() {
            Ok(ValidatedAutomation {
                name: name.to_string(),
                profile_id: self.profile_id.clone(),
                policy: self.policy.clone(),
            })
        } else {
            Err(errors)
        }
    }
}

/// Display name of a profile field code in an event line ("delete policy").
pub fn field_label(field: &str) -> &'static str {
    match field {
        "leftRoot" => "left folder",
        "rightRoot" => "right folder",
        "leftIdentity" => "left folder identity",
        "rightIdentity" => "right folder identity",
        "mode" => "direction",
        "deletePolicy" => "delete policy",
        "compare" => "compare mode",
        "filters" => "filters",
        "guard" => "delete guard",
        _ => "settings",
    }
}

#[cfg(test)]
mod automation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_engine::roots::{FsProbe, RootError};

    /// String-level probe: same refusal rules as the real one, but never touches the
    /// filesystem and answers identity from a fixed local volume (or network for UNC).
    struct FakeProbe {
        fixed: bool,
        fail_identity: bool,
    }

    impl Default for FakeProbe {
        fn default() -> Self {
            Self {
                fixed: true,
                fail_identity: false,
            }
        }
    }

    impl RootProbe for FakeProbe {
        fn canonical(&self, path: &str) -> Result<String, RootError> {
            let p = crate::path_guard::check_str(path).map_err(|_| RootError::Invalid)?;
            let b = p.as_bytes();
            if b.len() >= 3 && b[0].is_ascii_lowercase() && b[1] == b':' {
                return Ok(format!("{}{}", b[0].to_ascii_uppercase() as char, &p[1..]));
            }
            Ok(p)
        }

        fn identity(&self, canonical: &str) -> Result<RootIdentity, RootError> {
            if self.fail_identity {
                return Err(RootError::Io);
            }
            if canonical.starts_with(r"\\") {
                return Ok(RootIdentity::Network {
                    unc_prefix: canonical.to_lowercase(),
                    volume_serial: Some("00000002".into()),
                    file_id: Some("a".repeat(32)),
                });
            }
            Ok(RootIdentity::Local {
                volume_serial: "0123abcd".into(),
                file_id: "f".repeat(32),
                fs_name: "NTFS".into(),
                drive: if self.fixed {
                    DriveKind::Fixed
                } else {
                    DriveKind::Removable
                },
            })
        }
    }

    fn guard(enabled: bool) -> DraftGuard {
        DraftGuard {
            enabled,
            max_count: 50,
            max_percent: 20,
        }
    }

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
                guard: guard(true),
            },
        }
    }

    fn ok(d: &Draft, probe: &dyn RootProbe) -> ValidatedProfile {
        d.validate(probe)
            .unwrap_or_else(|e| panic!("errors: {e:?}"))
    }

    fn codes(errors: &[FieldError], field: &str) -> Vec<String> {
        errors
            .iter()
            .filter(|e| e.field == field)
            .map(|e| e.code.clone())
            .collect()
    }

    /// SP-1.4: every forbidden path form is rejected; drive and UNC forms are accepted.
    #[test]
    fn sp_1_4_rejects_every_forbidden_path_form() {
        let probe = FakeProbe::default();
        for bad in [
            r"\\?\C:\Photos",
            r"\\.\C:\Photos",
            r"\??\C:\Photos",
            r"C:\Photos:stream",
            r"C:\Photos:gone.txt",
            r"\\nas\pipe\x",
            r"\\nas\mailslot\x",
            r"\\nas\IPC$\x",
            "Photos",
            ".\\Photos",
            "C:relative",
            "C:",
        ] {
            let mut d = draft("n", bad, r"D:\Other");
            d.policy.left = bad.into();
            let errors = d.validate(&probe).unwrap_err();
            assert!(
                codes(&errors, "left").iter().any(|c| c == "root_invalid"),
                "{bad:?} must be rejected, got {errors:?}"
            );
        }
        // Empty / whitespace-only roots are a required-field error.
        for bad in ["", "   "] {
            let mut d = draft("n", r"C:\From", r"D:\Other");
            d.policy.left = bad.into();
            let errors = d.validate(&probe).unwrap_err();
            assert_eq!(codes(&errors, "left"), ["root_required"], "{bad:?}");
        }
        for good in [r"D:\Photos", r"\\nas\photos", r"D:/mixed/separators"] {
            let d = draft("n", r"C:\From", good);
            let v = ok(&d, &probe);
            assert_eq!(
                v.policy.right.path,
                FakeProbe::default().canonical(good).unwrap()
            );
        }
    }

    /// SP-1.5: same folder (including different case) and nested roots are rejected; sibling
    /// folders and prefix-name collisions stay allowed.
    #[test]
    fn sp_1_5_rejects_identical_and_nested_roots() {
        let probe = FakeProbe::default();
        let errors = draft("n", r"C:\Data", r"c:\data")
            .validate(&probe)
            .unwrap_err();
        assert_eq!(codes(&errors, "right"), ["roots_identical"]);
        for (left, right) in [(r"C:\Data", r"C:\Data\Sub"), (r"C:\Data\Sub", r"C:\Data")] {
            let errors = draft("n", left, right).validate(&probe).unwrap_err();
            assert_eq!(codes(&errors, "right"), ["roots_nested"], "{left} {right}");
        }
        for (left, right) in [
            (r"C:\Data", r"C:\Data-Backup"),
            (r"C:\Data1\Sub", r"C:\Data\Sub"),
        ] {
            assert!(
                draft("n", left, right).validate(&probe).is_ok(),
                "{left} vs {right}"
            );
        }
    }

    /// SP-1.5 with the real probe: a junction pointing at the left root must be detected as the
    /// same folder after canonicalization.
    #[cfg(windows)]
    #[test]
    fn sp_1_5_detects_a_junction_alias_after_canonicalization() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("via-junction");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(made, "junction created");
        let probe = FsProbe;
        let errors = draft("n", &real.to_string_lossy(), &link.to_string_lossy())
            .validate(&probe)
            .unwrap_err();
        assert_eq!(codes(&errors, "right"), ["roots_identical"]);
    }

    /// SP-1.3: name length, trimming, control characters.
    #[test]
    fn sp_1_3_name_rules() {
        let probe = FakeProbe::default();
        assert_eq!(
            codes(
                &draft("", r"C:\A", r"D:\B").validate(&probe).unwrap_err(),
                "name"
            ),
            ["name_required"]
        );
        let long = "x".repeat(NAME_MAX_CHARS + 1);
        assert_eq!(
            codes(
                &draft(&long, r"C:\A", r"D:\B").validate(&probe).unwrap_err(),
                "name"
            ),
            ["name_too_long"]
        );
        // Exactly 80 characters is fine, and the stored name is trimmed.
        let v = ok(
            &draft(&"x".repeat(NAME_MAX_CHARS), r"C:\A", r"D:\B"),
            &probe,
        );
        assert_eq!(v.name.len(), NAME_MAX_CHARS);
        let v = ok(&draft("  spaced  ", r"C:\A", r"D:\B"), &probe);
        assert_eq!(v.name, "spaced");
        assert_eq!(
            codes(
                &draft("bad\nname", r"C:\A", r"D:\B")
                    .validate(&probe)
                    .unwrap_err(),
                "name"
            ),
            ["name_control"]
        );
    }

    /// SP-4.2: at most 64 patterns of at most 260 characters per list; control characters are
    /// refused.
    #[test]
    fn sp_4_2_filter_limits() {
        let probe = FakeProbe::default();
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.exclude = (0..MAX_FILTERS + 1).map(|i| format!("p{i}")).collect();
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "exclude"),
            ["filters_too_many"]
        );
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.include = vec!["*".repeat(MAX_FILTER_CHARS + 1)];
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "include"),
            ["filter_too_long"]
        );
        // At the limits everything passes.
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.include = (0..MAX_FILTERS).map(|i| format!("p{i}")).collect();
        d.policy.exclude = vec!["*".repeat(MAX_FILTER_CHARS)];
        ok(&d, &probe);
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.exclude = vec!["bad\npattern".into()];
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "exclude"),
            ["filter_control"]
        );
    }

    /// FER-522 rework 1: an enabled guard that can never fire counts as off.
    #[test]
    fn an_ineffective_guard_is_off() {
        let g = |enabled, max_count, max_percent| DeleteGuard {
            enabled,
            max_count,
            max_percent,
        };
        assert!(g(true, 50, 20).effective());
        assert!(g(true, 1, 99).effective());
        assert!(!g(false, 50, 20).effective());
        assert!(!g(true, 50, 100).effective());
        assert!(!g(true, 50, 255).effective());
        assert!(!g(true, 0, 20).effective());
    }

    /// SP-5.1: guard ranges (count 1..=100000, percent 1..=99; 100 % could never trip).
    #[test]
    fn sp_5_1_guard_ranges() {
        let probe = FakeProbe::default();
        for (count, percent) in [
            (0u32, 20u8),
            (GUARD_MAX_COUNT + 1, 20),
            (50, 0),
            (50, 100),
            (50, 101),
        ] {
            let mut d = draft("n", r"C:\A", r"D:\B");
            d.policy.guard.max_count = count;
            d.policy.guard.max_percent = percent;
            let errors = d.validate(&probe).unwrap_err();
            assert_eq!(codes(&errors, "guard").len(), 1, "{count} {percent}");
        }
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.guard.max_count = GUARD_MAX_COUNT;
        d.policy.guard.max_percent = GUARD_MAX_PERCENT;
        ok(&d, &probe);
    }

    /// SP-2: oneWay/mirror need a source side; twoWay takes none; propagation is two-way only.
    #[test]
    fn mode_side_and_propagation_rules() {
        let probe = FakeProbe::default();
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.from = None;
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "from"),
            ["side_required"]
        );
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.mode = WireMode::TwoWay;
        d.policy.from = Some(WireSide::Left);
        let errors = d.validate(&probe).unwrap_err();
        assert_eq!(codes(&errors, "from"), ["side_not_allowed"]);
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.mode = WireMode::TwoWay;
        d.policy.from = None;
        d.policy.propagate_deletes = Some(true);
        let v = ok(&d, &probe);
        assert_eq!(
            v.policy.mode,
            SyncMode::TwoWay {
                propagate_deletes: true
            }
        );
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.propagate_deletes = Some(true);
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "propagateDeletes"),
            ["propagate_not_allowed"]
        );
    }

    /// SP-3.1: `recycleBin` is refused when a side that can receive deletes is not a local fixed
    /// drive (removable or network here).
    #[test]
    fn sp_3_1_recycle_bin_needs_a_fixed_drive_on_every_receiving_side() {
        let probe = FakeProbe {
            fixed: false,
            fail_identity: false,
        };
        // Mirror onto a removable right side: refused for `right`.
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.mode = WireMode::Mirror;
        let errors = d.validate(&probe).unwrap_err();
        assert_eq!(codes(&errors, "right"), ["recycle_bin_unavailable"]);
        // OneWay never deletes: allowed anywhere.
        assert!(draft("n", r"C:\A", r"D:\B").validate(&probe).is_ok());
        // Two-way with propagation can delete on both sides.
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.mode = WireMode::TwoWay;
        d.policy.from = None;
        d.policy.propagate_deletes = Some(true);
        let errors = d.validate(&probe).unwrap_err();
        assert!(codes(&errors, "left").contains(&"recycle_bin_unavailable".into()));
        assert!(codes(&errors, "right").contains(&"recycle_bin_unavailable".into()));
    }

    /// SP-3.2: the versioning root must exist in the draft, be a valid absolute folder and
    /// neither sit inside a sync root nor contain one.
    #[test]
    fn sp_3_2_versioning_root_rules() {
        let probe = FakeProbe::default();
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.delete_policy = WireDeletePolicy::Versioning;
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "versioningRoot"),
            ["versioning_root_required"]
        );
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.delete_policy = WireDeletePolicy::Versioning;
        d.policy.versioning_root = Some(r"E:\Versions".into());
        let v = ok(&d, &probe);
        assert_eq!(
            v.policy.delete_policy,
            DeletePolicy::Versioning {
                root: r"E:\Versions".into()
            }
        );
        // Inside a root / containing a root / identical to a root: all refused.
        for vr in [r"C:\A\Versions", r"C:\", r"C:\A"] {
            let mut d = draft("n", r"C:\A", r"D:\B");
            d.policy.delete_policy = WireDeletePolicy::Versioning;
            d.policy.versioning_root = Some(vr.into());
            let errors = d.validate(&probe).unwrap_err();
            assert_eq!(
                codes(&errors, "versioningRoot"),
                ["versioning_root_overlap"],
                "{vr:?}"
            );
        }
        // A versioning root with a non-versioning policy is refused.
        let mut d = draft("n", r"C:\A", r"D:\B");
        d.policy.versioning_root = Some(r"E:\Versions".into());
        assert_eq!(
            codes(&d.validate(&probe).unwrap_err(), "versioningRoot"),
            ["versioning_root_not_allowed"]
        );
    }

    /// A root that exists but cannot be probed is `root_unavailable`; a missing folder would be
    /// `root_missing` — never treated as empty.
    #[test]
    fn unreachable_roots_are_reported_not_guessed() {
        let probe = FakeProbe {
            fixed: true,
            fail_identity: true,
        };
        let errors = draft("n", r"C:\A", r"D:\B").validate(&probe).unwrap_err();
        assert_eq!(codes(&errors, "left"), ["root_unavailable"]);
        assert_eq!(codes(&errors, "right"), ["root_unavailable"]);
    }

    /// SP-6.3: the changes list carries old → new display values per policy field; a rename is
    /// not part of it (revisions compare policy against policy).
    #[test]
    fn field_changes_lists_from_and_to_display_values() {
        let probe = FakeProbe::default();
        let old = ok(&draft("n", r"C:\A", r"D:\B"), &probe).policy;
        let mut d = draft("n", r"C:\A", r"E:\New");
        d.policy.delete_policy = WireDeletePolicy::Permanent;
        d.policy.guard.enabled = false;
        let new = ok(&d, &probe).policy;
        let changes = field_changes(&old, &new);
        let find = |f: &str| changes.iter().find(|c| c.field == f).unwrap();
        assert_eq!(find("rightRoot").from, r"D:\B");
        assert_eq!(find("rightRoot").to, r"E:\New");
        assert_eq!(find("deletePolicy").from, "Recycle Bin");
        assert_eq!(find("deletePolicy").to, "Permanent");
        assert!(find("guard").from.starts_with("on · "));
        assert_eq!(find("guard").to, "off");
        assert_eq!(changes.len(), 3);
        // Identical policies never claim a change.
        assert!(field_changes(&old, &old).is_empty());
    }

    /// Display values are bounded (a huge path must not blow up a dialog row).
    #[test]
    fn display_values_are_shortened_in_the_middle() {
        assert_eq!(shorten_middle("abcdef", 6), "abcdef");
        let s = shorten_middle(&"x".repeat(300), 120);
        assert_eq!(s.chars().count(), 120);
        assert!(s.contains('…'));
        let head = s.chars().take(60).collect::<String>();
        assert_eq!(head, "x".repeat(60));
    }

    /// SE-7: ids are strictly lowercase UUID v4 text, and freshly minted ids pass the same check.
    #[test]
    fn profile_ids_are_lowercase_uuid_v4() {
        assert!(is_profile_id("3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f"));
        assert!(!is_profile_id("3F2A9C1E-5B6D-4C8A-9E0F-1A2B3C4D5E6F")); // uppercase
        assert!(!is_profile_id("3f2a9c1e5b6d4c8a9e0f1a2b3c4d5e6f")); // no dashes
        assert!(!is_profile_id("3f2a9c1e-5b6d-2c8a-9e0f-1a2b3c4d5e6f")); // not v4
        assert!(!is_profile_id("3f2a9c1e-5b6d-4c8a-ce0f-1a2b3c4d5e6f")); // bad variant
        assert!(!is_profile_id(""));
        let fresh = new_profile_id().unwrap();
        assert!(is_profile_id(&fresh), "{fresh}");
        assert_ne!(fresh, new_profile_id().unwrap());
    }

    /// SE-1 backstop: an input field that is not part of the contract (for example something
    /// behaving like a `confirmed` flag) is refused at deserialization, never interpreted.
    #[test]
    fn unknown_draft_fields_are_refused() {
        let base = serde_json::json!({
            "name": "n",
            "left": "C:\\A", "right": "D:\\B",
            "mode": "oneWay", "from": "left",
            "deletePolicy": "recycleBin", "compare": "sizeAndDate",
            "include": [], "exclude": [],
            "guard": { "enabled": true, "maxCount": 50, "maxPercent": 20 }
        });
        assert!(serde_json::from_value::<Draft>(base.clone()).is_ok());
        for extra in ["confirmed", "approved", "force"] {
            let mut sneaky = base.clone();
            sneaky[extra] = serde_json::json!(true);
            assert!(
                serde_json::from_value::<Draft>(sneaky).is_err(),
                "{extra} must not pass"
            );
        }
        // Unknown enum values are refused too.
        let mut bad = base.clone();
        bad["mode"] = serde_json::json!("bothWays");
        assert!(serde_json::from_value::<Draft>(bad).is_err());
        let mut bad = base;
        bad["guard"]["maxPercent"] = serde_json::json!(500);
        assert!(serde_json::from_value::<Draft>(bad).is_err());
    }

    /// The stored policy round-trips through its JSON form with `deny_unknown_fields`.
    #[test]
    fn policy_json_round_trip_is_strict() {
        let probe = FakeProbe::default();
        let policy = ok(&draft("n", r"C:\A", r"D:\B"), &probe).policy;
        let json = serde_json::to_vec(&policy).unwrap();
        let back: ProfilePolicy = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, policy);
        let mut sneaky = serde_json::from_slice::<serde_json::Value>(&json).unwrap();
        sneaky["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<ProfilePolicy>(sneaky).is_err());
    }

    /// FER-562: a root inside (or equal to) Ferrum's state folder is refused on any side; a root
    /// that contains it is refused only where the profile writes.
    #[test]
    fn fer_562_roots_in_or_above_a_protected_folder_are_refused() {
        let _g = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::protected_dirs::register([std::path::PathBuf::from(r"C:\Ferrum\State")]);
        let probe = FakeProbe::default();
        let check = |d: &Draft| d.validate(&probe).err().unwrap_or_default();

        // One-way left -> right: right is written, left is only read.
        for inside in [
            r"C:\Ferrum\State",
            r"C:\Ferrum\State\sync",
            r"c:\ferrum\STATE\x",
        ] {
            let e = check(&draft("n", r"D:\From", inside));
            assert_eq!(codes(&e, "right"), ["root_protected_folder"], "{inside}");
            let e = check(&draft("n", inside, r"D:\To"));
            assert_eq!(
                codes(&e, "left"),
                ["root_protected_folder"],
                "read {inside}"
            );
        }
        // A share that reaches this very machine can alias a protected folder: refused with
        // its own code, on a read-only side too.
        let e = check(&draft("n", r"D:\From", r"\\localhost\c$\Users\x"));
        assert_eq!(codes(&e, "right"), ["root_local_share"]);
        let e = check(&draft("n", r"\\127.0.0.1\share", r"D:\To"));
        assert_eq!(codes(&e, "left"), ["root_local_share"]);
        let e = check(&draft("n", r"D:\From", r"C:\Ferrum"));
        assert_eq!(codes(&e, "right"), ["root_contains_protected_folder"]);
        assert!(
            check(&draft("n", r"C:\Ferrum", r"D:\To")).is_empty(),
            "read-only ancestor"
        );
        assert!(check(&draft("n", r"C:\Data", r"D:\To")).is_empty());

        // Two-way writes both sides.
        let mut two = draft("n", r"C:\Ferrum", r"D:\Other");
        two.policy.mode = WireMode::TwoWay;
        two.policy.from = None;
        two.policy.propagate_deletes = Some(false);
        two.policy.delete_policy = WireDeletePolicy::Permanent;
        assert_eq!(
            codes(&check(&two), "left"),
            ["root_contains_protected_folder"]
        );

        // The versioning root is always written.
        let mut v = draft("n", r"D:\From", r"D:\To");
        v.policy.delete_policy = WireDeletePolicy::Versioning;
        v.policy.versioning_root = Some(r"C:\Ferrum\State\versions".into());
        assert_eq!(
            codes(&check(&v), "versioningRoot"),
            ["root_protected_folder"]
        );
        v.policy.versioning_root = Some(r"C:\Ferrum".into());
        assert_eq!(
            codes(&check(&v), "versioningRoot"),
            ["root_contains_protected_folder"]
        );
        v.policy.versioning_root = Some(r"D:\Versions".into());
        assert!(check(&v).is_empty());
        crate::protected_dirs::register(Vec::<std::path::PathBuf>::new());
    }

    /// FER-562: the Windows Startup folder is refused the same way.
    #[cfg(windows)]
    #[test]
    fn fer_562_roots_in_or_above_startup_are_refused() {
        let _g = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let probe = FakeProbe::default();
        let startup =
            std::env::var("APPDATA").unwrap() + r"\Microsoft\Windows\Start Menu\Programs\Startup";
        let programs = std::path::Path::new(&startup)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let e = draft("n", r"D:\From", &startup)
            .validate(&probe)
            .unwrap_err();
        assert_eq!(codes(&e, "right"), ["root_startup_folder"]);
        let e = draft("n", r"D:\From", &format!(r"{startup}\sync"))
            .validate(&probe)
            .unwrap_err();
        assert_eq!(codes(&e, "right"), ["root_startup_folder"]);
        let e = draft("n", r"D:\From", &programs)
            .validate(&probe)
            .unwrap_err();
        assert_eq!(codes(&e, "right"), ["root_contains_startup_folder"]);
        let home = std::env::var("USERPROFILE").unwrap();
        let e = draft("n", r"D:\From", &home).validate(&probe).unwrap_err();
        assert_eq!(codes(&e, "right"), ["root_contains_startup_folder"]);
        // Reading from a profile that holds Startup stays possible.
        assert!(draft("n", &home, r"D:\To").validate(&probe).is_ok());
    }
}
