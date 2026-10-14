//! The operations of one profile run: derived from the FINAL plan, ordered per design §5.4, with
//! the delete safety guard (SP-5) and the plan fingerprint used to tie a manual run to the preview
//! the user looked at.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::automation::model::{DeleteGuard, Side};
use crate::compare::{CompareDirsReport, DirDiffEntry, DirDiffState};
use crate::sync_engine::plan::{SyncAction, SyncPlanItem};

/// What an operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// Non-recursive `mkdir` on the target side.
    CreateDir,
    /// Copy a file that does not exist on the target side.
    CopyNew,
    /// Replace an existing target file.
    Overwrite,
    /// Delete one file on the target side.
    DeleteFile,
    /// Non-recursive `remove_dir` on the target side (not empty = skipped).
    RemoveEmptyDir,
}

/// Status of one operation (RQ-5: Done / In progress / Failed / Not started / Held / Pending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OpStatus {
    NotStarted,
    Running,
    Done,
    /// Nothing to do any more (folder already gone, not empty, ...): not an error.
    Skipped,
    /// Source or target changed since the plan (RQ-7.5): not done, a recheck is pending.
    Changed,
    Failed,
    /// A conflict: never resolved unattended (RQ-6).
    Held,
}

/// One operation with the facts the plan saw (for the just-in-time check).
#[derive(Debug, Clone)]
pub struct Op {
    pub index: usize,
    pub name: String,
    pub action: SyncAction,
    pub kind: OpKind,
    /// The side that is written to or deleted from.
    pub target: Side,
    pub entry: DirDiffEntry,
    /// Held conflicts have no executable kind.
    pub held: bool,
}

impl Op {
    pub fn source(&self) -> Side {
        self.target.opposite()
    }

    /// Number of path components (folders order by depth).
    pub fn depth(&self) -> usize {
        self.name.matches('/').count() + 1
    }
}

/// Build the operations of a final plan. `Skip` items produce none; conflicts are `held`.
pub fn build_ops(plan: &[SyncPlanItem], report: &CompareDirsReport) -> Vec<Op> {
    let by_name: std::collections::HashMap<&str, &DirDiffEntry> = report
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();
    let mut ops = Vec::new();
    for item in plan {
        let Some(entry) = by_name.get(item.name.as_str()) else {
            continue;
        };
        let (kind, target, held) = match item.action {
            SyncAction::Skip => continue,
            SyncAction::Conflict => (OpKind::CopyNew, Side::Right, true),
            SyncAction::CopyToRight => (OpKind::CopyNew, Side::Right, false),
            SyncAction::CopyToLeft => (OpKind::CopyNew, Side::Left, false),
            SyncAction::UpdateRight => (OpKind::Overwrite, Side::Right, false),
            SyncAction::UpdateLeft => (OpKind::Overwrite, Side::Left, false),
            SyncAction::DeleteRight => (OpKind::DeleteFile, Side::Right, false),
            SyncAction::DeleteLeft => (OpKind::DeleteFile, Side::Left, false),
            SyncAction::CreateDirRight => (OpKind::CreateDir, Side::Right, false),
            SyncAction::CreateDirLeft => (OpKind::CreateDir, Side::Left, false),
            SyncAction::RemoveEmptyDirRight => (OpKind::RemoveEmptyDir, Side::Right, false),
            SyncAction::RemoveEmptyDirLeft => (OpKind::RemoveEmptyDir, Side::Left, false),
        };
        ops.push(Op {
            index: ops.len(),
            name: item.name.clone(),
            action: item.action,
            kind,
            target,
            entry: (*entry).clone(),
            held,
        });
    }
    ops
}

/// Fingerprint of the plan's composition: names and actions only (sizes and times may move
/// between a preview and its run without making the plan a different one). Order independent.
pub fn plan_fingerprint(plan: &[SyncPlanItem]) -> String {
    let mut lines: Vec<String> = plan
        .iter()
        .filter(|i| i.action != SyncAction::Skip)
        .map(|i| format!("{:?}|{}|{}", i.action, i.is_dir, i.name))
        .collect();
    lines.sort();
    let mut hash = Sha256::new();
    for line in &lines {
        hash.update(line.as_bytes());
        hash.update(b"\n");
    }
    hex::encode(hash.finalize())
}

// ── Delete safety guard (SP-5) ──

