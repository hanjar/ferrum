//! The `controls` table (Automation design §3.5): requests from the app to the host that runs an
//! automation (there is no command channel, only the store). A control is a request, never an
//! authorization - the host runs the same gate as for any trigger, and `runNow` on a held
//! automation is answered `held`.
//!
//! A control older than 10 minutes is ignored and swept: a request that sat unhandled while no
//! host was alive must not fire when one starts hours later. A creation time in the far future
//! (more than [`SKEW_MS`] ahead of now) is ignored too, so a forged stamp cannot keep a request
//! alive for ever.

use rusqlite::{Connection, OptionalExtension, params};

use super::StoreError;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// A control older than this is ignored and swept (design §3.5).
pub const TTL_MS: i64 = 10 * 60 * 1000;
/// How far a creation time may lie ahead of the reader's clock.
pub const SKEW_MS: i64 = 60 * 1000;
/// Longest handled-result text.
pub const RESULT_MAX: usize = 64;

/// The closed set of control actions (the table's CHECK constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    RunNow,
    Stop,
    PauseFinish,
    PauseStop,
    Recheck,
    Retry,
    Reload,
    RestartRunner,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::RunNow => "runNow",
            Action::Stop => "stop",
            Action::PauseFinish => "pauseFinish",
            Action::PauseStop => "pauseStop",
            Action::Recheck => "recheck",
            Action::Retry => "retry",
            Action::Reload => "reload",
            Action::RestartRunner => "restartRunner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "runNow" => Action::RunNow,
            "stop" => Action::Stop,
            "pauseFinish" => Action::PauseFinish,
            "pauseStop" => Action::PauseStop,
            "recheck" => Action::Recheck,
            "retry" => Action::Retry,
            "reload" => Action::Reload,
            "restartRunner" => Action::RestartRunner,
            _ => return None,
        })
    }

    /// Which target the action needs: (automation id, run id).
    fn needs(self) -> (bool, bool) {
        match self {
            Action::RunNow | Action::PauseFinish | Action::PauseStop => (true, false),
            Action::Stop | Action::Recheck | Action::Retry => (false, true),
            Action::Reload | Action::RestartRunner => (false, false),
        }
    }
}

/// A request to post.
#[derive(Debug, Clone)]
pub struct NewControl<'a> {
    pub action: Action,
    pub automation_id: Option<&'a str>,
    pub run_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub id: i64,
    pub automation_id: Option<String>,
    pub run_id: Option<i64>,
    pub action: Action,
    pub created_ms: i64,
    pub handled_ms: Option<i64>,
    pub result: Option<String>,
}

/// Post a control. The target must match the action (`runNow` names an automation, `stop` a run,
/// `reload` neither); a mismatch is [`StoreError::Invalid`].
pub fn post(conn: &Connection, control: &NewControl<'_>, now_ms: i64) -> Result<i64, StoreError> {
    let (needs_automation, needs_run) = control.action.needs();
    if control.automation_id.is_some() != needs_automation
        || control.run_id.is_some() != needs_run
        || control.run_id.is_some_and(|id| id <= 0)
    {
        return Err(StoreError::Invalid);
    }
    conn.execute(
        "INSERT INTO controls(automation_id, run_id, action, created_ms) VALUES (?1, ?2, ?3, ?4)",
        params![
            control.automation_id,
            control.run_id,
            control.action.as_str(),
            now_ms
        ],
    )
    .map_err(unavailable)?;
    Ok(conn.last_insert_rowid())
}

/// Whether a control created at `created_ms` still counts at `now_ms`.
pub fn is_live(created_ms: i64, now_ms: i64) -> bool {
    let age = now_ms.saturating_sub(created_ms);
    (-SKEW_MS..=TTL_MS).contains(&age)
}

