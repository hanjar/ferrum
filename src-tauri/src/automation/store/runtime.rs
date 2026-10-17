//! The `automation_runtime` row (Automation design §4.3, §9): where an automation's trigger is
//! right now, whether a rerun is pending, and the attention hold (RQ-9.1). One row per
//! automation, created by an approval or a resume (`automations::reset_runtime`).
//!
//! Reads never panic and never loosen a hold: an attention column that does not parse is still an
//! attention (kind `failed`), so a damaged row cannot release the new-runs-held rule.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::StoreError;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// Longest attention text kept (display only).
pub const ATTENTION_TEXT_MAX: usize = 200;

/// Where an automation's trigger is (the `trigger_state` column). Pending rerun is the separate
/// flag `pending_rerun`, shown only while an overlapping run is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerState {
    Watching,
    Quiet,
    Checking,
    Device,
    Recheck,
    Unsupported,
    Scheduled,
}

impl TriggerState {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerState::Watching => "watching",
            TriggerState::Quiet => "quiet",
            TriggerState::Checking => "checking",
            TriggerState::Device => "device",
            TriggerState::Recheck => "recheck",
            TriggerState::Unsupported => "unsupported",
            TriggerState::Scheduled => "scheduled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "watching" => TriggerState::Watching,
            "quiet" => TriggerState::Quiet,
            "checking" => TriggerState::Checking,
            "device" => TriggerState::Device,
            "recheck" => TriggerState::Recheck,
            "unsupported" => TriggerState::Unsupported,
            "scheduled" => TriggerState::Scheduled,
            _ => return None,
        })
    }
}

/// Why an automation is held (RQ-9.1): the outcome that set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionKind {
    Partial,
    Conflict,
    Failed,
}

/// The `attention` column: `{kind, runId, text}`. Cleared by a resolved recheck or retry (P1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attention {
    pub kind: AttentionKind,
    pub run_id: i64,
    pub text: String,
}

/// One runtime row as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRow {
    pub automation_id: String,
    /// `app` or `background`.
    pub host_class: String,
    /// `None` when the stored text is not a known state (the reader falls back, never panics).
    pub trigger_state: Option<TriggerState>,
    pub state_since_ms: i64,
    pub quiet_until_ms: Option<i64>,
    pub burst_started_ms: Option<i64>,
    pub pending_rerun: bool,
    pub check_reason: Option<String>,
    /// A damaged column reads as an attention of kind `failed` with run id 0 (still held).
    pub attention: Option<Attention>,
    pub schedule_cursor_ms: Option<i64>,
    pub last_fired_occurrence_ms: Option<i64>,
    pub next_occurrence_ms: Option<i64>,
    pub updated_ms: i64,
}

const COLUMNS: &str = "automation_id, host_class, trigger_state, state_since_ms, quiet_until_ms, \
     burst_started_ms, pending_rerun, check_reason, attention, schedule_cursor_ms, \
     last_fired_occurrence_ms, next_occurrence_ms, updated_ms";

/// The attention a damaged column reads as: still a hold.
fn damaged_attention() -> Attention {
    Attention {
        kind: AttentionKind::Failed,
        run_id: 0,
        text: "Needs attention".to_string(),
    }
}

fn parse_attention(text: Option<String>) -> Option<Attention> {
    let text = text?;
    Some(serde_json::from_str(&text).unwrap_or_else(|_| damaged_attention()))
}

fn row_to_runtime(row: &rusqlite::Row) -> rusqlite::Result<RuntimeRow> {
    let state: String = row.get(2)?;
    Ok(RuntimeRow {
        automation_id: row.get(0)?,
        host_class: row.get(1)?,
        trigger_state: TriggerState::parse(&state),
        state_since_ms: row.get(3)?,
        quiet_until_ms: row.get(4)?,
        burst_started_ms: row.get(5)?,
        pending_rerun: row.get::<_, i64>(6)? != 0,
        check_reason: row.get(7)?,
        attention: parse_attention(row.get(8)?),
        schedule_cursor_ms: row.get(9)?,
        last_fired_occurrence_ms: row.get(10)?,
        next_occurrence_ms: row.get(11)?,
        updated_ms: row.get(12)?,
    })
}

pub fn get(conn: &Connection, automation_id: &str) -> Result<Option<RuntimeRow>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM automation_runtime WHERE automation_id = ?1"),
        [automation_id],
        row_to_runtime,
    )
    .optional()
    .map_err(unavailable)
}