/// Verdict of the delete guard, decided before any operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "camelCase")]
pub enum GuardVerdict {
    /// The guard is off in the profile (the empty-source rule still applies).
    Off,
    /// Within the limits.
    Ok,
    /// More than `max(count, percent of the side's entries)` would be deleted (SP-5.1).
    #[serde(rename_all = "camelCase")]
    Tripped {
        side: Side,
        deletes: u32,
        entries: u32,
        percent: u32,
    },
    /// The source side has no entries while the target has some and deletes are planned
    /// (SP-5.2), whatever the thresholds.
    #[serde(rename_all = "camelCase")]
    EmptySource { empty: Side },
}

impl GuardVerdict {
    pub fn trips(&self) -> bool {
        matches!(
            self,
            GuardVerdict::Tripped { .. } | GuardVerdict::EmptySource { .. }
        )
    }

    pub fn code(&self) -> &'static str {
        match self {
            GuardVerdict::Off => "off",
            GuardVerdict::Ok => "ok",
            GuardVerdict::Tripped { .. } => "tripped",
            GuardVerdict::EmptySource { .. } => "emptySource",
        }
    }
}

fn present(e: &DirDiffEntry, side: Side) -> bool {
    match side {
        Side::Left => e.state != DirDiffState::RightOnly,
        Side::Right => e.state != DirDiffState::LeftOnly,
    }
}

/// Entries of `side` that were compared (after filters).
pub fn side_entries(report: &CompareDirsReport, side: Side) -> u32 {
    report.entries.iter().filter(|e| present(e, side)).count() as u32
}

/// FILE entries of `side` (the guard counts files on both sides of its fraction: deleting files
/// is what it limits, so folders do not dilute the denominator).
pub fn side_files(report: &CompareDirsReport, side: Side) -> u32 {
    report
        .entries
        .iter()
        .filter(|e| !e.is_dir && present(e, side))
        .count() as u32
}

/// Evaluate the guard over the executable operations. Only FILE deletes count (the unit of data
/// loss); removing an empty folder destroys nothing.
pub fn evaluate_guard(guard: &DeleteGuard, report: &CompareDirsReport, ops: &[Op]) -> GuardVerdict {
    for side in [Side::Left, Side::Right] {
        let deletes = ops
            .iter()
            .filter(|o| !o.held && o.kind == OpKind::DeleteFile && o.target == side)
            .count() as u32;
        if deletes == 0 {
            continue;
        }
        // SP-5.2: regardless of the thresholds.
        let other = side.opposite();
        if side_entries(report, other) == 0 && side_entries(report, side) > 0 {
            return GuardVerdict::EmptySource { empty: other };
        }
    }
    if !guard.enabled {
        return GuardVerdict::Off;
    }
    for side in [Side::Left, Side::Right] {
        let deletes = ops
            .iter()
            .filter(|o| !o.held && o.kind == OpKind::DeleteFile && o.target == side)
            .count() as u32;
        if deletes == 0 {
            continue;
        }
        let entries = side_files(report, side).max(1);
        // deletes > max(count, percent% of entries), without floating point.
        let over_count = deletes > guard.max_count;
        let over_percent =
            u64::from(deletes) * 100 > u64::from(guard.max_percent) * u64::from(entries);
        if over_count && over_percent {
            let percent = (u64::from(deletes) * 100 / u64::from(entries)) as u32;
            return GuardVerdict::Tripped {
                side,
                deletes,
                entries,
                percent,
            };
        }
    }
    GuardVerdict::Ok
}

