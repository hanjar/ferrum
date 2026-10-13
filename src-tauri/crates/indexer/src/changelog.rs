//! Per-volume log of changed directories (FER-477, incremental sync).
//!
//! The worker records, for every journal batch it applies, the directories
//! whose LISTING may have changed: the parents of a refreshed record's old
//! and new names, and the parent the journal record itself names. A
//! directory whose own identity changed (created, renamed, moved, deleted,
//! record reused) is recorded `recursive`: everything below it counts as
//! changed (a different folder with the same name may now sit there).
//!
//! The log answers "which directories changed since USN `s`" only when it
//! is complete for `s`: every change with a USN `>= floor` is in it. The
//! floor starts at the journal position the index was built or loaded at
//! (changes before it were never seen) and rises when old directories are
//! evicted. Anything below the floor is `None` — the caller must rescan;
//! the log never answers "nothing changed" for a window it did not see.
//!
//! Memory is bounded: at most [`MAX_LOGGED_DIRS`] directories (one slot per
//! directory, holding its latest change), ≈ 80 bytes each.
//!
//! Review P2-3: NTFS writes a journal record only when a reason flag is
//! NEW for an open file; further writes through the same handle are silent
//! until `CLOSE`. A file written through a handle that stays open across a
//! mark (a database, a mounted container) would look unchanged. The log
//! therefore also keeps every OPEN WRITE SESSION (a data reason seen, no
//! `CLOSE` yet) and the query reports its parents as changed on every
//! answer until it closes, whatever the window. Sessions that started
//! before the index (the journal is scanned from its first record when an
//! index starts, `engine::prime_open_writes`) are found the same way; when
//! that scan cannot finish, or too many sessions are open, the log is
//! `open_unknown` and every answer is "rescan". FER-503: the engine retries
//! that scan on a live index (`engine::reprime_open_writes`); a scan that
//! completes without overflowing the session cap again clears the flag —
//! but only when it started at or below the lowest USN of every session the
//! log could not see since the flag was set (`lowest_dropped_usn`: a session
//! dropped for the cap, or the point an incomplete priming scan stopped
//! at). Once that USN has fallen out of the journal no scan can find the
//! session again, and the flag stays until the index is rebuilt or
//! reloaded. Sessions that opened before the journal's first record when
//! the index STARTED are invisible either way (documented residual gap).
//!
//! FER-503 review 2: the re-priming scan runs across worker passes while
//! the live journal follow keeps calling [`ChangeLog::note_write`]. Its
//! replay therefore never touches `open` directly: it collects sessions in
//! a separate [`Replay`], and [`ChangeLog::finish_reprime`] merges them —
//! a replayed session is added unless the follow saw its record `CLOSE`
//! (at a USN at or above the scan's target) while the scan ran, and an old
//! replayed `CLOSE` never removes a session the follow tracks.

use crate::usn::{UsnChange, reason};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Directories one volume's log keeps (oldest evicted first).
pub const MAX_LOGGED_DIRS: usize = 32 * 1024;
/// Open write sessions tracked per volume (more: `open_unknown`).
pub const MAX_OPEN_WRITES: usize = 4096;
/// Parents remembered per open session (hardlinks / renames while open).
const MAX_SESSION_PARENTS: usize = 8;
/// FER-503 review 2: records whose `CLOSE` the follow may remember while a
/// re-priming scan runs (more: that scan cannot clear the flag).
pub const MAX_FOLLOW_CLOSES: usize = 64 * 1024;

/// Reasons that start a write session (any data change of any stream).
pub const WRITE_REASONS: u32 = reason::DATA_OVERWRITE
    | reason::DATA_EXTEND
    | reason::DATA_TRUNCATION
    | reason::NAMED_DATA_OVERWRITE
    | reason::NAMED_DATA_EXTEND
    | reason::NAMED_DATA_TRUNCATION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Logged {
    /// Latest change of the directory's listing.
    usn: i64,
    /// Latest change of the directory itself (recursive), if any.
    recursive: Option<i64>,
}

impl Logged {
    fn key(&self) -> i64 {
        self.usn.max(self.recursive.unwrap_or(i64::MIN))
    }
}

