//! Lease service on top of the store's lease table (Automation design §3.4, §5.7): a holder
//! guard that heartbeats while it lives and releases on drop, the registry of manual leases a
//! process hands to its webview, and the coverage check that guards baseline saves.
//!
//! Tauri-free (the future headless runner links this). The clock, the process probe, the holder
//! identity and the heartbeat interval are injected through [`LeaseEnv`].
//!
//! Why the backend beats instead of the webview: a webview timer is throttled when the window is
//! hidden or minimized, and a lease that silently expired mid-sync would let an automation start
//! on the same folders. The backend thread beats as long as the guard exists, but only up to
//! [`MAX_MANUAL_HOLD_MS`]: a webview that vanished without releasing (reload, crash) cannot keep
//! folders locked for the rest of the session. A process that dies stops beating by itself.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::store::leases::{
    self, AcquireOutcome, AcquireRequest, BusyHolder, Holder, HolderKind, OsProcessProbe,
    ProcessProbe,
};
use super::store::{Store, StoreError};

/// Heartbeat period of a holder (design §3.4: every 5 s; a lease goes stale after 30 s).
pub const BEAT_EVERY: Duration = Duration::from_secs(5);
/// A manual lease stops being refreshed after this long, so a webview that never released it
/// cannot hold folders for the whole session. Twelve hours is longer than any sane manual sync.
pub const MAX_MANUAL_HOLD_MS: i64 = 12 * 60 * 60 * 1000;
/// Manual leases one process may hold at once (each owns a heartbeat thread).
pub const MAX_MANUAL_LEASES: usize = 8;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Everything that makes lease behaviour depend on the outside world.
#[derive(Clone)]
pub struct LeaseEnv {
    /// Unix epoch milliseconds.
    pub clock: Clock,
    pub probe: Arc<dyn ProcessProbe>,
    pub holder: Holder,
    pub beat_every: Duration,
    pub max_manual_hold_ms: i64,
}

impl LeaseEnv {
    /// The real clock, the OS process probe and the calling process.
    pub fn system() -> LeaseEnv {
        LeaseEnv {
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
            }),
            probe: Arc::new(OsProcessProbe),
            holder: Holder::current(),
            beat_every: BEAT_EVERY,
            max_manual_hold_ms: MAX_MANUAL_HOLD_MS,
        }
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }
}

/// A held lease. Heartbeats on its own thread while alive; dropping it stops the thread and
/// releases the row (the "finally" of design §3.4).
pub struct LeaseGuard {
    store: Arc<Store>,
    env: LeaseEnv,
    id: String,
    stop: Arc<(Mutex<bool>, Condvar)>,
    lost: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub enum GuardOutcome {
    Acquired(LeaseGuard),
    Busy(BusyHolder),
}

impl LeaseGuard {
    /// Acquire a lease on canonical `roots` and start its heartbeat. `max_hold_ms` bounds how
    /// long the heartbeat keeps going (`None` = until dropped, for engine runs that always end).
    pub fn acquire(
        store: &Arc<Store>,
        env: &LeaseEnv,
        kind: HolderKind,
        run_id: Option<i64>,
        roots: &[String],
        max_hold_ms: Option<i64>,
    ) -> Result<GuardOutcome, StoreError> {
        let now = env.now();
        let outcome = store.with_write(|conn| -> Result<AcquireOutcome, StoreError> {
            leases::acquire(
                conn,
                &AcquireRequest {
                    kind,
                    holder: env.holder,
                    run_id,
                    roots,
                },
                now,
                env.probe.as_ref(),
            )
        })?;
        let id = match outcome {
            AcquireOutcome::Busy { holder } => return Ok(GuardOutcome::Busy(holder)),
            AcquireOutcome::Acquired { id } => id,
        };
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let lost = Arc::new(AtomicBool::new(false));
        let thread = {
            let (store, env, id, stop, lost) = (
                store.clone(),
                env.clone(),
                id.clone(),
                stop.clone(),
                lost.clone(),
            );
            std::thread::Builder::new()
                .name("ferrum-lease-beat".into())
                .spawn(move || beat_loop(&store, &env, &id, &stop, &lost, now, max_hold_ms))
                .ok()
        };
        let guard = LeaseGuard {
            store: store.clone(),
            env: env.clone(),
            id,
            stop,
            lost,
            thread,
        };
        if guard.thread.is_none() {
            // No heartbeat thread, no lease: it would expire in 30 s under the caller's feet.
            drop(guard);
            return Err(StoreError::Unavailable);
        }
        Ok(GuardOutcome::Acquired(guard))
    }

