//! Queries over the served indexes (FER-476, IDX-S3): name search and
//! subtree listing for Disk Usage. Platform-neutral; the Windows
//! AccessCheck lives behind [`DirAccess`] (`access.rs`).
//!
//! Rules:
//! - **Bounded work.** The read lock of a volume index is taken for at most
//!   [`SCAN_CHUNK`] node slots at a time and is NEVER held while a result is
//!   written to a client (the caller writes after this module returns, or
//!   between chunks of a subtree). Every query has a deadline; a search that
//!   reaches it returns what it has plus a cursor.
//! - **Bounded output.** A search page holds at most `limit` (≤
//!   [`MAX_PAGE`]) hits and about [`PAGE_BYTES`]; a subtree chunk about
//!   [`CHUNK_BYTES`]. Paths longer than [`MAX_PATH_BYTES`] are skipped.
//! - **Shared mode** (review FER-475 S3 hook): a name is visible only when
//!   EVERY directory from the volume root down to its parent can be listed
//!   (`FILE_LIST_DIRECTORY`) by the client — checked with the client's own
//!   token (`DirAccess`), memoised per query. A scope the client cannot
//!   list, a scope inside an excluded area (profiles, recycle bin) and a
//!   scope that does not exist all answer the SAME `NotIndexed` (no oracle).
//! - **Mode changes** abort a running query (`abort()`), so no result of an
//!   index of the previous scope leaves after the switch.

use crate::engine::{LiveIndex, ServedVolume, SharedIndex, read_index};
use crate::index::{Entry, Kind, MAX_DEPTH, Reach, valid_name};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Longest name pattern (UTF-8 bytes).
pub const MAX_PATTERN: usize = 256;
/// Longest scope / subtree path (UTF-8 bytes).
pub const MAX_SCOPE: usize = 1024;
pub const MAX_EXTENSIONS: usize = 16;
pub const MAX_EXTENSION: usize = 16;
/// Most hits per search page.
pub const MAX_PAGE: u16 = 1000;
/// Approximate byte budget of one search page.
pub const PAGE_BYTES: usize = 256 * 1024;
/// Approximate byte budget of one subtree chunk.
pub const CHUNK_BYTES: usize = 256 * 1024;
/// Hits whose full path is longer are skipped (the frame stays bounded).
pub const MAX_PATH_BYTES: usize = 4096;
/// Node slots scanned per read-lock hold.
pub const SCAN_CHUNK: u32 = 32 * 1024;
/// Wall-clock budget of one search request (then: partial page + cursor).
pub const SEARCH_TIME: Duration = Duration::from_millis(1500);
/// Wall-clock budget of one subtree request (then: `Timeout`).
pub const SUBTREE_TIME: Duration = Duration::from_secs(120);
/// Most entries one subtree request streams.
pub const MAX_SUBTREE_ENTRIES: u64 = 30_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// Name contains the pattern.
    Substring,
    /// Name starts with the pattern.
    Prefix,
    /// Whole name matches; `*` = any run, `?` = one character (Ferrum's
    /// glob search: wildcards never cross a path separator — names have
    /// none).
    Glob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindFilter {
    Files,
    Dirs,
    Both,
}

/// Where a search continues: volume letter (`0` = from the start) + node
/// slot. Approximate across index changes (slots may be reused), which is
/// fine for paging a live index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueryCursor {
    pub letter: u8,
    pub slot: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub pattern: String,
    pub match_kind: MatchKind,
    /// `""` = every volume, else an absolute directory (`C:\` or `C:\a\b`).
    pub scope: String,
    /// Lowercase ASCII extensions without the dot; empty = any.
    pub extensions: Vec<String>,
    pub kinds: KindFilter,
    pub limit: u16,
    pub cursor: QueryCursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub kind: Kind,
    pub size: u64,
    pub allocated: Option<u64>,
    /// FILETIME.
    pub modified: u64,
    pub attributes: u32,
    /// Full path, `C:\a\b.txt`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    /// `None` = done.
    pub next: Option<QueryCursor>,
    /// At least one volume answered from the previous index while it is
    /// rebuilt (complete but not live).
    pub stale: bool,
    /// Volumes in scope without a searchable index (bit 0 = `A`).
    pub unavailable: u32,
    /// The scope contains excluded areas (shared mode: profiles, recycle
    /// bin) whose content is not indexed.
    pub excluded_inside: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtreeEntry {
    pub frn: u64,
    pub parent: u64,
    pub kind: Kind,
    pub size: u64,
    pub allocated: Option<u64>,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtreeInfo {
    /// File reference of the requested directory (parent of the top
    /// entries).
    pub root_frn: u64,
    pub stale: bool,
    pub excluded_inside: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    BadQuery(&'static str),
    /// Scope/path not found, not listable by the client, inside an
    /// excluded area, or its volume has no index right now. One answer for
    /// all of them: no existence oracle.
    NotIndexed,
    Timeout,
    TooLarge,
    /// The mode changed or the client went away.
    Aborted,
}

/// Can the client list directory `frn` on volume `letter`?
pub trait DirAccess {
    /// The decision if it is already known, WITHOUT any I/O (review N3:
    /// queries ask this while holding the index read lock and do the real
    /// AccessChecks only after releasing it).
    fn cached(&mut self, _letter: u8, _dir_frn: u64) -> Option<bool> {
        None
    }
    /// The decision, doing the AccessCheck when needed (never called with
    /// an index lock held).
    fn can_list(&mut self, letter: u8, dir_frn: u64) -> bool;
    /// Review P2-4: the decision with a third outcome, `Unknown`, when it
    /// could not be evaluated at all (the directory or its security
    /// descriptor cannot be read by the service, AccessCheck failed). Search
    /// and subtree treat `Unknown` as "no" (privacy, fail closed); `changes`
    /// must not, so it answers "rescan". Default: from [`Self::can_list`].
    fn check(&mut self, letter: u8, dir_frn: u64) -> Access {
        if self.can_list(letter, dir_frn) {
            Access::Yes
        } else {
            Access::No
        }
    }
    /// A known tri-state decision without I/O. Default: from
    /// [`Self::cached`]; an implementation whose cache folds `Unknown` into
    /// `false` must only report cached `Yes` here.
    fn cached_access(&mut self, letter: u8, dir_frn: u64) -> Option<Access> {
        self.cached(letter, dir_frn)
            .map(|v| if v { Access::Yes } else { Access::No })
    }
}

/// Outcome of one listability decision (review P2-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Yes,
    No,
    /// Could not be evaluated.
    Unknown,
}

impl Access {
    /// Both directories on one chain: any `No` wins, then any `Unknown`.
    fn and(self, other: Access) -> Access {
        match (self, other) {
            (Access::No, _) | (_, Access::No) => Access::No,
            (Access::Unknown, _) | (_, Access::Unknown) => Access::Unknown,
            _ => Access::Yes,
        }
    }
}

// ── validation ───────────────────────────────────────────────────────────

fn no_controls(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// `C:\a\b` → (`C`, ["a", "b"]); `C:\` → (`C`, []). Strict: drive letter,
/// `:\`, then valid names separated by single backslashes (a trailing one
/// is allowed).
pub fn parse_abs_dir(path: &str) -> Option<(u8, Vec<&str>)> {
    let b = path.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return None;
    }
    let rest = path[3..].strip_suffix('\\').unwrap_or(&path[3..]);
    let comps: Vec<&str> = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split('\\').collect()
    };
    if comps.iter().any(|c| !valid_name(c)) {
        return None;
    }
    Some((b[0].to_ascii_uppercase(), comps))
}

impl SearchQuery {
    pub fn validate(&self) -> Result<(), QueryError> {
        let p = &self.pattern;
        if p.is_empty() || p.len() > MAX_PATTERN || !no_controls(p) {
            return Err(QueryError::BadQuery("pattern"));
        }
        if p.contains(['\\', '/', ':']) {
            return Err(QueryError::BadQuery("pattern"));
        }
        if self.scope.len() > MAX_SCOPE
            || (!self.scope.is_empty() && parse_abs_dir(&self.scope).is_none())
        {
            return Err(QueryError::BadQuery("scope"));
        }
        if self.extensions.len() > MAX_EXTENSIONS
            || self.extensions.iter().any(|e| {
                e.is_empty()
                    || e.len() > MAX_EXTENSION
                    || !e
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            })
        {
            return Err(QueryError::BadQuery("extensions"));
        }
        if self.limit == 0 || self.limit > MAX_PAGE {
            return Err(QueryError::BadQuery("limit"));
        }
        if self.cursor.letter != 0 && !self.cursor.letter.is_ascii_uppercase() {
            return Err(QueryError::BadQuery("cursor"));
        }
        Ok(())
    }
}

// ── name matching ────────────────────────────────────────────────────────

/// Case-insensitive matcher. An ASCII pattern is compared byte-wise with
/// ASCII folding against ASCII names (no allocation per entry); anything
/// else is compared on lowercased chars.
pub struct Matcher {
    kind: MatchKind,
    ascii: bool,
    bytes: Vec<u8>,
    chars: Vec<char>,
}

