//! Synchronize Directories planning in Rust (SP-2.1, SP-2.2, SP-2.3; design §5.2).
//!
//! 1:1 port of TypeScript `src/lib/syncPlan.ts` with identical classification,
//! plan construction, summary, blockers, and stale plan detection, verified
//! by shared fixtures against Vitest.
//!
//! SP-2.3 extension: two-way deletion propagation is reachable only when
//! `propagate_deletes` is true on two-way sync.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::compare::{CompareDirsReport, DirDiffEntry, DirDiffState};
use crate::sync_baseline::{BASELINE_VERSION, BaselineDto, EntryDto, FactsDto};

pub const MTIME_TOLERANCE_MS: i64 = 2000;
pub const PROFILE_ENTRY_CAP: usize = 200_000;
pub const PROFILE_CAP_BLOCKER_TEXT: &str =
    "Too many items (more than 200,000) — narrow the profile with filters.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SyncDirection {
    #[default]
    LeftToRight,
    RightToLeft,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncAction {
    CopyToRight,
    CopyToLeft,
    UpdateRight,
    UpdateLeft,
    DeleteRight,
    DeleteLeft,
    Conflict,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncConflictReason {
    NoBaseline,
    BothChanged,
    ModifyDelete,
    DeletedLeft,
    DeletedRight,
    CaseCollision,
    TypeMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncDecision {
    KeepLeft,
    KeepRight,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlanItem {
    pub name: String,
    pub state: DirDiffState,
    pub action: SyncAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SyncConflictReason>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dir: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<SyncDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncOptions {
    pub direction: SyncDirection,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete_extras: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub propagate_deletes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub action: SyncAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SyncConflictReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideChange {
    Unchanged,
    Changed,
    Missing,
}

pub fn facts_equal(a: &FactsDto, b: &FactsDto) -> bool {
    a.size == b.size && (a.modified_ms - b.modified_ms).abs() <= MTIME_TOLERANCE_MS
}

pub fn side_change(base: &FactsDto, now: Option<&FactsDto>) -> SideChange {
    match now {
        None => SideChange::Missing,
        Some(now) => {
            if facts_equal(base, now) {
                SideChange::Unchanged
            } else {
                SideChange::Changed
            }
        }
    }
}

pub fn normalize_root(root: &str) -> String {
    root.replace('\\', "/").trim_end_matches('/').to_string()
}

pub fn is_baseline_trusted(baseline: &BaselineDto, left_root: &str, right_root: &str) -> bool {
    if baseline.version != BASELINE_VERSION {
        return false;
    }
    normalize_root(&baseline.left_root) == normalize_root(left_root)
        && normalize_root(&baseline.right_root) == normalize_root(right_root)
}

/// Classify a single entry. One-directional mode follows user direction (mirror/update).
/// Two-way mode never picks winners from timestamps without proof from a trusted baseline.
pub fn classify_detailed(
    entry: &DirDiffEntry,
    opts: &SyncOptions,
    trusted: Option<&BaselineDto>,
) -> Classification {
    let both = opts.direction == SyncDirection::Both;
    let base = if both {
        trusted.and_then(|t| t.entries.get(&entry.name))
    } else {
        None
    };
    let left = FactsDto {
        size: entry.left_size,
        modified_ms: entry.left_modified_ms,
    };
    let right = FactsDto {
        size: entry.right_size,
        modified_ms: entry.right_modified_ms,
    };

    // Directories are valid only as one-sided entries or 'same'
    if entry.is_dir
        && entry.state != DirDiffState::Same
        && entry.state != DirDiffState::LeftOnly
        && entry.state != DirDiffState::RightOnly
    {
        return Classification {
            action: SyncAction::Conflict,
            reason: Some(SyncConflictReason::TypeMismatch),
        };
    }

    match entry.state {
        DirDiffState::Same => Classification {
            action: SyncAction::Skip,
            reason: None,
        },
        DirDiffState::LeftOnly => {
            if opts.direction == SyncDirection::RightToLeft {
                return Classification {
                    action: if opts.delete_extras {
                        SyncAction::DeleteLeft
                    } else {
                        SyncAction::Skip
                    },
                    reason: None,
                };
            }
            if let Some(base) = base {
                let left_change = side_change(&base.left, Some(&left));
                if left_change == SideChange::Changed {
                    Classification {
                        action: SyncAction::Conflict,
                        reason: Some(SyncConflictReason::ModifyDelete),
                    }
                } else if opts.propagate_deletes {
                    // SP-2.3: right deleted, left unchanged -> delete left
                    Classification {
                        action: SyncAction::DeleteLeft,
                        reason: None,
                    }
                } else {
                    Classification {
                        action: SyncAction::Conflict,
                        reason: Some(SyncConflictReason::DeletedRight),
                    }
                }
            } else if both && opts.propagate_deletes {
                // SP-2.3: two-way with propagate_deletes requires trusted baseline for deletions
                Classification {
                    action: SyncAction::Conflict,
                    reason: Some(SyncConflictReason::NoBaseline),
                }
            } else {
                Classification {
                    action: SyncAction::CopyToRight,
                    reason: None,
                }
            }
        }
        DirDiffState::RightOnly => {
            if opts.direction == SyncDirection::LeftToRight {
                return Classification {
                    action: if opts.delete_extras {
                        SyncAction::DeleteRight
                    } else {
                        SyncAction::Skip
                    },
                    reason: None,
                };
            }
            if let Some(base) = base {
                let right_change = side_change(&base.right, Some(&right));
                if right_change == SideChange::Changed {
                    Classification {
                        action: SyncAction::Conflict,
                        reason: Some(SyncConflictReason::ModifyDelete),
                    }
                } else if opts.propagate_deletes {
                    // SP-2.3: left deleted, right unchanged -> delete right
                    Classification {
                        action: SyncAction::DeleteRight,
                        reason: None,
                    }
                } else {
                    Classification {
                        action: SyncAction::Conflict,
                        reason: Some(SyncConflictReason::DeletedLeft),
                    }
                }
            } else if both && opts.propagate_deletes {
                Classification {
                    action: SyncAction::Conflict,
                    reason: Some(SyncConflictReason::NoBaseline),
                }
            } else {
                Classification {
                    action: SyncAction::CopyToLeft,
                    reason: None,
                }
            }
        }
        DirDiffState::NewerLeft => {
            if opts.direction == SyncDirection::RightToLeft {
                Classification {
                    action: SyncAction::Skip,
                    reason: None,
                }
            } else if opts.direction == SyncDirection::LeftToRight {
                Classification {
                    action: SyncAction::UpdateRight,
                    reason: None,
                }
            } else {
                resolve_both(base, &left, &right)
            }
        }
        DirDiffState::NewerRight => {
            if opts.direction == SyncDirection::LeftToRight {
                Classification {
                    action: SyncAction::Skip,
                    reason: None,
                }
            } else if opts.direction == SyncDirection::RightToLeft {
                Classification {
                    action: SyncAction::UpdateLeft,
                    reason: None,
                }
            } else {
                resolve_both(base, &left, &right)
            }
        }
        DirDiffState::SizeDifferent | DirDiffState::ContentDifferent => {
            if opts.direction == SyncDirection::LeftToRight {
                Classification {
                    action: SyncAction::UpdateRight,
                    reason: None,
                }
            } else if opts.direction == SyncDirection::RightToLeft {
                Classification {
                    action: SyncAction::UpdateLeft,
                    reason: None,
                }
            } else {
                resolve_both(base, &left, &right)
            }
        }
    }
}

fn resolve_both(base: Option<&EntryDto>, left: &FactsDto, right: &FactsDto) -> Classification {
    let Some(base) = base else {
        return Classification {
            action: SyncAction::Conflict,
            reason: Some(SyncConflictReason::NoBaseline),
        };
    };
    let l = side_change(&base.left, Some(left));
    let r = side_change(&base.right, Some(right));
    if l == SideChange::Changed && r == SideChange::Unchanged {
        Classification {
            action: SyncAction::UpdateRight,
            reason: None,
        }
    } else if r == SideChange::Changed && l == SideChange::Unchanged {
        Classification {
            action: SyncAction::UpdateLeft,
            reason: None,
        }
    } else if l == SideChange::Changed && r == SideChange::Changed {
        Classification {
            action: SyncAction::Conflict,
            reason: Some(SyncConflictReason::BothChanged),
        }
    } else if l == SideChange::Unchanged && r == SideChange::Unchanged {
        // FER-414: identical content verified by baseline and neither changed since
        Classification {
            action: SyncAction::Skip,
            reason: None,
        }
    } else {
        Classification {
            action: SyncAction::Conflict,
            reason: Some(SyncConflictReason::NoBaseline),
        }
    }
}

/// Build the full sync plan from a compare report.
pub fn build_sync_plan(
    report: &CompareDirsReport,
    opts: &SyncOptions,
    baseline: Option<&BaselineDto>,
) -> Vec<SyncPlanItem> {
    let sep = if report.left_root.contains('\\') || report.right_root.contains('\\') {
        '\\'
    } else {
        '/'
    };
    let trusted =
        baseline.filter(|b| is_baseline_trusted(b, &report.left_root, &report.right_root));

    let mut by_fold: HashMap<String, usize> = HashMap::new();
    for entry in &report.entries {
        let key = entry.name.to_lowercase();
        *by_fold.entry(key).or_insert(0) += 1;
    }

    report
        .entries
        .iter()
        .map(|entry| {
            let rel = if sep == '\\' {
                entry.name.replace('/', "\\")
            } else {
                entry.name.clone()
            };
            let mut class = classify_detailed(entry, opts, trusted);
            if class.action != SyncAction::Skip
                && by_fold
                    .get(&entry.name.to_lowercase())
                    .copied()
                    .unwrap_or(0)
                    > 1
            {
                class.action = SyncAction::Conflict;
                class.reason = Some(SyncConflictReason::CaseCollision);
            }
            SyncPlanItem {
                name: entry.name.clone(),
                state: entry.state,
                action: class.action,
                reason: class.reason,
                is_dir: entry.is_dir,
                decision: None,
                left_path: Some(format!("{}{sep}{rel}", report.left_root)),
                right_path: Some(format!("{}{sep}{rel}", report.right_root)),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlanSummary {
    pub total: usize,
    pub to_right: usize,
    pub to_left: usize,
    pub overwrites: usize,
    pub deletes: usize,
    pub conflicts: usize,
    pub skipped: usize,
    pub user_skipped: usize,
}

pub fn plan_summary(items: &[SyncPlanItem]) -> PlanSummary {
    let mut s = PlanSummary {
        total: items.len(),
        ..Default::default()
    };
    for item in items {
        if item.action == SyncAction::UpdateRight || item.action == SyncAction::UpdateLeft {
            s.overwrites += 1;
        }
        if item.action == SyncAction::CopyToRight || item.action == SyncAction::UpdateRight {
            s.to_right += 1;
        } else if item.action == SyncAction::CopyToLeft || item.action == SyncAction::UpdateLeft {
            s.to_left += 1;
        } else if item.action == SyncAction::DeleteRight || item.action == SyncAction::DeleteLeft {
            s.deletes += 1;
        } else if item.action == SyncAction::Conflict {
            s.conflicts += 1;
        } else {
            s.skipped += 1;
            if item.decision == Some(SyncDecision::Skip) {
                s.user_skipped += 1;
            }
        }
    }
    s
}

pub fn sync_plan_blockers(report: &CompareDirsReport) -> Vec<String> {
    let mut reasons = Vec::new();
    if report.truncated {
        reasons.push("truncated".to_string());
    }
    if report.cancelled {
        reasons.push("cancelled".to_string());
    }
    if !report.errors.is_empty() {
        reasons.push("errors".to_string());
    }
    reasons
}

pub fn describe_blockers(report: &CompareDirsReport) -> String {
    let mut parts = Vec::new();
    if report.truncated {
        parts.push(
            "the comparison was truncated (too many entries), so the plan would be incomplete"
                .to_string(),
        );
    }
    if report.cancelled {
        parts.push("the comparison was cancelled, so the result is partial".to_string());
    }
    if !report.errors.is_empty() {
        let count = report.errors.len();
        let sample = report
            .errors
            .iter()
            .take(3)
            .map(|e| format!("{}: {}", e.name, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        let suffix = if count > 3 { "; …" } else { "" };
        let word = if count == 1 { "entry" } else { "entries" };
        parts.push(format!(
            "{count} {word} could not be read ({sample}{suffix})"
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("Synchronization blocked: {}.", parts.join("; "))
    }
}

pub fn describe_profile_blockers(report: &CompareDirsReport) -> String {
    if report.truncated {
        PROFILE_CAP_BLOCKER_TEXT.to_string()
    } else {
        describe_blockers(report)
    }
}

pub fn is_resolvable(item: &SyncPlanItem) -> bool {
    item.action == SyncAction::Conflict
        && !matches!(
            item.reason,
            Some(SyncConflictReason::TypeMismatch | SyncConflictReason::CaseCollision)
        )
}

pub fn apply_decisions(
    plan: &[SyncPlanItem],
    decisions: &HashMap<String, SyncDecision>,
) -> Vec<SyncPlanItem> {
    plan.iter()
        .map(|item| {
            let Some(decision) = decisions.get(&item.name).copied() else {
                return item.clone();
            };
            if item.action != SyncAction::Conflict {
                return item.clone();
            }
            if decision == SyncDecision::Skip {
                let mut updated = item.clone();
                updated.action = SyncAction::Skip;
                updated.decision = Some(decision);
                return updated;
            }
            if !is_resolvable(item) {
                return item.clone();
            }
            let action = if decision == SyncDecision::KeepLeft {
                match item.state {
                    DirDiffState::RightOnly => SyncAction::DeleteRight,
                    DirDiffState::LeftOnly => SyncAction::CopyToRight,
                    _ => SyncAction::UpdateRight,
                }
            } else {
                match item.state {
                    DirDiffState::LeftOnly => SyncAction::DeleteLeft,
                    DirDiffState::RightOnly => SyncAction::CopyToLeft,
                    _ => SyncAction::UpdateLeft,
                }
            };
            let mut updated = item.clone();
            updated.action = action;
            updated.decision = Some(decision);
            updated
        })
        .collect()
}

fn state_str(state: DirDiffState) -> &'static str {
    match state {
        DirDiffState::LeftOnly => "leftOnly",
        DirDiffState::RightOnly => "rightOnly",
        DirDiffState::Same => "same",
        DirDiffState::NewerLeft => "newerLeft",
        DirDiffState::NewerRight => "newerRight",
        DirDiffState::SizeDifferent => "sizeDifferent",
        DirDiffState::ContentDifferent => "contentDifferent",
    }
}

pub fn plan_facts(report: &CompareDirsReport, with_state: bool) -> Vec<String> {
    report
        .entries
        .iter()
        .map(|e| {
            let kind = if e.is_dir { 'd' } else { 'f' };
            let left_mtime = if e.is_dir { 0 } else { e.left_modified_ms };
            let right_mtime = if e.is_dir { 0 } else { e.right_modified_ms };
            let state_part = if with_state {
                format!("|{}", state_str(e.state))
            } else {
                String::new()
            };
            format!(
                "{}|{}|{}|{}|{}|{}{}",
                e.name, kind, e.left_size, left_mtime, e.right_size, right_mtime, state_part
            )
        })
        .collect()
}

pub fn is_plan_stale(
    previous: &CompareDirsReport,
    fresh: &CompareDirsReport,
    by_content: bool,
) -> bool {
    if previous.left_root != fresh.left_root || previous.right_root != fresh.right_root {
        return true;
    }
    if previous.recursive != fresh.recursive {
        return true;
    }
    let a = plan_facts(previous, by_content);
    let b = plan_facts(fresh, by_content);
    if a.len() != b.len() {
        return true;
    }
    a.iter().zip(b.iter()).any(|(x, y)| x != y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureExpected {
        items: Vec<SyncPlanItem>,
        summary: PlanSummary,
        blockers: Vec<String>,
        blockers_description: String,
        is_stale: Option<bool>,
        is_stale_by_content: Option<bool>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        name: String,
        report: CompareDirsReport,
        options: SyncOptions,
        baseline: Option<BaselineDto>,
        fresh_report: Option<CompareDirsReport>,
        expected: FixtureExpected,
    }

    #[test]
    fn test_sync_plan_fixtures_parity() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures_dir = manifest_dir.join("../contract-fixtures/sync-plan");
        let mut entries: Vec<_> = fs::read_dir(&fixtures_dir)
            .expect("read fixtures dir")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        entries.sort();

        assert!(!entries.is_empty(), "fixtures should not be empty");

        for path in entries {
            let content = fs::read_to_string(&path).expect("read fixture file");
            let fix: Fixture = serde_json::from_str(&content)
                .unwrap_or_else(|e| panic!("parse fixture {}: {}", path.display(), e));

            let items = build_sync_plan(&fix.report, &fix.options, fix.baseline.as_ref());
            assert_eq!(
                items,
                fix.expected.items,
                "fixture items mismatch: {} ({})",
                path.display(),
                fix.name
            );

            assert_eq!(
                plan_summary(&items),
                fix.expected.summary,
                "fixture summary mismatch: {} ({})",
                path.display(),
                fix.name
            );

            assert_eq!(
                sync_plan_blockers(&fix.report),
                fix.expected.blockers,
                "fixture blockers mismatch: {} ({})",
                path.display(),
                fix.name
            );

            assert_eq!(
                describe_blockers(&fix.report),
                fix.expected.blockers_description,
                "fixture blockers_description mismatch: {} ({})",
                path.display(),
                fix.name
            );

            if let Some(ref fresh) = fix.fresh_report {
                assert_eq!(
                    is_plan_stale(&fix.report, fresh, false),
                    fix.expected.is_stale.unwrap(),
                    "fixture stale mismatch: {} ({})",
                    path.display(),
                    fix.name
                );

                if let Some(stale_by_content) = fix.expected.is_stale_by_content {
                    assert_eq!(
                        is_plan_stale(&fix.report, fresh, true),
                        stale_by_content,
                        "fixture stale by content mismatch: {} ({})",
                        path.display(),
                        fix.name
                    );
                }
            }
        }
    }

    #[test]
    fn test_sp_2_3_two_way_deletion_propagation() {
        let left_root = "C:\\Left";
        let right_root = "D:\\Right";

        let mut baseline_entries = std::collections::BTreeMap::new();
        baseline_entries.insert(
            "file.txt".to_string(),
            EntryDto {
                left: FactsDto {
                    size: 100,
                    modified_ms: 1000,
                },
                right: FactsDto {
                    size: 100,
                    modified_ms: 1000,
                },
            },
        );
        let baseline = BaselineDto {
            version: 1,
            left_root: left_root.to_string(),
            right_root: right_root.to_string(),
            entries: baseline_entries,
            journal: None,
        };

        // 1. deleted-left-unchanged-right with propagate_deletes = true -> deleteRight
        let report_del_left = CompareDirsReport {
            left_root: left_root.to_string(),
            right_root: right_root.to_string(),
            entries: vec![DirDiffEntry {
                name: "file.txt".to_string(),
                is_dir: false,
                state: DirDiffState::RightOnly,
                left_size: 0,
                left_modified_ms: 0,
                right_size: 100,
                right_modified_ms: 1000,
            }],
            truncated: false,
            recursive: true,
            cancelled: false,
            errors: Vec::new(),
            fast_scan: None,
            journal: None,
            incremental: None,
        };

        let opts_prop = SyncOptions {
            direction: SyncDirection::Both,
            delete_extras: false,
            propagate_deletes: true,
        };
        let plan_prop = build_sync_plan(&report_del_left, &opts_prop, Some(&baseline));
        assert_eq!(plan_prop[0].action, SyncAction::DeleteRight);

        // deleted-right-unchanged-left with propagate_deletes = true -> deleteLeft
        let report_del_right = CompareDirsReport {
            left_root: left_root.to_string(),
            right_root: right_root.to_string(),
            entries: vec![DirDiffEntry {
                name: "file.txt".to_string(),
                is_dir: false,
                state: DirDiffState::LeftOnly,
                left_size: 100,
                left_modified_ms: 1000,
                right_size: 0,
                right_modified_ms: 0,
            }],
            truncated: false,
            recursive: true,
            cancelled: false,
            errors: Vec::new(),
            fast_scan: None,
            journal: None,
            incremental: None,
        };
        let plan_del_right = build_sync_plan(&report_del_right, &opts_prop, Some(&baseline));
        assert_eq!(plan_del_right[0].action, SyncAction::DeleteLeft);

        // 2. modified on other side -> conflict modifyDelete
        let report_mod_other = CompareDirsReport {
            left_root: left_root.to_string(),
            right_root: right_root.to_string(),
            entries: vec![DirDiffEntry {
                name: "file.txt".to_string(),
                is_dir: false,
                state: DirDiffState::RightOnly,
                left_size: 0,
                left_modified_ms: 0,
                right_size: 200, // changed
                right_modified_ms: 5000,
            }],
            truncated: false,
            recursive: true,
            cancelled: false,
            errors: Vec::new(),
            fast_scan: None,
            journal: None,
            incremental: None,
        };
        let plan_mod = build_sync_plan(&report_mod_other, &opts_prop, Some(&baseline));
        assert_eq!(plan_mod[0].action, SyncAction::Conflict);
        assert_eq!(plan_mod[0].reason, Some(SyncConflictReason::ModifyDelete));

        // 3. no trusted baseline -> conflict
        let plan_no_base = build_sync_plan(&report_del_left, &opts_prop, None);
        assert_eq!(plan_no_base[0].action, SyncAction::Conflict);
        assert_eq!(plan_no_base[0].reason, Some(SyncConflictReason::NoBaseline));

        // 4. propagate_deletes = false -> conflict deletedLeft (does not delete)
        let opts_no_prop = SyncOptions {
            direction: SyncDirection::Both,
            delete_extras: false,
            propagate_deletes: false,
        };
        let plan_no_prop = build_sync_plan(&report_del_left, &opts_no_prop, Some(&baseline));
        assert_eq!(plan_no_prop[0].action, SyncAction::Conflict);
        assert_eq!(
            plan_no_prop[0].reason,
            Some(SyncConflictReason::DeletedLeft)
        );
    }
}
