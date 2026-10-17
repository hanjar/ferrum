//! The watcher (Automation design §9.2; FER-528 / P2.1): recursive change notifications for one
//! watched root, the filter that decides which events count, and the timer of a burst of changes.
//!
//! This file is the **source of change events and nothing else**. It has no store, no queue, no
//! gate and no engine: the host hook (`host/watch.rs`) turns what it reports into queue entries,
//! and every run still goes through the gate (design §10.4). A source test keeps those names out
//! of this file.
//!
//! - **Events are hints, never data.** A path an event carries is an untrusted string. It is read
//!   here for one purpose, to decide whether the event is about something the profile looks at
//!   (a path under the root, not excluded, not the engine's own staging file, not the versioning
//!   root), and is then forgotten: only a COUNT leaves this file. No path is ever used to build an
//!   operation, a log line or a file name; the sync engine always compares the folders afresh
//!   (WA-2.4).
//! - **No polling.** [`NotifyFactory`] is backed by the operating system's change notification
//!   (`ReadDirectoryChangesW` through `notify`). There is no timer that lists a folder, and no
//!   fallback for a root without notifications (a network share is Unsupported, WA-2.3). The
//!   host talks to the factory through [`WatchFactory`], so the tests replace it with one that the
//!   test feeds by hand, and can assert it is the ONLY source of events.
//! - **Availability.** [`probe_root`] asks whether a root is there and is still the folder that
//!   was approved. It opens the folder to read its identity; it never lists its contents.
//!
//! Tauri-free (the runner links this file).

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::automation::model::{Filters, RootIdentity, RootSpec};
use crate::sync_engine::filters::ProfileFilter;
use crate::sync_engine::roots::{self, RootProbeOutcome};

// ── Raw events ──

/// What a backend reports for a root. Paths are UNTRUSTED strings (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawEvent {
    /// Something under the root changed (created, changed, renamed or removed).
    Change(Vec<String>),
    /// The backend says it lost events (the `notify` rescan flag).
    Overflow,
    /// The backend failed: what it reported can no longer be trusted (the watch was lost).
    Failed,
}

/// Where a backend delivers events. Called on the backend's own thread; it must be cheap.
pub type Sink = Arc<dyn Fn(RawEvent) + Send + Sync>;

/// A running watch. Dropping it stops the watch and frees what the operating system holds.
pub trait WatchHandle: Send {}

/// Why a watch could not be started. Carries no path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchError {
    /// The backend refused (folder gone, no access, too many watches).
    Failed,
}

/// The one source of change events (design §9.2). Production: [`NotifyFactory`]; tests: a fake
/// that the test feeds.
pub trait WatchFactory: Send + Sync {
    /// Start a RECURSIVE watch of `root`. Events go to `sink` until the handle is dropped.
    fn watch(&self, root: &str, sink: Sink) -> Result<Box<dyn WatchHandle>, WatchError>;
}

// ── The `notify` backend ──

/// Change notifications from the operating system (`notify`, recursive).
pub struct NotifyFactory;

struct NotifyHandle {
    // Held for its Drop: the watcher stops (and its thread ends) when this goes.
    _watcher: notify::RecommendedWatcher,
}

impl WatchHandle for NotifyHandle {}

/// Turn one `notify` result into a [`RawEvent`] (`None` = not a change: a read, an open, ...).
pub(crate) fn map_notify(res: notify::Result<notify::Event>) -> Option<RawEvent> {
    use notify::EventKind;
    match res {
        Err(_) => Some(RawEvent::Failed),
        Ok(event) => {
            if event.need_rescan() {
                return Some(RawEvent::Overflow);
            }
            match event.kind {
                // Reading or opening a file changes nothing.
                EventKind::Access(_) | EventKind::Other => None,
                EventKind::Any
                | EventKind::Create(_)
                | EventKind::Modify(_)
                | EventKind::Remove(_) => Some(RawEvent::Change(
                    event
                        .paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect(),
                )),
            }
        }
    }
}

impl WatchFactory for NotifyFactory {
    fn watch(&self, root: &str, sink: Sink) -> Result<Box<dyn WatchHandle>, WatchError> {
        use notify::{RecursiveMode, Watcher};
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // A panic must not unwind into the backend's thread.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(event) = map_notify(res) {
                    sink(event);
                }
            }));
        })
        .map_err(|_| WatchError::Failed)?;
        watcher
            .watch(Path::new(root), RecursiveMode::Recursive)
            .map_err(|_| WatchError::Failed)?;
        Ok(Box::new(NotifyHandle { _watcher: watcher }))
    }
}

