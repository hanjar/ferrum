//! FER-471 adaptive worker control for the copy engine.
//!
//! - `CopyControl`: the live, shared knobs of ONE copy job (thread target, auto
//!   mode, priority, worker count). The progress dialog, the Settings default and
//!   the AI `copy.adjust` capability all go through `CopyControl::adjust`.
//! - `Budget`: one global worker budget shared by every job, so several jobs can
//!   never each run 16 workers.
//! - `Aimd`: the additive-increase / multiplicative-decrease policy used in
//!   auto mode (pure, unit-tested with synthetic throughput curves).
//! - A registry keyed by operation id so IPC can find the control of a running job.
//!
//! Nothing here touches files.

use crate::copy_native::Media;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// Hard ceiling of workers per job (manual mode) and of the global budget.
pub const MAX_THREADS: usize = 16;
/// Files below this size go to the parallel small-file pool.
pub const SMALL_FILE_LIMIT: u64 = 4 * 1024 * 1024;
/// Files at or above this size are copied unbuffered (no cache pollution).
pub const UNBUFFERED_LIMIT: u64 = 1024 * 1024 * 1024;
/// AIMD decision window.
pub const AIMD_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    Normal,
    Background,
}

impl Priority {
    pub fn parse(text: &str) -> Option<Priority> {
        match text {
            "normal" => Some(Priority::Normal),
            "background" => Some(Priority::Background),
            _ => None,
        }
    }
}

/// How many workers a job starts with (from Settings or a request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSetting {
    Auto,
    Fixed(usize),
}

impl ThreadSetting {
    /// Settings value: 0 = auto, 1..=16 fixed; anything else is clamped.
    pub fn from_pref(value: u32) -> ThreadSetting {
        if value == 0 {
            ThreadSetting::Auto
        } else {
            ThreadSetting::Fixed((value as usize).clamp(1, MAX_THREADS))
        }
    }
}

/// A change requested on a running job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Adjust {
    /// +`by` workers (manual mode).
    Up {
        #[serde(default = "one")]
        by: usize,
    },
    /// -`by` workers (manual mode); workers stop at a file boundary.
    Down {
        #[serde(default = "one")]
        by: usize,
    },
    Set {
        threads: usize,
    },
    /// Switch auto mode on or off (off keeps the current worker count).
    Auto {
        enabled: bool,
    },
    Priority {
        priority: Priority,
    },
}

fn one() -> usize {
    1
}

/// Snapshot sent to the UI (`operations://copy-control`) and returned by IPC.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyControlState {
    pub operation_id: String,
    pub auto: bool,
    /// Target worker count (the number the user sees between [-] and [+]).
    pub threads: usize,
    /// Workers actually alive right now (a [-] takes effect at the next file boundary).
    pub active_workers: usize,
    /// Auto-mode ceiling for this media.
    pub auto_max: usize,
    pub media: Media,
    pub priority: Priority,
    pub running: bool,
}

fn media_code(media: Media) -> u8 {
    match media {
        Media::Ssd => 0,
        Media::Hdd => 1,
        Media::Network => 2,
        Media::Unknown => 3,
    }
}

fn media_from(code: u8) -> Media {
    match code {
        0 => Media::Ssd,
        1 => Media::Hdd,
        2 => Media::Network,
        _ => Media::Unknown,
    }
}

/// (start, auto ceiling) for a media class, and whether source and destination share a volume.
pub fn media_profile(media: Media, same_volume: bool) -> (usize, usize) {
    match media {
        Media::Hdd => (1, 2),
        Media::Network => (4, 8),
        Media::Ssd if same_volume => (4, 8),
        Media::Ssd => (4, MAX_THREADS),
        Media::Unknown => (2, 8),
    }
}

/// Live knobs of one job. Cheap atomics; the engine polls them at file boundaries.
pub struct CopyControl {
    id: Option<String>,
    auto: AtomicBool,
    target: AtomicUsize,
    background: AtomicBool,
    active: AtomicUsize,
    auto_max: AtomicUsize,
    media: AtomicU8,
    running: AtomicBool,
    /// Controller wake-up (adjust → react within milliseconds, not at the next tick).
    wake: (Mutex<bool>, Condvar),
}

