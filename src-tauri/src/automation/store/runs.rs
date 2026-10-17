//! Run history (Automation design §4.3, MH-8, RQ-9, OP-2): one `runs` row per run - automation or
//! manual profile run - and its per-operation rows in `run_ops`.
//!
//! Rules kept here, not in a UI:
//!
//! - `runs.id` is `AUTOINCREMENT`: an id is never handed out twice, even after the newest run is
//!   deleted (a deleted "#1042" cannot come back as a different run).
//! - `run_ops` stores the FIRST 5 000 operations of a run; the rest are only counted. The run keeps
//!   its true totals (`ops_total`, `counts`) separately, so truncation never changes what a run
//!   reports (MH-8).
//! - A run that needs attention is never deleted while the automation's hold names it (OP-2), and a
//!   run that is still running is never deleted. This is the rule, not a retention feature.
//! - Partial, Conflict held and Failed set the attention hold in the same statement batch that
//!   ends the run; Interrupted, Cancelled, Skipped and Completed never do (RQ-9.1, RQ-9.2).

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::StoreError;
use super::runtime::{self, Attention, AttentionKind};

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// Operations stored per run (MH-8).
pub const OPS_KEPT: i64 = 5_000;
/// Longest stored error / note text.
pub const TEXT_MAX: usize = 512;
/// Longest stored relative path (display only; the engine holds the real one).
pub const REL_PATH_MAX: usize = 4_096;

fn bound(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

// ── Closed sets ──

/// How a run ended (RQ-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Partial,
    Conflict,
    Failed,
    Cancelled,
    Interrupted,
    Skipped,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Partial => "partial",
            Outcome::Conflict => "conflict",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Interrupted => "interrupted",
            Outcome::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "completed" => Outcome::Completed,
            "partial" => Outcome::Partial,
            "conflict" => Outcome::Conflict,
            "failed" => Outcome::Failed,
            "cancelled" => Outcome::Cancelled,
            "interrupted" => Outcome::Interrupted,
            "skipped" => Outcome::Skipped,
            _ => return None,
        })
    }

    /// RQ-9.1: Partial, Conflict held and Failed hold the automation's new runs. RQ-9.2:
    /// Interrupted and Cancelled (and Skipped, Completed) do not.
    pub fn sets_attention(self) -> bool {
        matches!(self, Outcome::Partial | Outcome::Conflict | Outcome::Failed)
    }

    fn attention_kind(self) -> Option<AttentionKind> {
        match self {
            Outcome::Partial => Some(AttentionKind::Partial),
            Outcome::Conflict => Some(AttentionKind::Conflict),
            Outcome::Failed => Some(AttentionKind::Failed),
            _ => None,
        }
    }
}

/// Where a run is (the `phase` column, design §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Gate,
    Compare,
    Plan,
    Sync,
    Settle,
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Gate => "gate",
            Phase::Compare => "compare",
            Phase::Plan => "plan",
            Phase::Sync => "sync",
            Phase::Settle => "settle",
            Phase::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "gate" => Phase::Gate,
            "compare" => Phase::Compare,
            "plan" => Phase::Plan,
            "sync" => Phase::Sync,
            "settle" => Phase::Settle,
            "done" => Phase::Done,
            _ => return None,
        })
    }
}

/// Which host started the run (the `host_class` column of `runs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunHost {
    App,
    Background,
    Manual,
}

