//! File-operation queue: stable ids, one heavy job at a time (D14), a real
//! cancel token, and progress events throttled to ≤10/s per operation.
//!
//! Deliberately Tauri-free: progress flows out through a `ProgressSink`
//! closure so the queue is fully unit-testable; `ipc.rs` bridges the sink to
//! Tauri events. `paused` exists in the state model (task 09 design) but no
//! pause control is exposed until real pause lands — no fake buttons.

use crate::domain::{
    ErrorCode, FsError, OperationId, OperationKind, OperationProgress, OperationState,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Cancellation handle handed to running jobs. Engines check `is_cancelled`
/// between units of work and must exit promptly with `Cancelled`.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>, Option<Arc<str>>);

impl CancelToken {
    /// Token of a queued job: carries the operation id so an engine can find
    /// the job's live controls (FER-471 copy threads/priority).
    pub fn for_operation(id: &str) -> Self {
        Self(Arc::default(), Some(Arc::from(id)))
    }
    pub fn operation_id(&self) -> Option<&str> {
        self.1.as_deref()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub type ProgressSink = Box<dyn Fn(&OperationProgress) + Send>;

/// What a job actually does. Engines (copy/move/delete/…) implement this.
/// They report bytes/files through `report` and must poll `cancel`.
pub type JobBody =
    Box<dyn FnOnce(&CancelToken, &dyn Fn(OperationProgress)) -> Result<(), FsError> + Send>;

/// Rebuilds a job body from its captured parameters (F2-26 BTM retry).
/// Factories are stored on the job, so a failed operation can be retried
/// repeatedly — each retry becomes a NEW operation with a fresh id and a
/// fresh cancel token, while the failed entry stays in the history.
/// `Sync` because jobs live in shared queue state.
pub type JobFactory = Arc<dyn Fn() -> JobBody + Send + Sync>;

struct Job {
    kind: OperationKind,
    state: OperationState,
    cancel: CancelToken,
    body: Option<JobBody>,
    /// Rebuilds the body for `retry` (absent → not retryable).
    factory: Option<JobFactory>,
    /// Human-readable failure for the `failed` state.
    error: Option<String>,
    progress: OperationProgress,
}

#[derive(Default)]
struct QueueInner {
    queue: Vec<OperationId>, // pending ids, FIFO
    jobs: HashMap<OperationId, Job>,
    seq: u64,
    shutdown: bool,
}

pub struct OperationQueue {
    inner: Arc<(Mutex<QueueInner>, Condvar)>,
}

impl OperationQueue {
    pub fn new(sink: ProgressSink) -> Arc<Self> {
        let q = Arc::new(Self {
            inner: Arc::new((Mutex::new(QueueInner::default()), Condvar::new())),
        });
        let worker_inner = Arc::clone(&q.inner);
        std::thread::Builder::new()
            .name("ferrum-ops-worker".into())
            .spawn(move || worker_loop(worker_inner, sink))
            .expect("spawn ops worker");
        q
    }

    /// Enqueue a job; returns its stable id. The job starts as soon as the
    /// single worker is free (one heavy job at a time — D14).
    pub fn enqueue(&self, kind: OperationKind, body: JobBody) -> OperationId {
        self.insert_job(kind, body, None)
    }

    /// Enqueue a job that can be retried after failure (F2-26 BTM). The
    /// factory rebuilds the body from captured parameters; `retry` re-enqueues
    /// under a NEW operation id. No pause/resume and no persistence: the
    /// engines have no safe checkpoints (D14), and recovery semantics for
    /// restart-after-crash are deliberately out of scope — no fake controls.
    pub fn enqueue_retryable(
        &self,
        kind: OperationKind,
        body: JobBody,
        factory: JobFactory,
    ) -> OperationId {
        self.insert_job(kind, body, Some(factory))
    }

    /// Retry a FAILED operation (F2-26): build a fresh body via the stored
    /// factory and enqueue it as a new operation. The failed entry remains
    /// in the snapshot (history) and keeps its factory, so it can be retried
    /// again. Returns None for unknown, still-active, cancelled, completed,
    /// or non-retryable ids.
    pub fn retry(&self, id: &str) -> Option<OperationId> {
        let mut inner = self.inner.0.lock().unwrap();
        let job = inner.jobs.get(id)?;
        if job.state != OperationState::Failed {
            return None;
        }
        let factory = job.factory.clone()?;
        let kind = job.kind;
        let body = factory();
        let id = insert_job(&mut inner, kind, body, Some(factory));
        drop(inner);
        self.inner.1.notify_one();
        Some(id)
    }

    fn insert_job(
        &self,
        kind: OperationKind,
        body: JobBody,
        factory: Option<JobFactory>,
    ) -> OperationId {
        let mut inner = self.inner.0.lock().unwrap();
        let id = insert_job(&mut inner, kind, body, factory);
        drop(inner);
        self.inner.1.notify_one();
        id
    }

    /// Cancel a queued or running job. Returns false for unknown/finished ids.
    pub fn cancel(&self, id: &str) -> bool {
        let mut inner = self.inner.0.lock().unwrap();
        match inner.jobs.get_mut(id) {
            Some(job) if matches!(job.state, OperationState::Queued | OperationState::Running) => {
                job.cancel.cancel();
                // Queued jobs can transition immediately; running ones are
                // finished by the worker when they observe the token.
                if job.state == OperationState::Queued {
                    job.state = OperationState::Cancelled;
                    job.progress.state = OperationState::Cancelled;
                    job.progress.finished_at_ms = Some(now_ms());
                }
                true
            }
            _ => false,
        }
    }

    /// Snapshot of every operation for (re)rendering the queue UI.
    pub fn snapshot(&self) -> Vec<OperationProgress> {
        let inner = self.inner.0.lock().unwrap();
        let mut out: Vec<OperationProgress> =
            inner.jobs.values().map(|j| j.progress.clone()).collect();
        out.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
        out
    }

    /// Test/support hook: drop finished entries so the panel stays readable.
    pub fn clear_finished(&self) {
        let mut inner = self.inner.0.lock().unwrap();
        let done: Vec<OperationId> = inner
            .jobs
            .iter()
            .filter(|(_, j)| {
                matches!(
                    j.state,
                    OperationState::Completed | OperationState::Failed | OperationState::Cancelled
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in done {
            inner.jobs.remove(&id);
            inner.queue.retain(|x| x != &id);
        }
    }
}

fn next_id(seq: &mut u64) -> OperationId {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    *seq += 1;
    format!("op_{millis:x}-{seq:x}")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Shared job insertion (enqueue + retry): fresh id, fresh cancel token,
/// zeroed progress stamped with the enqueue time (F2-26 history).
fn insert_job(
    inner: &mut QueueInner,
    kind: OperationKind,
    body: JobBody,
    factory: Option<JobFactory>,
) -> OperationId {
    let id = next_id(&mut inner.seq);
    let progress = OperationProgress {
        operation_id: id.clone(),
        kind,
        state: OperationState::Queued,
        processed_bytes: 0,
        total_bytes: None,
        processed_files: 0,
        total_files: None,
        current_path: None,
        rate_bytes_per_sec: None,
        error: None,
        failures: Vec::new(),
        queued_at_ms: Some(now_ms()),
        finished_at_ms: None,
    };
    inner.queue.push(id.clone());
    inner.jobs.insert(
        id.clone(),
        Job {
            kind,
            state: OperationState::Queued,
            cancel: CancelToken::for_operation(&id),
            body: Some(body),
            factory,
            error: None,
            progress,
        },
    );
    id
}

fn worker_loop(inner: Arc<(Mutex<QueueInner>, Condvar)>, sink: ProgressSink) {
    loop {
        let (lock, cvar) = &*inner;
        let next_id = {
            let mut guard = lock.lock().unwrap();
            loop {
                // Drop ids that were cancelled while queued.
                while let Some(id) = guard.queue.first().cloned() {
                    let cancelled = guard
                        .jobs
                        .get(&id)
                        .is_some_and(|j| j.state == OperationState::Cancelled);
                    if cancelled {
                        guard.queue.remove(0);
                        continue;
                    }
                    break;
                }
                if let Some(id) = guard.queue.first().cloned() {
                    guard.queue.remove(0); // hand-over: the job now owns itself
                    break id;
                }
                if guard.shutdown {
                    return;
                }
                guard = cvar.wait(guard).unwrap();
            }
        };

        run_job(&inner, &next_id, &sink);
    }
}

/// Runs one job on the worker thread. Progress is throttled to ≥100ms
/// between emissions (≤10/s — task 09 contract).
fn run_job(inner: &(Mutex<QueueInner>, Condvar), id: &str, sink: &ProgressSink) {
    let (lock, _cvar) = inner;
    let (kind, cancel, body, start_state) = {
        let mut guard = lock.lock().unwrap();
        let Some(job) = guard.jobs.get_mut(id) else {
            return;
        };
        if job.state == OperationState::Cancelled {
            return;
        }
        job.state = OperationState::Running;
        job.progress.state = OperationState::Running;
        (
            job.kind,
            job.cancel.clone(),
            job.body.take().expect("job body present once"),
            job.progress.clone(),
        )
    };

    let _ = start_state;
    // Fn-closure (job bodies take &dyn Fn): throttle state lives in a Cell.
    let last_emit = std::cell::RefCell::new(Instant::now() - Duration::from_secs(1));
    let kind_for_progress = kind;
    let report = |p: OperationProgress| {
        let p = OperationProgress {
            operation_id: id.to_string(),
            kind: kind_for_progress,
            state: OperationState::Running,
            ..p
        };
        // persist + emit under the ≥100ms spacing contract
        let mut guard = lock.lock().unwrap();
        if let Some(job) = guard.jobs.get_mut(id)
            && job.state == OperationState::Running
        {
            // Engines never own the history stamps (F2-26): queued_at stays
            // with the queue; finished_at only appears at terminalization.
            let queued_at = job.progress.queued_at_ms;
            job.progress = p.clone();
            job.progress.queued_at_ms = queued_at;
            job.progress.finished_at_ms = None;
        }
        drop(guard);
        let mut le = last_emit.borrow_mut();
        let now = Instant::now();
        if now.duration_since(*le) >= Duration::from_millis(100) {
            *le = now;
            sink(&p);
        }
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&cancel, &report)));

    let (final_state, error) = match result {
        Ok(Ok(())) => {
            if cancel.is_cancelled() {
                (OperationState::Cancelled, None)
            } else {
                (OperationState::Completed, None)
            }
        }
        Ok(Err(e)) => {
            if e.code == ErrorCode::Cancelled || cancel.is_cancelled() {
                (OperationState::Cancelled, None)
            } else {
                (OperationState::Failed, Some(e.message))
            }
        }
        Err(_) => (OperationState::Failed, Some("job panicked".to_string())),
    };

    crate::copy_pool::forget(id); // FER-471: the job's live copy controls end with the job
    let mut guard = lock.lock().unwrap();
    if let Some(job) = guard.jobs.get_mut(id) {
        job.state = final_state;
        job.error = error.clone();
        job.progress.state = final_state;
        job.progress.current_path = None;
        // F2-26 traceability: terminal stamp always, and the failure reason
        // travels on the progress so the BTM history stays explorable.
        job.progress.finished_at_ms = Some(now_ms());
        job.progress.error = if final_state == OperationState::Failed {
            Some(error.unwrap_or_else(|| "operation failed".into()))
        } else {
            None
        };
        let final_progress = job.progress.clone();
        drop(guard);
        sink(&final_progress); // terminal events always emitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn collecting_sink() -> (Arc<Mutex<Vec<OperationProgress>>>, ProgressSink) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        (
            seen,
            Box::new(move |p| sink_seen.lock().unwrap().push(p.clone())),
        )
    }

    /// Busy-wait until `pred` over the snapshot holds, up to `ms` millis.
    fn wait_for(
        q: &OperationQueue,
        ms: u64,
        pred: impl Fn(&OperationProgress) -> bool,
    ) -> OperationProgress {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let snap = q.snapshot();
            if let Some(p) = snap.iter().find(|p| pred(p))
                && !matches!(p.state, OperationState::Queued | OperationState::Running)
            {
                return p.clone();
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for condition; snapshot={snap:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn dummy(kind: OperationKind, ticks: u64, tick_ms: u64) -> JobBody {
        Box::new(move |cancel, report| {
            for i in 0..ticks {
                if cancel.is_cancelled() {
                    return Err(FsError::new(ErrorCode::Cancelled, "cancelled by user"));
                }
                report(OperationProgress {
                    operation_id: String::new(),
                    kind,
                    state: OperationState::Running,
                    processed_bytes: i,
                    total_bytes: Some(ticks),
                    processed_files: i,
                    total_files: Some(ticks),
                    current_path: Some(format!("C:\\dummy\\{i}.bin")),
                    rate_bytes_per_sec: None,
                    error: None,
                    failures: Vec::new(),
                    queued_at_ms: None,
                    finished_at_ms: None,
                });
                std::thread::sleep(Duration::from_millis(tick_ms));
            }
            Ok(())
        })
    }

    #[test]
    fn dummy_job_runs_to_completion() {
        let (seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 3, 10));
        let final_p = wait_for(&q, 2000, |p| p.operation_id == id);
        assert_eq!(final_p.state, OperationState::Completed);
        assert_eq!(final_p.total_files, Some(3));
        // Terminal progress must have been emitted.
        let events = seen.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|p| p.operation_id == id && p.state == OperationState::Completed)
        );
    }

    fn tagged_job(tag: u8, o: Arc<Mutex<Vec<u8>>>) -> JobBody {
        Box::new(move |_c: &CancelToken, _r: &dyn Fn(OperationProgress)| {
            o.lock().unwrap().push(tag);
            std::thread::sleep(Duration::from_millis(120));
            o.lock().unwrap().push(tag + 10);
            Ok(())
        })
    }

    #[test]
    fn jobs_run_serially_one_heavy_at_a_time() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let order = Arc::new(Mutex::new(Vec::<u8>::new()));
        q.enqueue(OperationKind::Copy, tagged_job(1, Arc::clone(&order)));
        let id2 = q.enqueue(OperationKind::Move, tagged_job(2, Arc::clone(&order)));
        wait_for(&q, 5000, |p| p.operation_id == id2);
        let seq = order.lock().unwrap().clone();
        // No interleaving: 1 finished (11) before 2 started.
        assert_eq!(seq, vec![1, 11, 2, 12]);
    }

    #[test]
    fn cancel_running_job_ends_it_promptly() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 100, 20)); // ~2s
        std::thread::sleep(Duration::from_millis(80)); // let it start
        assert!(q.cancel(&id));
        let final_p = wait_for(&q, 1000, |p| p.operation_id == id);
        assert_eq!(final_p.state, OperationState::Cancelled);
    }

    #[test]
    fn cancel_queued_job_never_runs_it() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let blocker = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 10, 25));
        let queued_id = q.enqueue(OperationKind::Move, dummy(OperationKind::Move, 5, 10));
        assert!(q.cancel(&queued_id));
        wait_for(&q, 3000, |p| p.operation_id == blocker);
        let snap = q.snapshot();
        let qj = snap.iter().find(|p| p.operation_id == queued_id).unwrap();
        assert_eq!(qj.state, OperationState::Cancelled);
        assert_eq!(qj.processed_files, 0);
    }

    #[test]
    fn progress_is_throttled_to_ten_per_second() {
        // FER-459: bound the count by the span the job REALLY took (Windows sleeps round up to the
        // ~15.6ms timer tick and a busy PC stretches them further), not by its nominal 400ms.
        let stamped: Arc<Mutex<Vec<(Instant, OperationProgress)>>> = Arc::default();
        let sink_stamped = Arc::clone(&stamped);
        let q = OperationQueue::new(Box::new(move |p| {
            sink_stamped
                .lock()
                .unwrap()
                .push((Instant::now(), p.clone()))
        }));
        // 20 ticks, one report each; unthrottled would emit 20 running events.
        let id = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 20, 20));
        wait_for(&q, 30_000, |p| p.operation_id == id);
        let events = stamped.lock().unwrap();
        let running: Vec<Instant> = events
            .iter()
            .filter(|(_, p)| p.operation_id == id && p.state == OperationState::Running)
            .map(|(at, _)| *at)
            .collect();
        let span = running.last().unwrap().duration_since(running[0]);
        // Emissions are >= 100ms apart: at most span/100ms + 1, plus one for sink-side jitter.
        let allowed = (span.as_millis() / 100) as usize + 2;
        assert!(
            running.len() <= allowed,
            "emitted {} running events in {span:?} (allowed {allowed})",
            running.len()
        );
        assert!(running.len() < 20, "throttle must drop most of 20 reports");
    }

    #[test]
    fn failing_job_reports_failure() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue(
            OperationKind::Delete,
            Box::new(|_c, _r| {
                Err(FsError::new(
                    ErrorCode::PermissionDenied,
                    "locked by another process",
                ))
            }),
        );
        let p = wait_for(&q, 2000, |p| p.operation_id == id);
        assert_eq!(p.state, OperationState::Failed);
    }

    #[test]
    fn panicking_job_fails_the_operation_not_the_worker() {
        let (tx, _rx) = mpsc::channel::<OperationProgress>();
        let q = OperationQueue::new(Box::new(move |p| {
            let _ = tx.send(p.clone());
        }));
        let bad = q.enqueue(OperationKind::Copy, Box::new(|_c, _r| panic!("engine bug")));
        let p = wait_for(&q, 2000, |p| p.operation_id == bad);
        assert_eq!(p.state, OperationState::Failed);
        // Worker still alive for the next job.
        let ok = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 1, 5));
        let p2 = wait_for(&q, 2000, |p| p.operation_id == ok);
        assert_eq!(p2.state, OperationState::Completed);
    }

    #[test]
    fn clear_finished_keeps_active_operations() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 1, 5));
        wait_for(&q, 2000, |p| p.operation_id == id);
        assert_eq!(q.snapshot().len(), 1);
        q.clear_finished();
        assert!(q.snapshot().is_empty());
    }

    #[test]
    fn cancel_unknown_or_finished_is_rejected() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        assert!(!q.cancel("op_missing"));
        let id = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 1, 5));
        wait_for(&q, 2000, |p| p.operation_id == id);
        assert!(!q.cancel(&id));
    }

    // ── F2-26 BTM: retry failed, history traceability ────────────────────

    /// A job whose body fails every time with a per-file failure reported.
    fn failing_factory(tag: String) -> JobBody {
        Box::new(move |_c, report| {
            report(OperationProgress {
                operation_id: String::new(),
                kind: OperationKind::Copy,
                state: OperationState::Running,
                processed_bytes: 0,
                total_bytes: Some(2),
                processed_files: 1,
                total_files: Some(2),
                current_path: Some(format!("{tag}\\second.bin")),
                rate_bytes_per_sec: None,
                error: None,
                failures: vec![crate::domain::OperationFailure {
                    path: format!("{tag}\\locked.bin"),
                    message: "locked by another process".into(),
                }],
                queued_at_ms: None,
                finished_at_ms: None,
            });
            Err(FsError::new(ErrorCode::Io, "1 of 2 item(s) failed: locked"))
        })
    }

    #[test]
    fn retry_failed_operation_runs_fresh_body_under_new_id() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue_retryable(
            OperationKind::Copy,
            failing_factory("C:\\t".into()),
            Arc::new(|| failing_factory("C:\\t".into())),
        );
        let failed = wait_for(&q, 2000, |p| p.operation_id == id);
        assert_eq!(failed.state, OperationState::Failed);
        // Traceability: failure reason + per-file detail + stamps on history.
        assert_eq!(
            failed.error.as_deref(),
            Some("1 of 2 item(s) failed: locked")
        );
        assert_eq!(failed.failures.len(), 1);
        assert_eq!(failed.failures[0].path, r"C:\t\locked.bin");
        assert!(failed.current_path.is_none());
        assert!(failed.finished_at_ms.is_some());
        assert!(failed.queued_at_ms.is_some());

        let retry_id = q.retry(&id).expect("failed + factory → retryable");
        assert_ne!(retry_id, id);
        let retried = wait_for(&q, 2000, |p| p.operation_id == retry_id);
        assert_eq!(retried.state, OperationState::Failed); // factory fails again
        // History kept BOTH entries, each traceable.
        let snap = q.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(
            snap.iter()
                .any(|p| p.operation_id == id && p.state == OperationState::Failed)
        );
        // The failed entry can be retried AGAIN (factory not consumed).
        assert!(q.retry(&id).is_some());
    }

    #[test]
    fn retry_rejects_active_finished_and_non_retryable() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        assert_eq!(q.retry("op_missing"), None); // unknown
        let plain = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 1, 5));
        wait_for(&q, 2000, |p| p.operation_id == plain);
        // Completed without factory → not retryable.
        assert_eq!(q.retry(&plain), None);
        // Queued (still active) → not retryable even with a factory.
        let blocker = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 10, 25));
        let queued = q.enqueue_retryable(
            OperationKind::Move,
            dummy(OperationKind::Move, 1, 5),
            Arc::new(|| dummy(OperationKind::Move, 1, 5)),
        );
        assert_eq!(q.retry(&queued), None);
        // Cancelled → not retryable (retry scope is FAILED operations).
        assert!(q.cancel(&queued));
        assert_eq!(q.retry(&queued), None);
        wait_for(&q, 5000, |p| p.operation_id == blocker);
    }

    #[test]
    fn retry_then_cancel_race_leaves_history_consistent() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue_retryable(
            OperationKind::Copy,
            failing_factory("C:\\race".into()),
            Arc::new(|| {
                // Second attempt: long enough to cancel mid-run.
                dummy(OperationKind::Copy, 100, 20)
            }),
        );
        let failed = wait_for(&q, 2000, |p| p.operation_id == id);
        assert_eq!(failed.state, OperationState::Failed);
        let retry_id = q.retry(&id).expect("retry after failure");
        std::thread::sleep(Duration::from_millis(60)); // let it start
        assert!(q.cancel(&retry_id));
        let cancelled = wait_for(&q, 2000, |p| p.operation_id == retry_id);
        assert_eq!(cancelled.state, OperationState::Cancelled);
        assert!(cancelled.finished_at_ms.is_some());
        // History: failed original + cancelled retry, no duplicate ids.
        let snap = q.snapshot();
        assert_eq!(snap.len(), 2);
        let ids: std::collections::HashSet<&str> =
            snap.iter().map(|p| p.operation_id.as_str()).collect();
        assert_eq!(ids.len(), 2, "retry produced a distinct id");
        // The ORIGINAL failed entry is still retryable after the race.
        assert!(q.retry(&id).is_some());
    }

    #[test]
    fn enqueue_stamps_queued_time_for_history() {
        let (_seen, sink) = collecting_sink();
        let q = OperationQueue::new(sink);
        let id = q.enqueue(OperationKind::Copy, dummy(OperationKind::Copy, 1, 5));
        let before = now_ms();
        let snap = q.snapshot();
        let p = snap.iter().find(|p| p.operation_id == id).unwrap();
        assert!(p.queued_at_ms.is_some());
        assert!(p.queued_at_ms.unwrap() <= before + 5);
        assert!(p.finished_at_ms.is_none()); // still queued/running
        wait_for(&q, 2000, |p| p.operation_id == id);
        let done = q
            .snapshot()
            .into_iter()
            .find(|p| p.operation_id == id)
            .unwrap();
        assert!(done.finished_at_ms.unwrap() >= done.queued_at_ms.unwrap());
    }
}