impl CopyControl {
    pub fn new(id: Option<String>, setting: ThreadSetting, priority: Priority) -> Arc<Self> {
        let (auto, target) = match setting {
            ThreadSetting::Auto => (true, 4),
            ThreadSetting::Fixed(n) => (false, n.clamp(1, MAX_THREADS)),
        };
        Arc::new(Self {
            id,
            auto: AtomicBool::new(auto),
            target: AtomicUsize::new(target),
            background: AtomicBool::new(priority == Priority::Background),
            active: AtomicUsize::new(0),
            auto_max: AtomicUsize::new(MAX_THREADS),
            media: AtomicU8::new(media_code(Media::Unknown)),
            running: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
        })
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn is_auto(&self) -> bool {
        self.auto.load(Ordering::SeqCst)
    }
    pub fn target(&self) -> usize {
        self.target.load(Ordering::SeqCst)
    }
    pub fn is_background(&self) -> bool {
        self.background.load(Ordering::SeqCst)
    }
    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }
    pub fn auto_max(&self) -> usize {
        self.auto_max.load(Ordering::SeqCst)
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Engine side: record media-derived limits at the start of a run.
    pub(crate) fn begin_run(&self, media: Media, start: usize, ceiling: usize) {
        self.media.store(media_code(media), Ordering::SeqCst);
        self.auto_max.store(ceiling, Ordering::SeqCst);
        if self.is_auto() {
            self.target.store(start.clamp(1, ceiling), Ordering::SeqCst);
        }
        self.running.store(true, Ordering::SeqCst);
    }

    pub(crate) fn end_run(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.active.store(0, Ordering::SeqCst);
    }

    /// Engine side: AIMD decided a new target (auto mode only; a racing manual
    /// change wins, so the compare is on the auto flag).
    pub(crate) fn set_auto_target(&self, n: usize) {
        if self.is_auto() {
            self.target
                .store(n.clamp(1, self.auto_max()), Ordering::SeqCst);
        }
    }

    /// Worker accounting. `try_claim` is used by the controller before spawning.
    pub(crate) fn worker_started(&self) {
        self.active.fetch_add(1, Ordering::SeqCst);
    }
    pub(crate) fn worker_stopped(&self) {
        let _ = self
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |a| {
                Some(a.saturating_sub(1))
            });
    }
    /// A worker at a file boundary asks whether it must retire (target lowered).
    /// Lock-free and race-safe: only one of several candidates wins each slot.
    pub(crate) fn should_retire(&self) -> bool {
        let want = self.target();
        self.active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |a| {
                if a > want && a > 1 { Some(a - 1) } else { None }
            })
            .is_ok()
    }

    pub fn state(&self) -> CopyControlState {
        CopyControlState {
            operation_id: self.id.clone().unwrap_or_default(),
            auto: self.is_auto(),
            threads: self.target(),
            active_workers: self.active(),
            auto_max: self.auto_max(),
            media: media_from(self.media.load(Ordering::SeqCst)),
            priority: if self.is_background() {
                Priority::Background
            } else {
                Priority::Normal
            },
            running: self.is_running(),
        }
    }

    /// Apply a change. Any explicit thread-count change turns auto mode off.
    pub fn adjust(&self, change: Adjust) -> CopyControlState {
        match change {
            Adjust::Up { by } => self.manual_target(|t| t.saturating_add(by.clamp(1, MAX_THREADS))),
            Adjust::Down { by } => {
                self.manual_target(|t| t.saturating_sub(by.clamp(1, MAX_THREADS)))
            }
            Adjust::Set { threads } => self.manual_target(|_| threads),
            Adjust::Auto { enabled } => self.auto.store(enabled, Ordering::SeqCst),
            Adjust::Priority { priority } => self
                .background
                .store(priority == Priority::Background, Ordering::SeqCst),
        }
        self.notify();
        let state = self.state();
        emit_state(&state);
        state
    }

    fn manual_target(&self, f: impl Fn(usize) -> usize) {
        self.auto.store(false, Ordering::SeqCst);
        let _ = self
            .target
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |t| {
                Some(f(t).clamp(1, MAX_THREADS))
            });
    }

    fn notify(&self) {
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_all();
    }

    /// Controller sleep that ends early on `adjust`.
    pub(crate) fn wait(&self, timeout: Duration) {
        let guard = self.wake.0.lock().unwrap();
        let (mut guard, _) = self
            .wake
            .1
            .wait_timeout_while(guard, timeout, |woken| !*woken)
            .unwrap();
        *guard = false;
    }
}

