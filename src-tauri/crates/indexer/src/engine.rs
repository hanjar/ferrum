//! Index engine of FerrumIndexer (FER-475, IDX-S2): build a volume index
//! from the MFT, keep it live from the USN journal, save/load it, recover.
//!
//! Everything here is platform-neutral and runs against two traits, so the
//! whole lifecycle is tested on synthetic MFT images and USN buffers
//! without admin rights:
//! - [`VolumeBackend`]: one opened volume (MFT reads, journal, records);
//! - [`Platform`]: volume discovery, profiles, saved-index storage, memory
//!   budget, background priority.
//!
//! Live update rule: a journal record only says "record N changed". The
//! engine re-reads record N (`FSCTL_GET_NTFS_FILE_RECORD`) and replaces
//! everything it knows about N with the record's CURRENT state. That one
//! rule covers create, delete, rename, hardlinks, data and attribute
//! changes, and it is idempotent — replaying records already reflected in
//! a saved index is harmless, so a restart replays from the saved USN.
//!
//! Recovery: a saved index is used only when its volume serial, scope
//! fingerprint and journal ID match and its USN is still inside the
//! journal. Journal wrap (`ERROR_JOURNAL_ENTRY_DELETED`), a new journal ID,
//! a corrupt file or a scope change all mean: rebuild from the MFT.
//! The journal is never created or modified (read-only service); without
//! an active journal the index is a non-live snapshot.

use crate::changelog::{ChangeLog, Replay};
use crate::index::{Kind, Meta, RECORD_MASK, Reach, VolumeIndex};
use crate::persist::{self, IndexHeader};
use crate::proto::{Mode, VolumeErrorCode, VolumeState, VolumeStatus};
use crate::scope::Scope;
use crate::usn::{self, reason};
use ferrum_ntfs_scan::mft::{
    self, DetailedEvent, LiveRecord, MftError, VolumeGeometry, VolumeReader,
};
use ferrum_ntfs_scan::protocol::EntryKind;
use std::cell::Cell;
use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

/// Journal position as reported by `FSCTL_QUERY_USN_JOURNAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalInfo {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
}

#[derive(Debug)]
pub enum JournalError {
    /// No active journal on the volume (never created by us: read-only).
    NotActive,
    /// The requested USN was already overwritten (journal wrapped).
    Wrapped,
    /// The journal ID changed or the journal is being deleted.
    Changed,
    Io(io::Error),
}

/// One opened volume.
pub trait VolumeBackend {
    fn geometry(&mut self) -> io::Result<VolumeGeometry>;
    fn reader(&mut self) -> &mut dyn VolumeReader;
    fn query_journal(&mut self) -> Result<JournalInfo, JournalError>;
    /// One `FSCTL_READ_USN_JOURNAL` from `start` (non-blocking): the raw
    /// output bytes (8-byte next USN + records).
    fn read_journal(&mut self, journal_id: u64, start: i64) -> Result<Vec<u8>, JournalError>;
    /// The current FILE record `record` (`FSCTL_GET_NTFS_FILE_RECORD`);
    /// `None` when the volume returned a different (lower) record.
    fn file_record(&mut self, record: u64) -> io::Result<Option<Vec<u8>>>;
}

/// A volume the platform found: drive letter + serial number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeId {
    pub letter: char,
    pub serial: u64,
}

/// Everything the engine needs from the OS.
pub trait Platform: Send {
    /// Local fixed NTFS volumes.
    fn volumes(&mut self) -> Vec<VolumeId>;
    fn open(&mut self, v: &VolumeId) -> Result<Box<dyn VolumeBackend>, VolumeErrorCode>;
    /// Absolute user profile paths for the shared-mode exclusions: the
    /// literal `ProfileImagePath`s, their FINAL paths (junctions/symlinks
    /// resolved), and the root (`E:\`) of any volume mounted inside a
    /// profile. `Err` (a short reason, never a path) when the list cannot be
    /// read or resolved completely — shared mode then fails closed.
    fn profiles(&mut self) -> Result<Vec<String>, &'static str>;
    fn load(&mut self, v: &VolumeId) -> Option<Vec<u8>>;
    fn save(&mut self, v: &VolumeId, bytes: &[u8]) -> io::Result<()>;
    /// Delete one saved index (scope changed) — `false` if that failed.
    fn remove(&mut self, v: &VolumeId) -> bool;
    /// Delete every saved index (mode changed).
    fn remove_all(&mut self);
    /// Review L1: true when the profile list may have changed since the
    /// last call (Windows: `RegNotifyChangeKeyValue` on `ProfileList`);
    /// the worker then rediscovers at once instead of within 5 minutes.
    fn profiles_changed(&mut self) -> bool {
        false
    }
    /// Upper bound for all indexes together, in bytes.
    fn memory_budget(&self) -> u64;
    /// Called once on the worker thread (Windows: THREAD_MODE_BACKGROUND).
    fn enter_background(&mut self) {}
    fn log(&mut self, msg: &str);
}

/// Pause / stop checks for long work. `checkpoint` returns true when the
/// work must be abandoned: stop, mode change, or pause (the worker then
/// closes the volume handle, review L7, and resumes later).
pub trait Control {
    fn checkpoint(&self) -> bool;
    fn progress(&self, _done: u64, _total: u64) {}
}

#[derive(Debug)]
pub enum BuildError {
    Abandoned,
    Failed(VolumeErrorCode),
}

fn error_from_mft(e: &MftError) -> VolumeErrorCode {
    match e {
        MftError::Layout(_) => VolumeErrorCode::MftLayout,
        MftError::Cancelled => VolumeErrorCode::Internal,
        MftError::Io(e) | MftError::Sink(e) => io_code(e),
    }
}

fn io_code(e: &io::Error) -> VolumeErrorCode {
    match e.raw_os_error() {
        Some(5) => VolumeErrorCode::AccessDenied,
        _ if e.kind() == io::ErrorKind::OutOfMemory => VolumeErrorCode::MemoryLimit,
        _ => VolumeErrorCode::ReadFailed,
    }
}

/// A live (or snapshot) index of one volume.
pub struct LiveIndex {
    pub index: VolumeIndex,
    /// Record numbers of excluded directories (shared mode).
    pub excluded: HashSet<u64>,
    pub scope: Scope,
    /// `None` = no active journal: snapshot only.
    pub journal_id: Option<u64>,
    pub next_usn: i64,
    pub dirty: bool,
    pub stats: LiveStats,
    /// Consecutive polls whose batch had unreadable records.
    pub failed_rounds: u32,
    /// Journal batches that carried `SECURITY_CHANGE` (review I5): the
    /// worker turns a change of this counter into a new security epoch.
    pub security_changes: u64,
    /// FER-477: directories changed since the index was built or loaded
    /// (incremental sync). Complete from `changes.floor()` on.
    pub changes: ChangeLog,
    /// FER-477: number of the last worker pass in which this index read the
    /// journal to its end (`CatchUp::UpToDate`) — the freshness barrier of
    /// the `changes` query.
    pub caught_pass: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveStats {
    pub records_refreshed: u64,
    /// Records whose attribute list could not be fully read (sizes kept).
    pub incomplete: u64,
    pub corrupt: u64,
    pub read_errors: u64,
    /// OS error code of the first failed record read (diagnostics: e.g. a
    /// privilege the service token lacks shows up here and in the log).
    pub first_read_error: Option<i32>,
}

impl LiveIndex {
    fn resolve_exclusions(index: &VolumeIndex, scope: &Scope) -> HashSet<u64> {
        scope
            .excluded
            .iter()
            .filter_map(|p| {
                let comps: Vec<&str> = p.iter().map(String::as_str).collect();
                index.find_dir(&comps).map(|frn| frn & RECORD_MASK)
            })
            .collect()
    }

