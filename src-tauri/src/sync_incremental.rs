//! FER-477b: incremental sync from the FerrumIndexer change log.
//!
//! A two-way sync's settle compare reads the CONTENT of every file pair that
//! matches on size, which dominates the cost of a large, mostly unchanged
//! tree. When the stored baseline carries journal marks (one per side) and
//! the service can say exactly which directories changed since them, a file
//! pair may skip that content read — and only when ALL of these hold:
//!
//! 1. both sides answered `complete` (same journal, history kept since the
//!    mark, no excluded area inside, not too many changes, index current —
//!    the service waited for its freshness barrier);
//! 2. the file's directory did not change on either side, and no directory
//!    above it was created, renamed, moved or replaced (`recursive`);
//! 3. the baseline has an entry for the file whose left AND right facts
//!    (size, mtime) are exactly the current ones.
//!
//! The baseline only gets marks after a settle compare that was clean in
//! full (every entry `same`, no error, nothing truncated or cancelled, every
//! entry recorded with fresh facts — `reconcileBaseline` in TS), so its
//! entries were verified by content at the mark. Anything else — no service,
//! old service, another journal, a rescan reason, a failed query — means the
//! ordinary compare runs unchanged. Directory listings are always read from
//! disk; only content reads are skipped.
//!
//! Marks are taken BEFORE the compare walks (the service answers after its
//! barrier), so a change during the walk is after the mark and shows up next
//! time. Journal IDs and USNs cross the IPC as decimal strings (u64 does not
//! fit a JS number).

use crate::compare::SideInfo;
use crate::sync_baseline::{BaselineDto, EntryDto, LoadResult, RootPair, SyncBaselineStore};
use ferrum_indexer::query::{ChangeStatus, ChangedDir, Changes, JournalMark};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// One side's journal position, as stored in the baseline, with the
/// identity of the root it was taken for (review P2-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalMarkDto {
    pub journal_id: String,
    pub usn: String,
    /// File reference (with sequence) of the root directory at the mark.
    pub root_frn: String,
    /// Canonical drive path of the root at the mark (`X:\a\b`, junctions
    /// resolved). Another one later (a junction retargeted) = rescan.
    pub path: String,
}

/// Both sides' marks (both or none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalMarksDto {
    pub left: JournalMarkDto,
    pub right: JournalMarkDto,
    /// When the marks were taken (ms since the Unix epoch, Ferrum's clock).
    /// Older than [`MARKS_MAX_AGE_MS`] = the next compare reads all content.
    pub taken_at_ms: u64,
}

/// Marks expire after 7 days: at least one full content verification a
/// week, whatever the journal says (security review FER-477).
pub const MARKS_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// A mark "from the future" beyond this skew is not trusted either.
const MARKS_MAX_SKEW_MS: u64 = 24 * 60 * 60 * 1000;

fn digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

/// The service's identity of one side, from an answer.
pub fn mark_of(c: &Changes, path: &str) -> Option<JournalMarkDto> {
    (c.mark.journal_id != 0 && c.mark.usn >= 0 && c.root_frn != 0).then(|| JournalMarkDto {
        journal_id: c.mark.journal_id.to_string(),
        usn: c.mark.usn.to_string(),
        root_frn: c.root_frn.to_string(),
        path: path.to_owned(),
    })
}

impl JournalMarkDto {
    /// Strict parse; anything odd is no mark (full compare).
    pub fn to_mark(&self) -> Option<JournalMark> {
        if !digits(&self.journal_id) || !digits(&self.usn) || !digits(&self.root_frn) {
            return None;
        }
        self.root_frn.parse::<u64>().ok().filter(|f| *f != 0)?;
        if crate::indexer_client::index_scope(&self.path).as_deref() != Some(self.path.as_str()) {
            return None;
        }
        let journal_id: u64 = self.journal_id.parse().ok()?;
        let usn: i64 = self.usn.parse().ok()?;
        (journal_id != 0).then_some(JournalMark { journal_id, usn })
    }

    fn root_frn(&self) -> u64 {
        self.root_frn.parse().unwrap_or(0)
    }
}

impl JournalMarksDto {
    pub fn is_valid(&self) -> bool {
        self.left.to_mark().is_some() && self.right.to_mark().is_some()
    }