// ── Which events count (WA-2.5, SP-4.3, SP-4.4) ──

/// What an event path means for one watched root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    /// A change in something the profile looks at.
    Change,
    /// The root folder itself was touched (it may be gone: the host looks at its availability).
    Root,
    /// Not ours: outside the root, excluded, the engine's staging file, the versioning root, or
    /// not a believable path.
    Ignored,
}

/// The rules of one watched root, built once from the profile.
pub struct EventFilter {
    root: String,
    root_parts: usize,
    skip: Vec<String>,
    /// `None` when the patterns do not parse (such a profile cannot have been approved): only
    /// the built-in excludes apply then, and a spurious event only costs a comparison.
    filter: Option<ProfileFilter>,
}

fn normalize(path: &str) -> Option<String> {
    if path.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut s = path.replace('/', "\\");
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    // A lexical `.` or `..` is never a believable event path.
    if s.split('\\').any(|c| c == "." || c == "..") {
        return None;
    }
    Some(s)
}

fn parts(path: &str) -> usize {
    path.split('\\').filter(|c| !c.is_empty()).count()
}

impl EventFilter {
    /// `root` is the canonical root of the profile; `skip` are folders whose changes never count
    /// (the versioning root); `filters` are the profile's include/exclude patterns.
    pub fn new(root: &str, skip: &[String], filters: &Filters) -> Self {
        let filter = ProfileFilter::new(&filters.include, &filters.exclude).ok();
        EventFilter {
            root: root.to_string(),
            root_parts: parts(root),
            skip: skip.to_vec(),
            filter,
        }
    }

    /// Classify one untrusted path. Pure string work: the file system is never asked.
    pub fn classify(&self, path: &str) -> PathClass {
        use crate::compare::EntryFilter;
        let Some(path) = normalize(path) else {
            return PathClass::Ignored;
        };
        if !roots::contains(&self.root, &path) {
            return PathClass::Ignored;
        }
        if self.skip.iter().any(|s| roots::contains(s, &path)) {
            return PathClass::Ignored;
        }
        let rel: Vec<&str> = path
            .split('\\')
            .filter(|c| !c.is_empty())
            .skip(self.root_parts)
            .collect();
        if rel.is_empty() {
            return PathClass::Root;
        }
        let rel = rel.join("\\");
        // The event does not say whether the path is a file or a folder, and the file system is
        // not asked. A path is read as a file: an excluded FOLDER hides everything below it (the
        // ancestors are checked as folders), and a folder that is not itself selected by an
        // include list only matters through the files in it. The one case this over-reports is
        // the excluded folder's own entry, which costs a comparison that finds nothing.
        let visible = match &self.filter {
            Some(f) => f.visible(&rel, false),
            None => !crate::sync_engine::filters::is_builtin_exclude(&rel),
        };
        if visible {
            PathClass::Change
        } else {
            PathClass::Ignored
        }
    }
}

// ── The inbox: counts, never paths ──

/// What the backends reported since the host last looked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drained {
    /// Events about something the profile looks at.
    pub changes: u64,
    /// The root folder itself was touched.
    pub root_touched: bool,
    /// The backend lost events.
    pub overflow: bool,
    /// The backend failed.
    pub failed: bool,
    /// Events dropped by the filter (diagnostics only).
    pub ignored: u64,
}

impl Drained {
    pub fn is_empty(&self) -> bool {
        self.changes == 0 && !self.root_touched && !self.overflow && !self.failed
    }

    fn absorb(&mut self, other: Drained) {
        self.changes = self.changes.saturating_add(other.changes);
        self.root_touched |= other.root_touched;
        self.overflow |= other.overflow;
        self.failed |= other.failed;
        self.ignored = self.ignored.saturating_add(other.ignored);
    }
}

/// Counts of the events of one automation. Filled on the backend's thread, taken by the host.
#[derive(Default)]
pub struct Inbox {
    state: Mutex<Drained>,
}