    /// Wrap a decoded (saved) index; exclusions are re-resolved by path.
    pub fn from_saved(index: VolumeIndex, scope: Scope, header: &IndexHeader) -> LiveIndex {
        let excluded = Self::resolve_exclusions(&index, &scope);
        LiveIndex {
            index,
            excluded,
            scope,
            journal_id: Some(header.journal_id),
            next_usn: header.next_usn,
            dirty: false,
            stats: LiveStats::default(),
            failed_rounds: 0,
            security_changes: 0,
            changes: ChangeLog::new(header.next_usn),
            caught_pass: 0,
        }
    }

    pub fn header(&self, serial: u64) -> Option<IndexHeader> {
        Some(IndexHeader {
            volume_serial: serial,
            journal_id: self.journal_id?,
            next_usn: self.next_usn,
            scope: self.scope.fingerprint(),
            root_frn: self.index.root_frn(),
        })
    }
}

fn kind_of(k: EntryKind) -> Kind {
    match k {
        EntryKind::File => Kind::File,
        EntryKind::Dir => Kind::Dir,
        EntryKind::Symlink => Kind::Symlink,
    }
}

/// Build the index of `backend`'s volume from the MFT. The journal position
/// is taken BEFORE the scan, so [`catch_up`] afterwards replays every change
/// made during the scan (idempotently).
pub fn build(
    backend: &mut dyn VolumeBackend,
    scope: &Scope,
    budget: u64,
    ctl: &dyn Control,
) -> Result<LiveIndex, BuildError> {
    let journal = match backend.query_journal() {
        Ok(j) => Some(j),
        Err(JournalError::NotActive) => None,
        Err(JournalError::Io(e)) => return Err(BuildError::Failed(io_code(&e))),
        Err(_) => return Err(BuildError::Failed(VolumeErrorCode::ReadFailed)),
    };
    let g = backend.geometry().map_err(|e| {
        BuildError::Failed(match io_code(&e) {
            VolumeErrorCode::ReadFailed => VolumeErrorCode::NotNtfs,
            c => c,
        })
    })?;
    let mut index = VolumeIndex::new(mft::ROOT_RECORD | (mft::ROOT_RECORD << 48));
    let mut over_budget = false;
    // Review L6: prune under memory pressure only when the index grew by
    // an eighth of the budget since the last prune — not every 4096
    // records once the build hovers near the budget.
    let mut pruned_at: Option<u64> = None;
    let step = (budget / 8).max(1);
    let result = mft::scan_detailed(backend.reader(), &g, &|| ctl.checkpoint(), &mut |ev| {
        match ev {
            DetailedEvent::Entry(e, m) => {
                let meta = Meta {
                    size: e.logical,
                    allocated: e.allocated,
                    modified: m.modified,
                    attributes: m.attributes,
                };
                // Out-of-range records are skipped (counted by the caller
                // as part of the index being smaller than the MFT).
                let _ = index.add_name(e.frn, e.parent, kind_of(e.kind), meta, &e.name);
            }
            DetailedEvent::Progress { done, total } => {
                ctl.progress(done, total);
                let mem = index.memory_bytes();
                if mem > budget
                    && !scope.excluded.is_empty()
                    && pruned_at.is_none_or(|at| mem >= at.saturating_add(step))
                {
                    // Shared mode: excluded subtrees (profiles) seen so far
                    // need not wait for the end of the scan. Only what is
                    // already provably excluded goes; entries whose parents
                    // have not been scanned yet stay.
                    let ex = LiveIndex::resolve_exclusions(&index, scope);
                    index.prune_excluded(&ex);
                    index.compact(true);
                    pruned_at = Some(index.memory_bytes());
                }
                if index.memory_bytes() > budget {
                    over_budget = true;
                    return Err(MftError::Sink(io::Error::other("memory budget")));
                }
            }
        }
        Ok(())
    });
    let summary = match result {
        Ok(s) => s,
        Err(_) if over_budget => return Err(BuildError::Failed(VolumeErrorCode::MemoryLimit)),
        Err(MftError::Cancelled) => return Err(BuildError::Abandoned),
        Err(e) => return Err(BuildError::Failed(error_from_mft(&e))),
    };
    let mut index = index.with_root(summary.root_frn);
    let excluded = LiveIndex::resolve_exclusions(&index, scope);
    // Excluded subtrees were held only for the duration of the scan (the
    // MFT is not in parent-first order); they go now, with every orphan.
    index.prune(&excluded);
    index.compact(true);
    if index.memory_bytes() > budget {
        return Err(BuildError::Failed(VolumeErrorCode::MemoryLimit));
    }
    Ok(LiveIndex {
        index,
        excluded,
        scope: scope.clone(),
        journal_id: journal.map(|j| j.id),
        next_usn: journal.map(|j| j.next_usn).unwrap_or(0),
        dirty: true,
        stats: LiveStats::default(),
        failed_rounds: 0,
        security_changes: 0,
        // Changes during the scan are replayed from the position taken
        // BEFORE it, and logged then.
        changes: ChangeLog::new(journal.map(|j| j.next_usn).unwrap_or(i64::MAX)),
        caught_pass: 0,
    })
}

/// Why a live index must be rebuilt from the MFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason {
    JournalWrapped,
    JournalChanged,
    /// A journal became active on a volume indexed as a snapshot.
    JournalAppeared,
    /// A directory became visible whose descendants were never indexed
    /// (moved out of an excluded profile).
    UnindexedSubtree,
    /// The kernel returned a journal buffer the parser refused.
    BadJournalData,
    /// File records kept failing to read for [`MAX_FAILED_ROUNDS`] polls.
    RecordReadsFailing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUp {
    /// No more records right now.
    UpToDate,
    /// Stopped early (pause/stop/mode change, or a batch to retry) —
    /// resume later from the same position.
    Interrupted,
    Rebuild(RebuildReason),
}

/// Most journal reads per call (keeps a long backlog from monopolising the
/// worker; the next poll continues).
pub const MAX_ROUNDS: usize = 256;
/// A batch with unreadable file records is retried on the next polls (the
/// journal position is NOT advanced, so no update is lost); after this many
/// failing polls in a row the volume is rebuilt instead.
pub const MAX_FAILED_ROUNDS: u32 = 3;

/// Apply journal records until the journal is drained (or interrupted).
pub fn catch_up(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    budget: u64,
    ctl: &dyn Control,
) -> Result<CatchUp, VolumeErrorCode> {
    catch_up_rounds(backend, live, budget, ctl, MAX_ROUNDS).0
}

/// [`catch_up`] with at most `rounds` journal reads. The flag is true when
/// the rounds ran out with more journal to read (the caller may continue
/// after releasing the index lock). Review L6: the index is pruned at most
/// ONCE per call (not once per batch), and always before returning — so a
/// query that takes the lock afterwards never sees content below a
/// directory that just became excluded.
pub fn catch_up_rounds(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    budget: u64,
    ctl: &dyn Control,
    rounds: usize,
) -> (Result<CatchUp, VolumeErrorCode>, bool) {
    let mut prune = false;
    let mut exhausted = false;
    let r = catch_up_inner(
        backend,
        live,
        budget,
        ctl,
        rounds,
        &mut prune,
        &mut exhausted,
    );
    if prune {
        live.index.prune(&live.excluded);
    }
    live.index.compact(false);
    (r, exhausted)
}

fn catch_up_inner(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    budget: u64,
    ctl: &dyn Control,
    rounds: usize,
    prune: &mut bool,
    exhausted: &mut bool,
) -> Result<CatchUp, VolumeErrorCode> {
    let Some(jid) = live.journal_id else {
        // Snapshot: a journal that appears later means changes we cannot
        // see incrementally.
        return Ok(match backend.query_journal() {
            Ok(_) => CatchUp::Rebuild(RebuildReason::JournalAppeared),
            Err(_) => CatchUp::UpToDate,
        });
    };
    for _ in 0..rounds {
        if ctl.checkpoint() {
            return Ok(CatchUp::Interrupted);
        }
        let buf = match backend.read_journal(jid, live.next_usn) {
            Ok(b) => b,
            Err(JournalError::Wrapped) => {
                return Ok(CatchUp::Rebuild(RebuildReason::JournalWrapped));
            }
            Err(JournalError::Changed) | Err(JournalError::NotActive) => {
                return Ok(CatchUp::Rebuild(RebuildReason::JournalChanged));
            }
            Err(JournalError::Io(e)) => return Err(io_code(&e)),
        };
        let batch = match usn::parse_read_buffer(&buf) {
            Ok(b) => b,
            Err(_) => return Ok(CatchUp::Rebuild(RebuildReason::BadJournalData)),
        };
        if batch.next_usn < live.next_usn {
            return Ok(CatchUp::Rebuild(RebuildReason::BadJournalData));
        }
        if batch.changes.is_empty() {
            let moved = batch.next_usn != live.next_usn;
            live.next_usn = batch.next_usn;
            live.dirty |= moved;
            if !moved || batch.skipped == 0 {
                return Ok(CatchUp::UpToDate);
            }
            continue;
        }
        if batch
            .changes
            .iter()
            .any(|c| c.reason & reason::SECURITY_CHANGE != 0)
        {
            live.security_changes += 1;
        }
        let outcome = apply_changes_deferred(backend, live, &batch.changes);
        *prune |= outcome.prune;
        live.dirty = true;
        if live.index.memory_bytes() > budget {
            return Err(VolumeErrorCode::MemoryLimit);
        }
        if outcome.rebuild {
            return Ok(CatchUp::Rebuild(RebuildReason::UnindexedSubtree));
        }
        if outcome.read_failed {
            // Keep the position: the whole batch is refreshed again next
            // poll (idempotent).
            live.failed_rounds += 1;
            if live.failed_rounds >= MAX_FAILED_ROUNDS {
                return Ok(CatchUp::Rebuild(RebuildReason::RecordReadsFailing));
            }
            return Ok(CatchUp::Interrupted);
        }
        live.failed_rounds = 0;
        live.next_usn = batch.next_usn;
    }
    *exhausted = true;
    Ok(CatchUp::Interrupted)
}

/// What one batch of refreshes requires.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    /// The index can no longer be completed incrementally (see
    /// [`RebuildReason::UnindexedSubtree`]).
    pub rebuild: bool,
    /// At least one file record could not be read (batch must be retried).
    pub read_failed: bool,
    /// Something left the indexed area: the index must be pruned.
    pub prune: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Refresh {
    prune: bool,
    rebuild: bool,
    read_failed: bool,
}

/// Refresh every record touched by `changes`, then prune if needed.
pub fn apply_changes(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    changes: &[usn::UsnChange],
) -> BatchOutcome {
    let out = apply_changes_deferred(backend, live, changes);
    if out.prune {
        live.index.prune(&live.excluded);
    }
    live.index.compact(false);
    out
}

/// Refresh every record touched by `changes`; pruning is left to the
/// caller (`out.prune`).
fn apply_changes_deferred(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    changes: &[usn::UsnChange],
) -> BatchOutcome {
    let mut out = BatchOutcome::default();
    // FER-477: every directory whose listing this batch may change. Logged
    // even when the batch is retried (a retry sees the index already
    // updated and would no longer know the old parents).
    let mut touched: Vec<(u64, bool)> = changes
        .iter()
        .filter(|c| c.reason & reason::RELEVANT != 0)
        .map(|c| (c.parent, false))
        .collect();
    for (rec, reasons) in usn::touched_records(changes) {
        let r = refresh_record(backend, live, rec, reasons, &mut touched);
        out.prune |= r.prune;
        out.rebuild |= r.rebuild;
        out.read_failed |= r.read_failed;
    }
    // Review P2-3: open write sessions, in journal order (a retried batch
    // replays them to the same state).
    for c in changes {
        live.changes.note_write(c);
    }
    // One USN for the batch: its highest (later than or equal to each
    // change's own — a window test `usn >= since` stays conservative).
    if let Some(usn) = changes.iter().map(|c| c.usn).max() {
        for (frn, recursive) in touched {
            live.changes.record(frn, recursive, usn);
        }
    }
    out
}

/// Journal reads the priming scan of [`prime_open_writes`] may do (64 KiB
/// each: up to 512 MiB of journal); more → the open sessions are unknown.
pub const MAX_PRIME_ROUNDS: usize = 8192;

/// FER-503: while a live index sits in `open_unknown`, the priming scan is
/// retried at this interval at most (worker pass, [`SlotState::Ready`]).
/// A completed scan recovers the volume's incremental sync; a failed one
/// keeps the flag until the next interval.
pub const OPEN_UNKNOWN_REPRIME: Duration = Duration::from_secs(10 * 60);

/// Review P2-3: find the write sessions that are still open when an index
/// starts (built or loaded), by reading the journal from its first record up
/// to the index position. A session opened before the index would
/// otherwise stay invisible: further writes through its handle add no
/// record until `CLOSE`. Any failure, an interrupt, or a journal longer than
/// [`MAX_PRIME_ROUNDS`] reads leaves the log `open_unknown` (the `changes`
/// query then always answers rescan for this index); FER-503 review P2:
/// the USN the scan stopped at is kept, so a later re-prime clears the flag
/// only when its own scan still reaches back to it.
pub fn prime_open_writes(backend: &mut dyn VolumeBackend, live: &mut LiveIndex, ctl: &dyn Control) {
    let Some(jid) = live.journal_id else {
        return; // snapshot: `changes` answers rescan anyway
    };
    let mut start = match backend.query_journal() {
        Ok(j) if j.id == jid => j.first_usn,
        _ => {
            live.changes.set_open_unknown();
            return;
        }
    };
    for _ in 0..MAX_PRIME_ROUNDS {
        if start >= live.next_usn {
            return;
        }
        if ctl.checkpoint() {
            break;
        }
        let batch = match backend
            .read_journal(jid, start)
            .ok()
            .and_then(|b| usn::parse_read_buffer(&b).ok())
        {
            Some(b) => b,
            None => break,
        };
        for c in batch.changes.iter().filter(|c| c.usn < live.next_usn) {
            live.changes.note_write(c);
        }
        if batch.next_usn <= start {
            return; // end of the journal
        }
        start = batch.next_usn;
    }
    // Every record below `start` was noted; sessions from `start` on were
    // not seen.
    live.changes.set_open_unknown_from(start);
}

/// FER-503: journal reads one worker pass spends on a re-priming scan
/// (64 KiB each: 16 MiB), so a long scan never holds the single worker
/// away from the other volumes' journal follow for more than one pass.
pub const REPRIME_ROUNDS_PER_STEP: usize = 256;

/// FER-503: retry the priming scan on a LIVE index whose sessions are
/// `open_unknown` (the scan at build/load was interrupted: paused service,
/// journal longer than [`MAX_PRIME_ROUNDS`]; or more than
/// [`MAX_OPEN_WRITES`](crate::changelog::MAX_OPEN_WRITES) sessions were
/// open, at priming or while following the journal). Until it succeeds,
/// incremental sync is off for EVERY client of the volume — every
/// `changes` answer is rescan (safe, only slower).
///
/// A scan that reaches the index position knows every open session whose
/// records the journal still holds from the scan's start on. It clears the
/// flag only when (review P2, [`ChangeLog::clear_open_unknown`]) nothing
/// overflowed the session cap since it started AND it started at or below
/// the lowest USN of every session the log could not see. A session dropped
/// while the index followed the journal whose opening record has since
/// left the journal (wrap) cannot be found by any scan: its handle may
/// still write without a new record, so the flag then stays until the
/// index is rebuilt or reloaded (the conservative pre-FER-503 behaviour).
///
/// The scan is resumable: the worker runs at most
/// [`REPRIME_ROUNDS_PER_STEP`] reads per pass, so other volumes keep
/// following their journals in between — and this volume's journal follow
/// keeps updating the live sessions meanwhile. Review 2: the replay
/// therefore goes into a separate [`Replay`], merged when the scan
/// completes ([`ChangeLog::finish_reprime`]): an old replayed `CLOSE` never
/// removes a session the follow tracks (a file closed and reopened during
/// the scan), and a session the follow saw close is not re-added. Locks:
/// one short write lock to begin and one to merge, a short read lock per
/// 64 KiB batch; none is held across a journal read, so queries keep
/// flowing during the scan.
#[derive(Debug, Clone)]
pub struct Reprime {
    jid: u64,
    /// The index position when the scan began; records from here on are
    /// noted by the regular journal-follow pass.
    target: i64,
    /// [`ChangeLog::open_overflows`] when the scan began.
    overflows: u64,
    /// The journal's first record when the scan began.
    scan_from: i64,
    /// Next read position.
    next: i64,
    rounds: usize,
    /// The sessions replayed so far (records below `target`).
    replay: Replay,
}

/// Why a re-priming scan did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReprimeSkip {
    /// A snapshot (no journal) or a journal that is not the index's (a
    /// rebuild follows from `catch_up`).
    Unavailable,
    /// Review P2: a session the log could not see has left the journal; no
    /// scan can find it, the flag stays until rebuild or reload.
    OutOfJournal,
}