/// Fixed run error text of a tripped guard (documented: may name the profile's own root).
pub fn guard_text(verdict: &GuardVerdict, left_root: &str, right_root: &str) -> String {
    match verdict {
        GuardVerdict::Tripped {
            side,
            deletes,
            percent,
            ..
        } => {
            let root = match side {
                Side::Left => left_root,
                Side::Right => right_root,
            };
            format!(
                "Would delete {deletes} items ({percent} % of {root}) — over the safety limit. Review in Sync."
            )
        }
        GuardVerdict::EmptySource { .. } => "Source folder is empty — nothing deleted".to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, state: DirDiffState) -> DirDiffEntry {
        DirDiffEntry {
            name: name.into(),
            is_dir: false,
            state,
            left_size: 1,
            left_modified_ms: 1000,
            right_size: 1,
            right_modified_ms: 1000,
        }
    }

    fn report(entries: Vec<DirDiffEntry>) -> CompareDirsReport {
        CompareDirsReport {
            left_root: "L".into(),
            right_root: "R".into(),
            entries,
            truncated: false,
            recursive: true,
            cancelled: false,
            errors: vec![],
            fast_scan: None,
            journal: None,
            incremental: None,
        }
    }

    fn delete_ops(report: &CompareDirsReport) -> Vec<Op> {
        report
            .entries
            .iter()
            .filter(|e| e.state == DirDiffState::RightOnly)
            .enumerate()
            .map(|(i, e)| Op {
                index: i,
                name: e.name.clone(),
                action: SyncAction::DeleteRight,
                kind: OpKind::DeleteFile,
                target: Side::Right,
                entry: e.clone(),
                held: false,
            })
            .collect()
    }

    const GUARD: DeleteGuard = DeleteGuard {
        enabled: true,
        max_count: 50,
        max_percent: 20,
    };

    // SP-5.1 boundaries: both the count and the percent threshold must be exceeded.
    #[test]
    fn guard_needs_both_thresholds_exceeded() {
        let mk = |same: usize, extra: usize| {
            let mut e = Vec::new();
            for i in 0..same {
                e.push(entry(&format!("s{i}"), DirDiffState::Same));
            }
            for i in 0..extra {
                e.push(entry(&format!("x{i}"), DirDiffState::RightOnly));
            }
            report(e)
        };
        // 51 of 100 entries on the right: over 50 and over 20 %.
        let r = mk(49, 51);
        let v = evaluate_guard(&GUARD, &r, &delete_ops(&r));
        assert!(matches!(
            v,
            GuardVerdict::Tripped {
                deletes: 51,
                entries: 100,
                percent: 51,
                ..
            }
        ));
        // Exactly 50: not over the count.
        let r = mk(50, 50);
        assert_eq!(
            evaluate_guard(&GUARD, &r, &delete_ops(&r)),
            GuardVerdict::Ok
        );
        // 60 of 1000: over the count, not over 20 %.
        let r = mk(940, 60);
        assert_eq!(
            evaluate_guard(&GUARD, &r, &delete_ops(&r)),
            GuardVerdict::Ok
        );
        // Guard off: the percent rule is skipped.
        let off = DeleteGuard {
            enabled: false,
            ..GUARD
        };
        let r = mk(49, 51);
        assert_eq!(evaluate_guard(&off, &r, &delete_ops(&r)), GuardVerdict::Off);
    }

    // SP-5.2: an empty source with a populated target holds the run, even with the guard off.
    #[test]
    fn empty_source_is_held_regardless_of_thresholds() {
        let r = report(vec![
            entry("a", DirDiffState::RightOnly),
            entry("b", DirDiffState::RightOnly),
        ]);
        for guard in [
            GUARD,
            DeleteGuard {
                enabled: false,
                ..GUARD
            },
        ] {
            let v = evaluate_guard(&guard, &r, &delete_ops(&r));
            assert_eq!(v, GuardVerdict::EmptySource { empty: Side::Left });
        }
        // No deletes planned: nothing to hold.
        assert_eq!(evaluate_guard(&GUARD, &r, &[]), GuardVerdict::Ok);
    }

    #[test]
    fn fingerprint_ignores_order_and_facts_but_not_actions() {
        let item = |name: &str, action| SyncPlanItem {
            name: name.into(),
            state: DirDiffState::LeftOnly,
            action,
            reason: None,
            is_dir: false,
            decision: None,
            left_path: None,
            right_path: None,
        };
        let a = [
            item("a", SyncAction::CopyToRight),
            item("b", SyncAction::DeleteRight),
        ];
        let b = [
            item("b", SyncAction::DeleteRight),
            item("a", SyncAction::CopyToRight),
        ];
        assert_eq!(plan_fingerprint(&a), plan_fingerprint(&b));
        let c = [
            item("a", SyncAction::CopyToRight),
            item("b", SyncAction::Skip),
        ];
        assert_ne!(plan_fingerprint(&a), plan_fingerprint(&c));
        // A skip item does not count.
        let d = [item("a", SyncAction::CopyToRight)];
        assert_eq!(plan_fingerprint(&c), plan_fingerprint(&d));
    }
}