fn row_to_control(row: &rusqlite::Row) -> rusqlite::Result<Option<Control>> {
    let action: String = row.get(3)?;
    // The CHECK constraint keeps unknown actions out; a row that still has one is skipped.
    let Some(action) = Action::parse(&action) else {
        return Ok(None);
    };
    Ok(Some(Control {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        run_id: row.get(2)?,
        action,
        created_ms: row.get(4)?,
        handled_ms: row.get(5)?,
        result: row.get(6)?,
    }))
}

const COLUMNS: &str = "id, automation_id, run_id, action, created_ms, handled_ms, result";

/// The controls a host still has to answer: unhandled and not older than [`TTL_MS`], oldest
/// first. Expired ones are never returned, whether or not a sweep ran.
pub fn pending(conn: &Connection, now_ms: i64) -> Result<Vec<Control>, StoreError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM controls WHERE handled_ms IS NULL
               AND created_ms >= ?1 AND created_ms <= ?2 ORDER BY id"
        ))
        .map_err(unavailable)?;
    let rows = stmt
        .query_map(
            params![
                now_ms.saturating_sub(TTL_MS),
                now_ms.saturating_add(SKEW_MS)
            ],
            row_to_control,
        )
        .map_err(unavailable)?;
    let mut out = Vec::new();
    for row in rows {
        if let Some(c) = row.map_err(unavailable)? {
            out.push(c);
        }
    }
    Ok(out)
}

/// The live, unhandled controls that name an automation or one of its runs (the status line
/// "Stopping after the current file" reads a pending `stop` this way).
pub fn pending_for(
    conn: &Connection,
    automation_id: &str,
    run_ids: &[i64],
    now_ms: i64,
) -> Result<Vec<Control>, StoreError> {
    Ok(pending(conn, now_ms)?
        .into_iter()
        .filter(|c| {
            c.automation_id.as_deref() == Some(automation_id)
                || c.run_id.is_some_and(|r| run_ids.contains(&r))
        })
        .collect())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Control>, StoreError> {
    let row = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM controls WHERE id = ?1"),
            [id],
            row_to_control,
        )
        .optional()
        .map_err(unavailable)?;
    Ok(row.flatten())
}

/// Record that a host answered a control. Only an unhandled, still-live control is marked; the
/// answer is bounded text (`held`, `queued`, ...). Returns whether it was marked.
pub fn mark_handled(
    conn: &Connection,
    id: i64,
    result: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let result: String = result
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(RESULT_MAX)
        .collect();
    conn.execute(
        "UPDATE controls SET handled_ms = ?2, result = ?3
         WHERE id = ?1 AND handled_ms IS NULL AND created_ms >= ?4 AND created_ms <= ?5",
        params![
            id,
            now_ms,
            result,
            now_ms.saturating_sub(TTL_MS),
            now_ms.saturating_add(SKEW_MS)
        ],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// Delete every control that is out of its window (older than [`TTL_MS`] or stamped in the far
/// future), handled or not. Returns how many went.
pub fn sweep(conn: &Connection, now_ms: i64) -> Result<usize, StoreError> {
    conn.execute(
        "DELETE FROM controls WHERE created_ms < ?1 OR created_ms > ?2",
        params![
            now_ms.saturating_sub(TTL_MS),
            now_ms.saturating_add(SKEW_MS)
        ],
    )
    .map_err(unavailable)
}

/// How many controls (handled or not) for this target were created at or after `since_ms`. The
/// request functions use it for the per-target rate limit (threat T5).
pub fn recent_count(
    conn: &Connection,
    action: Action,
    automation_id: Option<&str>,
    run_id: Option<i64>,
    since_ms: i64,
) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM controls
         WHERE created_ms >= ?1 AND action = ?4
           AND ((?2 IS NOT NULL AND automation_id = ?2) OR (?3 IS NOT NULL AND run_id = ?3))",
        params![since_ms, automation_id, run_id, action.as_str()],
        |row| row.get(0),
    )
    .map_err(unavailable)
}