/// Progress of a [`Reprime`] after one call of [`reprime_advance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReprimeProgress {
    /// More journal to read: call again (next worker pass).
    Running,
    /// The scan read the whole window; `recovered`: the flag was cleared.
    Completed { recovered: bool },
    /// Interrupted or a read failed: nothing changes, retry later.
    Failed,
}

/// Start a re-priming scan of `live` (see [`Reprime`]).
pub fn reprime_begin(
    backend: &mut dyn VolumeBackend,
    live: &SharedIndex,
) -> Result<Reprime, ReprimeSkip> {
    let Some(jid) = read_index(live).journal_id else {
        return Err(ReprimeSkip::Unavailable); // `changes` answers rescan anyway
    };
    let first = match backend.query_journal() {
        Ok(j) if j.id == jid => j.first_usn,
        _ => return Err(ReprimeSkip::Unavailable),
    };
    // Target, counter and the follow's close tracking start together,
    // under one lock: every journal record from `target` on is the
    // follow's.
    let mut g = write_index(live);
    if g.journal_id != Some(jid) {
        return Err(ReprimeSkip::Unavailable);
    }
    if g.changes.lowest_dropped_usn().is_some_and(|d| first > d) {
        return Err(ReprimeSkip::OutOfJournal);
    }
    g.changes.begin_reprime();
    Ok(Reprime {
        jid,
        target: g.next_usn,
        overflows: g.changes.open_overflows(),
        scan_from: first,
        next: first,
        rounds: 0,
        replay: Replay::default(),
    })
}