#[derive(Debug, Clone)]
pub struct ChangeLog {
    floor: i64,
    cap: usize,
    by_dir: HashMap<u64, Logged>,
    /// (latest usn, frn) for eviction and range queries.
    order: BTreeSet<(i64, u64)>,
    /// Review P2-3: record number → parents named by its journal records,
    /// for files with a data change and no `CLOSE` yet.
    open: HashMap<u64, Vec<u64>>,
    /// Open sessions are not completely known (priming scan incomplete, or
    /// more than [`MAX_OPEN_WRITES`]): answers must say rescan.
    open_unknown: bool,
    /// FER-503: how often [`Self::note_write`] dropped a session for the
    /// cap — lets a re-priming scan tell an overflow OF ITS OWN window
    /// apart from one that happened before it.
    open_overflows: u64,
    /// FER-503 review P2: lowest USN of a session the log could not see
    /// since `open_unknown` was last set (dropped for the cap, or not
    /// reached by an incomplete priming scan). A scan clears the flag only
    /// when it started at or below it. `None` with the flag set: the flag
    /// has no known USN (journal unreadable at priming) — any completed
    /// scan from the journal's first record clears it.
    lowest_dropped_usn: Option<i64>,
    /// FER-503 review 2: while a re-priming scan runs, the records whose
    /// `CLOSE` the journal follow saw (all at or above the scan's target).
    follow_closes: Option<HashSet<u64>>,
    /// More than [`MAX_FOLLOW_CLOSES`] of them: the running scan cannot
    /// tell its replayed sessions apart from closed ones.
    follow_closes_full: bool,
}

/// FER-503 review 2: the open sessions a re-priming scan replays from the
/// journal (records below its target), kept apart from the live log.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    /// record → (USN of the record that opened the session, parents).
    open: HashMap<u64, (i64, Vec<u64>)>,
    /// Lowest USN of a session dropped for the cap.
    dropped: Option<i64>,
}

impl Replay {
    /// Same rules as [`ChangeLog::note_write`], on the replay's own map.
    pub fn note(&mut self, c: &UsnChange) {
        let rec = c.frn & crate::index::RECORD_MASK;
        if c.reason & reason::CLOSE != 0 {
            self.open.remove(&rec);
            return;
        }
        if c.reason & WRITE_REASONS == 0 {
            return;
        }
        if !self.open.contains_key(&rec) && self.open.len() >= MAX_OPEN_WRITES {
            self.dropped = Some(self.dropped.map_or(c.usn, |d| d.min(c.usn)));
            return;
        }
        let (_, parents) = self.open.entry(rec).or_insert((c.usn, Vec::new()));
        add_parent(parents, c.parent);
    }
}

fn add_parent(parents: &mut Vec<u64>, parent: u64) {
    if parent != 0 && !parents.contains(&parent) && parents.len() < MAX_SESSION_PARENTS {
        parents.push(parent);
    }
}

impl ChangeLog {
    /// An empty log, complete from `floor` on.
    pub fn new(floor: i64) -> ChangeLog {
        Self::with_cap(floor, MAX_LOGGED_DIRS)
    }

    pub fn with_cap(floor: i64, cap: usize) -> ChangeLog {
        ChangeLog {
            floor,
            cap: cap.max(1),
            by_dir: HashMap::new(),
            order: BTreeSet::new(),
            open: HashMap::new(),
            open_unknown: false,
            open_overflows: 0,
            lowest_dropped_usn: None,
            follow_closes: None,
            follow_closes_full: false,
        }
    }

    /// Lowest USN from which the log is complete.
    pub fn floor(&self) -> i64 {
        self.floor
    }

    pub fn len(&self) -> usize {
        self.by_dir.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_dir.is_empty()
    }

    /// Directory `frn` changed at `usn` (`recursive`: the directory itself
    /// was created, renamed, moved, deleted or reused).
    pub fn record(&mut self, frn: u64, recursive: bool, usn: i64) {
        if frn == 0 {
            return;
        }
        let old = self.by_dir.get(&frn).copied();
        let mut cur = old.unwrap_or(Logged {
            usn: i64::MIN,
            recursive: None,
        });
        cur.usn = cur.usn.max(usn);
        if recursive {
            cur.recursive = Some(cur.recursive.map_or(usn, |r| r.max(usn)));
        }
        if let Some(o) = old {
            self.order.remove(&(o.key(), frn));
        }
        self.order.insert((cur.key(), frn));
        self.by_dir.insert(frn, cur);
        while self.by_dir.len() > self.cap {
            let Some((usn, victim)) = self.order.pop_first() else {
                break;
            };
            self.by_dir.remove(&victim);
            // Changes up to `usn` are no longer all known.
            self.floor = self.floor.max(usn.saturating_add(1));
        }
    }

    /// Review P2-2: was directory `frn` itself replaced (created, renamed,
    /// moved, deleted, reused) at or after `since`? Used for the requested
    /// directory and every directory above it.
    pub fn recursive_since(&self, frn: u64, since: i64) -> bool {
        self.by_dir
            .get(&frn)
            .and_then(|l| l.recursive)
            .is_some_and(|r| r >= since)
    }

