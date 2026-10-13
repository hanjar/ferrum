//! In-memory index of one NTFS volume (FER-475, IDX-S2).
//!
//! One [`Node`] per (file record, name) pair — a hardlinked file has one node
//! per name, exactly like the MFT scan emits them. The layout is compact:
//!
//! - nodes live in one `Vec<Node>` (56 bytes each), freed slots are reused
//!   through a free list;
//! - names live in one UTF-8 arena (`Vec<u8>`); a node stores offset +
//!   length. Removed names become garbage that [`VolumeIndex::compact`]
//!   reclaims;
//! - `first[record]` maps an MFT record number to its first node (4 bytes
//!   per record, MFT record numbers are dense), names of the same record are
//!   chained through `Node::next`.
//!
//! The index never trusts its input to be well formed: every insert is
//! bounded ([`MAX_RECORD`], name length, arena size), parent chains are
//! walked with a depth limit and cycle detection, and anything that does not
//! hang off the root directory is pruned instead of being kept as an orphan.

use std::collections::HashSet;

/// Low 48 bits of a file reference = MFT record number.
pub const RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// Highest record number the index accepts (the `first` table is sized by
/// it; real volumes stay far below: 2^28 records = a 256 GiB MFT).
pub const MAX_RECORD: u64 = (1 << 28) - 1;
/// Longest name in UTF-8 bytes: 255 UTF-16 units at most 3 bytes each.
pub const MAX_NAME_BYTES: usize = 255 * 3;
/// Deepest directory chain walked (NTFS paths are bounded by 32k chars, so
/// a real chain is far shorter; anything deeper is treated as a cycle).
pub const MAX_DEPTH: usize = 4096;
/// Most names (hard links) one record may have (the NTFS limit).
pub const MAX_NAMES_PER_RECORD: usize = 1024;
/// Allocation size not known (links, or the parser could not tell).
pub const ALLOC_UNKNOWN: u64 = u64::MAX;

const NIL: u32 = u32::MAX;
const FREE: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File = 0,
    Dir = 1,
    /// Name-surrogate reparse point (symlink, junction): never followed.
    Symlink = 2,
}

impl Kind {
    pub fn from_byte(b: u8) -> Option<Kind> {
        match b {
            0 => Some(Kind::File),
            1 => Some(Kind::Dir),
            2 => Some(Kind::Symlink),
            _ => None,
        }
    }
}

/// Metadata of one record (shared by all its names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Meta {
    /// Logical size of the unnamed data stream (0 for directories).
    pub size: u64,
    /// On-disk allocation; `None` = unknown.
    pub allocated: Option<u64>,
    /// Last write, Windows FILETIME; 0 = unknown.
    pub modified: u64,
    /// `FILE_ATTRIBUTE_*`.
    pub attributes: u32,
}

#[derive(Debug, Clone, Copy)]
struct Node {
    frn: u64,
    parent: u64,
    size: u64,
    alloc: u64,
    modified: u64,
    attributes: u32,
    name_off: u32,
    /// Next name of the same record, or next free slot.
    next: u32,
    name_len: u16,
    kind: u8,
    _pad: u8,
}

/// A borrowed view of one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub frn: u64,
    pub parent: u64,
    pub kind: Kind,
    pub meta: Meta,
    pub name: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// Record number above [`MAX_RECORD`].
    RecordOutOfRange,
    /// Empty, oversized or structurally invalid name.
    BadName,
    /// The name arena or node table hit its addressing limit.
    Full,
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::RecordOutOfRange => write!(f, "record number out of range"),
            IndexError::BadName => write!(f, "invalid name"),
            IndexError::Full => write!(f, "index full"),
        }
    }
}

/// Where a parent chain ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Reaches the root directory through indexed directories.
    Root,
    /// Passes through an excluded directory (shared mode: a user profile).
    Excluded,
    /// Missing parent, stale sequence number, non-directory parent, cycle,
    /// or too deep.
    Orphan,
}

/// Case-insensitive name comparison (NTFS default; simple Unicode folding).
pub fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
        || (!a.is_ascii() || !b.is_ascii()) && a.to_lowercase() == b.to_lowercase()
}

