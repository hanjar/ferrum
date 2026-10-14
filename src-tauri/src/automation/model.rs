//! Sync Profile model (Automation design §4.4): stored types, the IPC draft shape (design §7.1)
//! and validation. Validation is fail-closed: an invalid draft never reaches the store, an
//! invalid stored policy is reported as a profile needing review, never fixed silently.
//!
//! Tauri-free by design (the runner links this module).

use serde::{Deserialize, Serialize};

use crate::sync_engine::roots::{RootError, RootProbe, overlap};

// ── Limits (requirements SP-1.3, SP-1.4, SP-4.2, SP-5.1) ──

pub const NAME_MAX_CHARS: usize = 80;
pub const ROOT_MAX_CHARS: usize = 1024;
pub const MAX_FILTERS: usize = 64;
pub const MAX_FILTER_CHARS: usize = 260;
pub const GUARD_MAX_COUNT: u32 = 100_000;
pub const GUARD_MAX_PERCENT: u8 = 100;

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
    #[serde(rename_all = "camelCase")]
    Network { unc_prefix: String },
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
        if self.name.chars().any(char::is_control) {
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
                if pattern.chars().any(char::is_control) {
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
        RootIdentity::Network { unc_prefix } => unc_prefix.clone(),
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

    /// SP-5.1: guard ranges (count 1..=100000, percent 1..=100).
    #[test]
    fn sp_5_1_guard_ranges() {
        let probe = FakeProbe::default();
        for (count, percent) in [(0u32, 20u8), (GUARD_MAX_COUNT + 1, 20), (50, 0), (50, 101)] {
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
}
