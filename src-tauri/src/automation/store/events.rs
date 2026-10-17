//! The per-automation event log (Automation design §4.3, MH-7): time-stamped lines of kind
//! `ok | warn | bad | mute | info`, at most 512 characters each, the newest 200 per automation
//! kept (pruned on insert). Ordering is by insertion (`id`), which is what the pages cursor on:
//! a wall clock that steps backwards cannot reorder the log or hide an entry from a page.

use rusqlite::{Connection, params};
use serde::Serialize;

use super::StoreError;

/// Events kept per automation (design §4.3), newest first.
pub const EVENTS_KEPT: i64 = 200;
/// Longest event text (design §4.3).
pub const EVENT_MAX_CHARS: usize = 512;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// Event kinds (CHECK constraint of the table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Ok,
    Warn,
    Bad,
    Mute,
    Info,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Ok => "ok",
            EventKind::Warn => "warn",
            EventKind::Bad => "bad",
            EventKind::Mute => "mute",
            EventKind::Info => "info",
        }
    }
}

/// One stored event, newest first when listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRecord {
    pub id: i64,
    pub automation_id: String,
    pub at_ms: i64,
    pub kind: String,
    pub text: String,
    pub run_id: Option<i64>,
}

/// Bound an event text: control characters become spaces, at most [`EVENT_MAX_CHARS`] characters
/// with an ellipsis at the end.
pub fn event_text(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= EVENT_MAX_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(EVENT_MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Log one event and keep the newest [`EVENTS_KEPT`] per automation (design §4.3).
pub fn add_event(
    conn: &Connection,
    automation_id: &str,
    at_ms: i64,
    kind: EventKind,
    text: &str,
) -> Result<(), StoreError> {
    add_event_for_run(conn, automation_id, at_ms, kind, text, None)
}

/// [`add_event`] for an event that belongs to a run (the history links to it).
pub fn add_event_for_run(
    conn: &Connection,
    automation_id: &str,
    at_ms: i64,
    kind: EventKind,
    text: &str,
    run_id: Option<i64>,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO events(automation_id, at_ms, kind, text, run_id) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            automation_id,
            at_ms,
            kind.as_str(),
            event_text(text),
            run_id
        ],
    )
    .map_err(unavailable)?;
    conn.execute(
        "DELETE FROM events WHERE automation_id = ?1 AND id NOT IN
           (SELECT id FROM events WHERE automation_id = ?1 ORDER BY id DESC LIMIT ?2)",
        params![automation_id, EVENTS_KEPT],
    )
    .map(|_| ())
    .map_err(unavailable)
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<EventRecord> {
    Ok(EventRecord {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        at_ms: row.get(2)?,
        kind: row.get(3)?,
        text: row.get(4)?,
        run_id: row.get(5)?,
    })
}

/// The newest `limit` events, newest first.
pub fn events(
    conn: &Connection,
    automation_id: &str,
    limit: i64,
) -> Result<Vec<EventRecord>, StoreError> {
    page(conn, automation_id, None, limit)
}

/// Events older than the cursor `before` (an event id), newest first. A cursor is an id the
/// caller got from an earlier page; an id that no longer exists simply continues below it.
pub fn page(
    conn: &Connection,
    automation_id: &str,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<EventRecord>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, automation_id, at_ms, kind, text, run_id FROM events
             WHERE automation_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = stmt
        .query_map(
            params![automation_id, before.unwrap_or(i64::MAX), limit],
            row_to_record,
        )
        .map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// How many events an automation currently has (tests, diagnostics).
pub fn count(conn: &Connection, automation_id: &str) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE automation_id = ?1",
        [automation_id],
        |row| row.get(0),
    )
    .map_err(unavailable)
}
