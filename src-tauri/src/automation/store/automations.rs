//! `automations`, the runtime row, queue cleanup and the event log (Automation design §4.3, v2).
//! Every multi-row change takes a `&Transaction`/`&Connection` from the caller so the approval
//! and profile-save flows keep ONE `BEGIN IMMEDIATE` around all of it (design §8.2, SP-6.4).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::StoreError;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// One stored automation; `policy` is the JSON form of `AutomationPolicy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRecord {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub profile_id: String,
    pub policy: String,
    pub enabled: bool,
    pub config_rev: i64,
    pub created_ms: i64,
    pub updated_ms: i64,
}

const COLUMNS: &str =
    "id, name, kind, profile_id, policy, enabled, config_rev, created_ms, updated_ms";

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<AutomationRecord> {
    Ok(AutomationRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        profile_id: row.get(3)?,
        policy: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        config_rev: row.get(6)?,
        created_ms: row.get(7)?,
        updated_ms: row.get(8)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<AutomationRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM automations WHERE id = ?1"),
        [id],
        row_to_record,
    )
    .optional()
    .map_err(unavailable)
}

pub fn list(conn: &Connection) -> Result<Vec<AutomationRecord>, StoreError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM automations ORDER BY name COLLATE NOCASE, id"
        ))
        .map_err(unavailable)?;
    let rows = stmt.query_map([], row_to_record).map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// The fields an approval writes. `config_rev` is the NEW revision.
pub struct ApprovedAutomation<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub profile_id: &'a str,
    pub policy: &'a str,
    pub config_rev: i64,
    pub now_ms: i64,
}

/// Insert a new automation or update an existing one, enabled (design §8.2). `created` keeps the
/// original creation time on update.
pub fn upsert_approved(conn: &Connection, a: &ApprovedAutomation<'_>) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO automations(id, name, kind, profile_id, policy, enabled, config_rev, created_ms, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?7)
         ON CONFLICT(id) DO UPDATE SET
           name = ?2, kind = ?3, profile_id = ?4, policy = ?5, enabled = 1,
           config_rev = ?6, updated_ms = ?7",
        params![
            a.id,
            a.name,
            a.kind,
            a.profile_id,
            a.policy,
            a.config_rev,
            a.now_ms
        ],
    )
    .map(|_| ())
    .map_err(unavailable)
}

pub fn set_enabled(
    conn: &Connection,
    id: &str,
    enabled: bool,
    now_ms: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE automations SET enabled = ?2, updated_ms = ?3 WHERE id = ?1",
        params![id, i64::from(enabled), now_ms],
    )
    .map(|_| ())
    .map_err(unavailable)
}

/// Remove an automation with everything that hangs on it (approval, runtime row and queue
/// entries by cascade; events by hand because they carry no foreign key). Run history rows keep
/// their own snapshot of the name. Returns whether the automation existed.
pub fn delete(conn: &Connection, id: &str) -> Result<bool, StoreError> {
    conn.execute("DELETE FROM events WHERE automation_id = ?1", [id])
        .map_err(unavailable)?;
    conn.execute("DELETE FROM controls WHERE automation_id = ?1", [id])
        .map_err(unavailable)?;
    conn.execute("DELETE FROM automations WHERE id = ?1", [id])
        .map(|n| n > 0)
        .map_err(unavailable)
}

/// SP-9.5: how many automations use a profile (list DTO `usedBy`, delete refusal).
pub fn count_for_profile(conn: &Connection, profile_id: &str) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM automations WHERE profile_id = ?1",
        [profile_id],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Whether the automation has a run in progress (a `runs` row that has not ended).
pub fn has_running_run(conn: &Connection, id: &str) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE automation_id = ?1 AND ended_ms IS NULL)",
        [id],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n != 0)
    .map_err(unavailable)
}

// ── Queue and runtime row ──

/// Remove every queued entry of an automation (SP-6.4, edit, pause, delete).
pub fn clear_queue(conn: &Connection, automation_id: &str) -> Result<usize, StoreError> {
    conn.execute(
        "DELETE FROM queue WHERE automation_id = ?1",
        [automation_id],
    )
    .map_err(unavailable)
}

/// Fresh runtime row after an approval or a resume: the trigger starts from scratch (a watch
/// trigger is `watching`, a schedule `scheduled`), counters and pending flags are cleared and any
/// attention hold is dropped (design §8.2, SP-6.4).
pub fn reset_runtime(
    conn: &Connection,
    automation_id: &str,
    host_class: &str,
    trigger_state: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO automation_runtime(automation_id, host_class, trigger_state, state_since_ms, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(automation_id) DO UPDATE SET
           host_class = ?2, trigger_state = ?3, state_since_ms = ?4,
           quiet_until_ms = NULL, burst_started_ms = NULL, pending_rerun = 0,
           check_reason = NULL, attention = NULL, next_occurrence_ms = NULL, updated_ms = ?4",
        params![automation_id, host_class, trigger_state, now_ms],
    )
    .map(|_| ())
    .map_err(unavailable)
}

/// SP-6.4 part of a runtime row: back to `watching`, no countdown, no pending rerun. Does nothing
/// for an automation without a runtime row (it never ran).
pub fn reset_watch_state(
    conn: &Connection,
    automation_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE automation_runtime SET
           trigger_state = CASE WHEN trigger_state IN ('scheduled','unsupported') THEN trigger_state ELSE 'watching' END,
           state_since_ms = ?2, quiet_until_ms = NULL, burst_started_ms = NULL,
           pending_rerun = 0, check_reason = NULL, updated_ms = ?2
         WHERE automation_id = ?1",
        params![automation_id, now_ms],
    )
    .map(|_| ())
    .map_err(unavailable)
}

// ── Events ── (the log itself lives in `events.rs`; re-exported for the P1.1 callers)

pub use super::events::{
    EVENT_MAX_CHARS, EVENTS_KEPT, EventKind, EventRecord, add_event, event_text, events,
};

/// Bump `meta.change_seq`: hosts poll it to notice that another process changed the store
/// (design §3.5, MH-10).
pub fn bump_change_seq(conn: &Connection) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'change_seq'",
        [],
    )
    .map(|_| ())
    .map_err(unavailable)
}