impl RunHost {
    pub fn as_str(self) -> &'static str {
        match self {
            RunHost::App => "app",
            RunHost::Background => "background",
            RunHost::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "app" => Some(RunHost::App),
            "background" => Some(RunHost::Background),
            "manual" => Some(RunHost::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    CopyNew,
    Update,
    CreateFolder,
    CopyFolder,
    Delete,
    DeleteToVersions,
    Conflict,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::CopyNew => "copyNew",
            OpKind::Update => "update",
            OpKind::CreateFolder => "createFolder",
            OpKind::CopyFolder => "copyFolder",
            OpKind::Delete => "delete",
            OpKind::DeleteToVersions => "deleteToVersions",
            OpKind::Conflict => "conflict",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "copyNew" => OpKind::CopyNew,
            "update" => OpKind::Update,
            "createFolder" => OpKind::CreateFolder,
            "copyFolder" => OpKind::CopyFolder,
            "delete" => OpKind::Delete,
            "deleteToVersions" => OpKind::DeleteToVersions,
            "conflict" => OpKind::Conflict,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpSide {
    Left,
    Right,
}

impl OpSide {
    pub fn as_str(self) -> &'static str {
        match self {
            OpSide::Left => "left",
            OpSide::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" => Some(OpSide::Left),
            "right" => Some(OpSide::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpStatus {
    Pending,
    InProgress,
    Done,
    Failed,
    NotStarted,
    Held,
    Changed,
}

impl OpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OpStatus::Pending => "pending",
            OpStatus::InProgress => "inProgress",
            OpStatus::Done => "done",
            OpStatus::Failed => "failed",
            OpStatus::NotStarted => "notStarted",
            OpStatus::Held => "held",
            OpStatus::Changed => "changed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => OpStatus::Pending,
            "inProgress" => OpStatus::InProgress,
            "done" => OpStatus::Done,
            "failed" => OpStatus::Failed,
            "notStarted" => OpStatus::NotStarted,
            "held" => OpStatus::Held,
            "changed" => OpStatus::Changed,
            _ => return None,
        })
    }
}

// ── JSON columns ──

/// The `counts` column. File operations from the engine's comparison, never filesystem events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCounts {
    #[serde(default)]
    pub ok: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub not_started: i64,
    #[serde(default)]
    pub held: i64,
    #[serde(default)]
    pub skipped_changed: i64,
}

impl RunCounts {
    /// Operations the run has dealt with so far ("Running 3/6").
    pub fn processed(&self) -> i64 {
        self.ok
            .saturating_add(self.failed)
            .saturating_add(self.skipped_changed)
    }

    fn valid(&self) -> bool {
        [
            self.ok,
            self.failed,
            self.not_started,
            self.held,
            self.skipped_changed,
        ]
        .iter()
        .all(|n| *n >= 0)
    }
}

/// State of a recheck (RQ-10) as the run records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecheckState {
    Running,
    InSync,
    Remaining,
    Conflict,
}

/// The `recheck` column: `{state, remaining, text, atMs}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecheckRecord {
    pub state: RecheckState,
    pub remaining: i64,
    pub text: String,
    pub at_ms: i64,
}

// ── Rows ──

/// One stored run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: i64,
    pub automation_id: Option<String>,
    pub automation_name: Option<String>,
    pub profile_id: String,
    pub profile_revision: i64,
    pub policy_hash: Option<String>,
    pub host: RunHost,
    pub trigger_kind: String,
    pub trigger_text: String,
    pub merged_count: i64,
    pub retry_of: Option<i64>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub phase: Phase,
    pub outcome: Option<Outcome>,
    pub ops_total: i64,
    pub counts: RunCounts,
    pub error_text: Option<String>,
    pub note: Option<String>,
    pub resolved_by: Option<i64>,
    pub resolved_text: Option<String>,
    pub recheck: Option<RecheckRecord>,
}

const COLUMNS: &str = "id, automation_id, automation_name, profile_id, profile_revision, \
     policy_hash, host_class, trigger_kind, trigger_text, merged_count, retry_of, started_ms, \
     ended_ms, phase, outcome, ops_total, counts, error_text, note, resolved_by, resolved_text, \
     recheck";

fn row_to_run(row: &rusqlite::Row) -> rusqlite::Result<RunRecord> {
    let host: String = row.get(6)?;
    let phase: String = row.get(13)?;
    let outcome: Option<String> = row.get(14)?;
    let counts: String = row.get(16)?;
    let recheck: Option<String> = row.get(21)?;
    Ok(RunRecord {
        id: row.get(0)?,
        automation_id: row.get(1)?,
        automation_name: row.get(2)?,
        profile_id: row.get(3)?,
        profile_revision: row.get(4)?,
        policy_hash: row.get(5)?,
        // Unknown stored text reads as the least privileged reading, never a panic.
        host: RunHost::parse(&host).unwrap_or(RunHost::Manual),
        trigger_kind: row.get(7)?,
        trigger_text: row.get(8)?,
        merged_count: row.get(9)?,
        retry_of: row.get(10)?,
        started_ms: row.get(11)?,
        ended_ms: row.get(12)?,
        phase: Phase::parse(&phase).unwrap_or(Phase::Done),
        outcome: outcome.as_deref().and_then(Outcome::parse),
        ops_total: row.get(15)?,
        counts: serde_json::from_str(&counts).unwrap_or_default(),
        error_text: row.get(17)?,
        note: row.get(18)?,
        resolved_by: row.get(19)?,
        resolved_text: row.get(20)?,
        recheck: recheck.and_then(|j| serde_json::from_str(&j).ok()),
    })
}

