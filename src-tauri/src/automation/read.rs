//! The read side of the automation page (Automation design §7.2, P1.2): `automation_list`,
//! `automation_get`, `automation_history`, `automation_run_ops`, `automation_events`. Tauri-free
//! cores; `commands.rs` only wraps them.
//!
//! Every read gathers its facts in ONE `with_read` call (one consistent snapshot) and then hands
//! them to the pure [`status::compute`]. Nothing here writes, and nothing here authorizes: the
//! approval verdict comes from `policy::verify` (never from the `enabled` flag), and the flags
//! `canRunNow` / `showReview` are UI hints - the gate checks the approval again on every run.
//!
//! Rules for what crosses IPC: no lease id, no MAC, no key material (the approval row is only
//! read for its profile revision); every request type refuses unknown fields; ids, cursors, offsets
//! and page sizes are validated before the store is touched; no path through here panics.

use std::collections::HashMap;
use std::sync::OnceLock;

use rusqlite::Connection;
use serde::Deserialize;

use crate::automation::commands::CommandError;
use crate::automation::dpapi::{ApprovalKey, ApprovalKeys, KeyError, KeyLoad};
use crate::automation::dto::{
    AutomationDetailDto, AutomationRowDto, EventDto, EventsPageDto, HistoryDto, LastRunDto,
    NeedsReviewDto, PipelineDto, ProfileBriefDto, QueueEntryDto, QuietDto, RunOpDto, RunOpsDto,
    RunRowDto,
};
use crate::automation::model::{
    self, AutomationPolicy, DeletePolicy, Freq, ProfilePolicy, Trigger,
};
use crate::automation::policy::{self, NeedsReview, Verdict};
use crate::automation::status::{
    self, ActiveRun, AutoKind, QueueWait, StatusDto, StatusInput, StatusKind,
};
use crate::automation::store::automations::{self, AutomationRecord};
use crate::automation::store::controls::{self, Action, Control};
use crate::automation::store::events::{self, EventRecord};
use crate::automation::store::hosts;
use crate::automation::store::profiles::{self, ProfileRecord};
use crate::automation::store::queue::{self, QueueEntry};
use crate::automation::store::runs::{self, HistoryFilter, OPS_KEPT, Outcome, RunRecord};
use crate::automation::store::runtime::{self, RuntimeRow, TriggerState};
use crate::automation::store::{Store, approvals};

/// Recent runs on the detail page (MH-5).
pub const RECENT_RUNS: i64 = 4;
/// Events on the detail page (MH-5).
pub const DETAIL_EVENTS: i64 = 14;
/// Largest history page (design §7.2).
pub const HISTORY_MAX: i64 = 100;
/// Largest operations page (design §7.2).
pub const OPS_PAGE_MAX: i64 = 200;
/// Largest events page (design §7.2).
pub const EVENTS_PAGE_MAX: i64 = 50;

// ── Requests (flat IPC arguments are assembled into these; unknown fields are refused) ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryFilterWire {
    All,
    Attention,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryRequest {
    pub filter: HistoryFilterWire,
    #[serde(default)]
    pub automation_id: Option<String>,
    /// Cursor: a run id from a previous page's `next`.
    #[serde(default)]
    pub before: Option<i64>,
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunOpsRequest {
    pub run_id: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsRequest {
    pub id: String,
    /// Cursor: an event id from a previous page's `next`.
    #[serde(default)]
    pub before: Option<i64>,
    pub limit: i64,
}

fn invalid(what: &str) -> CommandError {
    CommandError::new("invalid_input", format!("The {what} is not valid"))
}

fn require_id(id: &str, what: &str) -> Result<(), CommandError> {
    if model::is_profile_id(id) {
        Ok(())
    } else {
        Err(invalid(what))
    }
}

fn require_cursor(before: Option<i64>) -> Result<(), CommandError> {
    match before {
        Some(b) if b < 1 => Err(invalid("cursor")),
        _ => Ok(()),
    }
}

fn require_limit(limit: i64, max: i64) -> Result<(), CommandError> {
    if (1..=max).contains(&limit) {
        Ok(())
    } else {
        Err(invalid("page size"))
    }
}

fn not_found() -> CommandError {
    CommandError::new("not_found", "The automation does not exist")
}

// ── Approval key, read once per snapshot ──

/// The approval key loaded at most once for a whole read: a list of N automations verifies N
/// approvals but decrypts the key file once.
struct SnapshotKeys<'a> {
    inner: &'a dyn ApprovalKeys,
    loaded: OnceLock<KeyLoad>,
}

impl ApprovalKeys for SnapshotKeys<'_> {
    fn load(&self) -> KeyLoad {
        self.loaded.get_or_init(|| self.inner.load()).clone()
    }
    fn load_or_create(&self) -> Result<ApprovalKey, KeyError> {
        // A read never creates or replaces a key.
        Err(KeyError)
    }
    fn create_if_missing(&self) -> Result<ApprovalKey, KeyError> {
        Err(KeyError)
    }
}

