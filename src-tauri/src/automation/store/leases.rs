//! Cross-process root leases (Automation design §3.4, RQ-2): the only lock that keeps two Ferrum
//! processes (the app, the background runner, a second app instance) from synchronizing the same
//! folders at once, and the only thing that lets a baseline be saved.
//!
//! Rules this module enforces:
//! - **Atomic acquire.** Reclaiming dead leases, the overlap check and the insert happen in ONE
//!   `BEGIN IMMEDIATE` transaction, so two connections (two processes) cannot both pass the
//!   check. Roots arrive already canonicalized (real path, mapped drive resolved, design §5.8);
//!   overlap is judged component-wise and case-insensitively by `sync_engine::roots::overlap`
//!   (`data` and `data-backup` are siblings, not nested). A versioning root is simply one more
//!   root of the request.
//! - **A stuck lease cannot block forever.** A lease whose holder process is gone (`pid` plus
//!   process creation time, so a reused PID is not mistaken for the holder) or whose heartbeat
//!   is older than [`STALE_MS`] is deleted by whoever acquires next. A heartbeat far in the
//!   future (clock jump, damaged row) counts as stale too.
//! - **Only the owner releases or beats.** A lease id is a random 128-bit token that only the
//!   acquiring process receives; release and heartbeat additionally require the holder identity
//!   `(pid, creation time)` to match. A wrong token or a foreign holder changes nothing.
//! - **Fail closed.** A live lease row whose roots cannot be read blocks every acquire (it is
//!   reclaimed only once stale or dead); an unknown holder state counts as alive.
//!
//! Time and process liveness are injected so tests run without sleeping or real processes.

use rusqlite::{Connection, TransactionBehavior, params};

use super::StoreError;
use crate::sync_engine::roots;

/// A lease whose last heartbeat is older than this is reclaimable (design §3.4).
pub const STALE_MS: i64 = 30_000;
/// Roots one lease may cover (left, right, versioning root, with room to spare).
pub const MAX_ROOTS: usize = 8;
/// Longest accepted canonical root, in characters.
pub const MAX_ROOT_CHARS: usize = 4096;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

// ── Holder identity and process liveness ──

/// A process named unambiguously by its id and creation time (FILETIME), design §3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Holder {
    pub pid: i64,
    pub created: i64,
}

impl Holder {
    /// The calling process.
    pub fn current() -> Holder {
        use std::sync::OnceLock;
        static CURRENT: OnceLock<Holder> = OnceLock::new();
        *CURRENT.get_or_init(|| Holder {
            pid: i64::from(std::process::id()),
            created: os::current_process_created(),
        })
    }
}

/// Whether a process still exists. Injected so tests can declare holders dead.
pub trait ProcessProbe: Send + Sync {
    /// `true` when the process `(pid, created)` is running, or when that cannot be established
    /// (fail closed: an unknown state never lets a lease be taken over).
    fn is_alive(&self, holder: Holder) -> bool;
}

/// The operating-system probe (`OpenProcess` + `GetProcessTimes` + exit code).
pub struct OsProcessProbe;

impl ProcessProbe for OsProcessProbe {
    fn is_alive(&self, holder: Holder) -> bool {
        match os::process_state(holder.pid) {
            os::ProcState::Gone => false,
            os::ProcState::Unknown => true,
            // The same pid with another creation time is a different process (PID reuse).
            os::ProcState::Alive(created) => created == holder.created,
        }
    }
}

#[cfg(windows)]
mod os {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const ERROR_INVALID_PARAMETER: u32 = 87;
    const STILL_ACTIVE: u32 = 259;

    pub enum ProcState {
        Alive(i64),
        Gone,
        Unknown,
    }

