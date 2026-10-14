//! Baseline reconciliation and verification in Rust (design §5.2).
//!
//! 1:1 port of TypeScript `src/lib/syncBaseline.ts` (`reconcileBaseline` and
//! `provesFullSync`), verified by shared contract fixtures against Vitest.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::compare::{CompareDirsReport, DirDiffEntry, DirDiffState};
use crate::sync_baseline::{BASELINE_VERSION, BaselineDto, EntryDto, FactsDto};
use crate::sync_engine::plan::{SyncAction, SyncPlanItem, is_baseline_trusted};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemOutcome {
    Ok,
    Failed,
    Cancelled,
}

/// Compute the next baseline after a sync run. `after` is a fresh compare
/// report after the run finished. Only verified identical entries (state Same)
/// with an Ok outcome advance the baseline. Failed/cancelled/conflict items
/// preserve any previous baseline entry without mutating `previous`.
pub fn reconcile_baseline(
    previous: Option<&BaselineDto>,
    after: &CompareDirsReport,
    plan: &[SyncPlanItem],
    outcomes: &HashMap<String, ItemOutcome>,
) -> BaselineDto {
    let mut next = BaselineDto {
        version: BASELINE_VERSION,
        left_root: after.left_root.clone(),
        right_root: after.right_root.clone(),
        entries: BTreeMap::new(),
        journal: None,
    };

    if let Some(prev) =
        previous.filter(|p| is_baseline_trusted(p, &after.left_root, &after.right_root))
    {
        for (name, entry) in &prev.entries {
            next.entries.insert(name.clone(), entry.clone());
        }
    }

    let after_by_name: HashMap<&str, &DirDiffEntry> =
        after.entries.iter().map(|e| (e.name.as_str(), e)).collect();

    for item in plan {
        let now = after_by_name.get(item.name.as_str()).copied();
        let outcome = outcomes.get(&item.name).copied();

        if item.action == SyncAction::Skip && item.state == DirDiffState::Same {
            if let Some(now) = now.filter(|n| n.state == DirDiffState::Same) {
                next.entries.insert(item.name.clone(), to_entry(now));
            }
            continue;
        }

        if outcome != Some(ItemOutcome::Ok) {
            continue;
        }

        match item.action {
            SyncAction::CopyToRight
            | SyncAction::CopyToLeft
            | SyncAction::UpdateRight
            | SyncAction::UpdateLeft => {
                if let Some(now) = now.filter(|n| n.state == DirDiffState::Same) {
                    next.entries.insert(item.name.clone(), to_entry(now));
                }
            }
            SyncAction::DeleteRight | SyncAction::DeleteLeft if now.is_none() => {
                next.entries.remove(&item.name);
            }
            _ => {}
        }
    }

    if after.journal.is_some() && proves_full_sync(after, &next) {
        next.journal = after.journal.clone().map(Box::new);
    }

    next
}

/// FER-477b: does `after` prove that `next` is the complete, content-verified state
/// of both trees? Every entry must be Same, the compare recursive, complete, uncancelled
/// and error-free, and every file recorded with the exact facts just verified.
pub fn proves_full_sync(after: &CompareDirsReport, next: &BaselineDto) -> bool {
    if !after.recursive || after.truncated || after.cancelled {
        return false;
    }
    if !after.errors.is_empty() {
        return false;
    }
    for e in &after.entries {
        if e.state != DirDiffState::Same {
            return false;
        }
        if e.is_dir {
            continue;
        }
        let Some(b) = next.entries.get(&e.name) else {
            return false;
        };
        if b.left.size != e.left_size
            || b.left.modified_ms != e.left_modified_ms
            || b.right.size != e.right_size
            || b.right.modified_ms != e.right_modified_ms
        {
            return false;
        }
    }
    true
}

fn to_entry(e: &DirDiffEntry) -> EntryDto {
    EntryDto {
        left: FactsDto {
            size: e.left_size,
            modified_ms: e.left_modified_ms,
        },
        right: FactsDto {
            size: e.right_size,
            modified_ms: e.right_modified_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BaselineFixture {
        name: String,
        previous: Option<BaselineDto>,
        after: CompareDirsReport,
        plan: Vec<SyncPlanItem>,
        outcomes: HashMap<String, ItemOutcome>,
        expected_baseline: BaselineDto,
        proves_full_sync: bool,
    }

    #[test]
    fn test_sync_baseline_fixtures_parity() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures_dir = manifest_dir.join("../contract-fixtures/sync-baseline");
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
            let fix: BaselineFixture = serde_json::from_str(&content)
                .unwrap_or_else(|e| panic!("parse fixture {}: {}", path.display(), e));

            let result =
                reconcile_baseline(fix.previous.as_ref(), &fix.after, &fix.plan, &fix.outcomes);

            assert_eq!(
                result,
                fix.expected_baseline,
                "fixture baseline mismatch: {} ({})",
                path.display(),
                fix.name
            );

            assert_eq!(
                proves_full_sync(&fix.after, &result),
                fix.proves_full_sync,
                "fixture proves_full_sync mismatch: {} ({})",
                path.display(),
                fix.name
            );
        }
    }
}