impl Inbox {
    /// Add `delta`; `true` when the inbox was empty before (the host is nudged only then, so a
    /// flood of events wakes the loop once per look, not once per event).
    fn add(&self, delta: Drained) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let first = state.is_empty();
        state.absorb(delta);
        first && !delta.is_empty()
    }

    /// Take everything counted so far.
    pub fn take(&self) -> Drained {
        std::mem::take(&mut *self.state.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// The sink the host gives a backend for one watched root: it classifies each event and counts.
/// `wake` runs when the inbox goes from empty to non-empty.
pub fn make_sink(
    filter: EventFilter,
    inbox: Arc<Inbox>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Sink {
    Arc::new(move |event: RawEvent| {
        let mut delta = Drained::default();
        match event {
            RawEvent::Overflow => delta.overflow = true,
            RawEvent::Failed => delta.failed = true,
            RawEvent::Change(paths) => {
                for path in &paths {
                    match filter.classify(path) {
                        PathClass::Change => delta.changes += 1,
                        PathClass::Root => delta.root_touched = true,
                        PathClass::Ignored => delta.ignored += 1,
                    }
                }
            }
        }
        if inbox.add(delta) {
            wake();
        }
    })
}

// ── The burst: quiet countdown and maximum wait (WA-1) ──

/// Why a burst is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// No event for the idle delay.
    Idle,
    /// Events kept coming; the maximum wait since the first one has passed (WA-1.4).
    MaxWait,
}

/// A burst of changes: starts with the first event, each further event restarts the idle delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Burst {
    pub started_ms: i64,
    pub until_ms: i64,
    pub count: u64,
}

impl Burst {
    pub fn start(now_ms: i64, idle_ms: i64, count: u64) -> Self {
        Burst {
            started_ms: now_ms,
            until_ms: now_ms.saturating_add(idle_ms),
            count,
        }
    }

    /// More events: the idle delay starts again.
    pub fn event(&mut self, now_ms: i64, idle_ms: i64, count: u64) {
        self.rebase(now_ms, idle_ms);
        self.count = self.count.saturating_add(count);
        self.until_ms = now_ms.saturating_add(idle_ms);
    }

    /// A clock that moved back must not freeze the countdown for the length of the step.
    fn rebase(&mut self, now_ms: i64, idle_ms: i64) {
        if now_ms < self.started_ms {
            self.started_ms = now_ms;
            self.until_ms = now_ms.saturating_add(idle_ms);
        }
    }

    pub fn due(&mut self, now_ms: i64, idle_ms: i64, max_wait_ms: i64) -> Option<Due> {
        self.rebase(now_ms, idle_ms);
        if now_ms.saturating_sub(self.started_ms) >= max_wait_ms {
            Some(Due::MaxWait)
        } else if now_ms >= self.until_ms {
            Some(Due::Idle)
        } else {
            None
        }
    }
}

// ── Availability of a root (WA-5) ──

/// What the host learned about a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Avail {
    /// The folder is there and is the one that was approved.
    Yes,
    /// Missing, unreadable, or a different folder now (drive letter reassigned).
    No,
    /// The answer did not come in time: nothing is concluded from it.
    Unknown,
}

/// Budget of one availability probe.
pub const PROBE_BUDGET: Duration = Duration::from_millis(1_000);

/// Is `spec` there, and still the approved folder? Opens the folder and reads its identity; never
/// lists its contents, and never waits longer than [`PROBE_BUDGET`].
pub fn probe_root(spec: &RootSpec) -> Avail {
    match roots::probe(&spec.path, PROBE_BUDGET) {
        RootProbeOutcome::Ready { identity, .. } => match (&identity, &spec.identity) {
            (RootIdentity::Local { .. }, RootIdentity::Local { .. }) => {
                if identity == spec.identity {
                    Avail::Yes
                } else {
                    Avail::No
                }
            }
            // A share: reachable is what we can say; the gate checks the identity at run time.
            (RootIdentity::Network { .. }, RootIdentity::Network { .. }) => Avail::Yes,
            _ => Avail::No,
        },
        RootProbeOutcome::Checking => Avail::Unknown,
        RootProbeOutcome::Missing | RootProbeOutcome::Invalid | RootProbeOutcome::Unavailable => {
            Avail::No
        }
    }
}