/// Continue `scan` for at most `rounds` journal reads.
pub fn reprime_advance(
    backend: &mut dyn VolumeBackend,
    live: &SharedIndex,
    scan: &mut Reprime,
    ctl: &dyn Control,
    rounds: usize,
) -> ReprimeProgress {
    for _ in 0..rounds {
        if scan.next >= scan.target {
            return reprime_finish(live, scan);
        }
        if scan.rounds >= MAX_PRIME_ROUNDS || ctl.checkpoint() {
            return reprime_abort(live);
        }
        scan.rounds += 1;
        // A read below the journal's first record fails (`Wrapped`): the
        // window is read contiguously or not at all.
        let batch = match backend
            .read_journal(scan.jid, scan.next)
            .ok()
            .and_then(|b| usn::parse_read_buffer(&b).ok())
        {
            Some(b) => b,
            None => return reprime_abort(live),
        };
        if read_index(live).journal_id != Some(scan.jid) {
            return reprime_abort(live);
        }
        for c in batch.changes.iter().filter(|c| c.usn < scan.target) {
            scan.replay.note(c);
        }
        if batch.next_usn <= scan.next {
            return reprime_finish(live, scan); // end of the journal
        }
        scan.next = batch.next_usn;
    }
    if scan.next >= scan.target {
        return reprime_finish(live, scan);
    }
    ReprimeProgress::Running
}

fn reprime_finish(live: &SharedIndex, scan: &mut Reprime) -> ReprimeProgress {
    let replay = std::mem::take(&mut scan.replay);
    let recovered =
        write_index(live)
            .changes
            .finish_reprime(replay, scan.scan_from, scan.overflows);
    ReprimeProgress::Completed { recovered }
}

fn reprime_abort(live: &SharedIndex) -> ReprimeProgress {
    write_index(live).changes.abort_reprime();
    ReprimeProgress::Failed
}

/// A whole re-priming scan in one call ([`reprime_begin`] +
/// [`reprime_advance`] until done). Returns true when the scan completed
/// (whether or not the flag could be cleared).
pub fn reprime_open_writes(
    backend: &mut dyn VolumeBackend,
    live: &SharedIndex,
    ctl: &dyn Control,
) -> bool {
    let Ok(mut scan) = reprime_begin(backend, live) else {
        return false;
    };
    matches!(
        reprime_advance(backend, live, &mut scan, ctl, MAX_PRIME_ROUNDS + 1),
        ReprimeProgress::Completed { .. }
    )
}

/// Remove every name of `rec`; it can no longer be an exclusion root (its
/// record number may be reused by an unrelated directory).
fn drop_record(live: &mut LiveIndex, rec: u64) -> usize {
    live.excluded.remove(&rec);
    live.index.remove_record(rec)
}

/// Re-read one record and make the index match it. FER-477: the
/// directories whose listing this may change go to `touched` — the parents
/// of the record's names before and after, and the record itself
/// (`recursive`) when it is a directory whose identity or names changed.
fn refresh_record(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    rec: u64,
    reasons: u32,
    touched: &mut Vec<(u64, bool)>,
) -> Refresh {
    let names = |live: &LiveIndex| -> (Option<u64>, bool, Vec<(u64, String)>) {
        let e = live.index.record_entries(rec);
        let mut n: Vec<(u64, String)> = e.iter().map(|e| (e.parent, e.name.to_owned())).collect();
        n.sort();
        (
            e.first().map(|e| e.frn),
            e.iter().any(|e| e.kind == Kind::Dir),
            n,
        )
    };
    let (old_frn, old_dir, old_names) = names(live);
    let out = refresh_record_inner(backend, live, rec, reasons);
    let (new_frn, new_dir, new_names) = names(live);
    for (p, _) in old_names.iter().chain(&new_names) {
        touched.push((*p, false));
    }
    if (old_dir || new_dir) && (old_frn != new_frn || old_names != new_names) {
        for f in [old_frn, new_frn].into_iter().flatten() {
            touched.push((f, true));
        }
    }
    out
}