    /// Too old (or from the future): a full content read is due.
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.taken_at_ms) > MARKS_MAX_AGE_MS
            || self.taken_at_ms > now_ms.saturating_add(MARKS_MAX_SKEW_MS)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What the report says about the incremental path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalInfo {
    /// The change lists were used (content reads could be skipped).
    pub used: bool,
    /// Why not (`no_baseline_marks`, `service_unavailable`, `left:<reason>`,
    /// `right:<reason>`, `not_local`). Codes only, never paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// File pairs whose content read was skipped.
    #[serde(default)]
    pub content_skipped: u64,
}

/// Changed directories of one side, as `/`-separated lowercase paths
/// relative to the compare root (`""` = the root).
#[derive(Debug, Default)]
pub struct ChangedSet {
    direct: HashSet<String>,
    recursive: HashSet<String>,
}

impl ChangedSet {
    pub fn from_dirs(dirs: &[ChangedDir]) -> Self {
        let mut s = ChangedSet::default();
        for d in dirs {
            let rel = d.rel.replace('\\', "/").to_lowercase();
            if d.recursive {
                s.recursive.insert(rel.clone());
            }
            s.direct.insert(rel);
        }
        s
    }

    /// Did the listing of directory `rel_dir` change, or was it (or a
    /// directory above it) replaced? Lowercase comparison is conservative:
    /// two names that differ only in case both count as changed.
    pub fn affects(&self, rel_dir: &str) -> bool {
        let rel = rel_dir.to_lowercase();
        if self.direct.contains(&rel) || self.recursive.contains("") {
            return true;
        }
        let mut at = 0;
        while let Some(i) = rel[at..].find('/') {
            if self.recursive.contains(&rel[..at + i]) {
                return true;
            }
            at += i + 1;
        }
        self.recursive.contains(&rel)
    }
}

/// The content-skip rule of one compare (see the module doc).
pub struct ContentHint {
    left: ChangedSet,
    right: ChangedSet,
    baseline: HashMap<String, EntryDto>,
    skipped: AtomicU64,
}

impl ContentHint {
    pub fn new(left: ChangedSet, right: ChangedSet, baseline: &BaselineDto) -> Self {
        ContentHint {
            left,
            right,
            baseline: baseline
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            skipped: AtomicU64::new(0),
        }
    }

    /// May the content read of `full` (`/`-separated, relative) be skipped?
    pub fn may_skip(&self, full: &str, l: &SideInfo, r: &SideInfo) -> bool {
        if l.is_dir || r.is_dir || l.is_symlink || r.is_symlink {
            return false;
        }
        let dir = full.rsplit_once('/').map_or("", |(d, _)| d);
        if self.left.affects(dir) || self.right.affects(dir) {
            return false;
        }
        let Some(b) = self.baseline.get(full) else {
            return false;
        };
        let same = b.left.size == l.size
            && b.left.modified_ms == l.modified_ms
            && b.right.size == r.size
            && b.right.modified_ms == r.modified_ms;
        if same {
            self.skipped.fetch_add(1, Ordering::Relaxed);
        }
        same
    }

    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }
}

/// Where the change lists come from (the service; a fake in tests).
pub trait ChangesSource {
    /// Changes below the two roots since the two marks (`None` = mark only).
    fn changes(
        &mut self,
        roots: [&str; 2],
        since: [Option<JournalMark>; 2],
    ) -> Result<[Changes; 2], String>;
}

/// Result of [`prepare`].
pub struct Prepared {
    pub hint: Option<std::sync::Arc<ContentHint>>,
    /// Marks for the NEXT run (taken now, before the walk).
    pub marks: Option<JournalMarksDto>,
    pub info: IncrementalInfo,
}

/// The path the service knows a compare root by: the canonical path of a
/// local drive-letter directory (junctions resolved), as `X:\a\b`.
pub fn service_path(root: &str) -> Option<String> {
    let canon = std::fs::canonicalize(root).ok()?;
    let s = canon.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let b = s.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None; // UNC, volume GUID path, …
    }
    crate::indexer_client::index_scope(s)
}