/// The fields a new run starts with.
#[derive(Debug, Clone)]
pub struct NewRun<'a> {
    /// `None` for a manual profile run.
    pub automation_id: Option<&'a str>,
    pub automation_name: Option<&'a str>,
    pub profile_id: &'a str,
    pub profile_revision: i64,
    pub policy_hash: Option<&'a str>,
    pub host: RunHost,
    pub trigger_kind: &'a str,
    pub trigger_text: &'a str,
    pub merged_count: i64,
    pub retry_of: Option<i64>,
    pub started_ms: i64,
}

/// Start a run (phase `gate`). Returns its id, which is never used again.
pub fn start(conn: &Connection, run: &NewRun<'_>) -> Result<i64, StoreError> {
    if run.merged_count < 0 || run.profile_revision < 1 || run.trigger_text.is_empty() {
        return Err(StoreError::Invalid);
    }
    conn.execute(
        "INSERT INTO runs(automation_id, automation_name, profile_id, profile_revision, policy_hash,
                          host_class, trigger_kind, trigger_text, merged_count, retry_of,
                          started_ms, phase)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'gate')",
        params![
            run.automation_id,
            run.automation_name.map(|n| bound(n, 120)),
            run.profile_id,
            run.profile_revision,
            run.policy_hash,
            run.host.as_str(),
            bound(run.trigger_kind, 32),
            bound(run.trigger_text, 128),
            run.merged_count,
            run.retry_of,
            run.started_ms
        ],
    )
    .map_err(unavailable)?;
    Ok(conn.last_insert_rowid())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<RunRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM runs WHERE id = ?1"),
        [id],
        row_to_run,
    )
    .optional()
    .map_err(unavailable)
}