fn refresh_record_inner(
    backend: &mut dyn VolumeBackend,
    live: &mut LiveIndex,
    rec: u64,
    reasons: u32,
) -> Refresh {
    let mut out = Refresh::default();
    live.stats.records_refreshed += 1;
    let old = live.index.record_entries(rec);
    let old_frn = old.first().map(|e| e.frn);
    let old_was_dir = old.iter().any(|e| e.kind == Kind::Dir);
    let old_meta = old.first().map(|e| e.meta);
    drop(old);
    let raw = match backend.file_record(rec) {
        Ok(r) => r,
        Err(e) => {
            live.stats.read_errors += 1;
            live.stats
                .first_read_error
                .get_or_insert(e.raw_os_error().unwrap_or(-1));
            out.read_failed = true;
            return out;
        }
    };
    let parsed = match raw {
        None => LiveRecord::Gone,
        Some(mut b) => {
            mft::parse_live_record(rec, &mut b, &mut |n| backend.file_record(n).ok().flatten())
        }
    };
    let (seq, entries, meta, complete) = match parsed {
        LiveRecord::Gone => {
            let removed = drop_record(live, rec);
            out.prune = old_was_dir && removed > 0;
            return out;
        }
        LiveRecord::Corrupt => {
            // A record read while NTFS was rewriting it can look corrupt:
            // treat it like a failed read (review L3) — the batch is
            // retried and, if it stays corrupt, the volume is rebuilt.
            live.stats.corrupt += 1;
            out.read_failed = true;
            return out;
        }
        LiveRecord::Live {
            seq,
            entries,
            meta,
            complete,
            ..
        } => (seq, entries, meta, complete),
    };
    let frn = rec | ((seq as u64) << 48);
    if old_frn.is_some_and(|f| f != frn) {
        // Record reused: whatever it was (including an exclusion root) is
        // gone; the path check below re-adds an exclusion if it applies.
        live.excluded.remove(&rec);
    }
    let Some(first) = entries.first() else {
        // In use, but no valid name (or a metafile): nothing to index.
        let removed = drop_record(live, rec);
        out.prune = old_was_dir && removed > 0;
        return out;
    };
    let kind = kind_of(first.kind);
    let mut new_meta = Meta {
        size: first.logical,
        allocated: first.allocated,
        modified: meta.modified,
        attributes: meta.attributes,
    };
    if !complete {
        live.stats.incomplete += 1;
        if old_frn == Some(frn)
            && let Some(m) = old_meta
        {
            // Data may live in an unread extension: keep the known sizes.
            new_meta.size = m.size;
            new_meta.allocated = m.allocated;
        }
    }
    let allowed: Vec<(u64, &str)> = entries
        .iter()
        .filter(|e| live.index.reach(e.parent, &live.excluded) == Reach::Root)
        .map(|e| (e.parent, e.name.as_str()))
        .collect();
    if allowed.is_empty() {
        let removed = live.index.remove_record(rec);
        // A directory that left the indexed area still has indexed
        // children: prune them.
        out.prune = old_was_dir && removed > 0;
        return out;
    }
    if live
        .index
        .replace_record(frn, kind, new_meta, &allowed)
        .is_err()
    {
        live.stats.corrupt += 1;
        return out;
    }
    if kind == Kind::Dir && !live.scope.excluded.is_empty() {
        // A directory at an excluded path (profile folder (re)created, or
        // renamed / moved onto a profile path) becomes an exclusion root
        // right away — and whatever was indexed below it must go now.
        if let Some(e) = live.index.record_entries(rec).first()
            && let Some(path) = live.index.path_of(e)
        {
            let comps: Vec<&str> = path.split('\\').collect();
            let is_excluded = live.scope.excluded.iter().any(|x| {
                x.len() == comps.len()
                    && x.iter()
                        .zip(&comps)
                        .all(|(a, b)| a.to_lowercase() == b.to_lowercase())
            });
            if is_excluded && live.excluded.insert(rec) {
                out.prune = true;
            }
        }
        // A directory that is new to the index but was NOT created in this
        // batch came from an excluded area: descendants created before this
        // batch were never indexed. (One created in this batch is complete:
        // every child touched in the batch is refreshed after it, against
        // the current state — "New folder" + rename stays incremental.)
        if old_frn != Some(frn) && reasons & reason::FILE_CREATE == 0 {
            out.rebuild = true;
        }
    }
    out
}

// ── manager: the worker thread ───────────────────────────────────────────

/// A volume index shared between the worker (writer) and the IPC query
/// threads (readers). FER-476: queries take the read lock in bounded chunks
/// and never hold it while writing to a client; the worker takes the write
/// lock for at most [`LOCK_ROUNDS`] journal reads at a time.
pub type SharedIndex = Arc<RwLock<LiveIndex>>;

/// Journal reads per write-lock hold (the worker releases the lock in
/// between so queries are not starved by a long backlog).
pub const LOCK_ROUNDS: usize = 16;

pub fn read_index(ix: &SharedIndex) -> RwLockReadGuard<'_, LiveIndex> {
    ix.read().unwrap_or_else(|e| e.into_inner())
}

fn write_index(ix: &SharedIndex) -> RwLockWriteGuard<'_, LiveIndex> {
    ix.write().unwrap_or_else(|e| e.into_inner())
}

/// What a query may read for one volume.
#[derive(Clone)]
pub struct ServedVolume {
    /// ASCII drive letter.
    pub letter: u8,
    /// `None` = not searchable right now (first build running, error,
    /// fail-closed shared mode).
    pub index: Option<SharedIndex>,
    /// The index is the previous one, kept while a journal-triggered
    /// rebuild runs (review L5): complete as of when it stopped being
    /// updated, but not live.
    pub stale: bool,
}

/// State shared between the worker and the IPC server.
pub struct IndexShared {
    pub stop: AtomicBool,
    pub paused: AtomicBool,
    mode: Mutex<Mode>,
    mode_epoch: AtomicU64,
    status: Mutex<Vec<VolumeStatus>>,
    served: Mutex<Vec<ServedVolume>>,
    memory: AtomicU64,
    budget: AtomicU64,
    /// Bumped whenever a journal batch carried `SECURITY_CHANGE` (review
    /// I5): AccessCheck caches of the query side are dropped.
    security_epoch: AtomicU64,
    /// FER-477: number of the last worker pass STARTED (over all volumes).
    poll_gen: AtomicU64,
    /// FER-477: a query asked for a pass now (ends the poll sleep early).
    kick: AtomicBool,
}

impl IndexShared {
    pub fn new(mode: Mode) -> Arc<IndexShared> {
        Arc::new(IndexShared {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            mode: Mutex::new(mode),
            mode_epoch: AtomicU64::new(0),
            status: Mutex::new(Vec::new()),
            served: Mutex::new(Vec::new()),
            memory: AtomicU64::new(0),
            budget: AtomicU64::new(0),
            security_epoch: AtomicU64::new(0),
            poll_gen: AtomicU64::new(0),
            kick: AtomicBool::new(false),
        })
    }

