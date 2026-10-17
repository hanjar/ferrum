//! Triggers that were merged into somebody else's queue entry (Automation design §9.5).
//!
//! The queue has ONE entry per profile (`UNIQUE(profile_id)`), so a second automation on the same
//! profile merges its trigger into the entry that already waits. When that entry is removed
//! because its OWNER was paused, edited, deleted, put under a hold or sent to review, the merged
//! triggers must not vanish with it: the profile still has changes to sync. This module moves
//! them to the first other automation of the profile that may run right now (enabled, same host
//! class, no attention hold, approval verified).
//!
//! It only creates queue entries. Whether such an entry may run is decided by the gate, as for
//! every other entry.

use rusqlite::Connection;

use crate::automation::dpapi::ApprovalKeys;
use crate::automation::model::{AutomationPolicy, Runtime, Trigger};
use crate::automation::policy::{self, Verdict};
use crate::automation::store::StoreError;
use crate::automation::store::automations::{self, EventKind};
use crate::automation::store::queue::{self, HostClass, NewEntry, QueueEntry, TriggerKind};
use crate::automation::store::runtime;

/// Give the merged triggers of `removed` entries to a healthy sibling. Returns how many entries
/// were created or merged. Call inside the same transaction that removed the entries.
///
/// Only a trigger that came from ANOTHER automation (`merged_from`) has anything to lose; the
/// owner's own repeated triggers die with it. While [`super::unattended_enabled`] is off this
/// does nothing: a moved trigger would start the engine without the user asking.
pub fn requeue_siblings(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    removed: &[QueueEntry],
    now_ms: i64,
) -> Result<usize, StoreError> {
    requeue_siblings_if(super::unattended_enabled(), conn, keys, removed, now_ms)
}

/// [`requeue_siblings`] with the switch given by the caller (the host passes its own).
pub fn requeue_siblings_if(
    enabled: bool,
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    removed: &[QueueEntry],
    now_ms: i64,
) -> Result<usize, StoreError> {
    if !enabled {
        return Ok(0);
    }
    let mut moved = 0;
    for entry in removed.iter().filter(|e| !e.merged_from.is_empty()) {
        let siblings: Vec<_> = automations::list(conn)?
            .into_iter()
            .filter(|a| a.profile_id == entry.profile_id && a.id != entry.automation_id)
            .filter(|a| a.enabled && entry.merged_from.contains(&a.id))
            .collect();
        for sibling in siblings {
            let Ok(policy) = serde_json::from_str::<AutomationPolicy>(&sibling.policy) else {
                continue;
            };
            let class = match policy.runtime {
                Runtime::App => HostClass::App,
                Runtime::Background => HostClass::Background,
            };
            if class != entry.host_class {
                continue;
            }
            let held = runtime::get(conn, &sibling.id)?.is_some_and(|r| r.attention.is_some());
            if held {
                continue;
            }
            // Verified, not merely enabled: `enabled` is never an authorization (design §10.4).
            if !matches!(policy::verify(conn, keys, &sibling.id)?, Verdict::Valid) {
                continue;
            }
            let kind = match policy.trigger {
                Trigger::Schedule(_) => TriggerKind::Schedule,
                Trigger::Watch(_) => TriggerKind::Watch,
            };
            let text = entry
                .merged_reasons
                .first()
                .map(String::as_str)
                .unwrap_or(entry.text.as_str());
            let text = if text.trim().is_empty() {
                "Moved trigger"
            } else {
                text
            };
            queue::enqueue(
                conn,
                &NewEntry {
                    automation_id: &sibling.id,
                    profile_id: &entry.profile_id,
                    host_class: class,
                    kind,
                    text,
                    retry_of: None,
                },
                now_ms,
            )?;
            automations::add_event(
                conn,
                &sibling.id,
                now_ms,
                EventKind::Info,
                "Queued: a trigger that was merged into another automation's run on these folders",
            )?;
            moved += 1;
            // One entry per profile: the first healthy sibling carries it.
            break;
        }
    }
    Ok(moved)
}