    /// The secret lease token (32 hex characters). Handed only to the acquiring process.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// True once the heartbeat found the lease gone (reclaimed) or hit the hold limit. A lost
    /// lease no longer protects anything; the owner must stop.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

fn beat_loop(
    store: &Store,
    env: &LeaseEnv,
    id: &str,
    stop: &(Mutex<bool>, Condvar),
    lost: &AtomicBool,
    started_ms: i64,
    max_hold_ms: Option<i64>,
) {
    let (flag, cv) = stop;
    loop {
        {
            let guard = flag.lock().unwrap_or_else(|p| p.into_inner());
            let (guard, _) = cv
                .wait_timeout_while(guard, env.beat_every, |stopped| !*stopped)
                .unwrap_or_else(|p| p.into_inner());
            if *guard {
                return;
            }
        }
        let now = env.now();
        if max_hold_ms.is_some_and(|max| now.saturating_sub(started_ms) > max) {
            // Stop refreshing: the lease expires by itself 30 s later.
            lost.store(true, Ordering::SeqCst);
            return;
        }
        match store.with_write(|c| leases::heartbeat(c, id, env.holder, now)) {
            Ok(true) => {}
            Ok(false) => {
                lost.store(true, Ordering::SeqCst);
                return;
            }
            // A busy or briefly unavailable store: try again at the next beat.
            Err(_) => {}
        }
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        {
            let (flag, cv) = &*self.stop;
            *flag.lock().unwrap_or_else(|p| p.into_inner()) = true;
            cv.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Best effort: if the store is gone the lease expires on its own.
        let _ = self
            .store
            .with_write(|c| leases::release(c, &self.id, self.env.holder));
    }
}

/// Outcome of a manual acquire.
pub enum ManualAcquire {
    Acquired {
        lease_id: String,
    },
    Busy(BusyHolder),
    /// The process already holds [`MAX_MANUAL_LEASES`] manual leases.
    TooMany,
}

/// The manual leases of this process, by token. The webview holds only the token; the guard
/// (heartbeat, release) lives here, so it cannot be released by anyone who does not know the
/// token, and never by another process (the table row also names this process as holder).
pub struct LeaseRegistry {
    env: LeaseEnv,
    held: Mutex<HashMap<String, LeaseGuard>>,
}

impl LeaseRegistry {
    pub fn new(env: LeaseEnv) -> Self {
        Self {
            env,
            held: Mutex::new(HashMap::new()),
        }
    }

    pub fn env(&self) -> &LeaseEnv {
        &self.env
    }

    pub fn held_count(&self) -> usize {
        self.held.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Drop guards whose lease is already gone (reclaimed, or past the hold limit) so they stop
    /// counting against [`MAX_MANUAL_LEASES`]. Dropping also deletes a leftover row.
    fn prune_lost(&self) {
        let lost: Vec<LeaseGuard> = {
            let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
            let ids: Vec<String> = held
                .iter()
                .filter(|(_, g)| g.is_lost())
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| held.remove(id)).collect()
        };
        // Thread join and row delete happen outside the registry lock.
        drop(lost);
    }

    pub fn acquire_manual(
        &self,
        store: &Arc<Store>,
        roots: &[String],
    ) -> Result<ManualAcquire, StoreError> {
        self.prune_lost();
        if self.held_count() >= MAX_MANUAL_LEASES {
            return Ok(ManualAcquire::TooMany);
        }
        let guard = match LeaseGuard::acquire(
            store,
            &self.env,
            HolderKind::Manual,
            None,
            roots,
            Some(self.env.max_manual_hold_ms),
        )? {
            GuardOutcome::Busy(holder) => return Ok(ManualAcquire::Busy(holder)),
            GuardOutcome::Acquired(guard) => guard,
        };
        let lease_id = guard.id().to_string();
        let overflow = {
            let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
            if held.len() >= MAX_MANUAL_LEASES {
                Some(guard)
            } else {
                held.insert(lease_id.clone(), guard);
                None
            }
        };
        // Dropping releases the row; done outside the registry lock.
        if overflow.is_some() {
            return Ok(ManualAcquire::TooMany);
        }
        Ok(ManualAcquire::Acquired { lease_id })
    }

    /// Release a manual lease by its token. Unknown, already released and foreign tokens do
    /// nothing and are not distinguished (idempotent, no existence oracle).
    pub fn release(&self, lease_id: &str) -> bool {
        let guard = self
            .held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(lease_id);
        // The drop (thread join + row delete) runs outside the lock.
        guard.is_some()
    }

    /// Release every manual lease of this process. Called when the webview (re)loads its page:
    /// the JavaScript that held the tokens is gone, so nothing can release these leases any
    /// more and they must not keep folders locked for the rest of the session. Returns how many
    /// were released.
    pub fn release_all(&self) -> usize {
        let all: Vec<LeaseGuard> = {
            let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
            held.drain().map(|(_, g)| g).collect()
        };
        let n = all.len();
        drop(all);
        n
    }
}

/// Why a baseline save was refused by the lease rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverError {
    /// This process holds no fresh lease covering both roots (or the store cannot say).
    Required,
}

/// Design §3.4 / §5.7: the calling process must hold a fresh lease that covers every one of
/// `canonical_roots`. Any store problem refuses too (fail closed).
pub fn require_cover(
    store: &Store,
    env: &LeaseEnv,
    canonical_roots: &[String],
) -> Result<(), CoverError> {
    let now = env.now();
    match store.with_read(|c| -> Result<bool, StoreError> {
        leases::covers(c, env.holder, canonical_roots, now)
    }) {
        Ok(true) => Ok(()),
        _ => Err(CoverError::Required),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::store::leases::test_support::FakeProbe;
    use crate::sync_engine::roots;
    use std::sync::atomic::AtomicI64;

    const OTHER: Holder = Holder {
        pid: 777,
        created: 9,
    };

    struct Rig {
        _dir: tempfile::TempDir,
        store: Arc<Store>,
        clock: Arc<AtomicI64>,
        probe: Arc<FakeProbe>,
        env: LeaseEnv,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path(), "0.2.0"));
        let clock = Arc::new(AtomicI64::new(1_000));
        let probe = Arc::new(FakeProbe::default());
        let env = LeaseEnv {
            clock: {
                let c = clock.clone();
                Arc::new(move || c.load(Ordering::SeqCst))
            },
            probe: probe.clone(),
            holder: Holder {
                pid: 4242,
                created: 77,
            },
            beat_every: Duration::from_millis(10),
            max_manual_hold_ms: MAX_MANUAL_HOLD_MS,
        };
        Rig {
            _dir: dir,
            store,
            clock,
            probe,
            env,
        }
    }