// ───────────────────────── global worker budget ─────────────────────────

/// Shared worker budget. A job's FIRST worker is always granted so a job can
/// never starve; additional workers are granted only while the budget lasts.
pub struct Budget {
    limit: usize,
    used: AtomicUsize,
}

impl Budget {
    pub const fn new(limit: usize) -> Self {
        Self {
            limit,
            used: AtomicUsize::new(0),
        }
    }

    /// `first` bypasses the limit (still counted).
    pub fn try_acquire(&self, first: bool) -> bool {
        self.used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |u| {
                if first || u < self.limit {
                    Some(u + 1)
                } else {
                    None
                }
            })
            .is_ok()
    }

    pub fn release(&self) {
        let _ = self
            .used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |u| {
                Some(u.saturating_sub(1))
            });
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }
}

static GLOBAL_BUDGET: Budget = Budget::new(MAX_THREADS);

pub fn global_budget() -> &'static Budget {
    &GLOBAL_BUDGET
}

// ───────────────────────── AIMD ─────────────────────────

/// Throughput over one window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub bytes_per_sec: f64,
    pub files_per_sec: f64,
}

/// What the policy knows besides the sample.
#[derive(Debug, Clone, Copy)]
pub struct AimdInput {
    pub sample: Sample,
    pub target: usize,
    pub ceiling: usize,
    /// There is queued work that more workers could pick up.
    pub backlog: bool,
}

/// Additive increase while throughput keeps improving by more than 10%;
/// back off by one when it stagnates after an increase, and by a quarter when
/// it collapses. After a back-off it holds for a few windows (no flapping).
#[derive(Debug, Default)]
pub struct Aimd {
    prev: Option<Sample>,
    /// Target before the last increase (to return to when it did not help).
    increased_from: Option<usize>,
    hold: u8,
}

const IMPROVE: f64 = 1.10;
const COLLAPSE: f64 = 0.85;
const HOLD_WINDOWS: u8 = 3;

impl Aimd {
    /// Returns the new target.
    pub fn step(&mut self, input: AimdInput) -> usize {
        let AimdInput {
            sample,
            target,
            ceiling,
            backlog,
        } = input;
        let ceiling = ceiling.clamp(1, MAX_THREADS);
        let target = target.clamp(1, ceiling);
        let prev = self.prev.replace(sample);
        let Some(prev) = prev else {
            return target; // warm-up window
        };
        let improved = sample.bytes_per_sec > prev.bytes_per_sec * IMPROVE
            || sample.files_per_sec > prev.files_per_sec * IMPROVE;
        let collapsed = sample.bytes_per_sec < prev.bytes_per_sec * COLLAPSE
            && sample.files_per_sec < prev.files_per_sec * COLLAPSE;
        if let Some(before) = self.increased_from.take() {
            if improved {
                // The last +1 helped: keep climbing.
                return self.climb(target, ceiling, backlog);
            }
            // It did not help: return to the previous level and hold.
            self.hold = HOLD_WINDOWS;
            return before.clamp(1, ceiling).min(target);
        }
        if collapsed && target > 1 {
            self.hold = HOLD_WINDOWS;
            return target.saturating_sub((target / 4).max(1)).max(1);
        }
        if self.hold > 0 {
            self.hold -= 1;
            return target;
        }
        self.climb(target, ceiling, backlog)
    }

    fn climb(&mut self, target: usize, ceiling: usize, backlog: bool) -> usize {
        if backlog && target < ceiling {
            self.increased_from = Some(target);
            target + 1
        } else {
            target
        }
    }
}