    pub fn mode(&self) -> Mode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The privacy mode changed: every index (and saved file) is dropped
    /// and rebuilt in the new scope.
    pub fn set_mode(&self, m: Mode) {
        let mut cur = self.mode.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != m {
            *cur = m;
            self.mode_epoch.fetch_add(1, Ordering::SeqCst);
            // Queries must not see an index of the old scope any more.
            self.served
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }

    /// Changes on every mode change; a query that sees it change aborts.
    pub fn epoch(&self) -> u64 {
        self.mode_epoch.load(Ordering::SeqCst)
    }

    pub fn security_epoch(&self) -> u64 {
        self.security_epoch.load(Ordering::SeqCst)
    }

    pub fn bump_security(&self) {
        self.security_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// FER-477: number of the last worker pass started. A pass numbered
    /// above the value read now starts after this call (the freshness
    /// barrier of `changes` waits for an index with such a `caught_pass`).
    pub fn poll_gen(&self) -> u64 {
        self.poll_gen.load(Ordering::SeqCst)
    }

    /// FER-477: wake the worker for a pass now (freshness barrier).
    pub fn request_poll(&self) {
        self.kick.store(true, Ordering::SeqCst);
    }

    /// Tests (also the IPC integration tests, which run no worker): run one
    /// pass in which every index in `caught` read its journal to the end.
    #[doc(hidden)]
    pub fn complete_pass_for_test(&self, caught: &[SharedIndex]) {
        let pass = self.poll_gen.fetch_add(1, Ordering::SeqCst) + 1;
        for ix in caught {
            write_index(ix).caught_pass = pass;
        }
    }

    /// Per-volume status snapshot (IPC `indexStatus`).
    pub fn status(&self) -> Vec<VolumeStatus> {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The indexes queries may read, with the mode epoch and the mode they
    /// belong to — one consistent snapshot (review M1): the `mode` lock is
    /// held across, and `set_mode` holds it while it bumps the epoch and
    /// clears the list, so a query never pairs an old list with a new
    /// epoch (its abort check would then never fire).
    pub fn served(&self) -> (Vec<ServedVolume>, u64, Mode) {
        let mode = self.mode.lock().unwrap_or_else(|e| e.into_inner());
        let list = self
            .served
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (list, self.epoch(), *mode)
    }

    /// Tests (also the IPC integration tests): serve these indexes.
    #[doc(hidden)]
    pub fn serve_for_test(&self, served: Vec<ServedVolume>) {
        *self.served.lock().unwrap_or_else(|e| e.into_inner()) = served;
    }

    /// (bytes held by all indexes, budget).
    pub fn memory(&self) -> (u64, u64) {
        (
            self.memory.load(Ordering::SeqCst),
            self.budget.load(Ordering::SeqCst),
        )
    }

    fn set_building(&self, letter: u8, done: u64, total: u64) {
        let mut cur = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cur.iter_mut().find(|c| c.letter == letter) {
            c.state = VolumeState::Building;
            c.error = VolumeErrorCode::None;
            c.progress_permille = permille(done, total);
        }
    }
}

/// FER-477: shortest pause between two worker passes when a `changes`
/// query asks for one (the freshness barrier waits for the next pass).
pub const MIN_KICKED_POLL: Duration = Duration::from_millis(200);

/// Worker timings (tests shrink them).
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub poll: Duration,
    pub discover: Duration,
    pub retry: Duration,
    pub save_every: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            poll: Duration::from_secs(1),
            discover: Duration::from_secs(300),
            retry: Duration::from_secs(300),
            save_every: Duration::from_secs(600),
        }
    }
}

enum SlotState {
    Pending,
    Ready(SharedIndex),
    Error {
        code: VolumeErrorCode,
        retry_at: Instant,
    },
}

struct Slot {
    id: VolumeId,
    /// `None` = unknown (shared mode with an unreadable profile list): the
    /// volume is not built (fail closed).
    scope: Option<Scope>,
    state: SlotState,
    /// Review L5: the previous index, still served while a rebuild caused
    /// by the JOURNAL (wrap, new ID, unindexed subtree, …) is pending.
    /// Never kept across a scope or mode change (it may hold names the new
    /// scope excludes) nor across an error.
    stale: Option<SharedIndex>,
    last_save: Option<Instant>,
    /// Rebuilds in a row (each within [`REBUILD_WINDOW`] of the previous):
    /// the second and later ones are delayed (backoff), so a journal that
    /// wraps faster than a scan cannot keep the disk busy.
    rebuilds: u32,
    last_rebuild: Option<Instant>,
    not_before: Option<Instant>,
    /// FER-503: last attempt of the periodic `open_unknown` re-prime
    /// ([`OPEN_UNKNOWN_REPRIME`] apart).
    last_reprime: Option<Instant>,
    /// FER-503: the re-priming scan in progress (a few batches per pass).
    reprime: Option<Reprime>,
}

/// Rebuilds closer together than this count as "in a row".
pub const REBUILD_WINDOW: Duration = Duration::from_secs(30 * 60);
/// Longest delay before a repeated rebuild.
pub const REBUILD_BACKOFF_MAX: Duration = Duration::from_secs(60 * 60);

/// Delay before rebuild number `n` in a row (1 = immediate).
pub fn rebuild_delay(n: u32, base: Duration) -> Duration {
    if n <= 1 {
        Duration::ZERO
    } else {
        base.saturating_mul(1u32 << (n - 2).min(16))
            .min(REBUILD_BACKOFF_MAX)
    }
}

struct WorkerCtl<'a> {
    shared: &'a IndexShared,
    epoch: u64,
    letter: Cell<u8>,
    /// FER-477: number of the pass running now.
    pass: u64,
}

impl Control for WorkerCtl<'_> {
    fn checkpoint(&self) -> bool {
        self.shared.stop.load(Ordering::SeqCst)
            || self.shared.epoch() != self.epoch
            || self.shared.paused.load(Ordering::SeqCst)
    }

    fn progress(&self, done: u64, total: u64) {
        self.shared.set_building(self.letter.get(), done, total);
    }
}

/// The worker: one thread for all volumes (sequential builds keep memory
/// and disk load low). Returns when `shared.stop` is set; saves changed
/// indexes before returning.
pub fn run(platform: &mut dyn Platform, shared: &IndexShared, t: Timings) {
    platform.enter_background();
    let mut slots: Vec<Slot> = Vec::new();
    let mut epoch = shared.epoch();
    let mut next_discovery = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) {
        // A mode change is applied even while paused (review L2): the old
        // scope's indexes leave memory and disk at once.
        if shared.epoch() != epoch {
            epoch = shared.epoch();
            slots.clear();
            platform.remove_all();
            platform.log("mode changed: indexes dropped, rebuilding");
            next_discovery = Instant::now();
            publish(shared, &slots, platform.memory_budget(), epoch);
        }
        if shared.paused.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50).min(t.poll));
            continue;
        }
        // Review L1: a profile added to ProfileList is excluded at once,
        // not at the next periodic discovery.
        if shared.mode() == Mode::Shared && platform.profiles_changed() {
            next_discovery = Instant::now();
        }
        if Instant::now() >= next_discovery {
            let complete = discover(platform, shared, &mut slots);
            next_discovery = Instant::now() + if complete { t.discover } else { t.retry };
            publish(shared, &slots, platform.memory_budget(), epoch);
        }
        // FER-477: a kick that arrives during this pass asks for the NEXT
        // one (the barrier needs a pass that started after the request).
        shared.kick.store(false, Ordering::SeqCst);
        let ctl = WorkerCtl {
            shared,
            epoch,
            letter: Cell::new(0),
            pass: shared.poll_gen.fetch_add(1, Ordering::SeqCst) + 1,
        };
        for i in 0..slots.len() {
            if ctl.checkpoint() {
                break;
            }
            ctl.letter.set(slots[i].id.letter as u8);
            if step(platform, &mut slots, i, &ctl, &t) {
                next_discovery = Instant::now();
            }
            publish(shared, &slots, platform.memory_budget(), epoch);
        }
        // One poll interval, waking early on stop / pause / mode change /
        // a query's freshness barrier (FER-477). A kick shortens the sleep
        // to at least `MIN_KICKED_POLL`, so clients asking for `changes` in
        // a loop cannot make the worker spin on the volumes.
        let start = Instant::now();
        let until = start + t.poll;
        let kick_at = start + MIN_KICKED_POLL.min(t.poll);
        while Instant::now() < until
            && !shared.stop.load(Ordering::SeqCst)
            && !shared.paused.load(Ordering::SeqCst)
            && !(shared.kick.load(Ordering::SeqCst) && Instant::now() >= kick_at)
            && shared.epoch() == epoch
        {
            std::thread::sleep(Duration::from_millis(20).min(t.poll));
        }
    }
    if shared.epoch() == epoch {
        for s in &mut slots {
            if let SlotState::Ready(live) = &s.state
                && read_index(live).dirty
            {
                save(platform, &s.id, live);
            }
        }
    } else {
        // Stopped right after a mode change: the saved indexes belong to
        // the old scope (possibly with profile content) — delete them now.
        platform.remove_all();
    }
    platform.log("index worker stopped");
}