// ── The snapshot ──

struct World {
    automations: Vec<AutomationRecord>,
    profiles: HashMap<String, ProfileRecord>,
    /// Profile id → canonical root paths (left, right, versioning root).
    roots: HashMap<String, Vec<String>>,
    approved_revisions: HashMap<String, i64>,
    runtime: HashMap<String, RuntimeRow>,
    queue: Vec<QueueEntry>,
    active: Vec<RunRecord>,
    last: HashMap<String, RunRecord>,
    held: Vec<i64>,
    controls: Vec<Control>,
    reviews: HashMap<String, Option<NeedsReview>>,
    runner_live: bool,
    names: HashMap<String, String>,
}

fn roots_of(policy: &ProfilePolicy) -> Vec<String> {
    let mut roots = vec![policy.left.path.clone(), policy.right.path.clone()];
    if let DeletePolicy::Versioning { root } = &policy.delete_policy {
        roots.push(root.clone());
    }
    roots
}

fn load_world(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    now_ms: i64,
    only: Option<&str>,
) -> Result<World, CommandError> {
    let all = automations::list(conn)?;
    let names = all.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
    let automations: Vec<AutomationRecord> = match only {
        Some(id) => all.into_iter().filter(|a| a.id == id).collect(),
        None => all,
    };
    let queue = queue::all(conn)?;
    let active = runs::active(conn)?;
    // Profiles of the listed automations, of the queued entries and of the active runs (their
    // roots decide who waits for whom).
    let mut profile_ids: Vec<&str> = automations.iter().map(|a| a.profile_id.as_str()).collect();
    profile_ids.extend(queue.iter().map(|q| q.profile_id.as_str()));
    profile_ids.extend(active.iter().map(|r| r.profile_id.as_str()));
    profile_ids.sort_unstable();
    profile_ids.dedup();
    let mut profile_map = HashMap::new();
    let mut roots = HashMap::new();
    for id in profile_ids {
        if let Some(rec) = profiles::get(conn, id)? {
            if let Ok(policy) = serde_json::from_str::<ProfilePolicy>(&rec.policy) {
                roots.insert(rec.id.clone(), roots_of(&policy));
            }
            profile_map.insert(rec.id.clone(), rec);
        }
    }
    // The approval key is read once; `verify` never creates it. The verdict comes from the stored
    // approval, never from `enabled`.
    let snapshot_keys = SnapshotKeys {
        inner: keys,
        loaded: OnceLock::new(),
    };
    let mut reviews = HashMap::new();
    let mut approved_revisions = HashMap::new();
    for a in &automations {
        let review = match policy::verify(conn, &snapshot_keys, &a.id) {
            Ok(Verdict::Valid) => None,
            Ok(Verdict::NeedsReview(r)) => Some(r),
            // A store failure while verifying never reads as "valid".
            Err(_) => Some(NeedsReview::PolicyChanged),
        };
        reviews.insert(a.id.clone(), review);
        if let Some(ap) = approvals::get(conn, &a.id)? {
            approved_revisions.insert(a.id.clone(), ap.profile_revision);
        }
    }
    let runner_live = hosts::all(conn)?
        .iter()
        .any(|h| h.kind == "runner" && now_ms - h.last_beat_ms <= hosts::LIVE_WINDOW_MS);
    Ok(World {
        automations,
        profiles: profile_map,
        roots,
        approved_revisions,
        runtime: runtime::all(conn)?,
        queue,
        active,
        last: runs::last_finished_by_automation(conn)?,
        held: runtime::held_run_ids(conn)?,
        controls: controls::pending(conn, now_ms)?,
        reviews,
        runner_live,
        names,
    })
}