    const ZERO: FILETIME = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };

    fn filetime(ft: &FILETIME) -> i64 {
        (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime)
    }

    pub fn current_process_created() -> i64 {
        let (mut created, mut exit, mut kernel, mut user) = (ZERO, ZERO, ZERO, ZERO);
        // SAFETY: the pseudo handle of the current process is always valid; the four out
        // parameters are writable FILETIMEs.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 { 0 } else { filetime(&created) }
    }

    pub fn process_state(pid: i64) -> ProcState {
        let Ok(pid) = u32::try_from(pid) else {
            return ProcState::Gone;
        };
        if pid == 0 {
            return ProcState::Gone;
        }
        // SAFETY: plain query access to a numeric pid; the handle is closed below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // SAFETY: reads the calling thread's last error.
            return match unsafe { GetLastError() } {
                ERROR_INVALID_PARAMETER => ProcState::Gone,
                _ => ProcState::Unknown,
            };
        }
        let (mut created, mut exit, mut kernel, mut user) = (ZERO, ZERO, ZERO, ZERO);
        let mut code: u32 = 0;
        // SAFETY: `handle` is a valid process handle with query access; the out parameters are
        // writable.
        let (times_ok, code_ok) = unsafe {
            (
                GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user),
                GetExitCodeProcess(handle, &mut code),
            )
        };
        // SAFETY: `handle` came from OpenProcess above and is closed exactly once.
        unsafe { CloseHandle(handle) };
        if times_ok == 0 || code_ok == 0 {
            return ProcState::Unknown;
        }
        if code != STILL_ACTIVE {
            return ProcState::Gone;
        }
        ProcState::Alive(filetime(&created))
    }
}

#[cfg(not(windows))]
mod os {
    pub enum ProcState {
        Alive(i64),
        Gone,
        Unknown,
    }
    pub fn current_process_created() -> i64 {
        0
    }
    pub fn process_state(_pid: i64) -> ProcState {
        ProcState::Unknown
    }
}

// ── Rows ──

/// Who holds a lease (column `holder_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderKind {
    /// Manual Synchronize Dirs / the AI chat sync, taken through `sync_lease_acquire`.
    Manual,
    /// A run hosted by the app process.
    App,
    /// A run hosted by the background runner.
    Runner,
    /// A row with a holder kind this binary does not know (still blocks, never trusted).
    Unknown,
}

impl HolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HolderKind::Manual => "manual",
            HolderKind::App => "app",
            HolderKind::Runner => "runner",
            HolderKind::Unknown => "unknown",
        }
    }

    fn parse(text: &str) -> HolderKind {
        match text {
            "manual" => HolderKind::Manual,
            "app" => HolderKind::App,
            "runner" => HolderKind::Runner,
            _ => HolderKind::Unknown,
        }
    }
}

/// What a refused acquirer is told about the lease in its way. No paths, no names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyHolder {
    pub kind: HolderKind,
    pub run_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRow {
    pub id: String,
    pub holder_kind: HolderKind,
    pub holder: Holder,
    pub run_id: Option<i64>,
    /// `None` when the stored roots could not be read (treated as overlapping everything).
    pub roots: Option<Vec<String>>,
    pub acquired_ms: i64,
    pub beat_ms: i64,
}

pub struct AcquireRequest<'a> {
    pub kind: HolderKind,
    pub holder: Holder,
    pub run_id: Option<i64>,
    /// Canonical roots (design §5.8) including a versioning root when the run has one.
    pub roots: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    Acquired { id: String },
    Busy { holder: BusyHolder },
}

/// A lease id is 32 lowercase hex characters (128 random bits).
pub fn is_lease_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn new_lease_id() -> Result<String, StoreError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| StoreError::Unavailable)?;
    Ok(hex::encode(bytes))
}

fn stale(now_ms: i64, beat_ms: i64) -> bool {
    // Either direction: an old beat is a dead holder, a beat from the far future is a broken
    // clock or row - neither may block others forever.
    now_ms.saturating_sub(beat_ms).abs() > STALE_MS
}

fn read_rows(conn: &Connection) -> Result<Vec<LeaseRow>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, holder_kind, holder_pid, holder_created, run_id, roots, acquired_ms, beat_ms
             FROM leases ORDER BY acquired_ms, id",
        )
        .map_err(unavailable)?;
    let rows = stmt
        .query_map([], |row| {
            let kind: String = row.get(1)?;
            let roots_json: String = row.get(5)?;
            Ok(LeaseRow {
                id: row.get(0)?,
                holder_kind: HolderKind::parse(&kind),
                holder: Holder {
                    pid: row.get(2)?,
                    created: row.get(3)?,
                },
                run_id: row.get(4)?,
                roots: serde_json::from_str::<Vec<String>>(&roots_json).ok(),
                acquired_ms: row.get(6)?,
                beat_ms: row.get(7)?,
            })
        })
        .map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// All lease rows (diagnostics and tests).