    fn roots(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..400 {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn row_beat(rig: &Rig, id: &str) -> Option<i64> {
        rig.store
            .with_read(|c| -> Result<_, StoreError> { leases::get(c, id) })
            .unwrap()
            .map(|r| r.beat_ms)
    }

    fn acquire_manual(rig: &Rig, list: &[&str]) -> GuardOutcome {
        LeaseGuard::acquire(
            &rig.store,
            &rig.env,
            HolderKind::Manual,
            None,
            &roots(list),
            None,
        )
        .unwrap()
    }

    /// LC-6: the holder's heartbeat keeps the row fresh as time passes, so a rival never sees it
    /// stale; the guard's drop releases the row.
    #[test]
    fn guard_heartbeats_and_releases_on_drop() {
        let rig = rig();
        let GuardOutcome::Acquired(guard) = acquire_manual(&rig, &[r"C:\a", r"D:\b"]) else {
            panic!("acquired");
        };
        let id = guard.id().to_string();
        assert_eq!(row_beat(&rig, &id), Some(1_000));
        rig.clock.store(90_000, Ordering::SeqCst);
        assert!(
            wait_for(|| row_beat(&rig, &id) == Some(90_000)),
            "the heartbeat thread refreshed the row"
        );
        // Far later than 30 s after acquire, yet still busy for a rival.
        let rival = LeaseGuard::acquire(
            &rig.store,
            &LeaseEnv {
                holder: OTHER,
                ..rig.env.clone()
            },
            HolderKind::Manual,
            None,
            &roots(&[r"C:\a"]),
            None,
        )
        .unwrap();
        assert!(matches!(rival, GuardOutcome::Busy(_)));
        drop(guard);
        assert_eq!(row_beat(&rig, &id), None, "released on drop");
    }

    /// A stopped heartbeat (the holder is stuck) lets the lease be reclaimed after 30 s.
    #[test]
    fn a_holder_that_stops_beating_is_reclaimed() {
        let rig = rig();
        let GuardOutcome::Acquired(guard) = acquire_manual(&rig, &[r"C:\a"]) else {
            panic!()
        };
        // Freeze the holder: the guard's thread is stopped without releasing the row.
        let id = guard.id().to_string();
        {
            let (flag, cv) = &*guard.stop;
            *flag.lock().unwrap() = true;
            cv.notify_all();
        }
        std::mem::forget(guard); // leak: no release, as for a hung process
        rig.clock.store(1_000 + 31_000, Ordering::SeqCst);
        let rival = LeaseGuard::acquire(
            &rig.store,
            &LeaseEnv {
                holder: OTHER,
                ..rig.env.clone()
            },
            HolderKind::Manual,
            None,
            &roots(&[r"C:\a"]),
            None,
        )
        .unwrap();
        assert!(matches!(rival, GuardOutcome::Acquired(_)));
        assert_eq!(row_beat(&rig, &id), None, "the stale row was removed");
    }

    /// A lease that was reclaimed behind the holder's back is reported lost by its heartbeat.
    #[test]
    fn a_reclaimed_lease_is_reported_lost() {
        let rig = rig();
        let GuardOutcome::Acquired(guard) = acquire_manual(&rig, &[r"C:\a"]) else {
            panic!()
        };
        rig.store
            .with_write(|c| -> Result<(), StoreError> {
                c.execute("DELETE FROM leases", []).unwrap();
                Ok(())
            })
            .unwrap();
        assert!(wait_for(|| guard.is_lost()));
    }

    /// The hold limit stops the heartbeat of a manual lease nobody released.
    #[test]
    fn a_manual_lease_stops_beating_after_the_hold_limit() {
        let rig = rig();
        let registry = LeaseRegistry::new(LeaseEnv {
            max_manual_hold_ms: 5_000,
            ..rig.env.clone()
        });
        let ManualAcquire::Acquired { lease_id } = registry
            .acquire_manual(&rig.store, &roots(&[r"C:\a"]))
            .unwrap()
        else {
            panic!()
        };
        rig.clock.store(1_000 + 6_000, Ordering::SeqCst);
        assert!(wait_for(|| {
            registry
                .held
                .lock()
                .unwrap()
                .get(&lease_id)
                .is_some_and(|g| g.is_lost())
        }));
        // No more beats: after 30 s of silence the row is reclaimable.
        let beat = row_beat(&rig, &lease_id).unwrap();
        assert!(beat <= 1_000 + 6_000);
        rig.clock.store(beat + 31_000, Ordering::SeqCst);
        let rival = LeaseGuard::acquire(
            &rig.store,
            &LeaseEnv {
                holder: OTHER,
                ..rig.env.clone()
            },
            HolderKind::Manual,
            None,
            &roots(&[r"C:\a"]),
            None,
        )
        .unwrap();
        assert!(matches!(rival, GuardOutcome::Acquired(_)));
    }

    /// The registry hands out a secret token; releasing needs it, and releasing is idempotent.
    #[test]
    fn registry_releases_by_token_only() {
        let rig = rig();
        let registry = LeaseRegistry::new(rig.env.clone());
        let ManualAcquire::Acquired { lease_id } = registry
            .acquire_manual(&rig.store, &roots(&[r"C:\a", r"D:\b"]))
            .unwrap()
        else {
            panic!()
        };
        assert!(leases::is_lease_id(&lease_id));
        assert!(!registry.release(&"f".repeat(32)), "wrong token");
        assert!(!registry.release("nonsense"));
        assert_eq!(registry.held_count(), 1);
        assert!(row_beat(&rig, &lease_id).is_some(), "row untouched");
        assert!(registry.release(&lease_id));
        assert!(!registry.release(&lease_id), "second release is a no-op");
        assert_eq!(row_beat(&rig, &lease_id), None);
        // Folders are free for another acquire.
        assert!(matches!(
            registry
                .acquire_manual(&rig.store, &roots(&[r"C:\a"]))
                .unwrap(),
            ManualAcquire::Acquired { .. }
        ));
    }

    /// Another process (here: another holder identity) cannot release this process's lease by
    /// knowing the token: the table row names the holder too.
    #[test]
    fn a_foreign_process_cannot_release_with_the_token() {
        let rig = rig();
        let registry = LeaseRegistry::new(rig.env.clone());
        let ManualAcquire::Acquired { lease_id } = registry
            .acquire_manual(&rig.store, &roots(&[r"C:\a"]))
            .unwrap()
        else {
            panic!()
        };
        let foreign = rig
            .store
            .with_write(|c| -> Result<bool, StoreError> { leases::release(c, &lease_id, OTHER) })
            .unwrap();
        assert!(!foreign);
        assert!(row_beat(&rig, &lease_id).is_some());
    }

    #[test]
    fn busy_and_cap_are_reported() {
        let rig = rig();
        let registry = LeaseRegistry::new(LeaseEnv {
            holder: Holder { pid: 1, created: 1 },
            ..rig.env.clone()
        });
        let ManualAcquire::Acquired { .. } = registry
            .acquire_manual(&rig.store, &roots(&[r"C:\a", r"D:\b"]))
            .unwrap()
        else {
            panic!()
        };
        match registry
            .acquire_manual(&rig.store, &roots(&[r"C:\a\sub", r"E:\x"]))
            .unwrap()
        {
            ManualAcquire::Busy(h) => assert_eq!(h.kind, HolderKind::Manual),
            _ => panic!("busy expected"),
        }
        for i in 1..MAX_MANUAL_LEASES {
            let r = roots(&[&format!(r"C:\cap{i}"), &format!(r"D:\cap{i}")]);
            assert!(matches!(
                registry.acquire_manual(&rig.store, &r).unwrap(),
                ManualAcquire::Acquired { .. }
            ));
        }
        assert_eq!(registry.held_count(), MAX_MANUAL_LEASES);
        assert!(matches!(
            registry
                .acquire_manual(&rig.store, &roots(&[r"C:\over", r"D:\over"]))
                .unwrap(),
            ManualAcquire::TooMany
        ));
    }

    /// RQ-2: a mapped drive and the UNC path it points to are the SAME folder. The resolver is
    /// injected (no share needed); the overlap is judged on the canonical strings.
    #[test]
    fn mapped_drive_and_its_unc_overlap() {
        let rig = rig();
        let mapped = |p: &str| -> String {
            // X: is mapped to \\ferrum-test.invalid\share
            match p.get(..2) {
                Some(d) if d.eq_ignore_ascii_case("x:") => {
                    format!(r"\\ferrum-test.invalid\share{}", &p[2..])
                }
                _ => p.to_string(),
            }
        };
        let canon = |p: &str| roots::canonical_with(p, &mapped).unwrap();
        let unc = canon(r"\\ferrum-test.invalid\share\data");
        let via_drive = canon(r"X:\data");
        assert_eq!(unc.to_lowercase(), via_drive.to_lowercase());
        let sibling = canon(r"X:\data-backup");

        let GuardOutcome::Acquired(_held) = LeaseGuard::acquire(
            &rig.store,
            &rig.env,
            HolderKind::Manual,
            None,
            &[unc.clone(), canon(r"C:\elsewhere")],
            None,
        )
        .unwrap() else {
            panic!()
        };
        let rival = LeaseEnv {
            holder: OTHER,
            ..rig.env.clone()
        };
        let busy = LeaseGuard::acquire(
            &rig.store,
            &rival,
            HolderKind::Manual,
            None,
            &[via_drive, canon(r"E:\y")],
            None,
        )
        .unwrap();
        assert!(matches!(busy, GuardOutcome::Busy(_)), "same folder");
        let ok = LeaseGuard::acquire(
            &rig.store,
            &rival,
            HolderKind::Manual,
            None,
            &[sibling, canon(r"E:\y")],
            None,
        )
        .unwrap();
        assert!(matches!(ok, GuardOutcome::Acquired(_)), "sibling folder");
    }

    #[test]
    fn dead_holder_is_reclaimed_through_the_guard_api() {
        let rig = rig();
        let first = LeaseEnv {
            holder: OTHER,
            ..rig.env.clone()
        };
        let GuardOutcome::Acquired(held) = LeaseGuard::acquire(
            &rig.store,
            &first,
            HolderKind::Runner,
            Some(3),
            &roots(&[r"C:\a"]),
            None,
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(
            acquire_manual(&rig, &[r"C:\a"]),
            GuardOutcome::Busy(BusyHolder {
                kind: HolderKind::Runner,
                run_id: Some(3)
            })
        ));
        rig.probe.kill(OTHER);
        assert!(matches!(
            acquire_manual(&rig, &[r"C:\a"]),
            GuardOutcome::Acquired(_)
        ));
        drop(held);
    }

    #[test]
    fn require_cover_follows_the_lease() {
        let rig = rig();
        let pair = roots(&[r"C:\a", r"D:\b"]);
        assert_eq!(
            require_cover(&rig.store, &rig.env, &pair),
            Err(CoverError::Required),
            "no lease"
        );
        let GuardOutcome::Acquired(guard) = acquire_manual(&rig, &[r"C:\a", r"D:\b"]) else {
            panic!()
        };
        assert_eq!(require_cover(&rig.store, &rig.env, &pair), Ok(()));
        // Not the lease of this process.
        let other_env = LeaseEnv {
            holder: OTHER,
            ..rig.env.clone()
        };
        assert_eq!(
            require_cover(&rig.store, &other_env, &pair),
            Err(CoverError::Required)
        );
        // Roots outside the lease.
        assert_eq!(
            require_cover(&rig.store, &rig.env, &roots(&[r"C:\a", r"D:\zzz"])),
            Err(CoverError::Required)
        );
        drop(guard);
        assert_eq!(
            require_cover(&rig.store, &rig.env, &pair),
            Err(CoverError::Required),
            "released"
        );
    }

    /// P2-1: a webview reload loses the tokens, so `release_all` frees every manual lease at once:
    /// rows gone, folders free, nothing left in the registry.
    #[test]
    fn release_all_frees_every_manual_lease() {
        let rig = rig();
        let registry = LeaseRegistry::new(rig.env.clone());
        let mut ids = Vec::new();
        for list in [[r"C:", r"D:"], [r"E:\c", r"F:\d"]] {
            let ManualAcquire::Acquired { lease_id } =
                registry.acquire_manual(&rig.store, &roots(&list)).unwrap()
            else {
                panic!()
            };
            ids.push(lease_id);
        }
        assert_eq!(registry.release_all(), 2);
        assert_eq!(registry.held_count(), 0);
        for id in &ids {
            assert_eq!(row_beat(&rig, id), None, "row deleted");
        }
        assert_eq!(registry.release_all(), 0, "idempotent");
        // The folders are free for a new acquire.
        assert!(matches!(
            registry
                .acquire_manual(&rig.store, &roots(&[r"C:", r"D:"]))
                .unwrap(),
            ManualAcquire::Acquired { .. }
        ));
    }

    /// P2-1: a guard that already lost its lease (hold limit) is pruned and does not count
    /// against the cap of 8.
    #[test]
    fn lost_guards_are_pruned_and_do_not_count_against_the_cap() {
        let rig = rig();
        let registry = LeaseRegistry::new(LeaseEnv {
            max_manual_hold_ms: 5_000,
            ..rig.env.clone()
        });
        for i in 0..MAX_MANUAL_LEASES {
            let r = roots(&[&format!(r"C:\cap{i}"), &format!(r"D:\cap{i}")]);
            assert!(matches!(
                registry.acquire_manual(&rig.store, &r).unwrap(),
                ManualAcquire::Acquired { .. }
            ));
        }
        let extra = roots(&[r"C:\extra", r"D:\extra"]);
        assert!(matches!(
            registry.acquire_manual(&rig.store, &extra).unwrap(),
            ManualAcquire::TooMany
        ));
        // Past the hold limit every guard stops beating and reports lost.
        rig.clock.store(1_000 + 6_000, Ordering::SeqCst);
        assert!(wait_for(|| {
            registry.held.lock().unwrap().values().all(|g| g.is_lost())
        }));
        assert!(matches!(
            registry.acquire_manual(&rig.store, &extra).unwrap(),
            ManualAcquire::Acquired { .. }
        ));
        assert_eq!(registry.held_count(), 1, "the lost guards were dropped");
    }
}