/// Ask the service for both sides and build the content hint when the
/// baseline of the pair carries marks. Never fails: every problem is a
/// reason in `info` and the ordinary compare.
pub fn prepare(
    left_root: &str,
    right_root: &str,
    store: &SyncBaselineStore,
    source: &mut dyn ChangesSource,
) -> Prepared {
    prepare_at(left_root, right_root, store, source, now_ms())
}

/// [`prepare`] at a given clock (tests).
pub fn prepare_at(
    left_root: &str,
    right_root: &str,
    store: &SyncBaselineStore,
    source: &mut dyn ChangesSource,
    now: u64,
) -> Prepared {
    let none = |reason: &str| Prepared {
        hint: None,
        marks: None,
        info: IncrementalInfo {
            used: false,
            reason: Some(reason.into()),
            content_skipped: 0,
        },
    };
    let (Some(lp), Some(rp)) = (service_path(left_root), service_path(right_root)) else {
        return none("not_local");
    };
    let pair = RootPair {
        left_root: left_root.to_owned(),
        right_root: right_root.to_owned(),
    };
    let baseline = match store.load(&pair) {
        Ok(LoadResult::Ok { baseline }) => Some(baseline),
        _ => None,
    };
    // Usable marks: valid, not expired, taken for the same canonical roots
    // (review P2-2: a junction retargeted since resolves elsewhere now).
    let mut why: Option<String> = None;
    let marks_in = baseline.as_ref().and_then(|b| b.journal.as_ref());
    let usable = match marks_in {
        None => {
            why = Some("no_baseline_marks".into());
            None
        }
        Some(m) if m.expired(now) => {
            why = Some("marks_expired".into());
            None
        }
        Some(m)
            if !m.left.path.eq_ignore_ascii_case(&lp)
                || !m.right.path.eq_ignore_ascii_case(&rp) =>
        {
            why = Some("root_changed".into());
            None
        }
        Some(m) => match (m.left.to_mark(), m.right.to_mark()) {
            (Some(l), Some(r)) => Some((m, [l, r])),
            _ => {
                why = Some("no_baseline_marks".into());
                None
            }
        },
    };
    let since = usable.map_or([None, None], |(_, s)| [Some(s[0]), Some(s[1])]);
    let [l, r] = match source.changes([&lp, &rp], since) {
        Ok(c) => c,
        Err(e) => {
            log::info!("incremental sync not used: {e}");
            return none("service_unavailable");
        }
    };
    let marks = match (mark_of(&l, &lp), mark_of(&r, &rp)) {
        (Some(left), Some(right)) => Some(JournalMarksDto {
            left,
            right,
            taken_at_ms: now,
        }),
        _ => None,
    };
    if let Some((m, s)) = usable {
        let side = |name: &str, c: &Changes, stored: &JournalMarkDto, since: &JournalMark| {
            match c.status {
                ChangeStatus::Complete if c.mark.journal_id != since.journal_id => {
                    Some(format!("{name}:journal_changed"))
                }
                // Review P2-2: the root directory is another one now.
                ChangeStatus::Complete if c.root_frn != stored.root_frn() => {
                    Some(format!("{name}:root_changed"))
                }
                ChangeStatus::Complete => None,
                ChangeStatus::MarkOnly => Some(format!("{name}:no_baseline_marks")),
                ChangeStatus::Rescan(r) => Some(format!("{name}:{}", r.as_str())),
            }
        };
        why = side("left", &l, &m.left, &s[0]).or_else(|| side("right", &r, &m.right, &s[1]));
    }
    let hint = match (&why, &baseline) {
        (None, Some(b)) => Some(std::sync::Arc::new(ContentHint::new(
            ChangedSet::from_dirs(&l.dirs),
            ChangedSet::from_dirs(&r.dirs),
            b,
        ))),
        _ => None,
    };
    Prepared {
        info: IncrementalInfo {
            used: hint.is_some(),
            reason: if hint.is_some() {
                None
            } else {
                Some(why.unwrap_or_else(|| "no_baseline_marks".into()))
            },
            content_skipped: 0,
        },
        hint,
        marks,
    }
}

/// The service, over one verified connection.
pub struct ServiceChanges;