/// Same validity rule as the MFT parser and the scan protocol: non-empty,
/// no path separators, `:` or control characters, not `.` / `..`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.encode_utf16().count() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| (c as u32) < 0x20 || c == '\\' || c == '/' || c == ':')
}

pub struct VolumeIndex {
    root_frn: u64,
    nodes: Vec<Node>,
    first: Vec<u32>,
    free: u32,
    arena: Vec<u8>,
    garbage: usize,
    live: usize,
}

impl VolumeIndex {
    /// Empty index whose root directory has file reference `root_frn`
    /// (record 5 + its sequence number, from the scan).
    pub fn new(root_frn: u64) -> Self {
        VolumeIndex {
            root_frn,
            nodes: Vec::new(),
            first: Vec::new(),
            free: NIL,
            arena: Vec::new(),
            garbage: 0,
            live: 0,
        }
    }

    pub fn root_frn(&self) -> u64 {
        self.root_frn
    }

    /// The same index with the root reference the scan reported.
    pub fn with_root(mut self, root_frn: u64) -> Self {
        self.root_frn = root_frn;
        self
    }

    /// Number of entries (names).
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Heap bytes held by the index (capacity, not just length — this is
    /// what the process really pays).
    pub fn memory_bytes(&self) -> u64 {
        (self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.first.capacity() * std::mem::size_of::<u32>()
            + self.arena.capacity()) as u64
    }

    fn name_of(&self, n: &Node) -> &str {
        let s = &self.arena[n.name_off as usize..n.name_off as usize + n.name_len as usize];
        // Only valid UTF-8 is ever appended to the arena.
        std::str::from_utf8(s).unwrap_or("")
    }

    fn view(&self, n: &Node) -> Entry<'_> {
        Entry {
            frn: n.frn,
            parent: n.parent,
            kind: Kind::from_byte(n.kind).unwrap_or(Kind::File),
            meta: Meta {
                size: n.size,
                allocated: (n.alloc != ALLOC_UNKNOWN).then_some(n.alloc),
                modified: n.modified,
                attributes: n.attributes,
            },
            name: self.name_of(n),
        }
    }

    fn first_slot(&self, record: u64) -> u32 {
        self.first.get(record as usize).copied().unwrap_or(NIL)
    }