impl Matcher {
    pub fn new(kind: MatchKind, pattern: &str) -> Matcher {
        let lower = pattern.to_lowercase();
        Matcher {
            kind,
            ascii: lower.is_ascii(),
            bytes: lower.as_bytes().to_vec(),
            chars: lower.chars().collect(),
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        if self.ascii && name.is_ascii() {
            let n: Vec<u8>;
            let name_b = if name.bytes().any(|b| b.is_ascii_uppercase()) {
                n = name.bytes().map(|b| b.to_ascii_lowercase()).collect();
                &n[..]
            } else {
                name.as_bytes()
            };
            return match self.kind {
                MatchKind::Prefix => name_b.starts_with(&self.bytes),
                MatchKind::Substring => contains(name_b, &self.bytes),
                MatchKind::Glob => glob(&self.bytes, name_b),
            };
        }
        let name_c: Vec<char> = name.to_lowercase().chars().collect();
        match self.kind {
            MatchKind::Prefix => name_c.starts_with(&self.chars),
            MatchKind::Substring => contains(&name_c, &self.chars),
            MatchKind::Glob => glob(&self.chars, &name_c),
        }
    }
}

fn contains<T: PartialEq>(hay: &[T], needle: &[T]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

trait GlobChar: PartialEq + Copy {
    const STAR: Self;
    const ONE: Self;
}
impl GlobChar for u8 {
    const STAR: u8 = b'*';
    const ONE: u8 = b'?';
}
impl GlobChar for char {
    const STAR: char = '*';
    const ONE: char = '?';
}

/// Anchored glob with `*` and `?`; linear backtracking (last star only),
/// O(pattern × name) worst case — both are bounded (256 / 255).
fn glob<T: GlobChar>(pat: &[T], name: &[T]) -> bool {
    let (mut p, mut n) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pat.len() && (pat[p] == T::ONE || (pat[p] == name[n] && pat[p] != T::STAR)) {
            p += 1;
            n += 1;
        } else if p < pat.len() && pat[p] == T::STAR {
            star = Some((p, n));
            p += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == T::STAR {
        p += 1;
    }
    p == pat.len()
}

fn extension_ok(name: &str, exts: &[String]) -> bool {
    if exts.is_empty() {
        return true;
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => exts.iter().any(|e| ext.eq_ignore_ascii_case(e)),
        _ => false,
    }
}

// ── visibility ───────────────────────────────────────────────────────────

/// Per-query, per-volume memo of directory visibility.
///
/// Review N3: AccessChecks (I/O) are never done while the index read lock
/// is held. Under the lock only known decisions are used (`raw` + the
/// shared cache via `DirAccess::cached_access`); a directory without one
/// makes `children_access` return the directories to check, the caller
/// releases the lock, runs [`Visibility::resolve`], and asks again.
struct Visibility<'a, 'b> {
    letter: u8,
    access: Option<&'a mut (dyn DirAccess + 'b)>,
    /// dir frn → combined decision for every directory from the volume root
    /// down to it.
    memo: HashMap<u64, Access>,
    /// dir frn → this directory alone (decisions of this query).
    raw: HashMap<u64, Access>,
}

impl<'a, 'b> Visibility<'a, 'b> {
    fn new(letter: u8, access: Option<&'a mut (dyn DirAccess + 'b)>) -> Self {
        Visibility {
            letter,
            access,
            memo: HashMap::new(),
            raw: HashMap::new(),
        }
    }

    /// Can the client see the children of directory `dir` (i.e. list `dir`
    /// and every directory above it)? Always true without a filter;
    /// `Unknown` counts as "no" here (search, subtree: fail closed).
    fn children_visible(&mut self, live: &LiveIndex, dir: u64) -> Result<bool, Vec<u64>> {
        self.children_access(live, dir).map(|a| a == Access::Yes)
    }

    /// The tri-state form (review P2-4). `Err(dirs)` = decisions missing for
    /// `dirs`: call [`Self::resolve`] without the index lock, then ask again.
    /// Which directories are checked depends only on the tree (review L3),
    /// never on names.
    fn children_access(&mut self, live: &LiveIndex, dir: u64) -> Result<Access, Vec<u64>> {
        let Some(acc) = self.access.as_deref_mut() else {
            return Ok(Access::Yes);
        };
        let root = live.index.root_frn();
        let mut chain: Vec<u64> = Vec::new();
        let mut cur = dir;
        let base = loop {
            if let Some(&v) = self.memo.get(&cur) {
                break v;
            }
            chain.push(cur);
            if cur == root {
                break Access::Yes;
            }
            if chain.len() > MAX_DEPTH {
                break Access::No;
            }
            match live.index.dir_parent(cur) {
                Some(p) => cur = p,
                None => break Access::No,
            }
        };
        let mut known = base;
        let mut missing = Vec::new();
        for d in chain.iter().rev() {
            if known == Access::No {
                break; // something above is not listable: all below is no
            }
            let r = match self.raw.get(d) {
                Some(&v) => Some(v),
                None => {
                    let c = acc.cached_access(self.letter, *d);
                    if let Some(v) = c {
                        self.raw.insert(*d, v);
                    }
                    c
                }
            };
            match r {
                Some(v) => known = known.and(v),
                None => missing.push(*d),
            }
        }
        if !missing.is_empty() {
            return Err(missing);
        }
        let mut ok = base;
        for d in chain.iter().rev() {
            ok = ok.and(self.raw.get(d).copied().unwrap_or(Access::No));
            self.memo.insert(*d, ok);
        }
        Ok(ok)
    }

    /// Run the AccessChecks for `dirs` (call WITHOUT the index lock).
    fn resolve(&mut self, dirs: &[u64]) {
        if let Some(acc) = self.access.as_deref_mut() {
            for d in dirs {
                if !self.raw.contains_key(d) {
                    let v = acc.check(self.letter, *d);
                    self.raw.insert(*d, v);
                }
            }
        }
    }
}

/// Is `dir` (an indexed directory or the root) at or below `top`?
fn under(live: &LiveIndex, mut dir: u64, top: u64, memo: &mut HashMap<u64, bool>) -> bool {
    let root = live.index.root_frn();
    let mut chain = Vec::new();
    let found = loop {
        if dir == top {
            break true;
        }
        if let Some(&v) = memo.get(&dir) {
            break v;
        }
        if dir == root || chain.len() > MAX_DEPTH {
            break false;
        }
        chain.push(dir);
        match live.index.dir_parent(dir) {
            Some(p) => dir = p,
            None => break false,
        }
    };
    for d in chain {
        memo.insert(d, found);
    }
    found
}

/// Resolve a scope/subtree directory for this client: exists, is not in
/// (or below) an excluded area, and is listable. Anything else is
/// `NotIndexed`. Returns the directory and whether it contains excluded
/// areas. AccessChecks run between lock holds (review N3).
fn resolve_dir(
    ix: &SharedIndex,
    comps: &[&str],
    vis: &mut Visibility<'_, '_>,
) -> Result<(u64, bool), QueryError> {
    for _ in 0..=MAX_DEPTH {
        let pending = {
            let live = read_index(ix);
            let frn = live
                .index
                .find_dir_path(comps)
                .ok_or(QueryError::NotIndexed)?;
            if frn != live.index.root_frn() {
                if live.excluded.contains(&(frn & crate::index::RECORD_MASK)) {
                    return Err(QueryError::NotIndexed);
                }
                let parent = live.index.dir_parent(frn).ok_or(QueryError::NotIndexed)?;
                if live.index.reach(parent, &live.excluded) != Reach::Root {
                    return Err(QueryError::NotIndexed);
                }
            }
            match vis.children_visible(&live, frn) {
                Ok(true) => return Ok((frn, excluded_inside(&live, frn))),
                Ok(false) => return Err(QueryError::NotIndexed),
                Err(m) => m,
            }
        };
        vis.resolve(&pending);
    }
    Err(QueryError::NotIndexed)
}

/// Does `top` contain an excluded directory (content not indexed)?
fn excluded_inside(live: &LiveIndex, top: u64) -> bool {
    let mut memo = HashMap::new();
    live.excluded.iter().any(|rec| {
        live.index
            .record_entries(*rec)
            .first()
            .is_some_and(|e| e.kind == Kind::Dir && under(live, e.frn, top, &mut memo))
    })
}

fn full_path(letter: u8, live: &LiveIndex, e: &Entry<'_>) -> Option<String> {
    let rel = live.index.path_of(e)?;
    let p = format!("{}:\\{rel}", letter as char);
    (p.len() <= MAX_PATH_BYTES).then_some(p)
}

fn bit(letter: u8) -> u32 {
    1u32 << (letter - b'A')
}

// ── search ───────────────────────────────────────────────────────────────

/// One page of a name search over `volumes`. `access` = `None` in
/// `high_performance` (no per-user filter), the client's AccessCheck in
/// `shared`. `abort` is polled between chunks.
pub fn search(
    volumes: &[ServedVolume],
    q: &SearchQuery,
    mut access: Option<&mut dyn DirAccess>,
    deadline: Instant,
    abort: &dyn Fn() -> bool,
) -> Result<SearchPage, QueryError> {
    q.validate()?;
    let matcher = Matcher::new(q.match_kind, &q.pattern);
    let scope = if q.scope.is_empty() {
        None
    } else {
        Some(parse_abs_dir(&q.scope).ok_or(QueryError::BadQuery("scope"))?)
    };
    let mut vols: Vec<&ServedVolume> = volumes
        .iter()
        .filter(|v| v.letter.is_ascii_uppercase())
        .filter(|v| scope.as_ref().is_none_or(|(l, _)| *l == v.letter))
        .filter(|v| q.cursor.letter == 0 || v.letter >= q.cursor.letter)
        .collect();
    vols.sort_by_key(|v| v.letter);
    if let Some((l, _)) = &scope
        && !volumes.iter().any(|v| v.letter == *l)
    {
        return Err(QueryError::NotIndexed);
    }
    let mut page = SearchPage::default();
    let mut bytes = 0usize;
    let limit = q.limit as usize;
    for v in vols {
        let Some(ix) = &v.index else {
            if scope.is_some() {
                return Err(QueryError::NotIndexed);
            }
            page.unavailable |= bit(v.letter);
            continue;
        };
        page.stale |= v.stale;
        let mut vis = Visibility::new(v.letter, access.as_deref_mut());
        let mut under_memo = HashMap::new();
        let mut slot = if v.letter == q.cursor.letter {
            q.cursor.slot
        } else {
            0
        };
        let comps = scope.as_ref().map(|(_, c)| c.as_slice()).unwrap_or(&[]);
        let (top, exc) = resolve_dir(ix, comps, &mut vis)?;
        page.excluded_inside |= exc;
        loop {
            if abort() {
                return Err(QueryError::Aborted);
            }
            let live = read_index(ix);
            let total = live.index.slot_count();
            let end = slot.saturating_add(SCAN_CHUNK).min(total);
            // A slot whose parent chain needs AccessChecks first (review N3).
            let mut pending: Option<(u32, Vec<u64>)> = None;
            for s in slot..end {
                let Some(e) = live.index.entry_at(s) else {
                    continue;
                };
                let kind_ok = match q.kinds {
                    KindFilter::Files => e.kind != Kind::Dir,
                    KindFilter::Dirs => e.kind == Kind::Dir,
                    KindFilter::Both => true,
                };
                // Review L3: visibility is decided BEFORE anything about the
                // name, so the AccessCheck work a query causes depends only
                // on the tree, never on whether a hidden name matches.
                if !under(&live, e.parent, top, &mut under_memo) {
                    continue;
                }
                match vis.children_visible(&live, e.parent) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(m) => {
                        pending = Some((s, m));
                        break;
                    }
                }
                if !kind_ok || !extension_ok(e.name, &q.extensions) || !matcher.matches(e.name) {
                    continue;
                }
                let Some(path) = full_path(v.letter, &live, &e) else {
                    continue;
                };
                let cost = 48 + path.len();
                if page.hits.len() >= limit || bytes + cost > PAGE_BYTES {
                    // Review M1: never hand out a page across a mode change.
                    if abort() {
                        return Err(QueryError::Aborted);
                    }
                    page.next = Some(QueryCursor {
                        letter: v.letter,
                        slot: s,
                    });
                    return Ok(page);
                }
                bytes += cost;
                page.hits.push(SearchHit {
                    kind: e.kind,
                    size: e.meta.size,
                    allocated: e.meta.allocated,
                    modified: e.meta.modified,
                    attributes: e.meta.attributes,
                    path,
                });
            }
            drop(live);
            if let Some((s, m)) = pending {
                // AccessChecks without the index lock, then resume here.
                vis.resolve(&m);
                slot = s;
            } else {
                slot = end;
                if slot >= total {
                    break;
                }
            }
            if Instant::now() >= deadline {
                if abort() {
                    return Err(QueryError::Aborted);
                }
                page.next = Some(QueryCursor {
                    letter: v.letter,
                    slot,
                });
                return Ok(page);
            }
        }
    }
    if abort() {
        return Err(QueryError::Aborted);
    }
    Ok(page)
}