// ───────────────────────── Settings defaults ─────────────────────────

static DEFAULTS: Mutex<(ThreadSetting, Priority)> =
    Mutex::new((ThreadSetting::Auto, Priority::Normal));

/// Settings defaults for engines that take no explicit options (the move
/// engine's cross-volume fallback). Refreshed whenever prefs are saved and when a
/// job is enqueued.
pub fn set_defaults(setting: ThreadSetting, priority: Priority) {
    *DEFAULTS.lock().unwrap() = (setting, priority);
}

pub fn defaults() -> (ThreadSetting, Priority) {
    *DEFAULTS.lock().unwrap()
}

// ───────────────────────── registry + event sink ─────────────────────────

fn registry() -> &'static Mutex<HashMap<String, Arc<CopyControl>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<CopyControl>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

type StateSink = Box<dyn Fn(&CopyControlState) + Send + Sync>;

fn sink() -> &'static Mutex<Option<StateSink>> {
    static SINK: OnceLock<Mutex<Option<StateSink>>> = OnceLock::new();
    SINK.get_or_init(Default::default)
}

/// Install the UI bridge (Tauri event emitter). Called once at startup.
pub fn set_state_sink(f: StateSink) {
    *sink().lock().unwrap() = Some(f);
}

pub(crate) fn emit_state(state: &CopyControlState) {
    if state.operation_id.is_empty() {
        return;
    }
    if let Some(f) = sink().lock().unwrap().as_ref() {
        f(state);
    }
}

/// Control of job `id`, created with the given initial settings on first use
/// (a move job may run several copies in sequence; they share one control).
pub fn control_for(id: &str, setting: ThreadSetting, priority: Priority) -> Arc<CopyControl> {
    registry()
        .lock()
        .unwrap()
        .entry(id.to_string())
        .or_insert_with(|| CopyControl::new(Some(id.to_string()), setting, priority))
        .clone()
}

pub fn get(id: &str) -> Option<Arc<CopyControl>> {
    registry().lock().unwrap().get(id).cloned()
}

/// Called by the queue when job `id` ends.
pub fn forget(id: &str) {
    if let Some(control) = registry().lock().unwrap().remove(id) {
        control.end_run();
        let mut state = control.state();
        state.running = false;
        emit_state(&state);
    }
}

/// States of every job that is copying right now.
pub fn running_states() -> Vec<CopyControlState> {
    let mut out: Vec<CopyControlState> = registry()
        .lock()
        .unwrap()
        .values()
        .filter(|c| c.is_running())
        .map(|c| c.state())
        .collect();
    out.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
    out
}