    /// Review P2-3: follow write sessions through one journal record (in
    /// journal order).
    pub fn note_write(&mut self, c: &UsnChange) {
        let rec = c.frn & crate::index::RECORD_MASK;
        if c.reason & reason::CLOSE != 0 {
            self.open.remove(&rec);
            if let Some(closes) = self.follow_closes.as_mut()
                && !closes.contains(&rec)
            {
                if closes.len() < MAX_FOLLOW_CLOSES {
                    closes.insert(rec);
                } else {
                    self.follow_closes_full = true;
                }
            }
            return;
        }
        if c.reason & WRITE_REASONS == 0 {
            return;
        }
        if !self.open.contains_key(&rec) && self.open.len() >= MAX_OPEN_WRITES {
            self.overflow(c.usn);
            return;
        }
        add_parent(self.open.entry(rec).or_default(), c.parent);
    }

    /// A session (opened at `usn`) was dropped for the cap.
    fn overflow(&mut self, usn: i64) {
        self.open_unknown = true;
        self.open_overflows += 1;
        self.note_dropped(usn);
    }

    /// FER-503 review 2: a re-priming scan starts; from now on the follow
    /// remembers every record it sees `CLOSE` (see [`Self::finish_reprime`]).
    pub fn begin_reprime(&mut self) {
        self.follow_closes = Some(HashSet::new());
        self.follow_closes_full = false;
    }

    /// The running re-priming scan was abandoned.
    pub fn abort_reprime(&mut self) {
        self.follow_closes = None;
        self.follow_closes_full = false;
    }

    /// FER-503 review 2: merge a COMPLETED re-priming scan (see
    /// [`Self::begin_reprime`]) into the log, then try to clear the flag
    /// ([`Self::clear_open_unknown`]). A replayed session goes into `open`
    /// unless the follow saw its record `CLOSE` while the scan ran (that
    /// close is later than everything replayed; a session reopened after
    /// it is the follow's own entry, which the replay never removes). A
    /// merge beyond the cap, or a replay that had to drop sessions, is an
    /// overflow (flag stays, `lowest_dropped_usn` updated). When the follow
    /// saw more closes than it could remember, nothing is merged (it might
    /// leave entries of closed sessions behind for good) and the flag stays.
    /// Returns true when the flag was cleared.
    pub fn finish_reprime(
        &mut self,
        replay: Replay,
        scan_from: i64,
        overflows_before: u64,
    ) -> bool {
        let closes = self.follow_closes.take();
        let full = std::mem::take(&mut self.follow_closes_full);
        let Some(closes) = closes else {
            return false; // no scan was begun
        };
        if full {
            return false;
        }
        if let Some(d) = replay.dropped {
            self.overflow(d);
        }
        let mut sessions: Vec<(u64, (i64, Vec<u64>))> = replay.open.into_iter().collect();
        sessions.sort_by_key(|(rec, (usn, _))| (*usn, *rec));
        for (rec, (usn, parents)) in sessions {
            if closes.contains(&rec) {
                continue; // closed after the scan's target
            }
            if let Some(live) = self.open.get_mut(&rec) {
                for p in parents {
                    add_parent(live, p);
                }
            } else if self.open.len() < MAX_OPEN_WRITES {
                self.open.insert(rec, parents);
            } else {
                self.overflow(usn);
            }
        }
        self.clear_open_unknown(scan_from, overflows_before)
    }

    /// Open write sessions: (record number, parents from the journal).
    pub fn open_writes(&self) -> impl Iterator<Item = (u64, &[u64])> {
        self.open.iter().map(|(r, p)| (*r, p.as_slice()))
    }

    pub fn open_unknown(&self) -> bool {
        self.open_unknown
    }

    /// The open sessions cannot be known completely (see the module doc).
    pub fn set_open_unknown(&mut self) {
        self.open_unknown = true;
    }

    fn note_dropped(&mut self, usn: i64) {
        self.lowest_dropped_usn = Some(self.lowest_dropped_usn.map_or(usn, |d| d.min(usn)));
    }

    /// FER-503 review P2: sessions with records from `usn` on were not
    /// seen (a priming scan stopped there): the log is `open_unknown`, and
    /// only a scan starting at or below `usn` may clear it.
    pub fn set_open_unknown_from(&mut self, usn: i64) {
        self.open_unknown = true;
        self.note_dropped(usn);
    }