fn collect(
    conn: &Connection,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<RunRecord>, StoreError> {
    let mut stmt = conn.prepare(sql).map_err(unavailable)?;
    let rows = stmt.query_map(args, row_to_run).map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// Every run that has not ended, newest first.
pub fn active(conn: &Connection) -> Result<Vec<RunRecord>, StoreError> {
    collect(
        conn,
        &format!("SELECT {COLUMNS} FROM runs WHERE ended_ms IS NULL ORDER BY id DESC"),
        &[],
    )
}

/// The newest `limit` runs of one automation, newest first.
pub fn recent(
    conn: &Connection,
    automation_id: &str,
    limit: i64,
) -> Result<Vec<RunRecord>, StoreError> {
    collect(
        conn,
        &format!("SELECT {COLUMNS} FROM runs WHERE automation_id = ?1 ORDER BY id DESC LIMIT ?2"),
        &[&automation_id, &limit],
    )
}

/// The newest run of each automation that has ended (the list's "last result"), by automation.
pub fn last_finished_by_automation(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, RunRecord>, StoreError> {
    let runs = collect(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM runs WHERE id IN (
               SELECT MAX(id) FROM runs WHERE automation_id IS NOT NULL AND ended_ms IS NOT NULL
               GROUP BY automation_id)"
        ),
        &[],
    )?;
    Ok(runs
        .into_iter()
        .filter_map(|r| r.automation_id.clone().map(|a| (a, r)))
        .collect())
}

/// Which runs the history lists (MH-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFilter {
    All,
    /// Runs an attention hold names right now.
    Attention,
}

/// One page of history, newest first: runs with `id < before`, at most `limit`, optionally of one
/// automation. The cursor is a run id (monotonic, never reused).
pub fn history(
    conn: &Connection,
    filter: HistoryFilter,
    automation_id: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<RunRecord>, StoreError> {
    let limit = limit.clamp(0, 500);
    let before = before.unwrap_or(i64::MAX);
    match filter {
        HistoryFilter::All => collect(
            conn,
            &format!(
                "SELECT {COLUMNS} FROM runs
                 WHERE id < ?1 AND (?2 IS NULL OR automation_id = ?2) ORDER BY id DESC LIMIT ?3"
            ),
            &[&before, &automation_id, &limit],
        ),
        HistoryFilter::Attention => {
            // The held runs are few (at most one per automation): fetch them by id.
            let mut runs = Vec::new();
            for id in runtime::held_run_ids(conn)? {
                if let Some(run) = get(conn, id)?
                    && run.id < before
                    && automation_id.is_none_or(|a| run.automation_id.as_deref() == Some(a))
                {
                    runs.push(run);
                }
            }
            runs.sort_by_key(|r| std::cmp::Reverse(r.id));
            runs.truncate(usize::try_from(limit).unwrap_or(0));
            Ok(runs)
        }
    }
}

// ── Updates of a running run ──

/// Move a run to a phase. Does nothing once the run has ended.
pub fn set_phase(conn: &Connection, id: i64, phase: Phase) -> Result<bool, StoreError> {
    conn.execute(
        "UPDATE runs SET phase = ?2 WHERE id = ?1 AND ended_ms IS NULL",
        params![id, phase.as_str()],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// The planned number of operations and the progress counts (called as the run advances).
pub fn set_progress(
    conn: &Connection,
    id: i64,
    ops_total: i64,
    counts: &RunCounts,
) -> Result<bool, StoreError> {
    if ops_total < 0 || !counts.valid() {
        return Err(StoreError::Invalid);
    }
    let json = serde_json::to_string(counts).map_err(|_| StoreError::Invalid)?;
    conn.execute(
        "UPDATE runs SET ops_total = ?2, counts = ?3 WHERE id = ?1 AND ended_ms IS NULL",
        params![id, ops_total, json],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// What a run ends with.
#[derive(Debug, Clone)]
pub struct Finish<'a> {
    pub ended_ms: i64,
    pub outcome: Outcome,
    pub ops_total: i64,
    pub counts: RunCounts,
    pub error_text: Option<&'a str>,
    pub note: Option<&'a str>,
}

/// The text an attention hold shows for a run that ended as `outcome` ("1 failed · 1 not started",
/// "1 conflict held for review"; the status line appends "· new runs held").
pub fn attention_text(outcome: Outcome, counts: &RunCounts, error_text: Option<&str>) -> String {
    match outcome {
        Outcome::Conflict => match counts.held {
            1 => "1 conflict held for review".to_string(),
            n if n > 1 => format!("{n} conflicts held for review"),
            _ => "Conflict held for review".to_string(),
        },
        Outcome::Partial | Outcome::Failed => {
            if outcome == Outcome::Failed
                && let Some(e) = error_text.map(str::trim).filter(|e| !e.is_empty())
            {
                return bound(e, 120);
            }
            let mut parts = Vec::new();
            if counts.failed > 0 {
                parts.push(format!("{} failed", counts.failed));
            }
            if counts.not_started > 0 {
                parts.push(format!("{} not started", counts.not_started));
            }
            if parts.is_empty() {
                if outcome == Outcome::Failed {
                    "Run failed".to_string()
                } else {
                    "Run partly done".to_string()
                }
            } else {
                parts.join(" \u{b7} ")
            }
        }
        _ => String::new(),
    }
}

/// End a run. Sets the outcome, the final counts and phase `done`, and - for Partial, Conflict
/// held and Failed of an automation run - the attention hold naming this run (RQ-9.1). Interrupted
/// and Cancelled never set one (RQ-9.2). A run that already ended is left alone (`Ok(None)`).
/// Returns the attention that was set, if any.
pub fn finish(
    conn: &Connection,
    id: i64,
    end: &Finish<'_>,
) -> Result<Option<Attention>, StoreError> {
    if end.ops_total < 0 || !end.counts.valid() {
        return Err(StoreError::Invalid);
    }
    let Some(run) = get(conn, id)? else {
        return Err(StoreError::NotFound);
    };
    if run.ended_ms.is_some() {
        return Ok(None);
    }
    let counts = serde_json::to_string(&end.counts).map_err(|_| StoreError::Invalid)?;
    let changed = conn
        .execute(
            "UPDATE runs SET ended_ms = ?2, phase = 'done', outcome = ?3, ops_total = ?4,
               counts = ?5, error_text = ?6, note = ?7
             WHERE id = ?1 AND ended_ms IS NULL",
            params![
                id,
                end.ended_ms,
                end.outcome.as_str(),
                end.ops_total,
                counts,
                end.error_text.map(|t| bound(t, TEXT_MAX)),
                end.note.map(|t| bound(t, TEXT_MAX)),
            ],
        )
        .map_err(unavailable)?;
    if changed == 0 {
        return Ok(None);
    }
    let (Some(kind), Some(automation_id)) = (end.outcome.attention_kind(), &run.automation_id)
    else {
        return Ok(None);
    };
    let attention = Attention {
        kind,
        run_id: id,
        text: attention_text(end.outcome, &end.counts, end.error_text),
    };
    // No runtime row (the automation was deleted meanwhile) means there is nothing to hold.
    if runtime::set_attention(conn, automation_id, Some(&attention), end.ended_ms)? {
        Ok(Some(attention))
    } else {
        Ok(None)
    }
}

/// Record the outcome of a recheck on the run (RQ-10).
pub fn set_recheck(
    conn: &Connection,
    id: i64,
    recheck: Option<&RecheckRecord>,
) -> Result<bool, StoreError> {
    let json = match recheck {
        Some(r) => {
            let bounded = RecheckRecord {
                state: r.state,
                remaining: r.remaining.max(0),
                text: bound(&r.text, TEXT_MAX),
                at_ms: r.at_ms,
            };
            Some(serde_json::to_string(&bounded).map_err(|_| StoreError::Invalid)?)
        }
        None => None,
    };
    conn.execute(
        "UPDATE runs SET recheck = ?2 WHERE id = ?1",
        params![id, json],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// Mark `original` as resolved by run `by` (or by hand, `by = None`: "Resolved in Sync by you")
/// and release the automation's hold when it names `original` (RQ-10.3..10.5).
pub fn resolve(
    conn: &Connection,
    original: i64,
    by: Option<i64>,
    text: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let Some(run) = get(conn, original)? else {
        return Ok(false);
    };
    if let Some(by) = by
        && get(conn, by)?.is_none()
    {
        return Err(StoreError::NotFound);
    }
    conn.execute(
        "UPDATE runs SET resolved_by = ?2, resolved_text = ?3 WHERE id = ?1",
        params![original, by, bound(text, TEXT_MAX)],
    )
    .map_err(unavailable)?;
    if let Some(automation_id) = run.automation_id
        && let Some(rt) = runtime::get(conn, &automation_id)?
        && rt.attention.as_ref().is_some_and(|a| a.run_id == original)
    {
        runtime::set_attention(conn, &automation_id, None, now_ms)?;
    }
    Ok(true)
}

// ── Deleting (OP-2) ──

/// What [`delete`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deleted {
    Deleted,
    Missing,
    /// An attention hold names the run (OP-2).
    HeldByAttention,
    /// The run has not ended.
    Running,
}

/// Delete a run and its operations - unless it is still running or an attention hold names it
/// (OP-2: a run that needs attention is never pruned).
pub fn delete(conn: &Connection, id: i64) -> Result<Deleted, StoreError> {
    let Some(run) = get(conn, id)? else {
        return Ok(Deleted::Missing);
    };
    if run.ended_ms.is_none() {
        return Ok(Deleted::Running);
    }
    if runtime::run_is_held(conn, id)? {
        return Ok(Deleted::HeldByAttention);
    }
    conn.execute("DELETE FROM run_ops WHERE run_id = ?1", [id])
        .map_err(unavailable)?;
    conn.execute("DELETE FROM runs WHERE id = ?1", [id])
        .map_err(unavailable)?;
    Ok(Deleted::Deleted)
}

// ── Operations ──

/// An operation to record.
#[derive(Debug, Clone)]
pub struct NewOp<'a> {
    pub kind: OpKind,
    pub side: OpSide,
    pub rel_path: &'a str,
    pub size: Option<i64>,
    pub status: OpStatus,
}

/// What [`add_ops`] stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddedOps {
    pub stored: usize,
    /// Offered operations beyond the [`OPS_KEPT`] cap: counted by the run, not stored.
    pub dropped: usize,
}

/// Append operations to a run. Only the first [`OPS_KEPT`] per run are stored; the run's totals
/// (`ops_total`, `counts`) are set by the caller through [`set_progress`] / [`finish`] and are
/// never derived from, or changed by, the stored rows.
pub fn add_ops(conn: &Connection, run_id: i64, ops: &[NewOp<'_>]) -> Result<AddedOps, StoreError> {
    if ops.iter().any(|o| o.size.is_some_and(|s| s < 0)) {
        return Err(StoreError::Invalid);
    }
    let next: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq) + 1, 0) FROM run_ops WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let room = usize::try_from((OPS_KEPT - next).max(0)).unwrap_or(0);
    let take = room.min(ops.len());
    // One savepoint around the batch: thousands of rows are one commit, and a failure half way
    // leaves no partial batch behind.
    conn.execute_batch("SAVEPOINT add_ops")
        .map_err(unavailable)?;
    let inserted = (|| -> Result<(), StoreError> {
        let mut stmt = conn
            .prepare(
                "INSERT INTO run_ops(run_id, seq, kind, side, rel_path, size, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .map_err(unavailable)?;
        for (i, op) in ops.iter().take(take).enumerate() {
            stmt.execute(params![
                run_id,
                next + i as i64,
                op.kind.as_str(),
                op.side.as_str(),
                bound(op.rel_path, REL_PATH_MAX),
                op.size,
                op.status.as_str(),
            ])
            .map_err(unavailable)?;
        }
        Ok(())
    })();
    match inserted {
        Ok(()) => conn.execute_batch("RELEASE add_ops").map_err(unavailable)?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO add_ops; RELEASE add_ops");
            return Err(e);
        }
    }
    Ok(AddedOps {
        stored: take,
        dropped: ops.len() - take,
    })
}

/// Set the status (and error) of one stored operation.
pub fn set_op_status(
    conn: &Connection,
    run_id: i64,
    seq: i64,
    status: OpStatus,
    error_code: Option<&str>,
    error_text: Option<&str>,
) -> Result<bool, StoreError> {
    conn.execute(
        "UPDATE run_ops SET status = ?3, error_code = ?4, error_text = ?5
         WHERE run_id = ?1 AND seq = ?2",
        params![
            run_id,
            seq,
            status.as_str(),
            error_code.map(|c| bound(c, 64)),
            error_text.map(|t| bound(t, TEXT_MAX)),
        ],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
}

/// Turn every stored operation in status `from` into `to` (a systemic stop makes the remaining
/// operations Not started). Returns how many changed.
pub fn retarget_ops(
    conn: &Connection,
    run_id: i64,
    from: OpStatus,
    to: OpStatus,
) -> Result<usize, StoreError> {
    conn.execute(
        "UPDATE run_ops SET status = ?3 WHERE run_id = ?1 AND status = ?2",
        params![run_id, from.as_str(), to.as_str()],
    )
    .map_err(unavailable)
}

/// One stored operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRecord {
    pub seq: i64,
    pub kind: OpKind,
    pub side: OpSide,
    pub rel_path: String,
    pub size: Option<i64>,
    pub status: OpStatus,
    pub error_code: Option<String>,
    pub error_text: Option<String>,
}

/// A page of operations in order (`seq`), `offset` operations skipped.
pub fn ops_page(
    conn: &Connection,
    run_id: i64,
    offset: i64,
    limit: i64,
) -> Result<Vec<OpRecord>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT seq, kind, side, rel_path, size, status, error_code, error_text
             FROM run_ops WHERE run_id = ?1 AND seq >= ?2 ORDER BY seq LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = stmt
        .query_map(params![run_id, offset, limit], |row| {
            let kind: String = row.get(1)?;
            let side: String = row.get(2)?;
            let status: String = row.get(5)?;
            Ok(OpRecord {
                seq: row.get(0)?,
                kind: OpKind::parse(&kind).unwrap_or(OpKind::Conflict),
                side: OpSide::parse(&side).unwrap_or(OpSide::Right),
                rel_path: row.get(3)?,
                size: row.get(4)?,
                // An unknown stored status is shown as Held: it never reads as done.
                status: OpStatus::parse(&status).unwrap_or(OpStatus::Held),
                error_code: row.get(6)?,
                error_text: row.get(7)?,
            })
        })
        .map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// How many operations are stored for a run (at most [`OPS_KEPT`]).
pub fn stored_ops(conn: &Connection, run_id: i64) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM run_ops WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )
    .map_err(unavailable)
}
