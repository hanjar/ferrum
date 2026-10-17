//! The run queue (Automation design §4.3, RQ-3): at most ONE queued entry per sync profile
//! (`UNIQUE(profile_id)`). A second trigger for a profile that already waits is merged into its
//! entry - the merge count and the distinct reasons are kept, no duplicate job exists. Entries
//! start in FIFO order within one host class; an entry that is blocked (overlapping roots in use,
//! root offline) never holds up a later one that can start.
//!
//! The queue is stored, so it survives a restart of its host (RQ-3.4). Entries are removed by a
//! pause, an edit, a profile change or a delete (`automations::clear_queue`, the FK cascade) - and
//! by the host once it starts them ([`remove`]).
//!
//! Nothing here decides whether an entry MAY run: the gate (design §10.4) verifies the approval on
//! every run path. A queue row, like the `enabled` flag, is never an authorization.

use rusqlite::{Connection, OptionalExtension, params};

use super::StoreError;
use crate::sync_engine::roots;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// Distinct merge reasons kept per entry.
pub const MERGED_REASONS_KEPT: usize = 10;
/// Longest trigger text or merge reason (characters).
pub const TEXT_MAX: usize = 128;

/// The host a queue entry belongs to (the `host_class` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostClass {
    App,
    Background,
}

impl HostClass {
    pub fn as_str(self) -> &'static str {
        match self {
            HostClass::App => "app",
            HostClass::Background => "background",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "app" => Some(HostClass::App),
            "background" => Some(HostClass::Background),
            _ => None,
        }
    }
}

/// What queued the entry (the `trigger_kind` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Schedule,
    Watch,
    RunNow,
    Retry,
    CatchUp,
    PendingRerun,
    Available,
    Recheck,
    Startup,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Schedule => "schedule",
            TriggerKind::Watch => "watch",
            TriggerKind::RunNow => "runNow",
            TriggerKind::Retry => "retry",
            TriggerKind::CatchUp => "catchUp",
            TriggerKind::PendingRerun => "pendingRerun",
            TriggerKind::Available => "available",
            TriggerKind::Recheck => "recheck",
            TriggerKind::Startup => "startup",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "schedule" => TriggerKind::Schedule,
            "watch" => TriggerKind::Watch,
            "runNow" => TriggerKind::RunNow,
            "retry" => TriggerKind::Retry,
            "catchUp" => TriggerKind::CatchUp,
            "pendingRerun" => TriggerKind::PendingRerun,
            "available" => TriggerKind::Available,
            "recheck" => TriggerKind::Recheck,
            "startup" => TriggerKind::Startup,
            _ => return None,
        })
    }
}

/// A trigger asking for a run.
#[derive(Debug, Clone)]
pub struct NewEntry<'a> {
    pub automation_id: &'a str,
    pub profile_id: &'a str,
    pub host_class: HostClass,
    pub kind: TriggerKind,
    /// How the trigger is shown ("Scheduled · 02:00 WIB"); also the merge reason.
    pub text: &'a str,
    /// The run a `Retry` continues (`runs.id`).
    pub retry_of: Option<i64>,
}

/// One stored entry. Ids of leases never appear here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub id: i64,
    pub automation_id: String,
    pub profile_id: String,
    pub host_class: HostClass,
    pub kind: TriggerKind,
    pub text: String,
    pub retry_of: Option<i64>,
    /// How many later triggers were merged into this entry.
    pub merged_count: i64,
    /// Distinct reasons of the merged triggers, oldest first.
    pub merged_reasons: Vec<String>,
    pub enqueued_ms: i64,
}

/// What an [`enqueue`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enqueued {
    /// A new entry.
    Queued { id: i64 },
    /// Merged into the entry that already waits for this profile (RQ-3.1).
    Merged { id: i64, merged_count: i64 },
    /// A retry cannot merge into another automation's entry: that run would not continue the
    /// retried run. The caller reports it as held/busy.
    Refused,
}

fn bound(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.chars().take(TEXT_MAX).collect()
}

const COLUMNS: &str = "id, automation_id, profile_id, host_class, trigger_kind, trigger_text, \
     retry_of, merged_count, merged_reasons, enqueued_ms";

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<QueueEntry> {
    let host: String = row.get(3)?;
    let kind: String = row.get(4)?;
    let reasons: String = row.get(8)?;
    Ok(QueueEntry {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        profile_id: row.get(2)?,
        // An unknown stored value falls back to the safest reading: an app entry that was
        // started by hand ("Run now") - never a panic on a read.
        host_class: HostClass::parse(&host).unwrap_or(HostClass::App),
        kind: TriggerKind::parse(&kind).unwrap_or(TriggerKind::RunNow),
        text: row.get(5)?,
        retry_of: row.get(6)?,
        merged_count: row.get(7)?,
        merged_reasons: serde_json::from_str(&reasons).unwrap_or_default(),
        enqueued_ms: row.get(9)?,
    })
}