    /// Lowest USN of a session the log could not see since the flag was
    /// last set (see the field).
    pub fn lowest_dropped_usn(&self) -> Option<i64> {
        self.lowest_dropped_usn
    }

    /// FER-503: a re-priming scan read the journal COMPLETELY from
    /// `scan_from` up to the index position; `overflows_before` is
    /// [`Self::open_overflows`] when it started. The open sessions are
    /// known again — and the flag is cleared, returning true — only when
    /// the scan overflowed nothing (no drop since it started, by this scan
    /// or by the live journal follow) AND it started at or below every USN
    /// the log could not see: then every unseen session's record was in
    /// the scan's window and is tracked in `open` now. Otherwise the flag
    /// stays (a dropped session whose record has left the journal stays
    /// unknown until the index is rebuilt or reloaded).
    pub fn clear_open_unknown(&mut self, scan_from: i64, overflows_before: u64) -> bool {
        if self.open_overflows != overflows_before
            || self.lowest_dropped_usn.is_some_and(|d| scan_from > d)
        {
            return false;
        }
        self.open_unknown = false;
        self.lowest_dropped_usn = None;
        true
    }

    /// How often [`Self::note_write`] dropped a session for the cap (a
    /// monotonous count, never reset).
    pub fn open_overflows(&self) -> u64 {
        self.open_overflows
    }

