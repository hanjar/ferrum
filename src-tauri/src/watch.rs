//! Filesystem watcher (F2-26, grown from backlog #2): watch open pane
//! directories, coalesce event storms in a debounce window, and emit the
//! affected registered roots TOGETHER WITH the changed paths (capped) so the
//! UI can tell a scoped listing change apart from an ambiguous one.
//! Tauri-free core with an injectable sink (ops.rs pattern).
//! FER-369 (opens D43): a watch root may also be a single FILE, so panes
//! inside an archive watch the parent archive file on disk.

use crate::domain::FsError;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// Upper bound on changed paths carried by one batch (a storm must not
/// inflate IPC payloads; roots alone already trigger the refresh).
pub const MAX_EVENT_PATHS: usize = 64;

/// Payload of the `fs://changed` event. `roots` are the registered watch
/// roots that saw activity; `paths` are the deduped event paths (capped to
/// [`MAX_EVENT_PATHS`]). `paths` containing a root itself means the watched
/// directory appeared/vanished/changed — ambiguous for incremental updates.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsChangeBatch {
    pub roots: Vec<String>,
    pub paths: Vec<String>,
}

pub type ChangesSink = Box<dyn Fn(FsChangeBatch) + Send>;

struct HubInner {
    roots: HashSet<PathBuf>,
    watcher: Option<RecommendedWatcher>,
}

/// One hub per app. `sink` receives the deduped registered roots (plus the
/// changed paths) that saw activity during a debounce window.
pub struct WatchHub {
    inner: Arc<Mutex<HubInner>>,
    event_tx: mpsc::Sender<PathBuf>,
}

impl WatchHub {
    pub fn new(sink: ChangesSink) -> Self {
        Self::with_debounce(sink, DEBOUNCE)
    }

    /// `debounce` is injectable so tests coalesce quickly instead of sleeping
    /// the production 500ms window.
    pub fn with_debounce(sink: ChangesSink, debounce: Duration) -> Self {
        let inner = Arc::new(Mutex::new(HubInner {
            roots: HashSet::new(),
            watcher: None,
        }));
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let drain_inner = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("ferrum-watch-debounce".into())
            .spawn(move || debounce_loop(rx, drain_inner, sink, debounce))
            .expect("spawn watch debounce worker");
        Self {
            inner,
            event_tx: tx,
        }
    }

    /// Registers one watch root. F2-26 watched directories; FER-369 also
    /// allows a single FILE so a pane inside an archive can watch its parent
    /// archive file on disk (non-recursive either way). A path that does not
    /// exist is refused — a vanished watch target cannot deliver events.
    pub fn watch_start(&self, path: &str) -> Result<(), FsError> {
        let p = PathBuf::from(path);
        if !p.exists() {
            return Err(FsError::new(
                crate::domain::ErrorCode::NotFound,
                "Cannot watch a missing path",
            )
            .with_path(path));
        }
        let mut hub = self
            .inner
            .lock()
            .map_err(|_| FsError::new(crate::domain::ErrorCode::Io, "watch hub poisoned"))?;
        if hub.roots.contains(&p) {
            return Ok(()); // already watched
        }
        if hub.watcher.is_none() {
            hub.watcher = Some(
                notify::recommended_watcher({
                    let tx = self.event_tx.clone();
                    move |res: Result<notify::Event, notify::Error>| {
                        if let Ok(ev) = res {
                            for path in ev.paths {
                                let _ = tx.send(path);
                            }
                        }
                    }
                })
                .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?,
            );
        }
        hub.watcher
            .as_mut()
            .expect("watcher just created")
            .watch(&p, RecursiveMode::NonRecursive)
            .map_err(|e| {
                FsError::new(crate::domain::ErrorCode::Io, e.to_string()).with_path(path)
            })?;
        hub.roots.insert(p);
        Ok(())
    }

    pub fn watch_stop(&self, path: &str) -> Result<(), FsError> {
        let mut hub = self
            .inner
            .lock()
            .map_err(|_| FsError::new(crate::domain::ErrorCode::Io, "watch hub poisoned"))?;
        let p = PathBuf::from(path);
        if hub.roots.remove(&p)
            && let Some(w) = hub.watcher.as_mut()
        {
            let _ = w.unwatch(&p); // already-gone dir: ignore
        }
        Ok(())
    }