pub fn has_running() -> bool {
    registry().lock().unwrap().values().any(|c| c.is_running())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: f64, f: f64) -> Sample {
        Sample {
            bytes_per_sec: b,
            files_per_sec: f,
        }
    }

    fn input(sample: Sample, target: usize, ceiling: usize) -> AimdInput {
        AimdInput {
            sample,
            target,
            ceiling,
            backlog: true,
        }
    }

    /// Throughput that scales linearly up to `knee` workers and then flat.
    fn curve(knee: usize, n: usize) -> Sample {
        let eff = n.min(knee) as f64;
        s(eff * 50e6, eff * 400.0)
    }

    #[test]
    fn adjust_changes_target_and_turns_auto_off() {
        let c = CopyControl::new(None, ThreadSetting::Auto, Priority::Normal);
        assert!(c.is_auto());
        let st = c.adjust(Adjust::Up { by: 1 });
        assert!(!st.auto);
        assert_eq!(st.threads, 5);
        assert_eq!(c.adjust(Adjust::Down { by: 2 }).threads, 3);
        assert_eq!(c.adjust(Adjust::Set { threads: 99 }).threads, MAX_THREADS);
        assert_eq!(c.adjust(Adjust::Set { threads: 0 }).threads, 1);
        assert_eq!(c.adjust(Adjust::Down { by: 1 }).threads, 1);
        assert!(c.adjust(Adjust::Auto { enabled: true }).auto);
        let st = c.adjust(Adjust::Priority {
            priority: Priority::Background,
        });
        assert_eq!(st.priority, Priority::Background);
    }

    #[test]
    fn retire_is_race_safe_and_keeps_one_worker() {
        let c = CopyControl::new(None, ThreadSetting::Fixed(4), Priority::Normal);
        for _ in 0..4 {
            c.worker_started();
        }
        c.adjust(Adjust::Set { threads: 2 });
        let retired = (0..4).filter(|_| c.should_retire()).count();
        assert_eq!(retired, 2);
        assert_eq!(c.active(), 2);
        c.adjust(Adjust::Set { threads: 1 });
        assert!(c.should_retire());
        assert!(!c.should_retire(), "never retire the last worker");
        assert_eq!(c.active(), 1);
    }

    #[test]
    fn budget_is_shared_but_first_worker_always_granted() {
        let b = Budget::new(3);
        assert!(b.try_acquire(false));
        assert!(b.try_acquire(false));
        assert!(b.try_acquire(false));
        assert!(!b.try_acquire(false), "limit reached");
        assert!(b.try_acquire(true), "first worker bypasses the limit");
        assert_eq!(b.used(), 4);
        b.release();
        assert!(!b.try_acquire(false), "still at the limit");
        b.release();
        assert!(b.try_acquire(false));
    }

    #[test]
    fn aimd_climbs_to_the_knee_and_settles() {
        let mut aimd = Aimd::default();
        let mut target = 1usize;
        let mut history = Vec::new();
        for _ in 0..40 {
            target = aimd.step(input(curve(6, target), target, 16));
            history.push(target);
        }
        assert!(
            history.iter().all(|t| *t <= 8),
            "overshoot beyond knee+2: {history:?}"
        );
        let tail = &history[history.len() - 8..];
        assert!(
            tail.iter().all(|t| (5..=7).contains(t)),
            "does not settle near the knee 6: {history:?}"
        );
    }

    #[test]
    fn aimd_respects_ceiling_and_backlog() {
        let mut aimd = Aimd::default();
        let mut target = 1usize;
        for _ in 0..30 {
            target = aimd.step(input(curve(100, target), target, 2));
        }
        assert_eq!(target, 2, "ceiling for HDD");
        let mut aimd = Aimd::default();
        let mut target = 3usize;
        for _ in 0..10 {
            target = aimd.step(AimdInput {
                backlog: false,
                ..input(curve(100, target), target, 16)
            });
        }
        assert_eq!(target, 3, "no backlog, no growth");
    }

    #[test]
    fn aimd_backs_off_on_collapse() {
        let mut aimd = Aimd::default();
        assert_eq!(aimd.step(input(s(500e6, 5000.0), 8, 16)), 8);
        let after = aimd.step(input(s(100e6, 900.0), 8, 16));
        assert_eq!(after, 6, "multiplicative decrease by a quarter");
        // holds for a few windows even if throughput is flat
        for _ in 0..HOLD_WINDOWS {
            assert_eq!(aimd.step(input(s(100e6, 900.0), 6, 16)), 6);
        }
    }

    #[test]
    fn media_profiles_match_the_design() {
        assert_eq!(media_profile(Media::Hdd, false), (1, 2));
        assert_eq!(media_profile(Media::Network, false), (4, 8));
        assert_eq!(media_profile(Media::Ssd, false), (4, 16));
        assert_eq!(media_profile(Media::Ssd, true), (4, 8));
    }

    #[test]
    fn registry_shares_one_control_per_job_and_forgets() {
        let a = control_for("op_test_reg", ThreadSetting::Fixed(3), Priority::Normal);
        let b = control_for("op_test_reg", ThreadSetting::Fixed(9), Priority::Background);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(b.target(), 3, "first settings win");
        a.begin_run(Media::Ssd, 4, 16);
        assert!(
            running_states()
                .iter()
                .any(|s| s.operation_id == "op_test_reg")
        );
        forget("op_test_reg");
        assert!(get("op_test_reg").is_none());
        assert!(!a.is_running());
    }
}