    /// Directories changed at or after `since`, with their recursive flag
    /// for that window. `None` = the log is not complete for `since`.
    pub fn since(&self, since: i64) -> Option<Vec<(u64, bool)>> {
        if since < self.floor {
            return None;
        }
        Some(
            self.order
                .range((since, 0)..)
                .filter_map(|(_, frn)| {
                    let l = self.by_dir.get(frn)?;
                    Some((*frn, l.recursive.is_some_and(|r| r >= since)))
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<(u64, bool)>) -> Vec<(u64, bool)> {
        v.sort();
        v
    }

    #[test]
    fn window_floor_and_recursive_flag() {
        let mut l = ChangeLog::new(100);
        assert_eq!(l.since(99), None, "before the floor: unknown");
        assert_eq!(l.since(100), Some(vec![]));
        l.record(7, false, 110);
        l.record(8, true, 120);
        l.record(7, false, 130);
        l.record(0, true, 140); // no parent: ignored
        assert_eq!(sorted(l.since(100).unwrap()), vec![(7, false), (8, true)]);
        assert_eq!(sorted(l.since(121).unwrap()), vec![(7, false)]);
        assert_eq!(l.since(131).unwrap(), vec![]);
        // A newer plain change keeps an older recursive one out of a later
        // window, but inside it.
        l.record(8, false, 150);
        assert_eq!(sorted(l.since(121).unwrap()), vec![(7, false), (8, false)]);
        assert_eq!(sorted(l.since(115).unwrap()), vec![(7, false), (8, true)]);
        assert_eq!(l.len(), 2);
    }

    fn ch(frn: u64, parent: u64, reason: u32) -> UsnChange {
        ch_at(1, frn, parent, reason)
    }

    fn ch_at(usn: i64, frn: u64, parent: u64, reason: u32) -> UsnChange {
        UsnChange {
            usn,
            frn,
            parent,
            reason,
        }
    }

    #[test]
    fn write_sessions_open_on_data_and_close_on_close() {
        let mut l = ChangeLog::new(0);
        l.note_write(&ch(7 | (1 << 48), 30, reason::DATA_OVERWRITE));
        l.note_write(&ch(7 | (2 << 48), 31, reason::DATA_EXTEND)); // same record
        l.note_write(&ch(8, 30, reason::BASIC_INFO_CHANGE)); // not a write
        let open: Vec<(u64, Vec<u64>)> = l.open_writes().map(|(r, p)| (r, p.to_vec())).collect();
        assert_eq!(open, vec![(7, vec![30, 31])]);
        l.note_write(&ch(7, 30, reason::DATA_OVERWRITE | reason::CLOSE));
        assert_eq!(l.open_writes().count(), 0);
        assert!(!l.open_unknown());
    }

    #[test]
    fn too_many_open_sessions_make_the_log_unknown() {
        let mut l = ChangeLog::new(0);
        for r in 0..MAX_OPEN_WRITES as u64 {
            l.note_write(&ch(100 + r, 5, reason::DATA_OVERWRITE));
        }
        assert!(!l.open_unknown());
        l.note_write(&ch(1, 5, reason::DATA_OVERWRITE));
        assert!(l.open_unknown(), "sticky: a session was not tracked");
        // FER-503: the overflow is counted so a re-priming scan can tell an
        // overflow of its own window apart from a previous one.
        assert_eq!(l.open_overflows(), 1);
        // Clearing is possible (a completed clean scan from at or below the
        // dropped session does it), and a further overflow sets the flag
        // again and counts.
        assert!(l.clear_open_unknown(1, 1));
        assert!(!l.open_unknown());
        l.note_write(&ch(2, 5, reason::DATA_OVERWRITE)); // still full
        assert!(l.open_unknown());
        assert_eq!(l.open_overflows(), 2);
    }

    /// FER-503 review P2: the flag is cleared only by a scan that started
    /// at or below the lowest dropped session and saw no new drop.
    #[test]
    fn clearing_needs_a_scan_covering_every_dropped_session() {
        let mut l = ChangeLog::new(0);
        for r in 0..MAX_OPEN_WRITES as u64 {
            l.note_write(&ch_at(10, 100 + r, 5, reason::DATA_OVERWRITE));
        }
        l.note_write(&ch_at(500, 1, 5, reason::DATA_OVERWRITE));
        l.note_write(&ch_at(300, 2, 5, reason::DATA_OVERWRITE));
        assert_eq!(l.lowest_dropped_usn(), Some(300));
        let n = l.open_overflows();
        assert!(!l.clear_open_unknown(301, n), "a dropped session is below");
        assert!(l.open_unknown());
        assert!(!l.clear_open_unknown(300, n - 1), "a drop since the scan");
        assert!(l.open_unknown());
        assert!(l.clear_open_unknown(300, n));
        assert!(!l.open_unknown());
        assert_eq!(l.lowest_dropped_usn(), None, "reset with the flag");
        // An incomplete priming scan names its stop point the same way.
        l.set_open_unknown_from(700);
        assert!(!l.clear_open_unknown(701, n));
        assert!(l.clear_open_unknown(650, n));
        // A flag without a USN (journal unreadable): any completed scan.
        l.set_open_unknown();
        assert!(l.clear_open_unknown(i64::MAX, n));
    }

    /// FER-503 review 2: the replay never removes a session the follow
    /// tracks, and never re-adds one the follow saw close.
    #[test]
    fn finish_reprime_merges_without_undoing_the_follow() {
        let mut l = ChangeLog::new(0);
        l.set_open_unknown();
        let n = l.open_overflows();
        l.begin_reprime();
        let mut r = Replay::default();
        // 22: opened and closed below the target; reopened by the follow.
        r.note(&ch_at(10, 22, 5, reason::DATA_OVERWRITE));
        l.note_write(&ch_at(100, 22, 5, reason::DATA_OVERWRITE));
        r.note(&ch_at(20, 22, 5, reason::CLOSE));
        // 23: opened below the target, closed by the follow.
        r.note(&ch_at(30, 23, 6, reason::DATA_EXTEND));
        l.note_write(&ch_at(110, 23, 6, reason::CLOSE));
        // 24: still open.
        r.note(&ch_at(40, 24, 7, reason::DATA_EXTEND));
        assert!(l.finish_reprime(r, 0, n));
        let mut open: Vec<u64> = l.open_writes().map(|(r, _)| r).collect();
        open.sort();
        assert_eq!(open, vec![22, 24]);
        assert!(!l.open_unknown());
        // Without a begun scan nothing is merged or cleared.
        l.set_open_unknown();
        assert!(!l.finish_reprime(Replay::default(), 0, l.open_overflows()));
        assert!(l.open_unknown());
    }

    #[test]
    fn recursive_since_is_per_directory_and_window() {
        let mut l = ChangeLog::new(0);
        l.record(5, false, 10);
        l.record(6, true, 20);
        assert!(!l.recursive_since(5, 0));
        assert!(l.recursive_since(6, 20));
        assert!(!l.recursive_since(6, 21));
        assert!(!l.recursive_since(9, 0));
    }

    #[test]
    fn eviction_raises_the_floor() {
        let mut l = ChangeLog::with_cap(0, 2);
        l.record(1, false, 10);
        l.record(2, false, 20);
        l.record(3, false, 30);
        assert_eq!(l.len(), 2);
        assert_eq!(l.floor(), 11);
        assert_eq!(l.since(10), None, "dir 1's change was evicted");
        assert_eq!(sorted(l.since(11).unwrap()), vec![(2, false), (3, false)]);
        // Re-recording a kept dir moves it to the young end.
        l.record(2, false, 40);
        l.record(4, false, 50);
        assert_eq!(l.floor(), 31);
        assert_eq!(sorted(l.since(31).unwrap()), vec![(2, false), (4, false)]);
    }
}