impl World {
    fn roots(&self, profile_id: &str) -> &[String] {
        self.roots.get(profile_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The name of an active run whose folders overlap `profile_id`'s (RQ-2), skipping the runs
    /// of `except_automation`.
    fn blocker(&self, profile_id: &str, except_automation: Option<&str>) -> Option<String> {
        let mine = self.roots(profile_id);
        if mine.is_empty() {
            return None;
        }
        self.active
            .iter()
            .filter(|r| {
                except_automation.is_none() || r.automation_id.as_deref() != except_automation
            })
            .find(|r| queue::roots_overlap(mine, self.roots(&r.profile_id)))
            .map(|r| {
                r.automation_name
                    .clone()
                    .or_else(|| self.profiles.get(&r.profile_id).map(|p| p.name.clone()))
                    .unwrap_or_else(|| "another run".to_string())
            })
    }

    fn stopping(&self, automation_id: &str, run_id: Option<i64>) -> bool {
        self.controls.iter().any(|c| {
            matches!(c.action, Action::Stop | Action::PauseStop)
                && (c.automation_id.as_deref() == Some(automation_id)
                    || (run_id.is_some() && c.run_id == run_id))
        })
    }
}

struct Built {
    row: AutomationRowDto,
    input: StatusInput,
    policy: Option<AutomationPolicy>,
    runtime: Option<RuntimeRow>,
}

fn trigger_summary(trigger: &Trigger) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    match trigger {
        Trigger::Schedule(s) => match &s.freq {
            Freq::Once { date } => format!("Once on {date} at {} ({})", s.time, s.tz),
            Freq::Daily {} => format!("Daily at {} ({})", s.time, s.tz),
            Freq::Weekly { days } => {
                let names: Vec<&str> = days
                    .iter()
                    .filter_map(|d| DAYS.get(usize::from(*d)).copied())
                    .collect();
                format!("Weekly on {} at {} ({})", names.join(", "), s.time, s.tz)
            }
        },
        Trigger::Watch(w) => {
            let sides = match (w.watch_left, w.watch_right) {
                (true, true) => "both folders",
                (true, false) => "the left folder",
                (false, true) => "the right folder",
                (false, false) => "no folder",
            };
            let mut text = format!("Watches {sides}");
            if w.on_changes {
                text.push_str(&format!(
                    " \u{b7} runs after {} s without changes",
                    w.idle_secs
                ));
            }
            if w.on_available {
                text.push_str(" \u{b7} runs when a folder comes back");
            }
            text
        }
    }
}

fn run_row(run: &RunRecord, held: &[i64]) -> RunRowDto {
    let result = match (run.ended_ms, run.outcome) {
        (None, _) => status::running_chip(run.phase, &run.counts, run.ops_total),
        (Some(_), Some(outcome)) => status::outcome_chip(
            outcome,
            &run.counts,
            run.ops_total,
            run.error_text.as_deref(),
        ),
        // A finished run without a readable outcome never reads as a success.
        (Some(_), None) => StatusDto {
            kind: StatusKind::Attention,
            label: "Unknown result".to_string(),
            sub: None,
        },
    };
    RunRowDto {
        id: run.id,
        automation_id: run.automation_id.clone(),
        automation_name: run.automation_name.clone(),
        profile_id: run.profile_id.clone(),
        profile_revision: run.profile_revision,
        host: run.host.as_str().to_string(),
        trigger_kind: run.trigger_kind.clone(),
        trigger_text: run.trigger_text.clone(),
        merged_count: run.merged_count,
        retry_of: run.retry_of,
        started_ms: run.started_ms,
        ended_ms: run.ended_ms,
        phase: run.phase.as_str().to_string(),
        outcome: run.outcome.map(|o| o.as_str().to_string()),
        ops_total: run.ops_total,
        counts: run.counts,
        result,
        error_text: run.error_text.clone(),
        note: run.note.clone(),
        resolved_by: run.resolved_by,
        resolved_text: run.resolved_text.clone(),
        recheck: run.recheck.clone(),
        needs_attention: held.contains(&run.id),
    }
}

fn event_dto(e: EventRecord) -> EventDto {
    EventDto {
        id: e.id,
        at_ms: e.at_ms,
        kind: e.kind,
        text: e.text,
        run_id: e.run_id,
    }
}

fn build(world: &World, rec: &AutomationRecord, now_ms: i64) -> Built {
    let policy = serde_json::from_str::<AutomationPolicy>(&rec.policy).ok();
    let kind = if rec.kind == "schedule" {
        AutoKind::Schedule
    } else {
        AutoKind::Watch
    };
    let rt = world.runtime.get(&rec.id).cloned();
    let profile = world.profiles.get(&rec.profile_id);
    // An automation whose stored policy cannot be read is held for review, whatever verify said.
    let review = if policy.is_none() {
        Some(NeedsReview::PolicyChanged)
    } else {
        world.reviews.get(&rec.id).copied().flatten()
    };

    let background = policy
        .as_ref()
        .map(|p| p.runtime == model::Runtime::Background)
        .unwrap_or(false);
    let idle_secs = match policy.as_ref().map(|p| &p.trigger) {
        Some(Trigger::Watch(w)) => w.idle_secs,
        _ => 0,
    };
    let trigger = rt
        .as_ref()
        .and_then(|r| r.trigger_state)
        .unwrap_or(match kind {
            AutoKind::Schedule => TriggerState::Scheduled,
            AutoKind::Watch => TriggerState::Watching,
        });
    // An unknown stored state re-checks rather than claiming everything is quiet.
    let trigger = if rt.as_ref().is_some_and(|r| r.trigger_state.is_none()) {
        TriggerState::Checking
    } else {
        trigger
    };

    let active_run = world
        .active
        .iter()
        .find(|r| r.automation_id.as_deref() == Some(rec.id.as_str()));
    let active = active_run.map(|r| ActiveRun {
        phase: r.phase,
        done: r.counts.processed(),
        total: r.ops_total,
        trigger_text: r.trigger_text.clone(),
        stopping: world.stopping(&rec.id, Some(r.id)),
    });

    let queued = world
        .queue
        .iter()
        .find(|q| q.profile_id == rec.profile_id)
        .map(|_| {
            if trigger == TriggerState::Device {
                QueueWait::Device
            } else if let Some(name) = world.blocker(&rec.profile_id, Some(&rec.id)) {
                QueueWait::WaitsFor(name)
            } else {
                QueueWait::Starting
            }
        });

    let overlap_run_active = world.blocker(&rec.profile_id, Some(&rec.id)).is_some();
    let quiet_secs = rt.as_ref().and_then(|r| r.quiet_until_ms).map(|until| {
        let left = (until - now_ms).max(0);
        u64::try_from((left + 999) / 1000).unwrap_or(0)
    });
    let attention = rt.as_ref().and_then(|r| r.attention.clone());
    let input = StatusInput {
        review,
        kind,
        background,
        runner_live: world.runner_live,
        enabled: rec.enabled,
        active,
        queued,
        attention: attention.clone(),
        trigger,
        pending_rerun: rt.as_ref().is_some_and(|r| r.pending_rerun),
        overlap_run_active,
        quiet_secs,
        idle_secs,
        next_label: match kind {
            AutoKind::Schedule => rt
                .as_ref()
                .and_then(|r| r.next_occurrence_ms)
                .map(status::format_utc_short),
            AutoKind::Watch => None,
        },
    };
    let status = status::compute(&input);

    let (can_run_now, run_now_reason) = match (&input.review, &attention, rec.enabled) {
        (Some(r), _, _) => (false, Some(status::review_label(r))),
        (None, Some(a), _) => (false, Some(format!("{} \u{b7} new runs held", a.text))),
        (None, None, false) => (false, Some("Paused \u{2014} resume it first".to_string())),
        (None, None, true) => (true, None),
    };

    let last_run = world.last.get(&rec.id).and_then(|r| {
        Some(LastRunDto {
            run_id: r.id,
            started_ms: r.started_ms,
            ended_ms: r.ended_ms?,
            result: status::outcome_chip(
                r.outcome.unwrap_or(Outcome::Failed),
                &r.counts,
                r.ops_total,
                r.error_text.as_deref(),
            ),
        })
    });

    let row = AutomationRowDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        kind: rec.kind.clone(),
        profile_id: rec.profile_id.clone(),
        profile_name: profile.map(|p| p.name.clone()).unwrap_or_default(),
        profile_revision: profile.map(|p| p.revision).unwrap_or(0),
        approved_revision: world.approved_revisions.get(&rec.id).copied(),
        enabled: rec.enabled,
        config_rev: rec.config_rev,
        status,
        trigger: policy
            .as_ref()
            .map(|p| trigger_summary(&p.trigger))
            .unwrap_or_default(),
        runtime: if background { "background" } else { "app" }.to_string(),
        runtime_note: if background {
            "Background runner"
        } else {
            "Only while open"
        }
        .to_string(),
        last_run,
        can_run_now,
        run_now_reason,
        show_review: input.review.is_some(),
        review: input.review.map(NeedsReviewDto::from),
        attention,
    };
    Built {
        row,
        input,
        policy,
        runtime: rt,
    }
}