/// Refresh the volume list and each volume's scope. Returns false when the
/// shared-mode profile list was unavailable (every volume then fails
/// closed: its in-memory index is dropped and it is not built).
fn discover(platform: &mut dyn Platform, shared: &IndexShared, slots: &mut Vec<Slot>) -> bool {
    let mode = shared.mode();
    let vols = platform.volumes();
    let profiles = if mode == Mode::Shared {
        match platform.profiles() {
            Ok(p) => Some(p),
            Err(why) => {
                platform.log(&format!(
                    "profile list unavailable ({why}): shared-mode indexing stopped (fail closed)"
                ));
                None
            }
        }
    } else {
        Some(Vec::new())
    };
    slots.retain(|s| vols.contains(&s.id));
    for v in vols {
        let scope = profiles
            .as_ref()
            .map(|p| Scope::for_volume(mode, v.letter, p));
        match slots.iter_mut().find(|s| s.id == v) {
            Some(s) if scope.is_none() => {
                s.scope = None;
                s.state = SlotState::Pending;
                s.stale = None;
                s.last_save = None;
            }
            Some(s) if s.scope != scope => {
                // Scope changed (profile added/removed): the saved index,
                // which may hold names that are now excluded, goes first.
                if !platform.remove(&v) {
                    // load_saved deletes it again (fingerprint mismatch).
                    platform.log(&format!("{}: could not delete the old index", v.letter));
                }
                platform.log(&format!("{}: scope changed, rebuilding", v.letter));
                s.scope = scope;
                s.state = SlotState::Pending;
                s.stale = None;
                s.last_save = None;
            }
            Some(_) => {}
            None => slots.push(Slot {
                id: v,
                scope,
                state: SlotState::Pending,
                stale: None,
                last_save: None,
                rebuilds: 0,
                last_rebuild: None,
                not_before: None,
                last_reprime: None,
                reprime: None,
            }),
        }
    }
    slots.sort_by_key(|s| s.id.letter);
    profiles.is_some()
}

/// Bytes held by a slot: its index plus a stale one kept for queries.
fn slot_memory(s: &Slot) -> u64 {
    let ready = match &s.state {
        SlotState::Ready(l) => read_index(l).index.memory_bytes(),
        _ => 0,
    };
    ready
        + s.stale
            .as_ref()
            .map_or(0, |l| read_index(l).index.memory_bytes())
}

/// Memory of every OTHER slot (a stale index of slot `except` is not
/// counted: during its rebuild the volume may briefly hold old + new, at
/// most one extra volume index above the budget — documented in §8.7).
fn other_memory(slots: &[Slot], except: usize) -> u64 {
    slots
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != except)
        .map(|(_, s)| slot_memory(s))
        .sum()
}

fn save(platform: &mut dyn Platform, id: &VolumeId, live: &SharedIndex) {
    let bytes = {
        let g = read_index(live);
        let Some(h) = g.header(id.serial) else {
            return; // snapshot without a journal: nothing to resume from
        };
        persist::encode(&g.index, &h)
    };
    match platform.save(id, &bytes) {
        // Only this worker thread writes the index, so nothing changed
        // between the encode and here.
        Ok(()) => write_index(live).dirty = false,
        Err(e) => platform.log(&format!("{}: save failed: {e}", id.letter)),
    }
}

/// One volume's turn. Returns true when a rediscovery is due now (the
/// profile list changed under a shared-mode index, review L1).
fn step(
    platform: &mut dyn Platform,
    slots: &mut [Slot],
    i: usize,
    ctl: &WorkerCtl<'_>,
    t: &Timings,
) -> bool {
    let budget = platform
        .memory_budget()
        .saturating_sub(other_memory(slots, i));
    let slot = &mut slots[i];
    if let SlotState::Error { retry_at, .. } = slot.state {
        if Instant::now() < retry_at {
            return false;
        }
        slot.state = SlotState::Pending;
    }
    if slot.not_before.is_some_and(|at| Instant::now() < at) {
        return false;
    }
    let Some(scope) = slot.scope.clone() else {
        // Fail closed: nothing is opened, read, saved or served.
        slot.stale = None;
        slot.state = SlotState::Error {
            code: VolumeErrorCode::ProfilesUnavailable,
            retry_at: Instant::now() + t.retry,
        };
        return false;
    };
    if scope.whole_volume {
        // Mounted inside a profile: never indexed in shared mode.
        platform.remove(&slot.id);
        slot.stale = None;
        slot.state = SlotState::Error {
            code: VolumeErrorCode::PrivateVolume,
            retry_at: Instant::now() + t.retry,
        };
        return false;
    }
    // The volume is opened per step and closed after it: no long-lived
    // handle that would block a volume lock (chkdsk, dismount).
    let mut backend = match platform.open(&slot.id) {
        Ok(b) => b,
        Err(code) => {
            fail(platform, slot, code, t);
            return false;
        }
    };
    let current = match std::mem::replace(&mut slot.state, SlotState::Pending) {
        SlotState::Ready(l) => Some(l),
        _ => None,
    };
    let live = match current {
        Some(l) => l,
        None => {
            let fresh = match load_saved(platform, &slot.id, &scope, backend.as_mut(), budget) {
                Some(l) => l,
                None => {
                    ctl.progress(0, 0);
                    match build(backend.as_mut(), &scope, budget, ctl) {
                        Ok(l) => {
                            platform.log(&format!(
                                "{}: built from MFT: {} entries, {} KiB",
                                slot.id.letter,
                                l.index.len(),
                                l.index.memory_bytes() / 1024
                            ));
                            l
                        }
                        Err(BuildError::Abandoned) => return false,
                        Err(BuildError::Failed(code)) => {
                            fail(platform, slot, code, t);
                            return false;
                        }
                    }
                }
            };
            // The new index replaces the stale one (review L5). The journal
            // was not followed while it was missing or stale: SECURITY_CHANGE
            // may have been missed, so cached AccessChecks go (review L5 S3).
            let mut fresh = fresh;
            prime_open_writes(backend.as_mut(), &mut fresh, ctl);
            slot.stale = None;
            slot.last_reprime = None;
            slot.reprime = None;
            ctl.shared.bump_security();
            Arc::new(RwLock::new(fresh))
        }
    };
    // FER-503: a live index can sit in `open_unknown` — its priming scan at
    // build/load failed (the service was paused during the first journal
    // scan, the journal was longer than the scan budget, more sessions open
    // than the cap, or a session without CLOSE after a crash), or more
    // sessions than the cap opened while it followed the journal. While it
    // lasts, incremental sync is off for every client of the volume (every
    // `changes` answer is rescan — safe, only slower). Retry the scan at a
    // low rate (the first retry at once, then every OPEN_UNKNOWN_REPRIME):
    // a completed one recovers the volume when it may (see `Reprime`), a
    // failed one keeps the flag until the next interval. The scan runs
    // REPRIME_ROUNDS_PER_STEP reads per pass, so the other volumes keep
    // following their journals in between.
    if slot.reprime.is_none()
        && slot
            .last_reprime
            .is_none_or(|at| at.elapsed() >= OPEN_UNKNOWN_REPRIME)
        && read_index(&live).changes.open_unknown()
    {
        slot.last_reprime = Some(Instant::now());
        match reprime_begin(backend.as_mut(), &live) {
            Ok(scan) => slot.reprime = Some(scan),
            Err(ReprimeSkip::OutOfJournal) => platform.log(&format!(
                "{}: an untracked open write session left the journal; \
                 incremental sync stays off until the index is rebuilt or reloaded",
                slot.id.letter
            )),
            Err(ReprimeSkip::Unavailable) => {}
        }
    }
    if let Some(scan) = slot.reprime.as_mut() {
        match reprime_advance(backend.as_mut(), &live, scan, ctl, REPRIME_ROUNDS_PER_STEP) {
            ReprimeProgress::Running => {}
            ReprimeProgress::Completed { recovered } => {
                slot.reprime = None;
                if recovered {
                    platform.log(&format!(
                        "{}: open write sessions known again, incremental sync available",
                        slot.id.letter
                    ));
                }
            }
            ReprimeProgress::Failed => slot.reprime = None,
        }
    }
    let (errors_before, security_before) = {
        let g = read_index(&live);
        (g.stats.read_errors, g.security_changes)
    };
    // Short write-lock holds: queries interleave with a long backlog.
    let mut rounds = 0;
    let caught = loop {
        let (r, exhausted) = {
            let mut g = write_index(&live);
            catch_up_rounds(backend.as_mut(), &mut g, budget, ctl, LOCK_ROUNDS)
        };
        rounds += LOCK_ROUNDS;
        if !exhausted || rounds >= MAX_ROUNDS {
            break r;
        }
    };
    let (read_errors, first_error, security_after, dirty) = {
        let g = read_index(&live);
        (
            g.stats.read_errors,
            g.stats.first_read_error,
            g.security_changes,
            g.dirty,
        )
    };
    if read_errors > errors_before {
        platform.log(&format!(
            "{}: {} file record reads failed so far (first OS error {})",
            slot.id.letter,
            read_errors,
            first_error.unwrap_or(0)
        ));
    }
    if security_after != security_before {
        ctl.shared.bump_security();
    }
    match caught {
        // FER-477: this pass read the journal to its end.
        Ok(CatchUp::UpToDate) => write_index(&live).caught_pass = ctl.pass,
        Ok(CatchUp::Interrupted) => {}
        Ok(CatchUp::Rebuild(why)) => {
            let in_a_row = slot
                .last_rebuild
                .is_some_and(|at| at.elapsed() < REBUILD_WINDOW);
            slot.rebuilds = if in_a_row { slot.rebuilds + 1 } else { 1 };
            slot.last_rebuild = Some(Instant::now());
            let delay = rebuild_delay(slot.rebuilds, t.retry);
            slot.not_before = Some(Instant::now() + delay);
            platform.log(&format!(
                "{}: rebuild from MFT ({why:?}) in {}s; previous index served meanwhile",
                slot.id.letter,
                delay.as_secs()
            ));
            platform.remove(&slot.id);
            slot.last_save = None;
            // Review L5: the scope did not change (journal reasons only),
            // so the previous index stays searchable until the new one is
            // built — a user who keeps forcing rebuilds no longer takes the
            // feature away from everyone.
            slot.stale = Some(live);
            return false; // state stays Pending: a later step rebuilds
        }
        Err(code) => {
            fail(platform, slot, code, t);
            return false;
        }
    }
    let due = slot.last_save.is_none_or(|at| at.elapsed() >= t.save_every);
    // Never save an index of the previous scope after a mode change (the
    // worker drops everything on its next iteration).
    let current = ctl.shared.epoch() == ctl.epoch;
    let mut rediscover = false;
    if dirty && due && current {
        // Review L1: re-read the profile list right before writing to disk;
        // a profile created since the last discovery must never be saved.
        let scope_current = scope.mode != Mode::Shared
            || platform
                .profiles()
                .is_ok_and(|p| Scope::for_volume(Mode::Shared, slot.id.letter, &p) == scope);
        if scope_current {
            save(platform, &slot.id, &live);
            slot.last_save = Some(Instant::now());
        } else {
            platform.log(&format!(
                "{}: profile list changed; save skipped, rediscovering",
                slot.id.letter
            ));
            rediscover = true;
        }
    }
    slot.state = SlotState::Ready(live);
    rediscover
}