pub fn all(conn: &Connection) -> Result<Vec<LeaseRow>, StoreError> {
    read_rows(conn)
}

/// One lease by id (diagnostics and tests).
pub fn get(conn: &Connection, id: &str) -> Result<Option<LeaseRow>, StoreError> {
    Ok(read_rows(conn)?.into_iter().find(|r| r.id == id))
}

fn request_ok(req: &AcquireRequest<'_>) -> bool {
    !req.roots.is_empty()
        && req.roots.len() <= MAX_ROOTS
        && req
            .roots
            .iter()
            .all(|r| !r.is_empty() && r.chars().count() <= MAX_ROOT_CHARS)
}

fn overlaps_any(existing: &LeaseRow, wanted: &[String]) -> bool {
    match &existing.roots {
        // Unreadable roots on a live lease: assume the worst.
        None => true,
        Some(roots) => roots
            .iter()
            .any(|have| wanted.iter().any(|want| roots::overlap(have, want))),
    }
}

/// Acquire a lease on `req.roots`. Reclaiming, the overlap check and the insert are one
/// `BEGIN IMMEDIATE` transaction: a second connection waits (busy timeout) and then sees this
/// lease. Overlap with a live lease is `Busy`, never an error.
pub fn acquire(
    conn: &mut Connection,
    req: &AcquireRequest<'_>,
    now_ms: i64,
    probe: &dyn ProcessProbe,
) -> Result<AcquireOutcome, StoreError> {
    if !request_ok(req) {
        // The caller canonicalizes and bounds roots; an empty or absurd set is a bug, and a
        // lease that covers nothing must never be handed out.
        return Err(StoreError::Unavailable);
    }
    let id = new_lease_id()?;
    let roots_json = serde_json::to_string(req.roots).map_err(|_| StoreError::Unavailable)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let mut blocker = None;
    for row in read_rows(&tx)? {
        if stale(now_ms, row.beat_ms) || !probe.is_alive(row.holder) {
            tx.execute("DELETE FROM leases WHERE id = ?1", [&row.id])
                .map_err(unavailable)?;
            continue;
        }
        if blocker.is_none() && overlaps_any(&row, req.roots) {
            blocker = Some(BusyHolder {
                kind: row.holder_kind,
                run_id: row.run_id,
            });
        }
    }
    if let Some(holder) = blocker {
        // Commit the reclaim of dead leases even though this acquire is refused.
        tx.commit().map_err(unavailable)?;
        return Ok(AcquireOutcome::Busy { holder });
    }
    tx.execute(
        "INSERT INTO leases(id, holder_kind, holder_pid, holder_created, run_id, roots,
                            acquired_ms, beat_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            id,
            req.kind.as_str(),
            req.holder.pid,
            req.holder.created,
            req.run_id,
            roots_json,
            now_ms
        ],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    Ok(AcquireOutcome::Acquired { id })
}

/// Refresh the heartbeat. `false` = no such lease for this holder (reclaimed, released, wrong
/// token or someone else's lease): the caller has lost it.
pub fn heartbeat(
    conn: &Connection,
    id: &str,
    holder: Holder,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !is_lease_id(id) {
        return Ok(false);
    }
    let changed = conn
        .execute(
            "UPDATE leases SET beat_ms = ?1
             WHERE id = ?2 AND holder_pid = ?3 AND holder_created = ?4",
            params![now_ms, id, holder.pid, holder.created],
        )
        .map_err(unavailable)?;
    Ok(changed == 1)
}

/// Release a lease. Only its holder can: a wrong token or another process's lease is left
/// untouched and reports `false` (idempotent: releasing twice also reports `false`).
pub fn release(conn: &Connection, id: &str, holder: Holder) -> Result<bool, StoreError> {
    if !is_lease_id(id) {
        return Ok(false);
    }
    let changed = conn
        .execute(
            "DELETE FROM leases WHERE id = ?1 AND holder_pid = ?2 AND holder_created = ?3",
            params![id, holder.pid, holder.created],
        )
        .map_err(unavailable)?;
    Ok(changed == 1)
}