    pub fn watched(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|h| {
                h.roots
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for WatchHub {
    fn drop(&mut self) {
        // The drain worker co-owns `inner`, and the watcher callback holds a
        // sender clone, so merely dropping the hub would keep both (and the
        // OS watches) alive forever. Release the watcher explicitly: its
        // sender clone dies, then our own sender dies with `self`, the drain
        // loop sees a recv error and exits.
        if let Ok(mut hub) = self.inner.lock() {
            hub.watcher = None;
            hub.roots.clear();
        }
    }
}

/// Collect raw event paths for one debounce window, map them onto the
/// registered roots, and emit the deduped hit set plus the changed paths.
fn debounce_loop(
    rx: mpsc::Receiver<PathBuf>,
    inner: Arc<Mutex<HubInner>>,
    sink: ChangesSink,
    debounce: Duration,
) {
    loop {
        let Ok(first) = rx.recv() else {
            return; // hub dropped: shut down
        };
        let mut seen: Vec<PathBuf> = vec![first];
        std::thread::sleep(debounce);
        while let Ok(p) = rx.try_recv() {
            seen.push(p);
        }
        let Ok(hub) = inner.lock() else { return };
        let batch = coalesce(&seen, &hub.roots);
        drop(hub);
        if !batch.roots.is_empty() {
            sink(batch);
        }
    }
}

/// Pure core of the debounce window (unit-tested without sleeps): map raw
/// event paths onto registered roots, dedupe, cap the path list, and sort
/// the roots so the emitted batch is deterministic.
fn coalesce(seen: &[PathBuf], roots: &HashSet<PathBuf>) -> FsChangeBatch {
    let mut paths: Vec<String> = Vec::new();
    for p in seen {
        if roots.iter().any(|root| p.starts_with(root)) {
            let s = p.to_string_lossy().into_owned();
            if !paths.contains(&s) {
                paths.push(s);
            }
        }
    }
    paths.truncate(MAX_EVENT_PATHS);
    let mut hit_roots: Vec<String> = roots
        .iter()
        .filter(|root| seen.iter().any(|p| p.starts_with(root.as_path())))
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    hit_roots.sort();
    FsChangeBatch {
        roots: hit_roots,
        paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-watch-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Sink paired with a receiver so tests await coalesced batches.
    fn batch_sink() -> (
        mpsc::Receiver<FsChangeBatch>,
        Box<dyn Fn(FsChangeBatch) + Send>,
    ) {
        let (tx, rx) = mpsc::channel::<FsChangeBatch>();
        let sink = move |b: FsChangeBatch| {
            let _ = tx.send(b);
        };
        (rx, Box::new(sink))
    }

    #[test]
    fn watch_start_stop_and_events() {
        let root = temp("basic");
        let (rx, sink) = batch_sink();
        let hub = WatchHub::new(Box::new(sink));
        hub.watch_start(&root.to_string_lossy()).unwrap();
        assert_eq!(hub.watched(), vec![root.to_string_lossy().into_owned()]);

        std::fs::write(root.join("new.txt"), b"x").unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("change event within 5s");
        assert_eq!(got.roots, vec![root.to_string_lossy().into_owned()]);
        assert_eq!(
            got.paths,
            vec![root.join("new.txt").to_string_lossy().into_owned()]
        );

        hub.watch_stop(&root.to_string_lossy()).unwrap();
        assert!(hub.watched().is_empty());
        std::fs::write(root.join("more.txt"), b"x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watch_start_rejects_missing_path() {
        let root = temp("missing");
        let ghost = root.join("ghost.txt"); // never created
        let (_rx, sink) = batch_sink();
        let hub = WatchHub::new(Box::new(sink));
        let err = hub.watch_start(&ghost.to_string_lossy()).unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotFound);
        assert!(hub.watched().is_empty());
        assert!(hub.watch_stop(&ghost.to_string_lossy()).is_ok()); // unwatched: Ok
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watch_start_follows_a_single_file() {
        // FER-369: a pane inside an archive watches the parent archive FILE.
        // Changes to the file itself must surface as a batch rooted at it.
        let root = temp("file");
        let archive = root.join("bundle.zip");
        std::fs::write(&archive, b"zip").unwrap();
        let (rx, sink) = batch_sink();
        let hub = WatchHub::new(Box::new(sink));
        hub.watch_start(&archive.to_string_lossy()).unwrap();
        assert_eq!(hub.watched(), vec![archive.to_string_lossy().into_owned()]);

        std::fs::write(&archive, b"zip2").unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("archive file change within 5s");
        assert_eq!(got.roots, vec![archive.to_string_lossy().into_owned()]);
        assert_eq!(got.paths, vec![archive.to_string_lossy().into_owned()]);

        hub.watch_stop(&archive.to_string_lossy()).unwrap();
        assert!(hub.watched().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watched_archive_file_ignores_sibling_changes() {
        // Only the archive file is registered: writes to SIBLING files in the
        // same directory must not hit its root (scoped archive refresh).
        let root = temp("sibling");
        let archive = root.join("bundle.zip");
        std::fs::write(&archive, b"zip").unwrap();
        let (rx, sink) = batch_sink();
        let hub = WatchHub::new(Box::new(sink));
        hub.watch_start(&archive.to_string_lossy()).unwrap();
        std::fs::write(root.join("unrelated.txt"), b"x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(900)).is_err());
        // The archive file changing still lands after the sibling noise.
        std::fs::write(&archive, b"zip3").unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("archive change after sibling noise");
        assert_eq!(got.roots, vec![archive.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn active_watch_survives_its_root_vanishing_mid_watch() {
        // Device-disconnect shape with an ALREADY-ACTIVE watch: the root is
        // deleted while events flow. Expect no panic, a sane batch rooted at
        // the vanished directory (the UI treats a root hit as ambiguous →
        // reset relist, which then surfaces the error), a hub that still
        // serves other roots, a clean stop, and a drain worker that exits
        // when the hub is dropped (sink dropped → receiver disconnects).
        let gone = temp("vanish");
        let other = temp("vanish-other");
        let (rx, sink) = batch_sink();
        let hub = WatchHub::with_debounce(Box::new(sink), Duration::from_millis(50));
        let gone_s = gone.to_string_lossy().into_owned();
        hub.watch_start(&gone_s).unwrap();
        hub.watch_start(&other.to_string_lossy()).unwrap();
        std::fs::write(gone.join("x.txt"), b"x").unwrap();
        std::fs::remove_dir_all(&gone).unwrap();

        // Collect whatever the platform reports for the vanished root; every
        // batch must be rooted at a registered root, never a stray path.
        let mut saw_gone = false;
        while let Ok(b) = rx.recv_timeout(Duration::from_millis(1500)) {
            assert!(!b.roots.is_empty());
            assert!(b.roots.iter().all(|r| hub.watched().contains(r)));
            saw_gone |= b.roots.contains(&gone_s);
        }
        assert!(saw_gone, "vanished root must be reported at least once");

        // The hub is not wedged: the other root still delivers events.
        std::fs::write(other.join("alive.txt"), b"x").unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("other root still delivers after a sibling root vanished");
        assert_eq!(got.roots, vec![other.to_string_lossy().into_owned()]);

        // Stopping the vanished root is Ok and leaves only the live root.
        assert!(hub.watch_stop(&gone_s).is_ok());
        assert_eq!(hub.watched(), vec![other.to_string_lossy().into_owned()]);

        // Drop: the debounce worker exits and releases its sink.
        drop(hub);
        let end = rx.recv_timeout(Duration::from_secs(5));
        assert!(
            matches!(end, Err(mpsc::RecvTimeoutError::Disconnected)),
            "worker must exit on hub drop, got {end:?}"
        );
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn watch_stop_after_dir_removed_is_ok() {
        // Device-disconnect shape: the watched directory vanishes under us;
        // deregistration must still succeed so the pane lifecycle can move on.
        let root = temp("gone");
        let (_rx, sink) = batch_sink();
        let hub = WatchHub::new(Box::new(sink));
        hub.watch_start(&root.to_string_lossy()).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(hub.watch_stop(&root.to_string_lossy()).is_ok());
        assert!(hub.watched().is_empty());
    }

    #[test]
    fn watcher_storm_coalesces_into_one_batch_with_paths() {
        // 50 writes land inside one debounce window → exactly one sink emission (anti-thrash
        // contract), roots + capped paths. FER-459: the window is widened to 1.5s (the production
        // default is 500ms) so a busy PC stretching the 50 writes cannot split them into two
        // batches; the quiet check below outlasts the window.
        let root = temp("storm");
        let (rx, sink) = batch_sink();
        let hub = WatchHub::with_debounce(Box::new(sink), Duration::from_millis(1500));
        hub.watch_start(&root.to_string_lossy()).unwrap();
        for i in 0..50 {
            std::fs::write(root.join(format!("f{i:03}.txt")), b"x").unwrap();
        }
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("storm batch");
        assert_eq!(got.roots, vec![root.to_string_lossy().into_owned()]);
        assert_eq!(got.paths.len(), 50, "one deduped path per written file");
        assert!(rx.recv_timeout(Duration::from_millis(1800)).is_err());
        // Liveness: later changes still flow after the storm.
        std::fs::write(root.join("after.txt"), b"x").unwrap();
        let again = rx.recv_timeout(Duration::from_secs(5)).expect("post-storm");
        assert_eq!(again.roots, vec![root.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn events_outside_registered_roots_are_filtered() {
        let root_a = temp("a");
        let root_b = temp("b"); // not watched
        let (rx, sink) = batch_sink();
        let hub = WatchHub::new(Box::new(sink));
        hub.watch_start(&root_a.to_string_lossy()).unwrap();
        std::fs::write(root_b.join("x.txt"), b"x").unwrap();
        std::fs::write(root_a.join("y.txt"), b"x").unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).expect("batch");
        assert_eq!(got.roots, vec![root_a.to_string_lossy().into_owned()]);
        assert_eq!(
            got.paths,
            vec![root_a.join("y.txt").to_string_lossy().into_owned()]
        );
        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    #[test]
    fn injectable_debounce_delivers_fast() {
        let root = temp("fast");
        let (rx, sink) = batch_sink();
        let hub = WatchHub::with_debounce(Box::new(sink), Duration::from_millis(20));
        hub.watch_start(&root.to_string_lossy()).unwrap();
        std::fs::write(root.join("quick.txt"), b"x").unwrap();
        let started = std::time::Instant::now();
        let got = rx.recv_timeout(Duration::from_secs(5)).expect("fast batch");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "20ms debounce must not wait the 500ms production window"
        );
        assert_eq!(got.roots, vec![root.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── pure coalesce core (no sleeps, no real watcher) ──────────────────

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn coalesce_maps_dedupes_and_sorts_roots() {
        let roots: HashSet<PathBuf> = [p("C:\\a"), p("C:\\b")].into();
        let batch = coalesce(
            &[
                p("C:\\b\\2.txt"),
                p("C:\\a\\1.txt"),
                p("C:\\b\\2.txt"),
                p("C:\\z\\3.txt"),
            ],
            &roots,
        );
        assert_eq!(batch.roots, vec!["C:\\a".to_string(), "C:\\b".to_string()]);
        assert_eq!(
            batch.paths,
            vec![
                "C:\\b\\2.txt".to_string(),
                "C:\\a\\1.txt".to_string(), // deduped "C:\\b\\2.txt"
            ]
        );
    }

    #[test]
    fn coalesce_caps_path_list_at_limit() {
        let roots: HashSet<PathBuf> = [p("C:\\a")].into();
        let seen: Vec<PathBuf> = (0..(MAX_EVENT_PATHS + 20))
            .map(|i| p(&format!("C:\\a\\f{i}")))
            .collect();
        let batch = coalesce(&seen, &roots);
        assert_eq!(batch.paths.len(), MAX_EVENT_PATHS);
        assert_eq!(batch.roots, vec!["C:\\a".to_string()]);
    }

    #[test]
    fn coalesce_without_hits_is_empty() {
        let roots: HashSet<PathBuf> = [p("C:\\a")].into();
        let batch = coalesce(&[p("C:\\zz\\f.txt")], &roots);
        assert_eq!(batch, FsChangeBatch::default());
    }

    #[test]
    fn change_batch_serializes_camel_case() {
        let batch = FsChangeBatch {
            roots: vec!["C:\\a".into()],
            paths: vec!["C:\\a\\x".into()],
        };
        let json = serde_json::to_value(&batch).unwrap();
        assert!(json.get("roots").is_some());
        assert!(json.get("paths").is_some());
    }
}