/// Native change notifications exist for local drives only (WA-2.3).
pub fn supports_notifications(spec: &RootSpec) -> bool {
    !matches!(spec.identity, RootIdentity::Network { .. }) && !roots::is_unc(&spec.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn filters(include: &[&str], exclude: &[&str]) -> Filters {
        Filters {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn classification_is_string_work_with_closed_answers() {
        let f = EventFilter::new(
            r"C:\Data\Photos",
            &[r"C:\Data\Photos\.versions".to_string()],
            &filters(&[], &["*.tmp", "cache/"]),
        );
        assert_eq!(f.classify(r"C:\Data\Photos\a\b.jpg"), PathClass::Change);
        assert_eq!(f.classify(r"c:\data\photos\A\B.JPG"), PathClass::Change);
        assert_eq!(f.classify(r"C:\Data\Photos"), PathClass::Root);
        assert_eq!(f.classify(r"\\?\C:\Data\Photos\x.txt"), PathClass::Change);
        // Outside the root, a sibling with the same prefix, relative and forged paths.
        assert_eq!(f.classify(r"C:\Data\Photos2\a.jpg"), PathClass::Ignored);
        assert_eq!(f.classify(r"D:\Data\Photos\a.jpg"), PathClass::Ignored);
        assert_eq!(f.classify(r"a\b.jpg"), PathClass::Ignored);
        assert_eq!(f.classify(r"C:\Data\Photos\..\x.txt"), PathClass::Ignored);
        assert_eq!(f.classify("C:\\Data\\Photos\\a\nb"), PathClass::Ignored);
        // Excluded by the profile, and by the built-in rules (staging, recycle bin).
        assert_eq!(f.classify(r"C:\Data\Photos\x.tmp"), PathClass::Ignored);
        assert_eq!(
            f.classify(r"C:\Data\Photos\cache\deep\x.jpg"),
            PathClass::Ignored
        );
        assert_eq!(
            f.classify(r"C:\Data\Photos\sub\.a.jpg.ferrum-stage-12-1"),
            PathClass::Ignored
        );
        assert_eq!(
            f.classify(r"C:\Data\Photos\.a.jpg.ferrum-backup-12-1"),
            PathClass::Ignored
        );
        assert_eq!(
            f.classify(r"C:\Data\Photos\$RECYCLE.BIN\x"),
            PathClass::Ignored
        );
        // The versioning root.
        assert_eq!(
            f.classify(r"C:\Data\Photos\.versions\a"),
            PathClass::Ignored
        );
    }

    #[test]
    fn an_include_list_hides_what_it_does_not_select() {
        let f = EventFilter::new(r"C:\R", &[], &filters(&["*.md"], &[]));
        assert_eq!(f.classify(r"C:\R\notes.md"), PathClass::Change);
        assert_eq!(f.classify(r"C:\R\deep\notes.md"), PathClass::Change);
        assert_eq!(f.classify(r"C:\R\notes.txt"), PathClass::Ignored);
    }

    #[test]
    fn unc_roots_classify_like_any_other() {
        let f = EventFilter::new(r"\\host\share\dir", &[], &filters(&[], &[]));
        assert_eq!(f.classify(r"\\host\share\dir\a.txt"), PathClass::Change);
        assert_eq!(
            f.classify(r"\\?\UNC\host\share\dir\a.txt"),
            PathClass::Change
        );
        assert_eq!(f.classify(r"\\host\share\other\a.txt"), PathClass::Ignored);
    }

    #[test]
    fn the_burst_restarts_on_each_event_and_stops_at_the_maximum_wait() {
        let idle = 10_000;
        let max = 900_000;
        // WA-1.1: events at t=0 and t=6 s, idle 10 s: due at t=16 s, not before.
        let mut b = Burst::start(0, idle, 1);
        assert_eq!(b.due(6_000, idle, max), None);
        b.event(6_000, idle, 1);
        assert_eq!(b.count, 2);
        assert_eq!(b.due(15_999, idle, max), None);
        assert_eq!(b.due(16_000, idle, max), Some(Due::Idle));
        // WA-1.4: events every 5 s never let it go quiet; the maximum wait ends it.
        let mut b = Burst::start(0, idle, 1);
        let mut t = 0;
        while t < max {
            t += 5_000;
            assert!(b.due(t, idle, max).is_none() || t >= max);
            b.event(t, idle, 1);
        }
        assert_eq!(b.due(t, idle, max), Some(Due::MaxWait));
    }

    #[test]
    fn a_clock_that_moves_back_does_not_freeze_the_countdown() {
        let mut b = Burst::start(1_000_000, 10_000, 1);
        assert_eq!(b.due(100_000, 10_000, 900_000), None);
        assert_eq!(b.due(110_000, 10_000, 900_000), Some(Due::Idle));
    }

    #[test]
    fn the_inbox_counts_and_wakes_once_per_look() {
        let inbox = Arc::new(Inbox::default());
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let sink = make_sink(
            EventFilter::new(r"C:\R", &[], &filters(&[], &["*.tmp"])),
            inbox.clone(),
            Arc::new(move || {
                w.fetch_add(1, Ordering::SeqCst);
            }),
        );
        for i in 0..1000 {
            sink(RawEvent::Change(vec![format!(r"C:\R\f{i}.txt")]));
        }
        sink(RawEvent::Change(vec![r"C:\R\x.tmp".into(), r"C:\R".into()]));
        sink(RawEvent::Overflow);
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "one wake for the flood");
        let d = inbox.take();
        assert_eq!(d.changes, 1000);
        assert!(d.root_touched && d.overflow && !d.failed);
        assert_eq!(d.ignored, 1);
        assert!(inbox.take().is_empty());
        // After a look the next event wakes again.
        sink(RawEvent::Failed);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert!(inbox.take().failed);
        // An event the filter drops wakes nobody.
        sink(RawEvent::Change(vec![r"C:\R\y.tmp".into()]));
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn notify_events_map_to_changes_overflow_and_failure() {
        use notify::event::{AccessKind, CreateKind, Flag};
        use notify::{Event, EventKind};
        let create = Event::new(EventKind::Create(CreateKind::File)).add_path(r"C:\R\a".into());
        assert_eq!(
            map_notify(Ok(create)),
            Some(RawEvent::Change(vec![r"C:\R\a".to_string()]))
        );
        let read = Event::new(EventKind::Access(AccessKind::Any)).add_path(r"C:\R\a".into());
        assert_eq!(map_notify(Ok(read)), None);
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(map_notify(Ok(rescan)), Some(RawEvent::Overflow));
        let err = notify::Error::generic("x");
        assert_eq!(map_notify(Err(err)), Some(RawEvent::Failed));
    }

    #[test]
    fn network_roots_have_no_notifications() {
        let local = RootSpec {
            path: r"C:\R".into(),
            identity: RootIdentity::Local {
                volume_serial: "1".into(),
                file_id: "2".into(),
                fs_name: "NTFS".into(),
                drive: crate::automation::model::DriveKind::Fixed,
            },
        };
        assert!(supports_notifications(&local));
        let unc = RootSpec {
            path: r"\\h\s\d".into(),
            identity: RootIdentity::Network {
                unc_prefix: r"\\h\s".into(),
                volume_serial: None,
                file_id: None,
            },
        };
        assert!(!supports_notifications(&unc));
        // A mapped drive that resolves to a share is a UNC path in the profile.
        let mapped = RootSpec {
            path: r"\\h\s\d".into(),
            identity: local.identity.clone(),
        };
        assert!(!supports_notifications(&mapped));
    }

    /// The real backend: a nested change under the root is delivered (recursive). The temp path is
    /// compared in canonical form (a runner's temp dir may be an 8.3 name such as `RUNNER~1`), and
    /// the test waits on a channel with a timeout, re-touching the file until the event arrives.
    #[test]
    fn real_notify_delivers_a_nested_change() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical root");
        let (tx, rx) = std::sync::mpsc::channel::<RawEvent>();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |event| {
            let _ = tx.lock().unwrap_or_else(|p| p.into_inner()).send(event);
        });
        let handle = NotifyFactory
            .watch(&root.to_string_lossy(), sink)
            .expect("the watch starts");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("nested dirs");
        let file = nested.join("c.txt");
        let want = file.clone();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut n = 0;
        let mut seen = false;
        while !seen && std::time::Instant::now() < deadline {
            n += 1;
            std::fs::write(&file, format!("v{n}")).expect("write");
            while let Ok(event) = rx.recv_timeout(Duration::from_millis(500)) {
                if let RawEvent::Change(paths) = event {
                    seen = paths.iter().any(|p| {
                        std::fs::canonicalize(p).is_ok_and(|c| {
                            c.to_string_lossy()
                                .eq_ignore_ascii_case(&want.to_string_lossy())
                        })
                    });
                    if seen {
                        break;
                    }
                }
            }
        }
        assert!(seen, "no event for the nested file arrived");
        drop(handle);
    }
}