// ── subtree (Disk Usage) ─────────────────────────────────────────────────

/// Stream every entry below directory `path` that the client may see, in
/// chunks: `begin` once (resolved root), then `chunk` per ≈[`CHUNK_BYTES`]
/// (return false to stop: client gone). The read lock is released before
/// every callback. Returns the number of entries streamed.
pub fn subtree(
    volumes: &[ServedVolume],
    path: &str,
    access: Option<&mut dyn DirAccess>,
    deadline: Instant,
    abort: &dyn Fn() -> bool,
    begin: &mut dyn FnMut(&SubtreeInfo) -> bool,
    chunk: &mut dyn FnMut(Vec<SubtreeEntry>) -> bool,
) -> Result<u64, QueryError> {
    if path.len() > MAX_SCOPE {
        return Err(QueryError::BadQuery("path"));
    }
    let (letter, comps) = parse_abs_dir(path).ok_or(QueryError::BadQuery("path"))?;
    let v = volumes
        .iter()
        .find(|v| v.letter == letter)
        .ok_or(QueryError::NotIndexed)?;
    let ix = v.index.as_ref().ok_or(QueryError::NotIndexed)?;
    let mut vis = Visibility::new(letter, access);
    let (top, exc) = resolve_dir(ix, &comps, &mut vis)?;
    let info = SubtreeInfo {
        root_frn: top,
        stale: v.stale,
        excluded_inside: exc,
    };
    if !begin(&info) {
        return Err(QueryError::Aborted);
    }
    let mut under_memo = HashMap::new();
    let mut slot = 0u32;
    let mut sent = 0u64;
    loop {
        if abort() {
            return Err(QueryError::Aborted);
        }
        if Instant::now() >= deadline {
            return Err(QueryError::Timeout);
        }
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut pending: Option<Vec<u64>> = None;
        let done = {
            let live = read_index(ix);
            let total = live.index.slot_count();
            let end = slot.saturating_add(SCAN_CHUNK).min(total);
            let mut s = slot;
            while s < end && bytes < CHUNK_BYTES {
                let visible = match live.index.entry_at(s) {
                    Some(e) if e.frn != top && under(&live, e.parent, top, &mut under_memo) => {
                        match vis.children_visible(&live, e.parent) {
                            Ok(v) => v.then_some(e),
                            Err(m) => {
                                // AccessChecks first, without the lock.
                                pending = Some(m);
                                break;
                            }
                        }
                    }
                    _ => None,
                };
                if let Some(e) = visible {
                    bytes += 40 + e.name.len();
                    out.push(SubtreeEntry {
                        frn: e.frn,
                        parent: e.parent,
                        kind: e.kind,
                        size: e.meta.size,
                        allocated: e.meta.allocated,
                        name: e.name.to_owned(),
                    });
                }
                s += 1;
            }
            slot = s;
            pending.is_none() && slot >= total
        };
        if let Some(m) = pending {
            vis.resolve(&m);
        }
        sent += out.len() as u64;
        if sent > MAX_SUBTREE_ENTRIES {
            return Err(QueryError::TooLarge);
        }
        if !out.is_empty() && !chunk(out) {
            return Err(QueryError::Aborted);
        }
        if done {
            return Ok(sent);
        }
    }
}

// ── folder sizes (FER-477a) ──────────────────────────────────────────────

/// Most folder names in one `folderSizes` request.
pub const MAX_FOLDER_NAMES: usize = 200;
/// Wall-clock budget of one `folderSizes` request (then: `Timeout`).
pub const FOLDER_SIZES_TIME: Duration = Duration::from_secs(5);

/// Size of one requested folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderSize {
    /// Total logical size of every file below the folder (recursive; each
    /// hardlink name counts), and their number.
    Ok { bytes: u64, files: u64 },
    /// Not an indexed folder of `parent` (missing, a file or link, or
    /// excluded).
    NotIndexed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSizes {
    /// Answered from the previous index while it is rebuilt.
    pub stale: bool,
    /// One per requested name, same order.
    pub sizes: Vec<FolderSize>,
}