/// `automation_list`: one row per automation, by name.
pub fn list_in(
    store: &Store,
    keys: &dyn ApprovalKeys,
    now_ms: i64,
) -> Result<Vec<AutomationRowDto>, CommandError> {
    store.with_read(|conn| -> Result<Vec<AutomationRowDto>, CommandError> {
        let world = load_world(conn, keys, now_ms, None)?;
        Ok(world
            .automations
            .iter()
            .map(|rec| build(&world, rec, now_ms).row)
            .collect())
    })
}

fn pipeline(built: &Built) -> PipelineDto {
    let i = &built.input;
    let held = matches!(
        built.row.status.kind,
        StatusKind::Review | StatusKind::Warn | StatusKind::Attention | StatusKind::Paused
    );
    match i.kind {
        AutoKind::Watch => {
            let current = if held {
                None
            } else if i.active.is_some() {
                Some(4)
            } else if i.queued.is_some() {
                Some(3)
            } else if built.row.status.kind == StatusKind::Quiet {
                Some(2)
            } else if built.row.status.kind == StatusKind::Pending {
                Some(1)
            } else {
                Some(0)
            };
            PipelineDto {
                steps: [
                    "Watching",
                    "Changes detected",
                    "Waiting for quiet",
                    "Queued",
                    "Running",
                ]
                .map(String::from)
                .to_vec(),
                current,
            }
        }
        AutoKind::Schedule => {
            let current = if held {
                None
            } else if i.active.is_some() {
                Some(2)
            } else if i.queued.is_some() {
                Some(1)
            } else {
                Some(0)
            };
            PipelineDto {
                steps: ["Scheduled", "Queued", "Running", "Finished"]
                    .map(String::from)
                    .to_vec(),
                current,
            }
        }
    }
}