impl ChangesSource for ServiceChanges {
    fn changes(
        &mut self,
        roots: [&str; 2],
        since: [Option<JournalMark>; 2],
    ) -> Result<[Changes; 2], String> {
        crate::indexer_client::journal_changes(roots, since)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_baseline::FactsDto;
    use ferrum_indexer::query::RescanReason;
    use std::collections::BTreeMap;

    fn cd(rel: &str, recursive: bool) -> ChangedDir {
        ChangedDir {
            rel: rel.into(),
            recursive,
        }
    }

    #[test]
    fn changed_set_direct_and_recursive() {
        let s = ChangedSet::from_dirs(&[cd(r"A\B", false), cd(r"x\Moved", true)]);
        assert!(s.affects("a/b"));
        assert!(!s.affects("a"), "the parent listing did not change");
        assert!(
            !s.affects("a/b/c"),
            "a plain change does not cover children"
        );
        assert!(s.affects("x/moved"));
        assert!(s.affects("X/Moved/deep/er"), "below a replaced directory");
        assert!(!s.affects("x"));
        assert!(
            !s.affects("x/movedother"),
            "component-wise, not a string prefix"
        );
        let root = ChangedSet::from_dirs(&[cd("", true)]);
        assert!(root.affects("") && root.affects("anything/below"));
        let none = ChangedSet::default();
        assert!(!none.affects("") && !none.affects("a"));
    }

    fn side(size: u64, ms: i64) -> SideInfo {
        SideInfo {
            is_dir: false,
            is_symlink: false,
            size,
            modified_ms: ms,
        }
    }

    /// (name, (left size, left mtime), (right size, right mtime)).
    type Row<'a> = (&'a str, (u64, i64), (u64, i64));