/// Total size of each folder `parent\name` from the index. NO per-user
/// filter: the server answers this only in `high_performance` (FER-477
/// design decision: in shared mode an aggregate would reveal the size of
/// content the client may not see, so it is refused there).
pub fn folder_sizes(
    volumes: &[ServedVolume],
    parent: &str,
    names: &[String],
    deadline: Instant,
    abort: &dyn Fn() -> bool,
) -> Result<FolderSizes, QueryError> {
    if parent.len() > MAX_SCOPE || names.is_empty() || names.len() > MAX_FOLDER_NAMES {
        return Err(QueryError::BadQuery("folder sizes"));
    }
    if names.iter().any(|n| !valid_name(n)) {
        return Err(QueryError::BadQuery("folder name"));
    }
    let (letter, comps) = parse_abs_dir(parent).ok_or(QueryError::BadQuery("path"))?;
    let v = volumes
        .iter()
        .find(|v| v.letter == letter)
        .ok_or(QueryError::NotIndexed)?;
    let ix = v.index.as_ref().ok_or(QueryError::NotIndexed)?;
    let mut vis = Visibility::new(letter, None);
    let (top, _) = resolve_dir(ix, &comps, &mut vis)?;
    // Lowercased name → requested positions (duplicates answer alike).
    let mut wanted: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in names.iter().enumerate() {
        wanted.entry(n.to_lowercase()).or_default().push(i);
    }
    // Pass 1: the child directories of `top` with a requested name.
    let mut target: HashMap<u64, usize> = HashMap::new(); // dir frn → index in `found`
    let mut found: Vec<(Vec<usize>, u64, u64)> = Vec::new(); // positions, bytes, files
    let mut slot = 0u32;
    loop {
        if abort() {
            return Err(QueryError::Aborted);
        }
        if Instant::now() >= deadline {
            return Err(QueryError::Timeout);
        }
        let live = read_index(ix);
        let total = live.index.slot_count();
        let end = slot.saturating_add(SCAN_CHUNK).min(total);
        for s in slot..end {
            let Some(e) = live.index.entry_at(s) else {
                continue;
            };
            if e.kind != Kind::Dir || e.parent != top || target.contains_key(&e.frn) {
                continue;
            }
            if live.excluded.contains(&(e.frn & crate::index::RECORD_MASK)) {
                continue;
            }
            if let Some(pos) = wanted.get(&e.name.to_lowercase()) {
                // Two directories whose names differ only in case (a
                // case-sensitive folder): the first one answers.
                if found.iter().any(|(p, ..)| p == pos) {
                    continue;
                }
                target.insert(e.frn, found.len());
                found.push((pos.clone(), 0, 0));
            }
        }
        drop(live);
        slot = end;
        if slot >= total {
            break;
        }
    }
    // Pass 2: every file below a target. `memo`: directory → the target it
    // lies in. Read lock per chunk; never held across chunks.
    let mut memo: HashMap<u64, Option<usize>> = HashMap::new();
    slot = 0;
    while !found.is_empty() {
        if abort() {
            return Err(QueryError::Aborted);
        }
        if Instant::now() >= deadline {
            return Err(QueryError::Timeout);
        }
        let live = read_index(ix);
        let total = live.index.slot_count();
        let end = slot.saturating_add(SCAN_CHUNK).min(total);
        for s in slot..end {
            let Some(e) = live.index.entry_at(s) else {
                continue;
            };
            if e.kind != Kind::File {
                continue;
            }
            if let Some(t) = target_of(&live, e.parent, top, &target, &mut memo) {
                let f = &mut found[t];
                f.1 = f.1.saturating_add(e.meta.size);
                f.2 += 1;
            }
        }
        drop(live);
        slot = end;
        if slot >= total {
            break;
        }
    }
    if abort() {
        return Err(QueryError::Aborted);
    }
    let mut sizes = vec![FolderSize::NotIndexed; names.len()];
    for (pos, bytes, files) in found {
        for p in pos {
            sizes[p] = FolderSize::Ok { bytes, files };
        }
    }
    Ok(FolderSizes {
        stale: v.stale,
        sizes,
    })
}

/// Which target directory (index into the caller's list) `dir` lies in;
/// `None` outside `top`, for an orphan, or too deep.
fn target_of(
    live: &LiveIndex,
    dir: u64,
    top: u64,
    target: &HashMap<u64, usize>,
    memo: &mut HashMap<u64, Option<usize>>,
) -> Option<usize> {
    let root = live.index.root_frn();
    let mut chain = Vec::new();
    let mut cur = dir;
    let found = loop {
        if let Some(&t) = target.get(&cur) {
            break Some(t);
        }
        if let Some(&t) = memo.get(&cur) {
            break t;
        }
        if cur == top || cur == root || chain.len() > MAX_DEPTH {
            break None;
        }
        chain.push(cur);
        match live.index.dir_parent(cur) {
            Some(p) => cur = p,
            None => break None,
        }
    };
    for d in chain {
        memo.insert(d, found);
    }
    found
}

// ── changes since a journal position (FER-477b) ──────────────────────────

/// Most changed directories one `changes` answer lists (more: rescan).
pub const MAX_CHANGED_DIRS: usize = 4096;
/// Approximate byte budget of the listed directories (more: rescan).
pub const CHANGED_BYTES: usize = 512 * 1024;

/// A journal position the client keeps between two syncs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalMark {
    pub journal_id: u64,
    pub usn: i64,
}

/// Why the client must rescan instead of trusting a change list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanReason {
    /// The volume's journal is another one (deleted and recreated).
    JournalChanged,
    /// The service has no complete history since the mark (restarted,
    /// index rebuilt, journal wrapped, log evicted, index not live).
    Unavailable,
    /// More changed directories than one answer may list (or a path too
    /// long for it).
    TooMany,
    /// Shared mode: the directory contains areas whose content is not
    /// indexed (profiles, recycle bin), so their changes are unknown.
    ExcludedInside,
    /// Review P2-2: the directory, or one above it, was created, renamed,
    /// moved or replaced since the mark.
    RootChanged,
    /// Review P2-4: an AccessCheck could not be evaluated for a changed
    /// directory (shared mode).
    Unverifiable,
}

impl RescanReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RescanReason::JournalChanged => "journal_changed",
            RescanReason::Unavailable => "unavailable",
            RescanReason::TooMany => "too_many",
            RescanReason::ExcludedInside => "excluded_inside",
            RescanReason::RootChanged => "root_changed",
            RescanReason::Unverifiable => "unverifiable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    /// `dirs` lists every changed directory below the path since the mark.
    Complete,
    /// No mark was given: only the current position is returned.
    MarkOnly,
    Rescan(RescanReason),
}

/// One changed directory, relative to the requested path (`""` = itself,
/// components joined with `\`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedDir {
    pub rel: String,
    /// Everything below it counts as changed too (the directory itself was
    /// created, renamed, moved or replaced).
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// The position the answer is complete up to: the client's next mark.
    /// `journal_id == 0`: the volume has no journal (no mark to keep).
    pub mark: JournalMark,
    /// Review P2-2: file reference of the requested directory (sequence
    /// included). The client keeps it with the mark; another one later
    /// (a junction retargeted, the folder replaced) means rescan.
    pub root_frn: u64,
    pub status: ChangeStatus,
    pub dirs: Vec<ChangedDir>,
}

/// Where a directory lies relative to the requested one.
#[derive(Debug, Clone)]
enum Rel {
    /// At or below `top`: its path (`""` = `top`).
    Below(String),
    /// Not an indexed directory at or below `top` (outside, gone, orphan).
    Outside,
    /// Below `top`, but its path is longer than one answer may carry.
    TooLong,
}

/// [`Rel`] of directory `frn`, memoised per directory: the work of one
/// query is bounded by the number of distinct directories on the chains,
/// not by log entries × depth.
fn rel_below(live: &LiveIndex, frn: u64, top: u64, memo: &mut HashMap<u64, Rel>) -> Rel {
    // Walk up to the first known directory, then build the paths down.
    let mut chain: Vec<(u64, &str)> = Vec::new();
    let mut cur = frn;
    let base = loop {
        if let Some(r) = memo.get(&cur) {
            break r.clone();
        }
        if cur == top {
            break Rel::Below(String::new());
        }
        if cur == live.index.root_frn() || chain.len() > MAX_DEPTH {
            break Rel::Outside;
        }
        let Some(e) = live
            .index
            .record_entries(cur & crate::index::RECORD_MASK)
            .into_iter()
            .find(|e| e.frn == cur && e.kind == Kind::Dir)
        else {
            break Rel::Outside;
        };
        chain.push((cur, e.name));
        cur = e.parent;
    };
    if chain.is_empty() {
        memo.entry(frn).or_insert_with(|| base.clone());
        return base;
    }
    let mut r = base;
    for (dir, name) in chain.into_iter().rev() {
        r = match r {
            Rel::Below(p) => {
                let next = if p.is_empty() {
                    name.to_owned()
                } else {
                    format!("{p}\\{name}")
                };
                if next.len() > MAX_PATH_BYTES {
                    Rel::TooLong
                } else {
                    Rel::Below(next)
                }
            }
            other => other,
        };
        memo.insert(dir, r.clone());
    }
    r
}