/// `automation_get`: the detail page of one automation.
pub fn get_in(
    store: &Store,
    keys: &dyn ApprovalKeys,
    now_ms: i64,
    id: &str,
) -> Result<AutomationDetailDto, CommandError> {
    require_id(id, "automation id")?;
    store.with_read(|conn| -> Result<AutomationDetailDto, CommandError> {
        let world = load_world(conn, keys, now_ms, Some(id))?;
        let rec = world.automations.first().ok_or_else(not_found)?;
        let built = build(&world, rec, now_ms);
        let policy = built.policy.clone().ok_or_else(|| {
            CommandError::new(
                "needs_review",
                "The stored automation is invalid and needs review",
            )
        })?;
        let profile = world.profiles.get(&rec.profile_id);
        let profile_policy =
            profile.and_then(|p| serde_json::from_str::<ProfilePolicy>(&p.policy).ok());
        let profile_dto = ProfileBriefDto {
            id: rec.profile_id.clone(),
            name: profile.map(|p| p.name.clone()).unwrap_or_default(),
            revision: profile.map(|p| p.revision).unwrap_or(0),
            left: profile_policy
                .as_ref()
                .map(|p| p.left.path.clone())
                .unwrap_or_default(),
            right: profile_policy
                .as_ref()
                .map(|p| p.right.path.clone())
                .unwrap_or_default(),
        };
        let current_run = world
            .active
            .iter()
            .find(|r| r.automation_id.as_deref() == Some(id))
            .map(|r| run_row(r, &world.held));
        let recent_runs = runs::recent(conn, id, RECENT_RUNS)?
            .iter()
            .map(|r| run_row(r, &world.held))
            .collect();
        let waiting = world
            .queue
            .iter()
            .filter(|q| q.profile_id == rec.profile_id)
            .map(|q| QueueEntryDto {
                id: q.id,
                automation_id: q.automation_id.clone(),
                automation_name: world.names.get(&q.automation_id).cloned(),
                trigger_kind: q.kind.as_str().to_string(),
                trigger_text: q.text.clone(),
                merged_count: q.merged_count,
                merged_reasons: q.merged_reasons.clone(),
                enqueued_ms: q.enqueued_ms,
                retry_of: q.retry_of,
                host_class: q.host_class.as_str().to_string(),
                waits_for: match built.input.queued {
                    Some(QueueWait::WaitsFor(ref name)) => Some(name.clone()),
                    _ => None,
                },
            })
            .collect();
        let events = events_page(conn, id, None, DETAIL_EVENTS)?;
        let quiet = built
            .runtime
            .as_ref()
            .and_then(|r| r.quiet_until_ms)
            .filter(|_| built.input.trigger == TriggerState::Quiet)
            .map(|until| QuietDto {
                until_ms: until,
                remaining_secs: i64::try_from(built.input.quiet_secs.unwrap_or(0))
                    .unwrap_or(i64::MAX),
                idle_secs: built.input.idle_secs,
            });
        Ok(AutomationDetailDto {
            pipeline: pipeline(&built),
            pending_rerun: built.input.pending_rerun,
            automation: built.row,
            policy,
            profile: profile_dto,
            quiet,
            current_run,
            waiting,
            recent_runs,
            events,
        })
    })
}