/// Queue a run for `entry.profile_id`, or merge into the entry that already waits for it.
///
/// A merge keeps the earlier entry's place (FIFO), adds one to `merged_count` and the trigger text
/// to `merged_reasons` (distinct, at most [`MERGED_REASONS_KEPT`]). A `Retry` merging into a
/// non-retry entry of the SAME automation upgrades that entry to the retry (the resolving run must
/// not be lost); into another automation's entry it is [`Enqueued::Refused`].
pub fn enqueue(
    conn: &Connection,
    entry: &NewEntry<'_>,
    now_ms: i64,
) -> Result<Enqueued, StoreError> {
    let text = bound(entry.text);
    if text.is_empty() {
        return Err(StoreError::Invalid);
    }
    if entry.kind == TriggerKind::Retry && entry.retry_of.is_none() {
        return Err(StoreError::Invalid);
    }
    let existing = by_profile(conn, entry.profile_id)?;
    let Some(existing) = existing else {
        conn.execute(
            "INSERT INTO queue(automation_id, profile_id, host_class, trigger_kind, trigger_text,
                               retry_of, merged_count, merged_reasons, enqueued_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, '[]', ?7)",
            params![
                entry.automation_id,
                entry.profile_id,
                entry.host_class.as_str(),
                entry.kind.as_str(),
                text,
                entry.retry_of,
                now_ms
            ],
        )
        .map_err(unavailable)?;
        return Ok(Enqueued::Queued {
            id: conn.last_insert_rowid(),
        });
    };
    let incoming_retry = entry.kind == TriggerKind::Retry;
    let upgrade = incoming_retry && existing.kind != TriggerKind::Retry;
    if incoming_retry && existing.automation_id != entry.automation_id {
        return Ok(Enqueued::Refused);
    }
    let mut reasons = existing.merged_reasons.clone();
    if text != existing.text && !reasons.contains(&text) && reasons.len() < MERGED_REASONS_KEPT {
        reasons.push(text.clone());
    }
    let merged = existing.merged_count.saturating_add(1);
    let reasons_json = serde_json::to_string(&reasons).map_err(|_| StoreError::Invalid)?;
    if upgrade {
        conn.execute(
            "UPDATE queue SET merged_count = ?2, merged_reasons = ?3, trigger_kind = 'retry',
               trigger_text = ?4, retry_of = ?5 WHERE id = ?1",
            params![existing.id, merged, reasons_json, text, entry.retry_of],
        )
    } else {
        conn.execute(
            "UPDATE queue SET merged_count = ?2, merged_reasons = ?3 WHERE id = ?1",
            params![existing.id, merged, reasons_json],
        )
    }
    .map_err(unavailable)?;
    Ok(Enqueued::Merged {
        id: existing.id,
        merged_count: merged,
    })
}

pub fn by_profile(conn: &Connection, profile_id: &str) -> Result<Option<QueueEntry>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM queue WHERE profile_id = ?1"),
        [profile_id],
        row_to_entry,
    )
    .optional()
    .map_err(unavailable)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<QueueEntry>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM queue WHERE id = ?1"),
        [id],
        row_to_entry,
    )
    .optional()
    .map_err(unavailable)
}

fn collect(
    conn: &Connection,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<QueueEntry>, StoreError> {
    let mut stmt = conn.prepare(sql).map_err(unavailable)?;
    let rows = stmt.query_map(args, row_to_entry).map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// Every entry, oldest first (FIFO).
pub fn all(conn: &Connection) -> Result<Vec<QueueEntry>, StoreError> {
    collect(
        conn,
        &format!("SELECT {COLUMNS} FROM queue ORDER BY enqueued_ms, id"),
        &[],
    )
}

/// The entries of one host class in start order (FIFO, RQ-3.3).
pub fn for_host(conn: &Connection, host: HostClass) -> Result<Vec<QueueEntry>, StoreError> {
    collect(
        conn,
        &format!("SELECT {COLUMNS} FROM queue WHERE host_class = ?1 ORDER BY enqueued_ms, id"),
        &[&host.as_str()],
    )
}

/// The entries of one automation (the detail's "waiting entries").
pub fn for_automation(
    conn: &Connection,
    automation_id: &str,
) -> Result<Vec<QueueEntry>, StoreError> {
    collect(
        conn,
        &format!("SELECT {COLUMNS} FROM queue WHERE automation_id = ?1 ORDER BY enqueued_ms, id"),
        &[&automation_id],
    )
}

/// Remove an entry (the host started it, or it was cancelled). Returns whether it existed.
pub fn remove(conn: &Connection, id: i64) -> Result<bool, StoreError> {
    conn.execute("DELETE FROM queue WHERE id = ?1", [id])
        .map(|n| n > 0)
        .map_err(unavailable)
}

/// The first entry of `fifo` (already in FIFO order) that is not blocked. A blocked entry waits
/// without holding up later ones (RQ-3.3: "when they can").
pub fn first_startable(
    fifo: &[QueueEntry],
    blocked: impl Fn(&QueueEntry) -> bool,
) -> Option<&QueueEntry> {
    fifo.iter().find(|e| !blocked(e))
}

/// True when any root of `a` equals, contains or lies inside any root of `b` (RQ-2, component-wise,
/// case-insensitive; the paths are the canonical ones stored with the profile).
pub fn roots_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| roots::overlap(x, y)))
}