    fn baseline(entries: &[Row<'_>]) -> BaselineDto {
        let mut m = BTreeMap::new();
        for (k, l, r) in entries {
            m.insert(
                k.to_string(),
                EntryDto {
                    left: FactsDto {
                        size: l.0,
                        modified_ms: l.1,
                    },
                    right: FactsDto {
                        size: r.0,
                        modified_ms: r.1,
                    },
                },
            );
        }
        BaselineDto {
            version: crate::sync_baseline::BASELINE_VERSION,
            left_root: "L".into(),
            right_root: "R".into(),
            entries: m,
            journal: None,
        }
    }

    #[test]
    fn content_skip_needs_unchanged_dirs_and_exact_baseline_facts() {
        let b = baseline(&[
            ("a.txt", (10, 1000), (10, 5000)),
            ("sub/b.txt", (20, 2000), (20, 2000)),
            ("moved/c.txt", (30, 3000), (30, 3000)),
        ]);
        let h = ContentHint::new(
            ChangedSet::from_dirs(&[cd("moved", true)]),
            ChangedSet::from_dirs(&[cd("sub", false)]),
            &b,
        );
        assert!(h.may_skip("a.txt", &side(10, 1000), &side(10, 5000)));
        // Facts differ from the baseline on either side → read.
        assert!(!h.may_skip("a.txt", &side(10, 1001), &side(10, 5000)));
        assert!(!h.may_skip("a.txt", &side(10, 1000), &side(10, 5001)));
        // Directory changed on the right only → read.
        assert!(!h.may_skip("sub/b.txt", &side(20, 2000), &side(20, 2000)));
        // Directory replaced on the left → read.
        assert!(!h.may_skip("moved/c.txt", &side(30, 3000), &side(30, 3000)));
        // Not in the baseline → read.
        assert!(!h.may_skip("new.txt", &side(1, 1), &side(1, 1)));
        // Never for directories or links.
        let mut link = side(10, 1000);
        link.is_symlink = true;
        assert!(!h.may_skip("a.txt", &link, &side(10, 5000)));
        assert_eq!(h.skipped(), 1);
    }

    fn dto(j: &str, u: &str, f: &str, path: &str) -> JournalMarkDto {
        JournalMarkDto {
            journal_id: j.into(),
            usn: u.into(),
            root_frn: f.into(),
            path: path.into(),
        }
    }

    #[test]
    fn marks_are_strict() {
        let c = Changes {
            mark: JournalMark {
                journal_id: u64::MAX,
                usn: i64::MAX,
            },
            root_frn: 77,
            status: ChangeStatus::MarkOnly,
            dirs: vec![],
        };
        let m = mark_of(&c, r"C:\Data").unwrap();
        assert_eq!(
            m.to_mark(),
            Some(JournalMark {
                journal_id: u64::MAX,
                usn: i64::MAX
            })
        );
        let mut no_journal = c.clone();
        no_journal.mark.journal_id = 0;
        assert!(
            mark_of(&no_journal, r"C:\Data").is_none(),
            "no journal: no mark"
        );
        for (j, u, f, path) in [
            ("", "1", "5", r"C:\Data"),
            ("1", "", "5", r"C:\Data"),
            ("-1", "1", "5", r"C:\Data"),
            ("1", "-1", "5", r"C:\Data"),
            ("1e3", "1", "5", r"C:\Data"),
            ("0", "1", "5", r"C:\Data"),
            ("1", "99999999999999999999", "5", r"C:\Data"),
            ("1", " 1", "5", r"C:\Data"),
            ("1", "1", "0", r"C:\Data"),
            ("1", "1", "x", r"C:\Data"),
            ("1", "1", "5", r"\\srv\share"),
            ("1", "1", "5", r"c:\data\"),
            ("1", "1", "5", r"C:\a\..\b"),
        ] {
            assert_eq!(
                dto(j, u, f, path).to_mark(),
                None,
                "{j:?} {u:?} {f:?} {path:?}"
            );
        }
        // Expiry: 7 days, and no marks from the future.
        let marks = JournalMarksDto {
            left: m.clone(),
            right: m,
            taken_at_ms: 1_000_000_000_000,
        };
        assert!(!marks.expired(1_000_000_000_000 + MARKS_MAX_AGE_MS));
        assert!(marks.expired(1_000_000_000_000 + MARKS_MAX_AGE_MS + 1));
        assert!(marks.expired(1_000_000_000_000 - 2 * MARKS_MAX_SKEW_MS));
    }

    struct Fake {
        answer: Result<[Changes; 2], String>,
        asked: Option<[Option<JournalMark>; 2]>,
    }
    impl ChangesSource for Fake {
        fn changes(
            &mut self,
            _: [&str; 2],
            since: [Option<JournalMark>; 2],
        ) -> Result<[Changes; 2], String> {
            self.asked = Some(since);
            self.answer.clone()
        }
    }

    fn ch(status: ChangeStatus, usn: i64, dirs: Vec<ChangedDir>) -> Changes {
        Changes {
            mark: JournalMark { journal_id: 7, usn },
            root_frn: 55,
            status,
            dirs,
        }
    }

    fn pair_dirs(tag: &str) -> (std::path::PathBuf, String, String) {
        let base = std::env::temp_dir().join(format!("ferrum-inc-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("l")).unwrap();
        std::fs::create_dir_all(base.join("r")).unwrap();
        let l = base.join("l").to_string_lossy().into_owned();
        let r = base.join("r").to_string_lossy().into_owned();
        (base, l, r)
    }

    const NOW: u64 = 1_800_000_000_000;

    fn complete(usn: i64, dirs: Vec<ChangedDir>) -> Changes {
        ch(ChangeStatus::Complete, usn, dirs)
    }

    #[test]
    fn prepare_uses_the_lists_only_when_everything_is_complete() {
        let (base, l, r) = pair_dirs("prep");
        let store = SyncBaselineStore::open(&base.join("store"));
        let pair = RootPair {
            left_root: l.clone(),
            right_root: r.clone(),
        };
        // No baseline yet: the service is asked for marks only; marks come
        // back for the next run (with the root identity), no hint.
        let mut f = Fake {
            answer: Ok([
                ch(ChangeStatus::MarkOnly, 100, vec![]),
                ch(ChangeStatus::MarkOnly, 200, vec![]),
            ]),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW);
        assert_eq!(f.asked, Some([None, None]));
        assert!(p.hint.is_none() && !p.info.used);
        assert_eq!(p.info.reason.as_deref(), Some("no_baseline_marks"));
        let marks = p.marks.clone().unwrap();
        assert_eq!(
            (marks.left.usn.as_str(), marks.right.usn.as_str()),
            ("100", "200")
        );
        assert_eq!(marks.left.root_frn, "55");
        assert_eq!(Some(marks.left.path.clone()), service_path(&l));
        assert_eq!(marks.taken_at_ms, NOW);
        // A baseline with those marks: the service is asked since them.
        let mut b = baseline(&[("a.txt", (1, 1), (1, 1))]);
        b.left_root = l.clone();
        b.right_root = r.clone();
        b.journal = Some(Box::new(marks.clone()));
        store.save(&pair, &b).unwrap();
        let ok = || Ok([complete(150, vec![cd("x", false)]), complete(250, vec![])]);
        let mut f = Fake {
            answer: ok(),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW + 1000);
        let mk = |usn| Some(JournalMark { journal_id: 7, usn });
        assert_eq!(f.asked, Some([mk(100), mk(200)]));
        assert!(p.info.used && p.hint.is_some());
        assert_eq!(p.marks.unwrap().left.usn, "150");
        // One side must rescan → no hint at all, reason says which.
        let mut f = Fake {
            answer: Ok([
                complete(150, vec![]),
                ch(
                    ChangeStatus::Rescan(RescanReason::JournalChanged),
                    9,
                    vec![],
                ),
            ]),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW);
        assert!(p.hint.is_none());
        assert_eq!(p.info.reason.as_deref(), Some("right:journal_changed"));
        assert!(
            p.marks.is_some(),
            "fresh marks are still valid for next time"
        );
        // Review P2-2: the root directory is another one (other frn).
        let mut other_root = complete(150, vec![]);
        other_root.root_frn = 56;
        let mut f = Fake {
            answer: Ok([other_root, complete(250, vec![])]),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW);
        assert!(p.hint.is_none());
        assert_eq!(p.info.reason.as_deref(), Some("left:root_changed"));
        // A "complete" answer for another journal than asked is not trusted.
        let mut other_journal = complete(150, vec![]);
        other_journal.mark.journal_id = 8;
        let mut f = Fake {
            answer: Ok([other_journal, complete(250, vec![])]),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW);
        assert_eq!(p.info.reason.as_deref(), Some("left:journal_changed"));
        // Review: marks expire after 7 days — the service is asked for new
        // marks only, the content is read.
        let mut f = Fake {
            answer: ok(),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW + MARKS_MAX_AGE_MS + 1);
        assert_eq!(f.asked, Some([None, None]));
        assert!(p.hint.is_none());
        assert_eq!(p.info.reason.as_deref(), Some("marks_expired"));
        // Review P2-2: marks taken for another canonical root (a junction
        // retargeted): not used.
        let mut moved = marks.clone();
        moved.left.path = r"Q:\elsewhere".into();
        b.journal = Some(Box::new(moved));
        store.save(&pair, &b).unwrap();
        let mut f = Fake {
            answer: ok(),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW);
        assert_eq!(f.asked, Some([None, None]));
        assert_eq!(p.info.reason.as_deref(), Some("root_changed"));
        // Service failure → no hint, no marks.
        let mut f = Fake {
            answer: Err("not running".into()),
            asked: None,
        };
        let p = prepare_at(&l, &r, &store, &mut f, NOW);
        assert!(p.hint.is_none() && p.marks.is_none());
        assert_eq!(p.info.reason.as_deref(), Some("service_unavailable"));
        // A volume without journal: no marks.
        let mut nj = ch(ChangeStatus::Rescan(RescanReason::Unavailable), 0, vec![]);
        nj.mark.journal_id = 0;
        let mut f = Fake {
            answer: Ok([nj, complete(1, vec![])]),
            asked: None,
        };
        assert!(prepare_at(&l, &r, &store, &mut f, NOW).marks.is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn non_local_roots_never_ask_the_service() {
        let store = SyncBaselineStore::unavailable();
        let mut f = Fake {
            answer: Err("must not be called".into()),
            asked: None,
        };
        let p = prepare(r"\\server\share\a", r"\\server\share\b", &store, &mut f);
        assert!(f.asked.is_none());
        assert_eq!(p.info.reason.as_deref(), Some("not_local"));
    }
}