/// Directories below `path` whose listing changed since `since` (FER-477b).
///
/// The caller (the server) has already run the freshness barrier: the
/// index read the journal to its end in a worker pass that started after
/// the request. `access` = `None` in high_performance, the client's
/// AccessCheck in shared: `path` must be visible (else `NotIndexed`, like
/// search), and a changed directory is listed only when the client can list
/// it and every directory above it.
///
/// Never a wrong "complete" (security review FER-477): a mark ahead of the
/// journal (P2-1), a replaced `path` or directory above it (P2-2), unknown
/// open write sessions (P2-3) and an AccessCheck that cannot be evaluated
/// (P2-4) all answer rescan. Open write sessions below `path` are always
/// listed, whatever the window. The answer limits are counted AFTER the
/// shared-mode filter (P3).
pub fn changes(
    volumes: &[ServedVolume],
    path: &str,
    since: Option<JournalMark>,
    access: Option<&mut dyn DirAccess>,
    abort: &dyn Fn() -> bool,
) -> Result<Changes, QueryError> {
    if path.len() > MAX_SCOPE {
        return Err(QueryError::BadQuery("path"));
    }
    let (letter, comps) = parse_abs_dir(path).ok_or(QueryError::BadQuery("path"))?;
    let v = volumes
        .iter()
        .find(|v| v.letter == letter)
        .ok_or(QueryError::NotIndexed)?;
    let ix = v.index.as_ref().ok_or(QueryError::NotIndexed)?;
    let mut vis = Visibility::new(letter, access);
    let (top, excluded_inside) = resolve_dir(ix, &comps, &mut vis)?;
    enum Pre {
        MarkOnly,
        Rescan(RescanReason),
        /// (directory, recursive), deduplicated, all at or below `top`.
        Candidates(Vec<(u64, bool)>),
    }
    // One consistent snapshot: the mark, the log window, the open sessions.
    let (mark, pre) = {
        let live = read_index(ix);
        let Some(jid) = live.journal_id else {
            return Ok(Changes {
                mark: JournalMark {
                    journal_id: 0,
                    usn: 0,
                },
                root_frn: top,
                status: ChangeStatus::Rescan(RescanReason::Unavailable),
                dirs: Vec::new(),
            });
        };
        let mark = JournalMark {
            journal_id: jid,
            usn: live.next_usn,
        };
        let pre = match since {
            None => Pre::MarkOnly,
            Some(_) if v.stale => Pre::Rescan(RescanReason::Unavailable),
            Some(s) if s.journal_id != jid => Pre::Rescan(RescanReason::JournalChanged),
            // P2-1: a mark AHEAD of the journal (volume restored from an
            // image or snapshot: same journal ID, USN went back) proves
            // nothing about the changes since.
            Some(s) if s.usn > live.next_usn => Pre::Rescan(RescanReason::Unavailable),
            Some(_) if excluded_inside => Pre::Rescan(RescanReason::ExcludedInside),
            // P2-3: an open session may be unknown.
            Some(_) if live.changes.open_unknown() => Pre::Rescan(RescanReason::Unavailable),
            Some(s) => match live.changes.since(s.usn) {
                None => Pre::Rescan(RescanReason::Unavailable),
                Some(logged) => {
                    // P2-2: `path` or a directory above it replaced since the
                    // mark (another folder with the same name may sit there).
                    let mut cur = top;
                    let mut replaced = false;
                    for _ in 0..=MAX_DEPTH {
                        if live.changes.recursive_since(cur, s.usn) {
                            replaced = true;
                            break;
                        }
                        if cur == live.index.root_frn() {
                            break;
                        }
                        match live.index.dir_parent(cur) {
                            Some(p) => cur = p,
                            None => break,
                        }
                    }
                    if replaced {
                        Pre::Rescan(RescanReason::RootChanged)
                    } else {
                        let mut memo = HashMap::new();
                        let mut c: HashMap<u64, bool> = HashMap::new();
                        for (frn, rec) in logged {
                            if under(&live, frn, top, &mut memo) {
                                *c.entry(frn).or_default() |= rec;
                            }
                        }
                        // P2-3: parents of open write sessions, as named by
                        // the journal and by the record's current names.
                        for (rec, parents) in live.changes.open_writes() {
                            let now = live.index.record_entries(rec);
                            for p in parents.iter().copied().chain(now.iter().map(|e| e.parent)) {
                                if under(&live, p, top, &mut memo) {
                                    c.entry(p).or_default();
                                }
                            }
                        }
                        Pre::Candidates(c.into_iter().collect())
                    }
                }
            },
        };
        (mark, pre)
    };
    if abort() {
        return Err(QueryError::Aborted);
    }
    let answer = |status, dirs| {
        Ok(Changes {
            mark,
            root_frn: top,
            status,
            dirs,
        })
    };
    let candidates = match pre {
        Pre::MarkOnly => return answer(ChangeStatus::MarkOnly, Vec::new()),
        Pre::Rescan(why) => return answer(ChangeStatus::Rescan(why), Vec::new()),
        Pre::Candidates(c) => c,
    };
    // Shared mode: keep only directories the client may list (with every
    // directory above); AccessChecks without the index lock (review N3). A
    // decision that cannot be evaluated is never a silent omission (P2-4).
    let mut kept: Vec<(u64, bool)> = Vec::new();
    for (frn, recursive) in candidates {
        let a = loop {
            let pending = {
                let live = read_index(ix);
                match vis.children_access(&live, frn) {
                    Ok(a) => break a,
                    Err(m) => m,
                }
            };
            vis.resolve(&pending);
        };
        match a {
            Access::Yes => kept.push((frn, recursive)),
            Access::No => {}
            Access::Unknown => {
                return answer(ChangeStatus::Rescan(RescanReason::Unverifiable), Vec::new());
            }
        }
        // P3: the limits count what the client may see, not the rest.
        if kept.len() > MAX_CHANGED_DIRS {
            return answer(ChangeStatus::Rescan(RescanReason::TooMany), Vec::new());
        }
    }
    // Paths of what is kept (one lock hold, memoised per directory). A path
    // too long for the answer, or too many bytes, means rescan.
    let mut dirs = Vec::with_capacity(kept.len());
    {
        let live = read_index(ix);
        let mut memo = HashMap::new();
        let mut bytes = 0usize;
        for (frn, recursive) in kept {
            match rel_below(&live, frn, top, &mut memo) {
                // Moved out between the lock holds: it is listed (or not)
                // through its old and new parents, which are logged.
                Rel::Outside => {}
                Rel::TooLong => {
                    return answer(ChangeStatus::Rescan(RescanReason::TooMany), Vec::new());
                }
                Rel::Below(rel) => {
                    bytes += 8 + rel.len();
                    if bytes > CHANGED_BYTES {
                        return answer(ChangeStatus::Rescan(RescanReason::TooMany), Vec::new());
                    }
                    dirs.push(ChangedDir { rel, recursive });
                }
            }
        }
    }
    if abort() {
        return Err(QueryError::Aborted);
    }
    dirs.sort_by(|a, b| a.rel.cmp(&b.rel));
    answer(ChangeStatus::Complete, dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LiveIndex;
    use crate::index::{Meta, VolumeIndex};
    use crate::proto::Mode;
    use crate::scope::Scope;
    use std::collections::HashSet;
    use std::sync::{Arc, RwLock};

    const ROOT: u64 = 5 | (5 << 48);
    fn f(rec: u64) -> u64 {
        rec | (1 << 48)
    }
    fn m(size: u64) -> Meta {
        Meta {
            size,
            allocated: Some(size),
            modified: 9,
            attributes: 0x20,
        }
    }

    /// C:\Users\bob (excluded profile, empty), C:\Data\{a.txt, Report.DOC,
    /// sub\deep.txt}, C:\Secret\{plan.txt} (not listable in the fake
    /// access), C:\top.txt
    fn live() -> LiveIndex {
        let mut ix = VolumeIndex::new(ROOT);
        let d = |ix: &mut VolumeIndex, r, p, n| {
            ix.add_name(f(r), p, Kind::Dir, Meta::default(), n).unwrap()
        };
        let fl =
            |ix: &mut VolumeIndex, r, p, n, s| ix.add_name(f(r), p, Kind::File, m(s), n).unwrap();
        d(&mut ix, 20, ROOT, "Users");
        d(&mut ix, 21, f(20), "bob");
        d(&mut ix, 30, ROOT, "Data");
        fl(&mut ix, 31, f(30), "a.txt", 10);
        fl(&mut ix, 32, f(30), "Report.DOC", 20);
        d(&mut ix, 33, f(30), "sub");
        fl(&mut ix, 34, f(33), "deep.txt", 5);
        d(&mut ix, 40, ROOT, "Secret");
        fl(&mut ix, 41, f(40), "plan.txt", 7);
        fl(&mut ix, 50, ROOT, "top.txt", 1);
        let mut excluded = HashSet::new();
        excluded.insert(21u64);
        LiveIndex {
            index: ix,
            excluded,
            scope: Scope::for_volume(Mode::Shared, 'C', &[r"C:\Users\bob".into()]),
            journal_id: Some(1),
            next_usn: 0,
            dirty: false,
            stats: Default::default(),
            failed_rounds: 0,
            security_changes: 0,
            changes: crate::changelog::ChangeLog::new(0),
            caught_pass: 0,
        }
    }

    fn served(stale: bool) -> Vec<ServedVolume> {
        vec![
            ServedVolume {
                letter: b'C',
                index: Some(Arc::new(RwLock::new(live()))),
                stale,
            },
            ServedVolume {
                letter: b'D',
                index: None,
                stale: false,
            },
        ]
    }

    /// Denies listing `Secret` (record 40); counts checks.
    struct Fake {
        checks: usize,
    }
    impl DirAccess for Fake {
        fn can_list(&mut self, _: u8, frn: u64) -> bool {
            self.checks += 1;
            frn != f(40)
        }
    }

    fn q(pattern: &str, kind: MatchKind) -> SearchQuery {
        SearchQuery {
            pattern: pattern.into(),
            match_kind: kind,
            scope: String::new(),
            extensions: vec![],
            kinds: KindFilter::Both,
            limit: 100,
            cursor: QueryCursor::default(),
        }
    }

    fn paths(p: &SearchPage) -> Vec<&str> {
        let mut v: Vec<&str> = p.hits.iter().map(|h| h.path.as_str()).collect();
        v.sort();
        v
    }

    fn run(vols: &[ServedVolume], q: &SearchQuery, acc: Option<&mut dyn DirAccess>) -> SearchPage {
        search(vols, q, acc, Instant::now() + SEARCH_TIME, &|| false).unwrap()
    }

    #[test]
    fn glob_substring_prefix_are_case_insensitive() {
        let v = served(false);
        assert_eq!(
            paths(&run(&v, &q("*.TXT", MatchKind::Glob), None)),
            vec![
                r"C:\Data\a.txt",
                r"C:\Data\sub\deep.txt",
                r"C:\Secret\plan.txt",
                r"C:\top.txt"
            ]
        );
        assert_eq!(
            paths(&run(&v, &q("report", MatchKind::Substring), None)),
            vec![r"C:\Data\Report.DOC"]
        );
        assert_eq!(
            paths(&run(&v, &q("DE", MatchKind::Prefix), None)),
            vec![r"C:\Data\sub\deep.txt"]
        );
        assert_eq!(
            paths(&run(&v, &q("?.txt", MatchKind::Glob), None)),
            vec![r"C:\Data\a.txt"]
        );
        let p = run(&v, &q("a.txt", MatchKind::Glob), None);
        assert_eq!(p.unavailable, 1 << 3, "D: has no index");
        assert!(p.next.is_none());
    }

    #[test]
    fn glob_edge_cases() {
        let g = |p: &str, n: &str| Matcher::new(MatchKind::Glob, p).matches(n);
        assert!(g("*", "x"));
        assert!(g("a*b*c", "aXXbYYc"));
        assert!(!g("a*b*c", "aXXbYY"));
        assert!(g("*.tar.gz", "x.TAR.gz"));
        assert!(g("ä?", "Äb"));
        assert!(!g("ä?", "Ä"));
        assert!(g("**a", "a"));
        assert!(!g("abc", "abcd"));
        // Pathological backtracking stays linear-ish.
        let long = "a".repeat(255);
        assert!(!g(&format!("{}b", "*a".repeat(100)), &long));
    }

    #[test]
    fn filters_scope_extensions_and_kinds() {
        let v = served(false);
        let mut s = q("*", MatchKind::Glob);
        s.scope = r"c:\data".into();
        assert_eq!(
            paths(&run(&v, &s, None)),
            vec![
                r"C:\Data\Report.DOC",
                r"C:\Data\a.txt",
                r"C:\Data\sub",
                r"C:\Data\sub\deep.txt"
            ]
        );
        s.extensions = vec!["doc".into()];
        assert_eq!(paths(&run(&v, &s, None)), vec![r"C:\Data\Report.DOC"]);
        s.extensions.clear();
        s.kinds = KindFilter::Dirs;
        assert_eq!(paths(&run(&v, &s, None)), vec![r"C:\Data\sub"]);
        s.kinds = KindFilter::Files;
        assert_eq!(run(&v, &s, None).hits.len(), 3);
    }

    #[test]
    fn excluded_missing_or_unlistable_scopes_are_all_not_indexed() {
        let v = served(false);
        for scope in [
            r"C:\Users\bob",
            r"C:\Users\bob\Documents",
            r"C:\Nope",
            r"D:\",
        ] {
            let mut s = q("*", MatchKind::Glob);
            s.scope = scope.into();
            assert_eq!(
                search(&v, &s, None, Instant::now() + SEARCH_TIME, &|| false),
                Err(QueryError::NotIndexed),
                "{scope}"
            );
        }
        let mut s = q("*", MatchKind::Glob);
        s.scope = r"C:\Secret".into();
        let mut acc = Fake { checks: 0 };
        assert_eq!(
            search(
                &v,
                &s,
                Some(&mut acc),
                Instant::now() + SEARCH_TIME,
                &|| false
            ),
            Err(QueryError::NotIndexed)
        );
        // The root scope reports the excluded profile inside.
        s.scope = r"C:\".into();
        assert!(run(&v, &s, None).excluded_inside);
        s.scope = r"C:\Data".into();
        assert!(!run(&v, &s, None).excluded_inside);
    }

    #[test]
    fn shared_mode_hides_names_below_unlistable_directories() {
        let v = served(false);
        let mut acc = Fake { checks: 0 };
        let p = run(&v, &q("*.txt", MatchKind::Glob), Some(&mut acc));
        assert_eq!(
            paths(&p),
            vec![r"C:\Data\a.txt", r"C:\Data\sub\deep.txt", r"C:\top.txt"],
            "plan.txt is below Secret"
        );
        // Memoised: every directory with children (root, Users, Data, sub,
        // Secret) is checked once — names do not change that (review L3).
        assert_eq!(acc.checks, 5, "{}", acc.checks);
        // The unlistable directory's own name stays visible (its parent,
        // the root, is listable) — like Explorer.
        let p = run(
            &v,
            &q("secret", MatchKind::Glob),
            Some(&mut Fake { checks: 0 }),
        );
        assert_eq!(paths(&p), vec![r"C:\Secret"]);
    }

    /// Review L3: the AccessCheck work does not depend on whether a hidden
    /// name matches the pattern.
    #[test]
    fn access_checks_do_not_depend_on_the_pattern() {
        let v = served(false);
        let mut a = Fake { checks: 0 };
        run(&v, &q("plan.txt", MatchKind::Glob), Some(&mut a));
        let mut b = Fake { checks: 0 };
        run(&v, &q("nothing-matches", MatchKind::Glob), Some(&mut b));
        assert_eq!(a.checks, b.checks);
    }

    /// Review N3: AccessChecks never run while the query holds the index
    /// read lock (the fake proves it by taking the WRITE lock inside every
    /// check), for search, scope resolution and subtree alike; cached
    /// decisions are used under the lock.
    #[test]
    fn access_checks_run_without_the_index_lock() {
        struct LockProbe {
            ix: Arc<RwLock<LiveIndex>>,
            checks: usize,
            known: HashMap<u64, bool>,
        }
        impl DirAccess for LockProbe {
            fn cached(&mut self, _: u8, frn: u64) -> Option<bool> {
                self.known.get(&frn).copied()
            }
            fn can_list(&mut self, _: u8, frn: u64) -> bool {
                assert!(
                    self.ix.try_write().is_ok(),
                    "index lock held during AccessCheck"
                );
                self.checks += 1;
                let v = frn != f(40);
                self.known.insert(frn, v);
                v
            }
        }
        let v = served(false);
        let ix = v[0].index.clone().unwrap();
        let mut p = LockProbe {
            ix: ix.clone(),
            checks: 0,
            known: HashMap::new(),
        };
        let mut s = q("*.txt", MatchKind::Glob);
        s.scope = r"C:\Data".into();
        let page = run(&v, &s, Some(&mut p));
        assert_eq!(page.hits.len(), 2);
        let first = p.checks;
        assert!(first > 0);
        // Warm: everything comes from `cached`, no new checks.
        run(&v, &q("*.txt", MatchKind::Glob), Some(&mut p));
        let mut seen = 0;
        subtree(
            &v,
            r"C:\",
            Some(&mut p),
            Instant::now() + SUBTREE_TIME,
            &|| false,
            &mut |_| true,
            &mut |c| {
                seen += c.len();
                true
            },
        )
        .unwrap();
        assert!(seen > 0);
    }

    /// Review M1: a full page is not returned once the mode changed.
    #[test]
    fn full_page_and_deadline_returns_check_abort() {
        let v = served(false);
        let mut s = q("*", MatchKind::Glob);
        s.limit = 1;
        let calls = std::cell::Cell::new(0);
        // Abort turns true after the first chunk check.
        let abort = || {
            calls.set(calls.get() + 1);
            calls.get() > 1
        };
        assert_eq!(
            search(&v, &s, None, Instant::now() + SEARCH_TIME, &abort),
            Err(QueryError::Aborted)
        );
    }

    #[test]
    fn paging_by_limit_and_cursor_covers_everything_once() {
        let v = served(true);
        let mut s = q("*", MatchKind::Glob);
        s.limit = 2;
        let mut all = Vec::new();
        for _ in 0..20 {
            let p = run(&v, &s, None);
            assert!(p.hits.len() <= 2);
            assert!(p.stale);
            all.extend(p.hits.into_iter().map(|h| h.path));
            match p.next {
                Some(c) => s.cursor = c,
                None => break,
            }
        }
        all.sort();
        let mut dedup = all.clone();
        dedup.dedup();
        assert_eq!(all, dedup);
        assert_eq!(all.len(), 10);
    }

    #[test]
    fn deadline_returns_a_partial_page_with_cursor() {
        let v = served(false);
        let p = search(&v, &q("*", MatchKind::Glob), None, Instant::now(), &|| {
            false
        })
        .unwrap();
        // The first chunk covers this tiny index; with more slots than one
        // chunk, `next` would point inside the volume.
        assert!(p.next.is_none() || p.next.unwrap().letter == b'C');
        assert_eq!(
            search(
                &v,
                &q("*", MatchKind::Glob),
                None,
                Instant::now() + SEARCH_TIME,
                &|| true
            ),
            Err(QueryError::Aborted)
        );
    }

    #[test]
    fn query_validation() {
        let ok = q("x", MatchKind::Glob);
        assert!(ok.validate().is_ok());
        for bad in [
            SearchQuery {
                pattern: String::new(),
                ..ok.clone()
            },
            SearchQuery {
                pattern: "a\\b".into(),
                ..ok.clone()
            },
            SearchQuery {
                pattern: "a\u{1}".into(),
                ..ok.clone()
            },
            SearchQuery {
                pattern: "x".repeat(MAX_PATTERN + 1),
                ..ok.clone()
            },
            SearchQuery {
                scope: "relative".into(),
                ..ok.clone()
            },
            SearchQuery {
                scope: r"C:\a\..\b".into(),
                ..ok.clone()
            },
            SearchQuery {
                scope: r"\\srv\share".into(),
                ..ok.clone()
            },
            SearchQuery {
                extensions: vec!["TXT".into()],
                ..ok.clone()
            },
            SearchQuery {
                extensions: vec![".txt".into()],
                ..ok.clone()
            },
            SearchQuery {
                limit: 0,
                ..ok.clone()
            },
            SearchQuery {
                limit: MAX_PAGE + 1,
                ..ok.clone()
            },
            SearchQuery {
                cursor: QueryCursor {
                    letter: b'c',
                    slot: 0,
                },
                ..ok.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert_eq!(parse_abs_dir(r"c:\"), Some((b'C', vec![])));
        assert_eq!(parse_abs_dir(r"C:\a\b\"), Some((b'C', vec!["a", "b"])));
        assert_eq!(parse_abs_dir(r"C:\a\\b"), None);
        assert_eq!(parse_abs_dir("C:"), None);
    }

    #[test]
    fn subtree_streams_visible_entries_below_the_root() {
        let v = served(false);
        let mut info = None;
        let mut got: Vec<SubtreeEntry> = Vec::new();
        let n = subtree(
            &v,
            r"C:\Data",
            None,
            Instant::now() + SUBTREE_TIME,
            &|| false,
            &mut |i| {
                info = Some(*i);
                true
            },
            &mut |c| {
                got.extend(c);
                true
            },
        )
        .unwrap();
        assert_eq!(n, 4);
        assert_eq!(info.unwrap().root_frn, f(30));
        let mut names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["Report.DOC", "a.txt", "deep.txt", "sub"]);
        // Shared: from the root, Secret is listed (name) but its content
        // is not; the excluded profile is flagged.
        let mut got = Vec::new();
        let mut info = None;
        let mut acc = Fake { checks: 0 };
        subtree(
            &v,
            r"C:\",
            Some(&mut acc),
            Instant::now() + SUBTREE_TIME,
            &|| false,
            &mut |i| {
                info = Some(*i);
                true
            },
            &mut |c| {
                got.extend(c);
                true
            },
        )
        .unwrap();
        assert!(got.iter().any(|e| e.name == "Secret"));
        assert!(!got.iter().any(|e| e.name == "plan.txt"));
        assert!(info.unwrap().excluded_inside);
        // Not indexed / unlistable / excluded → NotIndexed; client gone →
        // Aborted.
        for p in [r"C:\Users\bob", r"D:\", r"C:\Missing"] {
            assert_eq!(
                subtree(
                    &v,
                    p,
                    None,
                    Instant::now() + SUBTREE_TIME,
                    &|| false,
                    &mut |_| true,
                    &mut |_| true
                ),
                Err(QueryError::NotIndexed),
                "{p}"
            );
        }
        assert_eq!(
            subtree(
                &v,
                r"C:\",
                None,
                Instant::now() + SUBTREE_TIME,
                &|| false,
                &mut |_| true,
                &mut |_| false
            ),
            Err(QueryError::Aborted)
        );
        assert_eq!(
            subtree(
                &v,
                r"C:\",
                None,
                Instant::now(),
                &|| false,
                &mut |_| true,
                &mut |_| true
            ),
            Err(QueryError::Timeout)
        );
    }

    // ── FER-477a: folder sizes ───────────────────────────────────────────

    fn sizes(v: &[ServedVolume], parent: &str, names: &[&str]) -> Result<FolderSizes, QueryError> {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        folder_sizes(
            v,
            parent,
            &names,
            Instant::now() + FOLDER_SIZES_TIME,
            &|| false,
        )
    }

    #[test]
    fn folder_sizes_sum_files_below_each_child() {
        let v = served(true);
        let r = sizes(
            &v,
            r"C:\",
            &["Data", "Secret", "top.txt", "Nope", "Users", "data"],
        )
        .unwrap();
        assert!(r.stale);
        assert_eq!(
            r.sizes,
            vec![
                FolderSize::Ok {
                    bytes: 35,
                    files: 3
                },
                FolderSize::Ok { bytes: 7, files: 1 },
                FolderSize::NotIndexed, // a file
                FolderSize::NotIndexed, // missing
                FolderSize::Ok { bytes: 0, files: 0 },
                FolderSize::Ok {
                    bytes: 35,
                    files: 3
                }, // case-insensitive
            ]
        );
        let r = sizes(&v, r"c:\data", &["sub"]).unwrap();
        assert_eq!(r.sizes, vec![FolderSize::Ok { bytes: 5, files: 1 }]);
        // An excluded child or parent is not answered.
        assert_eq!(
            sizes(&v, r"C:\Users", &["bob"]).unwrap().sizes,
            vec![FolderSize::NotIndexed]
        );
        assert_eq!(
            sizes(&v, r"C:\Users\bob", &["x"]),
            Err(QueryError::NotIndexed)
        );
        assert_eq!(sizes(&v, r"D:\", &["x"]), Err(QueryError::NotIndexed));
        assert_eq!(sizes(&v, r"E:\", &["x"]), Err(QueryError::NotIndexed));
    }

    #[test]
    fn folder_sizes_validate_and_stop() {
        let v = served(false);
        for bad in [&[][..], &[".."][..], &["a:b"][..], &["a\\b"][..], &[""][..]] {
            assert!(
                matches!(sizes(&v, r"C:\", bad), Err(QueryError::BadQuery(_))),
                "{bad:?}"
            );
        }
        let many: Vec<String> = (0..=MAX_FOLDER_NAMES).map(|i| format!("d{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(matches!(
            sizes(&v, r"C:\", &refs),
            Err(QueryError::BadQuery(_))
        ));
        assert!(matches!(
            sizes(&v, r"C:\a\..\b", &["x"]),
            Err(QueryError::BadQuery(_))
        ));
        let names = vec!["Data".to_string()];
        assert_eq!(
            folder_sizes(&v, r"C:\", &names, Instant::now(), &|| false),
            Err(QueryError::Timeout)
        );
        assert_eq!(
            folder_sizes(
                &v,
                r"C:\",
                &names,
                Instant::now() + FOLDER_SIZES_TIME,
                &|| true
            ),
            Err(QueryError::Aborted)
        );
    }

    // ── FER-477b: changes since a mark ───────────────────────────────────

    /// `live()` with a journal at 500 and a log complete from 100: Data
    /// (200), Data\sub recursive (300), Secret (250), root (260) and a
    /// directory that no longer exists (270).
    fn served_with_log(stale: bool) -> Vec<ServedVolume> {
        let mut l = live();
        l.next_usn = 500;
        l.changes = crate::changelog::ChangeLog::new(100);
        l.changes.record(f(30), false, 200);
        l.changes.record(f(33), true, 300);
        l.changes.record(f(40), false, 250);
        l.changes.record(ROOT, false, 260);
        l.changes.record(f(99), true, 270);
        vec![ServedVolume {
            letter: b'C',
            index: Some(Arc::new(RwLock::new(l))),
            stale,
        }]
    }

    fn mark(usn: i64) -> Option<JournalMark> {
        Some(JournalMark { journal_id: 1, usn })
    }

    fn rels(c: &Changes) -> Vec<(&str, bool)> {
        c.dirs
            .iter()
            .map(|d| (d.rel.as_str(), d.recursive))
            .collect()
    }

    #[test]
    fn changes_list_directories_below_the_path_since_the_mark() {
        let v = served_with_log(false);
        let c = changes(&v, r"C:\Data", mark(100), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Complete);
        assert_eq!(
            c.mark,
            JournalMark {
                journal_id: 1,
                usn: 500
            }
        );
        assert_eq!(rels(&c), vec![("", false), ("sub", true)]);
        // A later mark sees only what changed after it.
        let c = changes(&v, r"C:\Data", mark(250), None, &|| false).unwrap();
        assert_eq!(rels(&c), vec![("sub", true)]);
        let c = changes(&v, r"C:\Data", mark(301), None, &|| false).unwrap();
        assert_eq!((c.status, c.dirs.len()), (ChangeStatus::Complete, 0));
        // No mark: position only.
        let c = changes(&v, r"C:\Data", None, None, &|| false).unwrap();
        assert_eq!((c.status, c.mark.usn), (ChangeStatus::MarkOnly, 500));
    }

    #[test]
    fn changes_never_claim_completeness_they_do_not_have() {
        let v = served_with_log(false);
        let rescan = |v: &[ServedVolume], path: &str, m| {
            changes(v, path, m, None, &|| false).unwrap().status
        };
        assert_eq!(
            rescan(&v, r"C:\Data", mark(99)),
            ChangeStatus::Rescan(RescanReason::Unavailable),
            "before the log floor"
        );
        assert_eq!(
            rescan(
                &v,
                r"C:\Data",
                Some(JournalMark {
                    journal_id: 2,
                    usn: 400
                })
            ),
            ChangeStatus::Rescan(RescanReason::JournalChanged)
        );
        assert_eq!(
            rescan(&v, r"C:\", mark(100)),
            ChangeStatus::Rescan(RescanReason::ExcludedInside),
            "the root holds an excluded profile"
        );
        assert_eq!(
            rescan(&served_with_log(true), r"C:\Data", mark(100)),
            ChangeStatus::Rescan(RescanReason::Unavailable),
            "a stale index is not followed"
        );
        // Snapshot without a journal: no mark at all.
        let mut l = live();
        l.journal_id = None;
        let snap = vec![ServedVolume {
            letter: b'C',
            index: Some(Arc::new(RwLock::new(l))),
            stale: false,
        }];
        let c = changes(&snap, r"C:\Data", mark(100), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Rescan(RescanReason::Unavailable));
        assert_eq!(c.mark.journal_id, 0);
        // Missing, excluded and unindexed paths answer NotIndexed.
        for p in [r"C:\Nope", r"C:\Users\bob", r"D:\"] {
            assert_eq!(
                changes(&v, p, mark(100), None, &|| false),
                Err(QueryError::NotIndexed),
                "{p}"
            );
        }
        assert_eq!(
            changes(&v, r"C:\Data", mark(100), None, &|| true),
            Err(QueryError::Aborted)
        );
    }

    #[test]
    fn changes_hide_directories_the_client_cannot_list() {
        // Shared mode: Data\sub is not listable; its change is not listed,
        // and an unlistable path is NotIndexed (no oracle).
        struct DenySub;
        impl DirAccess for DenySub {
            fn can_list(&mut self, _: u8, frn: u64) -> bool {
                frn != f(33)
            }
        }
        let v = served_with_log(false);
        let c = changes(&v, r"C:\Data", mark(100), Some(&mut DenySub), &|| false).unwrap();
        assert_eq!(rels(&c), vec![("", false)]);
        assert_eq!(
            changes(&v, r"C:\Data\sub", mark(100), Some(&mut DenySub), &|| false),
            Err(QueryError::NotIndexed)
        );
    }

    #[test]
    fn too_many_changed_directories_means_rescan() {
        let mut ix = VolumeIndex::new(ROOT);
        ix.add_name(f(30), ROOT, Kind::Dir, Meta::default(), "Data")
            .unwrap();
        let mut log = crate::changelog::ChangeLog::new(0);
        for i in 0..=MAX_CHANGED_DIRS as u64 {
            let rec = 100 + i;
            ix.add_name(f(rec), f(30), Kind::Dir, Meta::default(), &format!("d{i}"))
                .unwrap();
            log.record(f(rec), false, 10);
        }
        let mut l = live();
        l.index = ix;
        l.excluded.clear();
        l.changes = log;
        let v = vec![ServedVolume {
            letter: b'C',
            index: Some(Arc::new(RwLock::new(l))),
            stale: false,
        }];
        assert_eq!(
            changes(&v, r"C:\Data", mark(0), None, &|| false)
                .unwrap()
                .status,
            ChangeStatus::Rescan(RescanReason::TooMany)
        );
    }

    /// Review N3 for FER-477b: the AccessChecks of `changes` (path and every
    /// listed directory) never run while the index read lock is held.
    #[test]
    fn changes_access_checks_run_without_the_index_lock() {
        struct LockProbe {
            ix: Arc<RwLock<LiveIndex>>,
            checks: usize,
        }
        impl DirAccess for LockProbe {
            fn can_list(&mut self, _: u8, _: u64) -> bool {
                assert!(
                    self.ix.try_write().is_ok(),
                    "index lock held during AccessCheck"
                );
                self.checks += 1;
                true
            }
        }
        let v = served_with_log(false);
        let ix = v[0].index.clone().unwrap();
        let mut p = LockProbe { ix, checks: 0 };
        let c = changes(&v, r"C:\Data", mark(100), Some(&mut p), &|| false).unwrap();
        assert_eq!(rels(&c), vec![("", false), ("sub", true)]);
        assert!(p.checks > 0);
    }

    /// A changed directory whose path is too long for the answer is never
    /// dropped silently: the answer says rescan.
    #[test]
    fn a_too_long_changed_path_means_rescan() {
        let mut ix = VolumeIndex::new(ROOT);
        ix.add_name(f(30), ROOT, Kind::Dir, Meta::default(), "Data")
            .unwrap();
        let mut parent = f(30);
        for i in 0..20u64 {
            let rec = 100 + i;
            ix.add_name(f(rec), parent, Kind::Dir, Meta::default(), &"x".repeat(250))
                .unwrap();
            parent = f(rec);
        }
        let mut log = crate::changelog::ChangeLog::new(0);
        log.record(parent, false, 10);
        let mut l = live();
        l.index = ix;
        l.excluded.clear();
        l.changes = log;
        let v = vec![ServedVolume {
            letter: b'C',
            index: Some(Arc::new(RwLock::new(l))),
            stale: false,
        }];
        assert_eq!(
            changes(&v, r"C:\Data", mark(0), None, &|| false)
                .unwrap()
                .status,
            ChangeStatus::Rescan(RescanReason::TooMany)
        );
    }

    // ── FER-477 security review P2/P3 ───────────────────────────────────

    #[test]
    fn p2_1_a_mark_ahead_of_the_journal_means_rescan() {
        // Volume restored from an image: same journal, USN went back.
        let v = served_with_log(false);
        let c = changes(&v, r"C:\Data", mark(10_000), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Rescan(RescanReason::Unavailable));
        // At the position exactly: complete (nothing since).
        let c = changes(&v, r"C:\Data", mark(500), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Complete);
    }

    #[test]
    fn p2_2_a_replaced_path_or_ancestor_means_rescan() {
        // Data\sub itself was replaced (recursive at 300).
        let v = served_with_log(false);
        let c = changes(&v, r"C:\Data\sub", mark(100), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Rescan(RescanReason::RootChanged));
        assert_eq!(c.root_frn, f(33));
        // After the replacement: fine again.
        let c = changes(&v, r"C:\Data\sub", mark(301), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Complete);
        // An ANCESTOR replaced (Data recursive): the subtree below it is
        // another one now.
        let mut l = live();
        l.next_usn = 500;
        l.changes = crate::changelog::ChangeLog::new(100);
        l.changes.record(f(30), true, 200);
        let v = vec![ServedVolume {
            letter: b'C',
            index: Some(Arc::new(RwLock::new(l))),
            stale: false,
        }];
        let c = changes(&v, r"C:\Data\sub", mark(100), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Rescan(RescanReason::RootChanged));
        // The root frn is always reported (also for a mark).
        let c = changes(&v, r"C:\Data", None, None, &|| false).unwrap();
        assert_eq!((c.status, c.root_frn), (ChangeStatus::MarkOnly, f(30)));
    }

    #[test]
    fn p2_3_open_write_sessions_are_always_listed() {
        use crate::usn::{UsnChange, reason};
        let mut l = live();
        l.next_usn = 500;
        l.changes = crate::changelog::ChangeLog::new(100);
        // a.txt (record 31 in Data) written, handle still open; its record
        // is older than any later mark.
        l.changes.note_write(&UsnChange {
            usn: 150,
            frn: f(31),
            parent: f(30),
            reason: reason::DATA_OVERWRITE,
        });
        let ix = Arc::new(RwLock::new(l));
        let v = vec![ServedVolume {
            letter: b'C',
            index: Some(ix.clone()),
            stale: false,
        }];
        // A window after the session started: still listed.
        let c = changes(&v, r"C:\Data", mark(400), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Complete);
        assert_eq!(rels(&c), vec![("", false)]);
        // Closed: no longer listed.
        ix.write().unwrap().changes.note_write(&UsnChange {
            usn: 450,
            frn: f(31),
            parent: f(30),
            reason: reason::CLOSE,
        });
        let c = changes(&v, r"C:\Data", mark(400), None, &|| false).unwrap();
        assert!(c.dirs.is_empty());
        // Sessions unknown: rescan.
        ix.write().unwrap().changes.set_open_unknown();
        let c = changes(&v, r"C:\Data", mark(400), None, &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Rescan(RescanReason::Unavailable));
    }

    #[test]
    fn p2_4_an_access_check_that_cannot_be_evaluated_means_rescan() {
        struct Unreadable;
        impl DirAccess for Unreadable {
            fn can_list(&mut self, _: u8, frn: u64) -> bool {
                frn != f(33)
            }
            fn check(&mut self, _: u8, frn: u64) -> Access {
                if frn == f(33) {
                    Access::Unknown
                } else {
                    Access::Yes
                }
            }
        }
        let v = served_with_log(false);
        let c = changes(&v, r"C:\Data", mark(100), Some(&mut Unreadable), &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Rescan(RescanReason::Unverifiable));
        // Search and subtree keep failing closed for the same directory.
        let p = run(&v, &q("*.txt", MatchKind::Glob), Some(&mut Unreadable));
        assert!(!paths(&p).contains(&r"C:\Data\sub\deep.txt"));
    }

    #[test]
    fn p3_the_answer_limit_counts_only_visible_directories() {
        struct FewVisible;
        impl DirAccess for FewVisible {
            fn can_list(&mut self, _: u8, frn: u64) -> bool {
                (frn & crate::index::RECORD_MASK) < 103
            }
        }
        let mut ix = VolumeIndex::new(ROOT);
        ix.add_name(f(30), ROOT, Kind::Dir, Meta::default(), "Data")
            .unwrap();
        let mut log = crate::changelog::ChangeLog::new(0);
        for i in 0..=MAX_CHANGED_DIRS as u64 {
            let rec = 100 + i;
            ix.add_name(f(rec), f(30), Kind::Dir, Meta::default(), &format!("d{i}"))
                .unwrap();
            log.record(f(rec), false, 10);
        }
        let mut l = live();
        l.index = ix;
        l.excluded.clear();
        l.changes = log;
        let v = vec![ServedVolume {
            letter: b'C',
            index: Some(Arc::new(RwLock::new(l))),
            stale: false,
        }];
        let c = changes(&v, r"C:\Data", mark(0), Some(&mut FewVisible), &|| false).unwrap();
        assert_eq!(c.status, ChangeStatus::Complete);
        assert_eq!(rels(&c), vec![("d0", false), ("d1", false), ("d2", false)]);
    }
}