fn events_page(
    conn: &Connection,
    id: &str,
    before: Option<i64>,
    limit: i64,
) -> Result<EventsPageDto, CommandError> {
    let mut list = events::page(conn, id, before, limit + 1)?;
    let more = list.len() as i64 > limit;
    list.truncate(usize::try_from(limit).unwrap_or(0));
    let next = if more {
        list.last().map(|e| e.id)
    } else {
        None
    };
    Ok(EventsPageDto {
        events: list.into_iter().map(event_dto).collect(),
        next,
    })
}

/// `automation_history`: runs newest first (MH-6).
pub fn history_in(store: &Store, req: &HistoryRequest) -> Result<HistoryDto, CommandError> {
    if let Some(id) = &req.automation_id {
        require_id(id, "automation id")?;
    }
    require_cursor(req.before)?;
    require_limit(req.limit, HISTORY_MAX)?;
    let filter = match req.filter {
        HistoryFilterWire::All => HistoryFilter::All,
        HistoryFilterWire::Attention => HistoryFilter::Attention,
    };
    store.with_read(|conn| -> Result<HistoryDto, CommandError> {
        let mut list = runs::history(
            conn,
            filter,
            req.automation_id.as_deref(),
            req.before,
            req.limit + 1,
        )?;
        let held = runtime::held_run_ids(conn)?;
        let more = list.len() as i64 > req.limit;
        list.truncate(usize::try_from(req.limit).unwrap_or(0));
        let next = if more {
            list.last().map(|r| r.id)
        } else {
            None
        };
        Ok(HistoryDto {
            runs: list.iter().map(|r| run_row(r, &held)).collect(),
            next,
        })
    })
}

/// `automation_run_ops`: the stored operations of a run, paged (RQ-5, MH-8).
pub fn run_ops_in(store: &Store, req: &RunOpsRequest) -> Result<RunOpsDto, CommandError> {
    if req.run_id < 1 {
        return Err(invalid("run id"));
    }
    if !(0..=OPS_KEPT).contains(&req.offset) {
        return Err(invalid("offset"));
    }
    require_limit(req.limit, OPS_PAGE_MAX)?;
    store.with_read(|conn| -> Result<RunOpsDto, CommandError> {
        let run = runs::get(conn, req.run_id)?
            .ok_or_else(|| CommandError::new("not_found", "The run does not exist"))?;
        let ops = runs::ops_page(conn, req.run_id, req.offset, req.limit)?
            .into_iter()
            .map(|o| RunOpDto {
                seq: o.seq,
                kind: o.kind.as_str().to_string(),
                side: o.side.as_str().to_string(),
                rel_path: o.rel_path,
                size: o.size,
                status: o.status.as_str().to_string(),
                error_code: o.error_code,
                error_text: o.error_text,
            })
            .collect();
        Ok(RunOpsDto {
            ops,
            // The true total never shrinks below what is stored.
            total: run.ops_total.max(runs::stored_ops(conn, req.run_id)?),
            stored: runs::stored_ops(conn, req.run_id)?,
        })
    })
}

/// `automation_events`: the event log of one automation, newest first (MH-7).
pub fn events_in(store: &Store, req: &EventsRequest) -> Result<EventsPageDto, CommandError> {
    require_id(&req.id, "automation id")?;
    require_cursor(req.before)?;
    require_limit(req.limit, EVENTS_PAGE_MAX)?;
    store.with_read(|conn| -> Result<EventsPageDto, CommandError> {
        if automations::get(conn, &req.id)?.is_none() {
            return Err(not_found());
        }
        events_page(conn, &req.id, req.before, req.limit)
    })
}

#[cfg(test)]
mod tests;