/// Every runtime row, keyed by automation id (the list view reads them in one query).
pub fn all(conn: &Connection) -> Result<HashMap<String, RuntimeRow>, StoreError> {
    let mut stmt = conn
        .prepare(&format!("SELECT {COLUMNS} FROM automation_runtime"))
        .map_err(unavailable)?;
    let rows = stmt.query_map([], row_to_runtime).map_err(unavailable)?;
    let mut map = HashMap::new();
    for row in rows {
        let row = row.map_err(unavailable)?;
        map.insert(row.automation_id.clone(), row);
    }
    Ok(map)
}

/// Move the trigger to `state`. The countdown fields belong to the state: `quiet_until_ms` and
/// `burst_started_ms` are only kept for `quiet`, and `check_reason` only for `checking`.
pub fn set_trigger_state(
    conn: &Connection,
    automation_id: &str,
    state: TriggerState,
    quiet_until_ms: Option<i64>,
    burst_started_ms: Option<i64>,
    check_reason: Option<&str>,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let quiet = state == TriggerState::Quiet;
    let checking = state == TriggerState::Checking;
    conn.execute(
        "UPDATE automation_runtime SET trigger_state = ?2, state_since_ms = ?3,
           quiet_until_ms = ?4, burst_started_ms = ?5, check_reason = ?6, updated_ms = ?3
         WHERE automation_id = ?1",
        params![
            automation_id,
            state.as_str(),
            now_ms,
            quiet_until_ms.filter(|_| quiet),
            burst_started_ms.filter(|_| quiet),
            check_reason
                .filter(|_| checking)
                .map(super::events::event_text),
        ],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

pub fn set_pending_rerun(
    conn: &Connection,
    automation_id: &str,
    pending: bool,
    now_ms: i64,
) -> Result<bool, StoreError> {
    conn.execute(
        "UPDATE automation_runtime SET pending_rerun = ?2, updated_ms = ?3 WHERE automation_id = ?1",
        params![automation_id, i64::from(pending), now_ms],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// Set or clear the attention hold (RQ-9.1). The text is bounded.
pub fn set_attention(
    conn: &Connection,
    automation_id: &str,
    attention: Option<&Attention>,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let json = match attention {
        Some(a) => {
            let bounded = Attention {
                kind: a.kind,
                run_id: a.run_id,
                text: a.text.chars().take(ATTENTION_TEXT_MAX).collect(),
            };
            Some(serde_json::to_string(&bounded).map_err(|_| StoreError::Invalid)?)
        }
        None => None,
    };
    conn.execute(
        "UPDATE automation_runtime SET attention = ?2, updated_ms = ?3 WHERE automation_id = ?1",
        params![automation_id, json, now_ms],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// The schedule bookkeeping of SC-3 (P1.4 fills it): cursor, last fired and next occurrence.
pub fn set_schedule(
    conn: &Connection,
    automation_id: &str,
    cursor_ms: Option<i64>,
    last_fired_ms: Option<i64>,
    next_ms: Option<i64>,
    now_ms: i64,
) -> Result<bool, StoreError> {
    conn.execute(
        "UPDATE automation_runtime SET schedule_cursor_ms = ?2, last_fired_occurrence_ms = ?3,
           next_occurrence_ms = ?4, updated_ms = ?5 WHERE automation_id = ?1",
        params![automation_id, cursor_ms, last_fired_ms, next_ms, now_ms],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// Whether any runtime row's attention names `run_id`, or is damaged for the automation that owns
/// the run (a damaged hold protects every run of its automation, OP-2).
pub fn run_is_held(conn: &Connection, run_id: i64) -> Result<bool, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT attention, automation_id FROM automation_runtime WHERE attention IS NOT NULL",
        )
        .map_err(unavailable)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    for row in rows {
        let (json, automation_id) = row.map_err(unavailable)?;
        match serde_json::from_str::<Attention>(&json) {
            Ok(a) if a.run_id == run_id => return Ok(true),
            Ok(_) => {}
            Err(_) => {
                let owner: Option<String> = conn
                    .query_row(
                        "SELECT automation_id FROM runs WHERE id = ?1",
                        [run_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(unavailable)?
                    .flatten();
                if owner.as_deref() == Some(automation_id.as_str()) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Run ids currently named by an attention hold (history filter `attention`).
pub fn held_run_ids(conn: &Connection) -> Result<Vec<i64>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT attention FROM automation_runtime WHERE attention IS NOT NULL")
        .map_err(unavailable)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(unavailable)?;
    let mut ids = Vec::new();
    for json in rows {
        let json = json.map_err(unavailable)?;
        if let Ok(a) = serde_json::from_str::<Attention>(&json)
            && a.run_id > 0
        {
            ids.push(a.run_id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}