/// Whether `holder` owns a fresh lease that covers every one of `wanted` (each wanted root
/// equals or lies inside a root of that one lease). Used by `sync_baseline_save` (design §3.4,
/// §5.7): no covering lease, no baseline write.
pub fn covers(
    conn: &Connection,
    holder: Holder,
    wanted: &[String],
    now_ms: i64,
) -> Result<bool, StoreError> {
    if wanted.is_empty() {
        return Ok(false);
    }
    Ok(read_rows(conn)?.iter().any(|row| {
        row.holder == holder
            && !stale(now_ms, row.beat_ms)
            && row.roots.as_ref().is_some_and(|have| {
                wanted
                    .iter()
                    .all(|want| have.iter().any(|root| roots::contains(root, want)))
            })
    }))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// Probe that knows which holders are dead.
    #[derive(Default)]
    pub struct FakeProbe {
        dead: Mutex<HashSet<(i64, i64)>>,
    }

    impl FakeProbe {
        pub fn kill(&self, h: Holder) {
            self.dead.lock().unwrap().insert((h.pid, h.created));
        }
    }

    impl ProcessProbe for FakeProbe {
        fn is_alive(&self, h: Holder) -> bool {
            !self.dead.lock().unwrap().contains(&(h.pid, h.created))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeProbe;
    use super::*;
    use crate::automation::store::Store;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

    const ME: Holder = Holder {
        pid: 100,
        created: 1,
    };
    const OTHER: Holder = Holder {
        pid: 200,
        created: 2,
    };

    fn roots(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn open_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        (dir, store)
    }

    fn take(
        store: &Store,
        holder: Holder,
        list: &[&str],
        now: i64,
        probe: &dyn ProcessProbe,
    ) -> AcquireOutcome {
        let r = roots(list);
        store
            .with_write(|conn| -> Result<_, StoreError> {
                acquire(
                    conn,
                    &AcquireRequest {
                        kind: HolderKind::Manual,
                        holder,
                        run_id: None,
                        roots: &r,
                    },
                    now,
                    probe,
                )
            })
            .unwrap()
    }

    fn is_busy(o: &AcquireOutcome) -> bool {
        matches!(o, AcquireOutcome::Busy { .. })
    }

    /// RQ-2 overlap matrix on canonical strings: equal, nested both ways, `data` vs
    /// `data-backup` (siblings), case-insensitive, and a versioning root counted. The mapped
    /// drive vs UNC row lives with the canonicalization in `lease.rs`.
    #[test]
    fn overlap_matrix() {
        let probe = FakeProbe::default();
        type Case = (&'static [&'static str], &'static [&'static str], bool);
        let cases: &[Case] = &[
            // equal
            (&[r"C:\data", r"D:\mirror"], &[r"C:\data", r"E:\x"], true),
            // new inside held
            (
                &[r"C:\data", r"D:\mirror"],
                &[r"C:\data\deep", r"E:\x"],
                true,
            ),
            // new contains held
            (
                &[r"C:\data\deep", r"D:\mirror"],
                &[r"C:\data", r"E:\x"],
                true,
            ),
            // siblings, not nested
            (
                &[r"C:\data", r"D:\mirror"],
                &[r"C:\data-backup", r"E:\x"],
                false,
            ),
            // case-insensitive
            (&[r"C:\Data", r"D:\Mirror"], &[r"c:\DATA", r"e:\x"], true),
            // disjoint
            (&[r"C:\a", r"D:\b"], &[r"C:\c", r"D:\d"], false),
            // the holder's versioning root counts as one of its roots
            (
                &[r"C:\a", r"D:\b", r"D:\versions"],
                &[r"D:\versions\old", r"E:\x"],
                true,
            ),
            // ... and the new request's versioning root counts too
            (&[r"C:\a", r"D:\b"], &[r"E:\x", r"F:\y", r"D:\b\v"], true),
            // a drive root contains everything on it
            (&[r"C:\a", r"D:\b"], &[r"D:\", r"E:\x"], true),
            // UNC siblings
            (&[r"\\nas\share\data"], &[r"\\nas\share\data-backup"], false),
            (&[r"\\NAS\Share\data"], &[r"\\nas\share\DATA\x"], true),
        ];
        for (held, wanted, expect_busy) in cases {
            let (_dir, store) = open_store();
            assert!(matches!(
                take(&store, ME, held, 1_000, &probe),
                AcquireOutcome::Acquired { .. }
            ));
            let second = take(&store, OTHER, wanted, 1_001, &probe);
            assert_eq!(is_busy(&second), *expect_busy, "{held:?} vs {wanted:?}");
        }
    }

    /// The refusal names the kind of holder but nothing else.
    #[test]
    fn busy_reports_holder_kind_and_run() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        let r = roots(&[r"C:\a", r"D:\b"]);
        store
            .with_write(|conn| -> Result<_, StoreError> {
                acquire(
                    conn,
                    &AcquireRequest {
                        kind: HolderKind::Runner,
                        holder: ME,
                        run_id: Some(42),
                        roots: &r,
                    },
                    10,
                    &probe,
                )
            })
            .unwrap();
        let AcquireOutcome::Busy { holder } = take(&store, OTHER, &[r"C:\a"], 11, &probe) else {
            panic!("expected busy");
        };
        assert_eq!(holder.kind, HolderKind::Runner);
        assert_eq!(holder.run_id, Some(42));
    }

    /// LC-6: a holder whose process is gone is reclaimed at the next acquire, even with a fresh
    /// heartbeat.
    #[test]
    fn dead_holder_is_reclaimed() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        assert!(!is_busy(&take(&store, ME, &[r"C:\a"], 1_000, &probe)));
        assert!(is_busy(&take(&store, OTHER, &[r"C:\a"], 1_100, &probe)));
        probe.kill(ME);
        assert!(!is_busy(&take(&store, OTHER, &[r"C:\a"], 1_200, &probe)));
        store
            .with_read(|conn| -> Result<(), StoreError> {
                let rows = all(conn)?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].holder, OTHER);
                Ok(())
            })
            .unwrap();
    }

    /// A reused PID with another creation time is a different process (OS probe contract,
    /// checked on the real process), and an impossible pid is gone.
    #[test]
    fn os_probe_distinguishes_pid_reuse() {
        let me = Holder::current();
        assert!(OsProcessProbe.is_alive(me));
        if me.created != 0 {
            assert!(!OsProcessProbe.is_alive(Holder {
                pid: me.pid,
                created: me.created + 12_345,
            }));
        }
        assert!(!OsProcessProbe.is_alive(Holder {
            pid: i64::from(u32::MAX) + 5,
            created: 1,
        }));
    }

    /// A real child process: alive while it runs, gone once it is killed and reaped.
    #[cfg(windows)]
    #[test]
    fn os_probe_sees_a_child_process_die() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = i64::from(child.id());
        let created = match super::os::process_state(pid) {
            super::os::ProcState::Alive(c) => c,
            _ => panic!("child must be alive"),
        };
        let holder = Holder { pid, created };
        assert!(OsProcessProbe.is_alive(holder));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!OsProcessProbe.is_alive(holder));
    }

    /// LC-6: a heartbeat older than 30 s (fake clock) is reclaimed; a fresh one is not; a beat
    /// from the far future is stale as well.
    #[test]
    fn stale_heartbeat_is_reclaimed() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        assert!(!is_busy(&take(&store, ME, &[r"C:\a"], 100_000, &probe)));
        assert!(is_busy(&take(
            &store,
            OTHER,
            &[r"C:\a"],
            100_000 + STALE_MS,
            &probe
        )));
        assert!(!is_busy(&take(
            &store,
            OTHER,
            &[r"C:\a"],
            100_000 + STALE_MS + 1,
            &probe
        )));

        let (_dir2, store2) = open_store();
        assert!(!is_busy(&take(&store2, ME, &[r"C:\a"], 10_000_000, &probe)));
        // The clock went back by an hour: the old row's beat is now in the far future.
        assert!(!is_busy(&take(
            &store2,
            OTHER,
            &[r"C:\a"],
            6_400_000,
            &probe
        )));
    }

    /// A heartbeat keeps a lease alive past 30 s.
    #[test]
    fn heartbeat_keeps_the_lease() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        let AcquireOutcome::Acquired { id } = take(&store, ME, &[r"C:\a"], 0, &probe) else {
            panic!()
        };
        for t in [10_000, 20_000, 30_000, 40_000, 50_000] {
            assert!(
                store
                    .with_write(|c| -> Result<_, StoreError> { heartbeat(c, &id, ME, t) })
                    .unwrap()
            );
        }
        assert!(is_busy(&take(&store, OTHER, &[r"C:\a"], 60_000, &probe)));
    }

    /// Release and heartbeat belong to the owner: a wrong token, a foreign holder and a
    /// malformed id change nothing; releasing twice is harmless.
    #[test]
    fn only_the_owner_releases_or_beats() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        let AcquireOutcome::Acquired { id } = take(&store, ME, &[r"C:\a"], 0, &probe) else {
            panic!()
        };
        let guess = "0".repeat(32);
        store
            .with_write(|c| -> Result<(), StoreError> {
                assert!(!release(c, &guess, ME)?, "wrong token");
                assert!(
                    !release(c, &id, OTHER)?,
                    "foreign holder with the right token"
                );
                assert!(!release(c, "x'; DROP TABLE leases;--", ME)?, "malformed id");
                assert!(!heartbeat(c, &guess, ME, 5)?);
                assert!(!heartbeat(c, &id, OTHER, 5)?);
                assert_eq!(all(c)?.len(), 1);
                assert_eq!(
                    get(c, &id)?.unwrap().beat_ms,
                    0,
                    "foreign beat changed nothing"
                );
                assert!(release(c, &id, ME)?);
                assert!(!release(c, &id, ME)?, "second release is a no-op");
                assert!(all(c)?.is_empty());
                Ok(())
            })
            .unwrap();
        // The roots are free again.
        assert!(!is_busy(&take(&store, OTHER, &[r"C:\a"], 1, &probe)));
    }

    /// Lease ids are random: no two acquires share one, and each is 32 hex characters.
    #[test]
    fn lease_ids_are_random_hex() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        let mut seen = HashSet::new();
        for i in 0..20 {
            let name = format!(r"C:\r{i}");
            let AcquireOutcome::Acquired { id } = take(&store, ME, &[&name], 0, &probe) else {
                panic!()
            };
            assert!(is_lease_id(&id));
            assert!(seen.insert(id));
        }
    }

    /// A live lease with unreadable roots blocks everything (fail closed) until it expires.
    #[test]
    fn unreadable_roots_fail_closed_until_stale() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        store
            .with_write(|c| -> Result<(), StoreError> {
                c.execute(
                    "INSERT INTO leases(id, holder_kind, holder_pid, holder_created, run_id, roots,
                                        acquired_ms, beat_ms)
                     VALUES (?1, 'manual', 100, 1, NULL, 'not json', 0, 0)",
                    [&"a".repeat(32)],
                )
                .map_err(unavailable)?;
                Ok(())
            })
            .unwrap();
        assert!(is_busy(&take(
            &store,
            OTHER,
            &[r"Z:\anything"],
            1_000,
            &probe
        )));
        assert!(!is_busy(&take(
            &store,
            OTHER,
            &[r"Z:\anything"],
            31_000,
            &probe
        )));
    }

    #[test]
    fn empty_or_oversized_requests_are_refused() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        for bad in [
            roots(&[]),
            roots(&[""]),
            (0..=MAX_ROOTS).map(|i| format!(r"C:\r{i}")).collect(),
            vec!["x".repeat(MAX_ROOT_CHARS + 1)],
        ] {
            let result = store.with_write(|c| -> Result<_, StoreError> {
                acquire(
                    c,
                    &AcquireRequest {
                        kind: HolderKind::Manual,
                        holder: ME,
                        run_id: None,
                        roots: &bad,
                    },
                    0,
                    &probe,
                )
            });
            assert!(result.is_err());
        }
    }

    /// Design §3.4 / §5.7: a baseline save needs a fresh lease of THIS holder that covers both
    /// roots (a lease on a parent folder covers the folder below it).
    #[test]
    fn covers_needs_a_fresh_lease_of_the_holder() {
        let (_dir, store) = open_store();
        let probe = FakeProbe::default();
        assert!(!is_busy(&take(&store, ME, &[r"C:\a", r"D:\b"], 0, &probe)));
        let both = roots(&[r"C:\a", r"D:\B\sub"]);
        store
            .with_read(|c| -> Result<(), StoreError> {
                assert!(covers(c, ME, &both, 1_000)?);
                assert!(!covers(c, OTHER, &both, 1_000)?, "someone else's lease");
                assert!(!covers(c, ME, &roots(&[r"C:\a", r"D:\other"]), 1_000)?);
                assert!(!covers(c, ME, &roots(&[r"C:\a", r"E:\x"]), 1_000)?);
                assert!(!covers(c, ME, &both, STALE_MS + 1)?, "stale beat");
                assert!(!covers(c, ME, &[], 1_000)?);
                Ok(())
            })
            .unwrap();
    }

    /// Eight connections on eight threads, one overlapping root set: exactly one acquire wins
    /// (the overlap check and the insert are one transaction); disjoint sets are all granted.
    #[test]
    fn concurrent_connections_exclude_overlap_and_allow_disjoint() {
        let dir = tempfile::tempdir().unwrap();
        // Open once first so the schema exists, then give every thread its own connection.
        drop(Store::open(dir.path(), "0.2.0"));
        let path = dir.path().to_path_buf();
        for disjoint in [false, true] {
            let barrier = Arc::new(Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let barrier = barrier.clone();
                    let path = path.clone();
                    std::thread::spawn(move || {
                        let store = Store::open(&path, "0.2.0");
                        let probe = FakeProbe::default();
                        let holder = Holder {
                            pid: 1000 + i,
                            created: 5,
                        };
                        let list = if disjoint {
                            roots(&[&format!(r"C:\disjoint{i}\a"), &format!(r"D:\disjoint{i}\b")])
                        } else {
                            roots(&[r"C:\shared", &format!(r"D:\own{i}")])
                        };
                        barrier.wait();
                        store
                            .with_write(|c| -> Result<_, StoreError> {
                                acquire(
                                    c,
                                    &AcquireRequest {
                                        kind: HolderKind::Manual,
                                        holder,
                                        run_id: None,
                                        roots: &list,
                                    },
                                    50,
                                    &probe,
                                )
                            })
                            .unwrap()
                    })
                })
                .collect();
            let won = handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|o| matches!(o, AcquireOutcome::Acquired { .. }))
                .count();
            if disjoint {
                assert_eq!(won, 8, "disjoint sets are all granted");
            } else {
                assert_eq!(won, 1, "overlapping sets: exactly one winner");
            }
            Store::open(&path, "0.2.0")
                .with_write(|c| -> Result<(), StoreError> {
                    c.execute("DELETE FROM leases", []).map_err(unavailable)?;
                    Ok(())
                })
                .unwrap();
        }
    }

    /// Schema compatibility: the `leases` table is part of schema v1 (created by the migration
    /// framework together with the store), so a store made before this package already has it
    /// and no version bump is needed; the columns are exactly design §3.4.
    #[test]
    fn leases_table_is_part_of_schema_v1() {
        let (_dir, store) = open_store();
        store
            .with_read(|c| -> Result<(), StoreError> {
                let version: i64 = c
                    .pragma_query_value(None, "user_version", |r| r.get(0))
                    .map_err(unavailable)?;
                assert_eq!(version, crate::automation::store::SCHEMA_VERSION);
                let cols: Vec<String> = c
                    .prepare("SELECT name FROM pragma_table_info('leases') ORDER BY cid")
                    .map_err(unavailable)?
                    .query_map([], |r| r.get(0))
                    .map_err(unavailable)?
                    .collect::<Result<_, _>>()
                    .map_err(unavailable)?;
                assert_eq!(
                    cols,
                    [
                        "id",
                        "holder_kind",
                        "holder_pid",
                        "holder_created",
                        "run_id",
                        "roots",
                        "acquired_ms",
                        "beat_ms"
                    ]
                );
                Ok(())
            })
            .unwrap();
    }
}