    /// All names of `record`.
    pub fn record_entries(&self, record: u64) -> Vec<Entry<'_>> {
        let mut out = Vec::new();
        let mut s = self.first_slot(record);
        while s != NIL {
            let n = &self.nodes[s as usize];
            out.push(self.view(n));
            s = n.next;
        }
        out
    }

    /// The file reference currently indexed for `record`, if any.
    pub fn record_frn(&self, record: u64) -> Option<u64> {
        let s = self.first_slot(record);
        (s != NIL).then(|| self.nodes[s as usize].frn)
    }

    /// The directory node for file reference `frn` (exact sequence match).
    fn dir_node(&self, frn: u64) -> Option<&Node> {
        let s = self.first_slot(frn & RECORD_MASK);
        if s == NIL {
            return None;
        }
        let n = &self.nodes[s as usize];
        (n.frn == frn && n.kind == Kind::Dir as u8).then_some(n)
    }

    /// Every entry, in slot order.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        self.nodes
            .iter()
            .filter(|n| n.kind != FREE)
            .map(|n| self.view(n))
    }

    fn alloc_slot(&mut self, node: Node) -> Result<u32, IndexError> {
        if self.free != NIL {
            let s = self.free;
            self.free = self.nodes[s as usize].next;
            self.nodes[s as usize] = node;
            return Ok(s);
        }
        if self.nodes.len() >= NIL as usize {
            return Err(IndexError::Full);
        }
        self.nodes.push(node);
        Ok((self.nodes.len() - 1) as u32)
    }

    /// Add one name of record `frn`. A duplicate (same parent + name) of an
    /// existing name of the record is ignored. Callers keep one sequence
    /// number per record ([`Self::replace_record`] does).
    pub fn add_name(
        &mut self,
        frn: u64,
        parent: u64,
        kind: Kind,
        meta: Meta,
        name: &str,
    ) -> Result<(), IndexError> {
        let record = frn & RECORD_MASK;
        if record > MAX_RECORD {
            return Err(IndexError::RecordOutOfRange);
        }
        if !valid_name(name) {
            return Err(IndexError::BadName);
        }
        let mut s = self.first_slot(record);
        let mut names = 0usize;
        while s != NIL {
            let n = &self.nodes[s as usize];
            if n.parent == parent && self.name_of(n) == name {
                return Ok(());
            }
            names += 1;
            s = n.next;
        }
        // NTFS allows at most 1024 links per file; more is crafted input
        // (and would make every insert into this record quadratic).
        if names >= MAX_NAMES_PER_RECORD {
            return Err(IndexError::Full);
        }
        let off = self.arena.len();
        if off + name.len() > u32::MAX as usize {
            return Err(IndexError::Full);
        }
        if self.first.len() <= record as usize {
            self.first.resize(record as usize + 1, NIL);
        }
        let node = Node {
            frn,
            parent,
            size: meta.size,
            alloc: meta.allocated.unwrap_or(ALLOC_UNKNOWN),
            modified: meta.modified,
            attributes: meta.attributes,
            name_off: off as u32,
            next: self.first[record as usize],
            name_len: name.len() as u16,
            kind: kind as u8,
            _pad: 0,
        };
        let slot = self.alloc_slot(node)?;
        self.arena.extend_from_slice(name.as_bytes());
        self.first[record as usize] = slot;
        self.live += 1;
        Ok(())
    }

    fn free_slot(&mut self, s: u32) {
        let n = &mut self.nodes[s as usize];
        self.garbage += n.name_len as usize;
        n.kind = FREE;
        n.name_len = 0;
        n.next = self.free;
        self.free = s;
        self.live -= 1;
    }

    /// Remove every name of `record`. Returns how many were removed.
    pub fn remove_record(&mut self, record: u64) -> usize {
        let mut s = self.first_slot(record);
        if s == NIL {
            return 0;
        }
        self.first[record as usize] = NIL;
        let mut removed = 0;
        while s != NIL {
            let next = self.nodes[s as usize].next;
            self.free_slot(s);
            removed += 1;
            s = next;
        }
        removed
    }

    /// Replace everything known about record `frn & RECORD_MASK` with these
    /// names (an empty list removes the record).
    pub fn replace_record(
        &mut self,
        frn: u64,
        kind: Kind,
        meta: Meta,
        names: &[(u64, &str)],
    ) -> Result<(), IndexError> {
        self.remove_record(frn & RECORD_MASK);
        for (parent, name) in names {
            self.add_name(frn, *parent, kind, meta, name)?;
        }
        Ok(())
    }

    /// Update only the metadata of an indexed record (all its names).
    pub fn update_meta(&mut self, frn: u64, meta: Meta) -> bool {
        let mut s = self.first_slot(frn & RECORD_MASK);
        let mut hit = false;
        while s != NIL {
            let n = &mut self.nodes[s as usize];
            if n.frn == frn {
                n.size = meta.size;
                n.alloc = meta.allocated.unwrap_or(ALLOC_UNKNOWN);
                n.modified = meta.modified;
                n.attributes = meta.attributes;
                hit = true;
            }
            s = n.next;
        }
        hit
    }

    /// Where the directory chain above `parent` ends.
    pub fn reach(&self, parent: u64, excluded: &HashSet<u64>) -> Reach {
        let mut cur = parent;
        for _ in 0..MAX_DEPTH {
            if cur == self.root_frn {
                return Reach::Root;
            }
            if excluded.contains(&(cur & RECORD_MASK)) {
                return Reach::Excluded;
            }
            match self.dir_node(cur) {
                Some(n) => cur = n.parent,
                None => return Reach::Orphan,
            }
        }
        Reach::Orphan
    }

    /// Remove every name whose parent chain does not reach the root, or
    /// passes through an excluded directory (records in `excluded`; the
    /// excluded directory itself stays, its contents go). Returns the number
    /// of names removed. O(entries) with memoisation.
    pub fn prune(&mut self, excluded: &HashSet<u64>) -> usize {
        self.prune_with(excluded, true)
    }

    /// Like [`Self::prune`], but only names that provably lie below an
    /// excluded directory go; names whose chain ends at a parent that is
    /// not indexed (yet) stay. Used during a build, when parents may still
    /// arrive later in the MFT.
    pub fn prune_excluded(&mut self, excluded: &HashSet<u64>) -> usize {
        self.prune_with(excluded, false)
    }

    fn prune_with(&mut self, excluded: &HashSet<u64>, drop_orphans: bool) -> usize {
        const UNKNOWN: u8 = 0;
        const OK: u8 = 1;
        const BAD: u8 = 2;
        const VISITING: u8 = 3;
        let mut memo = vec![UNKNOWN; self.first.len()];
        let root = self.root_frn;
        let orphan = if drop_orphans { BAD } else { OK };
        // Status of "being a child of directory `dir`".
        let status = |idx: &VolumeIndex, memo: &mut Vec<u8>, dir: u64| -> u8 {
            let mut stack: Vec<usize> = Vec::new();
            let mut cur = dir;
            let result = loop {
                if cur == root {
                    break OK;
                }
                let rec = cur & RECORD_MASK;
                if excluded.contains(&rec) {
                    break BAD;
                }
                // The exact reference (sequence included) must be an indexed
                // directory BEFORE the memo is consulted: the memo is keyed
                // by record number, and a stale sequence is an orphan.
                let Some(n) = idx.dir_node(cur) else {
                    break orphan;
                };
                let Some(m) = memo.get(rec as usize).copied() else {
                    break orphan;
                };
                match m {
                    OK | BAD => break m,
                    VISITING => break BAD, // cycle
                    _ => {}
                }
                if stack.len() >= MAX_DEPTH {
                    break BAD;
                }
                memo[rec as usize] = VISITING;
                stack.push(rec as usize);
                cur = n.parent;
            };
            for r in stack {
                memo[r] = result;
            }
            result
        };
        let mut doomed: Vec<u32> = Vec::new();
        for (slot, n) in self.nodes.iter().enumerate() {
            if n.kind != FREE && status(self, &mut memo, n.parent) != OK {
                doomed.push(slot as u32);
            }
        }
        let count = doomed.len();
        for s in doomed {
            self.unlink(s);
        }
        count
    }

    /// Remove one specific slot from its record chain and free it.
    fn unlink(&mut self, slot: u32) {
        let record = (self.nodes[slot as usize].frn & RECORD_MASK) as usize;
        let mut prev = NIL;
        let mut s = self.first.get(record).copied().unwrap_or(NIL);
        while s != NIL && s != slot {
            prev = s;
            s = self.nodes[s as usize].next;
        }
        if s == NIL {
            return;
        }
        let next = self.nodes[s as usize].next;
        if prev == NIL {
            self.first[record] = next;
        } else {
            self.nodes[prev as usize].next = next;
        }
        self.free_slot(slot);
    }

    /// Find the directory at `path` (components below the root, compared
    /// case-insensitively), e.g. `["Users", "bob"]`. O(entries) per
    /// component; used once per build to resolve exclusion roots.
    pub fn find_dir(&self, path: &[&str]) -> Option<u64> {
        let mut cur = self.root_frn;
        for comp in path {
            let want = comp.to_lowercase();
            cur = self
                .nodes
                .iter()
                .find(|n| {
                    n.kind == Kind::Dir as u8
                        && n.parent == cur
                        && self.name_of(n).to_lowercase() == want
                })?
                .frn;
        }
        (cur != self.root_frn).then_some(cur)
    }

    /// Number of node slots (live and free) — the cursor space of a scan.
    pub fn slot_count(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// The entry in `slot`, `None` for a free slot or one out of range.
    pub fn entry_at(&self, slot: u32) -> Option<Entry<'_>> {
        let n = self.nodes.get(slot as usize)?;
        (n.kind != FREE).then(|| self.view(n))
    }

    /// Parent of the indexed DIRECTORY `frn` (exact sequence match).
    pub fn dir_parent(&self, frn: u64) -> Option<u64> {
        self.dir_node(frn).map(|n| n.parent)
    }

    /// Is `frn` an indexed directory (or the root)?
    pub fn is_dir(&self, frn: u64) -> bool {
        frn == self.root_frn || self.dir_node(frn).is_some()
    }

    /// Find the directory at `path` (components below the root, compared
    /// case-insensitively) in ONE pass: only directories whose name equals
    /// the last component have their parent chain compared (FER-476; the
    /// per-component [`Self::find_dir`] is O(entries) per component).
    /// `Some(root)` for an empty path.
    pub fn find_dir_path(&self, path: &[&str]) -> Option<u64> {
        let Some((last, above)) = path.split_last() else {
            return Some(self.root_frn);
        };
        'nodes: for n in &self.nodes {
            if n.kind != Kind::Dir as u8 || !eq_ci(self.name_of(n), last) {
                continue;
            }
            let mut cur = n.parent;
            for comp in above.iter().rev() {
                match self.dir_node(cur) {
                    Some(d) if eq_ci(self.name_of(d), comp) => cur = d.parent,
                    _ => continue 'nodes,
                }
            }
            if cur == self.root_frn {
                return Some(n.frn);
            }
        }
        None
    }

    /// Full path below the root (`["Users", "bob", "a.txt"]` joined with
    /// `\`), for the S3 result filter. `None` for an orphan.
    pub fn path_of(&self, e: &Entry<'_>) -> Option<String> {
        let mut parts: Vec<&str> = vec![e.name];
        let mut cur = e.parent;
        for _ in 0..MAX_DEPTH {
            if cur == self.root_frn {
                parts.reverse();
                return Some(parts.join("\\"));
            }
            let n = self.dir_node(cur)?;
            parts.push(self.name_of(n));
            cur = n.parent;
        }
        None
    }

    /// Reclaim arena garbage when worthwhile (garbage above half the arena
    /// and 1 MiB), or when `force`; also releases spare capacity. Freed
    /// node slots are reused through the free list, not shrunk.
    pub fn compact(&mut self, force: bool) {
        if !force && (self.garbage < 1 << 20 || self.garbage * 2 < self.arena.len()) {
            return;
        }
        let mut arena = Vec::with_capacity(self.arena.len() - self.garbage);
        for n in self.nodes.iter_mut() {
            if n.kind == FREE {
                continue;
            }
            let start = n.name_off as usize;
            let off = arena.len();
            arena.extend_from_slice(&self.arena[start..start + n.name_len as usize]);
            n.name_off = off as u32;
        }
        self.arena = arena;
        self.garbage = 0;
        self.arena.shrink_to_fit();
        self.nodes.shrink_to_fit();
        self.first.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fref(rec: u64, seq: u16) -> u64 {
        rec | ((seq as u64) << 48)
    }
    const ROOT: u64 = 5 | (5 << 48);

    fn meta(size: u64) -> Meta {
        Meta {
            size,
            allocated: Some(size),
            modified: 1,
            attributes: 0x20,
        }
    }

    fn sample() -> VolumeIndex {
        let mut ix = VolumeIndex::new(ROOT);
        ix.add_name(fref(20, 1), ROOT, Kind::Dir, Meta::default(), "Users")
            .unwrap();
        ix.add_name(fref(21, 1), fref(20, 1), Kind::Dir, Meta::default(), "bob")
            .unwrap();
        ix.add_name(fref(22, 1), fref(21, 1), Kind::File, meta(10), "secret.txt")
            .unwrap();
        ix.add_name(fref(23, 1), ROOT, Kind::File, meta(5), "a.txt")
            .unwrap();
        ix.add_name(fref(23, 1), fref(20, 1), Kind::File, meta(5), "hard.txt")
            .unwrap();
        ix
    }

    #[test]
    fn add_lookup_remove() {
        let mut ix = sample();
        assert_eq!(ix.len(), 5);
        let mut names: Vec<&str> = ix.record_entries(23).iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "hard.txt"]);
        // Duplicate name is ignored.
        ix.add_name(fref(23, 1), ROOT, Kind::File, meta(5), "a.txt")
            .unwrap();
        assert_eq!(ix.len(), 5);
        assert_eq!(ix.remove_record(23), 2);
        assert_eq!(ix.len(), 3);
        assert!(ix.record_entries(23).is_empty());
        // Slots are reused.
        let before = ix.nodes.len();
        ix.add_name(fref(30, 1), ROOT, Kind::File, meta(1), "new.txt")
            .unwrap();
        assert_eq!(ix.nodes.len(), before);
    }

    #[test]
    fn rejects_bad_input() {
        let mut ix = VolumeIndex::new(ROOT);
        for bad in ["", ".", "..", "a\\b", "a/b", "a:b", "a\u{1}b"] {
            assert_eq!(
                ix.add_name(fref(20, 1), ROOT, Kind::File, meta(1), bad),
                Err(IndexError::BadName),
                "{bad:?}"
            );
        }
        let long = "x".repeat(256);
        assert_eq!(
            ix.add_name(fref(20, 1), ROOT, Kind::File, meta(1), &long),
            Err(IndexError::BadName)
        );
        assert_eq!(
            ix.add_name(MAX_RECORD + 1, ROOT, Kind::File, meta(1), "x"),
            Err(IndexError::RecordOutOfRange)
        );
        assert!(ix.is_empty());
    }

    #[test]
    fn replace_and_update_meta() {
        let mut ix = sample();
        ix.replace_record(fref(23, 1), Kind::File, meta(99), &[(ROOT, "renamed.txt")])
            .unwrap();
        let e = ix.record_entries(23);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].name, e[0].meta.size), ("renamed.txt", 99));
        assert!(ix.update_meta(fref(23, 1), meta(7)));
        assert_eq!(ix.record_entries(23)[0].meta.size, 7);
        assert!(!ix.update_meta(fref(23, 2), meta(8)), "stale sequence");
    }

    #[test]
    fn reach_and_prune_with_exclusions() {
        let mut ix = sample();
        let bob: HashSet<u64> = [21u64].into_iter().collect();
        assert_eq!(ix.reach(fref(21, 1), &bob), Reach::Excluded);
        assert_eq!(ix.reach(fref(20, 1), &bob), Reach::Root);
        assert_eq!(ix.reach(fref(20, 2), &bob), Reach::Orphan, "stale seq");
        assert_eq!(ix.reach(fref(99, 1), &bob), Reach::Orphan);
        // The excluded directory itself stays; its content goes.
        assert_eq!(ix.prune(&bob), 1);
        assert_eq!(ix.record_entries(21).len(), 1);
        assert!(ix.record_entries(22).is_empty());
        assert_eq!(ix.len(), 4);
    }

    #[test]
    fn prune_removes_orphans_cycles_and_file_parents() {
        let mut ix = sample();
        // Orphan (parent never indexed) and its child.
        ix.add_name(fref(40, 1), fref(39, 1), Kind::Dir, Meta::default(), "lost")
            .unwrap();
        ix.add_name(fref(41, 1), fref(40, 1), Kind::File, meta(1), "x")
            .unwrap();
        // Two directories parented on each other.
        ix.add_name(fref(50, 1), fref(51, 1), Kind::Dir, Meta::default(), "c1")
            .unwrap();
        ix.add_name(fref(51, 1), fref(50, 1), Kind::Dir, Meta::default(), "c2")
            .unwrap();
        // A child of a FILE.
        ix.add_name(fref(60, 1), fref(23, 1), Kind::File, meta(1), "under-file")
            .unwrap();
        let removed = ix.prune(&HashSet::new());
        assert_eq!(removed, 5);
        assert_eq!(ix.len(), 5);
    }

    #[test]
    fn hardlink_half_in_excluded_area() {
        let mut ix = sample();
        ix.add_name(fref(23, 1), fref(21, 1), Kind::File, meta(5), "in-bob.txt")
            .unwrap();
        let bob: HashSet<u64> = [21u64].into_iter().collect();
        ix.prune(&bob);
        let mut names: Vec<&str> = ix.record_entries(23).iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "hard.txt"]);
    }

    #[test]
    fn prune_memo_respects_sequence_numbers() {
        let mut ix = sample();
        // Two children of Users (20): one with the right sequence, one
        // with a stale one. The memo must not let the stale one through.
        ix.add_name(fref(70, 1), fref(20, 1), Kind::File, meta(1), "ok")
            .unwrap();
        ix.add_name(fref(71, 1), fref(20, 9), Kind::File, meta(1), "stale")
            .unwrap();
        ix.prune(&HashSet::new());
        assert_eq!(ix.record_entries(70).len(), 1);
        assert!(ix.record_entries(71).is_empty());
    }

    #[test]
    fn prune_excluded_keeps_not_yet_placed_entries() {
        let mut ix = sample();
        ix.add_name(
            fref(80, 1),
            fref(79, 1),
            Kind::File,
            meta(1),
            "parent-later",
        )
        .unwrap();
        let bob: HashSet<u64> = [21u64].into_iter().collect();
        assert_eq!(ix.prune_excluded(&bob), 1, "secret.txt only");
        assert_eq!(ix.record_entries(80).len(), 1);
        assert!(ix.record_entries(22).is_empty());
    }

    #[test]
    fn names_per_record_are_capped() {
        let mut ix = VolumeIndex::new(ROOT);
        for i in 0..MAX_NAMES_PER_RECORD {
            ix.add_name(fref(20, 1), ROOT, Kind::File, meta(1), &format!("n{i}"))
                .unwrap();
        }
        assert_eq!(
            ix.add_name(fref(20, 1), ROOT, Kind::File, meta(1), "one-more"),
            Err(IndexError::Full)
        );
    }

    #[test]
    fn find_dir_path_in_one_pass() {
        let mut ix = sample();
        // A decoy "bob" elsewhere must not match Users\bob.
        ix.add_name(fref(90, 1), ROOT, Kind::Dir, Meta::default(), "Other")
            .unwrap();
        ix.add_name(fref(91, 1), fref(90, 1), Kind::Dir, Meta::default(), "bob")
            .unwrap();
        assert_eq!(ix.find_dir_path(&["USERS", "Bob"]), Some(fref(21, 1)));
        assert_eq!(ix.find_dir_path(&["other", "bob"]), Some(fref(91, 1)));
        assert_eq!(ix.find_dir_path(&["bob"]), None);
        assert_eq!(
            ix.find_dir_path(&["Users", "bob", "secret.txt"]),
            None,
            "file"
        );
        assert_eq!(ix.find_dir_path(&[]), Some(ROOT));
        assert!(ix.is_dir(ROOT) && ix.is_dir(fref(21, 1)) && !ix.is_dir(fref(22, 1)));
        assert_eq!(ix.dir_parent(fref(21, 1)), Some(fref(20, 1)));
        let n = ix.slot_count();
        assert!((0..n).filter_map(|s| ix.entry_at(s)).count() == ix.len());
        assert!(ix.entry_at(n).is_none());
        assert!(eq_ci("ÄBC", "äbc") && !eq_ci("a", "b"));
    }

    #[test]
    fn find_dir_and_path() {
        let ix = sample();
        assert_eq!(ix.find_dir(&["users", "BOB"]), Some(fref(21, 1)));
        assert_eq!(ix.find_dir(&["Users", "nobody"]), None);
        assert_eq!(ix.find_dir(&[]), None);
        let e = ix.record_entries(22)[0];
        assert_eq!(ix.path_of(&e).as_deref(), Some("Users\\bob\\secret.txt"));
    }

    #[test]
    fn compaction_keeps_names() {
        let mut ix = VolumeIndex::new(ROOT);
        for i in 0..2000u64 {
            ix.add_name(
                fref(100 + i, 1),
                ROOT,
                Kind::File,
                meta(i),
                &format!("{i:€>255}"),
            )
            .unwrap();
        }
        for i in 0..1500u64 {
            ix.remove_record(100 + i);
        }
        assert!(ix.garbage > 1 << 20);
        let before = ix.arena.len();
        ix.compact(false);
        assert!(ix.arena.len() < before);
        assert_eq!(ix.garbage, 0);
        for i in 1500..2000u64 {
            assert_eq!(ix.record_entries(100 + i)[0].name, format!("{i:€>255}"));
        }
        assert!(ix.memory_bytes() > 0);
    }
}