fn fail(platform: &mut dyn Platform, slot: &mut Slot, code: VolumeErrorCode, t: &Timings) {
    platform.log(&format!("{}: error {}", slot.id.letter, code.as_str()));
    slot.stale = None;
    slot.state = SlotState::Error {
        code,
        retry_at: Instant::now() + t.retry,
    };
}

/// Use the saved index when it is valid for this volume, scope and journal.
/// Anything else deletes it (it is rebuilt and saved again).
fn load_saved(
    platform: &mut dyn Platform,
    id: &VolumeId,
    scope: &Scope,
    backend: &mut dyn VolumeBackend,
    budget: u64,
) -> Option<LiveIndex> {
    let bytes = platform.load(id)?;
    let why = match persist::decode(&bytes, budget) {
        Err(e) => format!("unusable ({e})"),
        Ok((h, _)) if h.volume_serial != id.serial => "other volume".into(),
        Ok((h, _)) if h.scope != scope.fingerprint() => "scope changed".into(),
        Ok((h, ix)) => match backend.query_journal() {
            Ok(j) if j.id != h.journal_id => "journal ID changed".into(),
            Ok(j) if h.next_usn < j.first_usn || h.next_usn > j.next_usn => {
                "journal wrapped".into()
            }
            Ok(_) => {
                platform.log(&format!(
                    "{}: saved index loaded, {} entries; replaying journal",
                    id.letter,
                    ix.len()
                ));
                return Some(LiveIndex::from_saved(ix, scope.clone(), &h));
            }
            Err(_) => "journal unavailable".into(),
        },
    };
    platform.log(&format!(
        "{}: saved index not used: {why}; rebuilding",
        id.letter
    ));
    platform.remove(id);
    None
}

fn publish(shared: &IndexShared, slots: &[Slot], budget: u64, epoch: u64) {
    let prev = shared.status();
    let mut total = 0u64;
    let mut served = Vec::with_capacity(slots.len());
    let list: Vec<VolumeStatus> = slots
        .iter()
        .map(|s| {
            let letter = s.id.letter as u8;
            let mut st = VolumeStatus {
                letter,
                state: VolumeState::Building,
                error: VolumeErrorCode::None,
                entries: 0,
                memory_bytes: 0,
                journal_id: 0,
                next_usn: 0,
                live: false,
                progress_permille: 0,
            };
            match &s.state {
                SlotState::Pending => {
                    // Keep the last build progress the worker reported.
                    if let Some(p) = prev.iter().find(|p| p.letter == letter) {
                        st.progress_permille = p.progress_permille.min(999);
                    }
                    served.push(ServedVolume {
                        letter,
                        index: s.stale.clone(),
                        stale: s.stale.is_some(),
                    });
                }
                SlotState::Ready(l) => {
                    let l = read_index(l);
                    st.state = VolumeState::Ready;
                    st.entries = l.index.len() as u64;
                    st.memory_bytes = l.index.memory_bytes();
                    st.journal_id = l.journal_id.unwrap_or(0);
                    st.next_usn = l.next_usn;
                    st.live = l.journal_id.is_some();
                    st.progress_permille = 1000;
                }
                SlotState::Error { code, .. } => {
                    st.state = VolumeState::Error;
                    st.error = *code;
                    served.push(ServedVolume {
                        letter,
                        index: None,
                        stale: false,
                    });
                }
            }
            if let SlotState::Ready(l) = &s.state {
                served.push(ServedVolume {
                    letter,
                    index: Some(l.clone()),
                    stale: false,
                });
            }
            total += slot_memory(s);
            st
        })
        .collect();
    *shared.status.lock().unwrap_or_else(|e| e.into_inner()) = list;
    {
        // Never publish indexes of a scope the mode has already left:
        // `set_mode` bumps the epoch BEFORE it clears this list, and the
        // epoch is checked while the list is locked.
        let mut cur = shared.served.lock().unwrap_or_else(|e| e.into_inner());
        if shared.epoch() == epoch {
            *cur = served;
        }
    }
    shared.memory.store(total, Ordering::SeqCst);
    shared.budget.store(budget, Ordering::SeqCst);
}

fn permille(done: u64, total: u64) -> u16 {
    if total == 0 {
        0
    } else {
        ((done.min(total) as u128 * 1000 / total as u128) as u16).min(999)
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
