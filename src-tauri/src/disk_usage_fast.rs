//! FER-460: turn the flat MFT entry stream from `ferrum-scan-helper.exe`
//! into the SAME `DiskUsageSnapshot` the regular walker produces, so the
//! tree, treemap, queries and actions need no second code path.
//!
//! Child module of `disk_usage` (it builds a `LiveState` directly and ends
//! in the shared `finalize_scan`, so stats, the single `done` event and the
//! snapshot are assembled by exactly the walker's code). Pure and
//! Tauri-free: input is already-decoded protocol entries.
//!
//! Untrusted-input rules (the entries came over a pipe):
//! - every directory is linked under its parent at most ONCE (the first
//!   entry for a directory reference wins), so the parent graph is a
//!   forest; a cycle can never be reached from the root and the walk below
//!   visits each directory at most once;
//! - entries whose parent is unknown (metafile children such as `$Extend\*`,
//!   or anything inconsistent) are dropped, never attached elsewhere;
//! - ids/paths are built with `Path::join` from validated names (no `\`,
//!   `/`, NUL, `.`, `..` — enforced by the decoder), exactly like the walker.

use super::*;
use ferrum_ntfs_scan::protocol::{EntryKind, WireEntry};

/// Volume-wide figures the helper reported alongside the entries.
#[derive(Debug, Clone, Default)]
pub struct MftScanInfo {
    /// File reference of the volume root directory (record 5).
    pub root_frn: u64,
    pub corrupt_records: u64,
}

/// Case-insensitive component match (NTFS is case-insensitive by default).
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// Path components of `root` below `volume_root` (`C:\` → none, `C:\Users\x`
/// → `["Users", "x"]`). `None` when `root` is not on that volume.
pub(crate) fn components_below(volume_root: &str, root: &str) -> Option<Vec<String>> {
    let vr = volume_root.trim_end_matches(['\\', '/']);
    let head = root.get(..vr.len())?;
    if !head.eq_ignore_ascii_case(vr) {
        return None;
    }
    let rest = &root[vr.len()..];
    if !rest.is_empty() && !rest.starts_with(['\\', '/']) {
        return None;
    }
    Some(
        rest.split(['\\', '/'])
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Build the scan outcome for `root` from the volume's MFT entries.
///
/// `volume_root` is the drive root the entries describe (`C:\`). Fails with
/// `NotFound` when `root` cannot be located in the entry tree (the caller
/// then falls back to the regular walk).
#[allow(clippy::too_many_arguments)]
pub fn build_outcome(
    root: &str,
    volume_root: &str,
    scan_id: &str,
    entries: &[WireEntry],
    info: &MftScanInfo,
    options: &ScanOptions,
    start: Instant,
    sink: impl FnMut(&DiskUsageEvent),
) -> Result<ScanOutcome, FsError> {
    // ── index directories (first entry per reference wins) ────────────────
    let mut dir_slot: HashMap<u64, usize> = HashMap::new();
    let mut dir_entry: Vec<usize> = vec![usize::MAX]; // slot 0 = volume root
    dir_slot.insert(info.root_frn, 0);
    for (i, e) in entries.iter().enumerate() {
        if e.kind == EntryKind::Dir && !dir_slot.contains_key(&e.frn) {
            dir_slot.insert(e.frn, dir_entry.len());
            dir_entry.push(i);
        }
    }
    let mut child_dirs: Vec<Vec<usize>> = vec![Vec::new(); dir_entry.len()];
    let mut files: Vec<Vec<usize>> = vec![Vec::new(); dir_entry.len()];
    for (i, e) in entries.iter().enumerate() {
        if e.frn == info.root_frn {
            continue; // the root is synthesized, never an entry
        }
        let Some(&parent) = dir_slot.get(&e.parent) else {
            continue; // orphan / metafile child: dropped
        };
        match e.kind {
            EntryKind::Dir => {
                let slot = dir_slot[&e.frn];
                if dir_entry[slot] == i {
                    child_dirs[parent].push(slot);
                }
            }
            EntryKind::File | EntryKind::Symlink => files[parent].push(i),
        }
    }

    // ── locate the scan root inside the volume tree ────────────────────────
    let not_found = || {
        FsError::new(
            ErrorCode::NotFound,
            "Scan root was not found in the MFT of its volume",
        )
        .with_path(root)
    };
    let mut cur = 0usize;
    for comp in components_below(volume_root, root).ok_or_else(not_found)? {
        cur = *child_dirs[cur]
            .iter()
            .find(|&&s| same_name(&entries[dir_entry[s]].name, &comp))
            .ok_or_else(not_found)?;
    }

    // ── walk the subtree (pre-order), then roll totals up (post-order) ────
    let root_id = root.to_string();
    let mut st = LiveState::new(&root_id);
    st.create_root(&root_id, root, &entry_name(Path::new(root)));
    // (slot, node id, depth, index of the parent in `order`)
    let mut order: Vec<(usize, String, usize, Option<usize>)> = Vec::new();
    let mut stack: Vec<(usize, String, usize, Option<usize>)> =
        vec![(cur, root_id.clone(), 0, None)];
    while let Some((slot, id, depth, parent_ord)) = stack.pop() {
        let my_ord = order.len();
        st.scanned_dirs += 1;
        let mut direct_files = 0u64;
        let mut direct_symlinks = 0u64;
        let mut direct_logical = 0u64;
        let mut direct_alloc = Some(0u64);
        let mut recs = Vec::with_capacity(files[slot].len());
        for &fi in &files[slot] {
            let e = &entries[fi];
            if e.kind == EntryKind::Symlink {
                direct_symlinks += 1;
                st.scanned_symlinks += 1;
                recs.push(DiskUsageFileEntry {
                    name: e.name.clone(),
                    logical_bytes: 0,
                    allocated_bytes: None,
                    is_symlink: true,
                });
            } else {
                direct_files += 1;
                st.scanned_files += 1;
                st.logical_bytes = st.logical_bytes.saturating_add(e.logical);
                if e.allocated.is_some() {
                    st.allocated_available_files += 1;
                } else {
                    st.allocated_unavailable_files += 1;
                }
                st.known_allocated_bytes = merge_allocated(st.known_allocated_bytes, e.allocated);
                direct_logical = direct_logical.saturating_add(e.logical);
                direct_alloc = merge_allocated(direct_alloc, e.allocated);
                recs.push(DiskUsageFileEntry {
                    name: e.name.clone(),
                    logical_bytes: e.logical,
                    allocated_bytes: e.allocated,
                    is_symlink: false,
                });
            }
        }
        if !recs.is_empty() {
            st.files_by_parent.insert(id.clone(), recs);
        }
        let parent_id = parent_ord.map(|p| order[p].1.clone());
        let name = match &parent_id {
            Some(_) => entries[dir_entry[slot]].name.clone(),
            None => entry_name(Path::new(root)),
        };
        let mut node = LiveState::blank_node(&id, &id, &name, parent_id);
        node.direct_files = direct_files;
        node.direct_symlinks = direct_symlinks;
        node.direct_logical_bytes = direct_logical;
        node.direct_allocated_bytes = direct_alloc;
        // Subtree fields start as the direct ones; children add on roll-up.
        node.subtree_files = direct_files;
        node.subtree_symlinks = direct_symlinks;
        node.subtree_logical_bytes = direct_logical;
        node.subtree_allocated_bytes = direct_alloc;
        st.nodes.insert(id.clone(), node);
        let depth_ok = options.max_depth.is_none_or(|md| depth < md);
        for &child in child_dirs[slot].iter().rev() {
            if !depth_ok {
                st.depth_capped = true;
                break;
            }
            let child_id = Path::new(&id)
                .join(&entries[dir_entry[child]].name)
                .to_string_lossy()
                .into_owned();
            stack.push((child, child_id, depth + 1, Some(my_ord)));
        }
        order.push((slot, id, depth, parent_ord));
    }
    // Children always come after their parent in `order`, so a reverse pass
    // has every child's subtree complete before it is added to the parent.
    for i in (1..order.len()).rev() {
        let parent_ord = order[i].3.expect("non-root has a parent");
        let c = &st.nodes[&order[i].1];
        let (dirs, files, links, logical, alloc) = (
            c.subtree_dirs,
            c.subtree_files,
            c.subtree_symlinks,
            c.subtree_logical_bytes,
            c.subtree_allocated_bytes,
        );
        let parent = st.nodes.get_mut(&order[parent_ord].1).expect("parent node");
        parent.children_ids.push(order[i].1.clone());
        parent.subtree_dirs += 1 + dirs;
        parent.subtree_files += files;
        parent.subtree_symlinks += links;
        parent.subtree_logical_bytes = parent.subtree_logical_bytes.saturating_add(logical);
        parent.subtree_allocated_bytes = merge_allocated(parent.subtree_allocated_bytes, alloc);
    }
    if info.corrupt_records > 0 {
        // Honest coverage: damaged records may hide entries anywhere.
        st.errors.record_parts(
            volume_root,
            ErrorCode::Io,
            &format!(
                "{} record/nama MFT rusak atau tidak valid dilewati — hasil bisa kurang lengkap",
                info.corrupt_records
            ),
        );
        if let Some(r) = st.nodes.get_mut(&root_id) {
            r.subtree_errors += 1;
        }
    }
    Ok(finalize_scan(st, root, scan_id, false, start, sink))
}

/// Running-totals event while the helper streams (no tree yet: the MFT is
/// unordered, so top children only exist after the build).
pub fn progress_event(
    scan_id: &str,
    entries: u64,
    records_done: u64,
    records_total: u64,
    start: Instant,
) -> DiskUsageEvent {
    let st = LiveState::new("");
    let mut ev = st.event(
        scan_id,
        false,
        false,
        Coverage::Partial,
        None,
        start.elapsed().as_millis() as u64,
    );
    ev.scanned_files = entries;
    ev.known_allocated_bytes = None;
    ev.engine = ScanEngine::NtfsMft;
    if records_total > 0 {
        ev.mft_records_done = Some(records_done.min(records_total));
        ev.mft_records_total = Some(records_total);
    }
    ev
}

/// Outcome of a fast scan cancelled before any tree existed: root only,
/// `cancelled` + coverage `partial` (never complete), via the shared tail.
pub fn cancelled_outcome(
    root: &str,
    scan_id: &str,
    start: Instant,
    sink: impl FnMut(&DiskUsageEvent),
) -> ScanOutcome {
    let mut st = LiveState::new(root);
    st.create_root(root, root, &entry_name(Path::new(root)));
    finalize_scan(st, root, scan_id, true, start, sink)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: u64 = 5 | (5 << 48);

    fn e(frn: u64, parent: u64, kind: EntryKind, logical: u64, name: &str) -> WireEntry {
        WireEntry {
            frn,
            parent,
            kind,
            logical,
            allocated: match kind {
                EntryKind::File => Some(logical.next_multiple_of(4096)),
                EntryKind::Dir => Some(0),
                EntryKind::Symlink => None,
            },
            modified: 0,
            name: name.into(),
        }
    }

    fn sample() -> Vec<WireEntry> {
        use EntryKind::*;
        vec![
            e(20, ROOT, Dir, 0, "Users"),
            e(21, 20, Dir, 0, "alice"),
            e(22, 21, File, 1000, "a.txt"),
            e(23, 21, File, 5000, "b.bin"),
            e(24, 20, File, 7, "desktop.ini"),
            e(25, ROOT, File, 100, "pagefile.sys"),
            e(26, 21, Symlink, 0, "link"),
            // Hardlink: same frn, second name elsewhere — counted per path.
            e(23, 20, File, 5000, "b-link.bin"),
            // Orphan (parent = $Extend, never emitted): dropped.
            e(30, 11 | (1 << 48), File, 999, "$UsnJrnl"),
            // Duplicate directory entry with another parent: ignored.
            e(21, ROOT, Dir, 0, "alice-dup"),
        ]
    }

    fn build(
        root: &str,
        entries: &[WireEntry],
        info: &MftScanInfo,
    ) -> Result<ScanOutcome, FsError> {
        let mut events = Vec::new();
        let r = build_outcome(
            root,
            "C:\\",
            "du-test",
            entries,
            info,
            &ScanOptions::default(),
            Instant::now(),
            |ev| events.push(ev.clone()),
        );
        if r.is_ok() {
            assert_eq!(events.len(), 1);
            assert!(events[0].done);
        }
        r
    }

    fn info() -> MftScanInfo {
        MftScanInfo {
            root_frn: ROOT,
            corrupt_records: 0,
        }
    }

    #[test]
    fn volume_root_totals_and_ids() {
        let out = build("C:\\", &sample(), &info()).unwrap();
        let s = &out.stats;
        assert_eq!(s.coverage, Coverage::Complete);
        assert_eq!(s.scanned_files, 5);
        assert_eq!(s.scanned_dirs, 3);
        assert_eq!(s.scanned_symlinks, 1);
        assert_eq!(s.logical_bytes, 1000 + 5000 + 7 + 100 + 5000);
        let snap = &out.snapshot;
        let root = &snap.nodes["C:\\"];
        assert_eq!(root.subtree_dirs, 2);
        assert_eq!(root.subtree_logical_bytes, s.logical_bytes);
        let alice = &snap.nodes["C:\\Users\\alice"];
        assert_eq!(alice.parent_id.as_deref(), Some("C:\\Users"));
        assert_eq!(alice.direct_files, 2);
        assert_eq!(alice.direct_symlinks, 1);
        assert_eq!(alice.subtree_logical_bytes, 6000);
        assert_eq!(alice.subtree_allocated_bytes, Some(4096 + 8192));
        let users = &snap.nodes["C:\\Users"];
        assert_eq!(users.children_ids, vec!["C:\\Users\\alice".to_string()]);
        assert_eq!(users.subtree_files, 4);
        assert_eq!(users.subtree_logical_bytes, 6000 + 7 + 5000);
        assert!(!snap.nodes.contains_key("C:\\alice-dup"));
        let listed: Vec<&str> = snap.files_by_parent["C:\\Users\\alice"]
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(listed, vec!["a.txt", "b.bin", "link"]);
        // Queries work on the built snapshot unchanged.
        let page = query_children_snapshot(
            snap,
            "C:\\Users",
            0,
            10,
            DiskUsageSort::LargestFirst,
            &DiskUsageFilter::default(),
        )
        .unwrap();
        assert_eq!(page.items[0].name, "alice");
    }

    #[test]
    fn subdirectory_root_is_resolved_case_insensitively() {
        let out = build("c:\\users\\ALICE", &sample(), &info()).unwrap();
        assert_eq!(out.snapshot.root_id, "c:\\users\\ALICE");
        let root = &out.snapshot.nodes["c:\\users\\ALICE"];
        assert_eq!(root.subtree_files, 2);
        assert_eq!(out.stats.scanned_dirs, 1);
        assert!(
            out.snapshot
                .files_by_parent
                .contains_key("c:\\users\\ALICE")
        );
    }

    #[test]
    fn unknown_root_is_not_found() {
        let err = build("C:\\Nope", &sample(), &info()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = build("D:\\Users", &sample(), &info()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        // A file is not a directory root.
        assert!(build("C:\\pagefile.sys", &sample(), &info()).is_err());
    }

    #[test]
    fn cycles_are_unreachable_and_terminate() {
        use EntryKind::*;
        let entries = vec![
            e(40, 41, Dir, 0, "x"),
            e(41, 40, Dir, 0, "y"),
            e(42, 40, File, 10, "inside-cycle"),
            e(43, ROOT, File, 1, "ok"),
        ];
        let out = build("C:\\", &entries, &info()).unwrap();
        assert_eq!(out.stats.scanned_files, 1);
        assert_eq!(out.stats.scanned_dirs, 1);
    }

    #[test]
    fn corrupt_records_make_coverage_partial() {
        let out = build(
            "C:\\",
            &sample(),
            &MftScanInfo {
                root_frn: ROOT,
                corrupt_records: 3,
            },
        )
        .unwrap();
        assert_eq!(out.stats.coverage, Coverage::Partial);
        assert_eq!(out.stats.total_errors, 1);
    }

    #[test]
    fn cancelled_outcome_is_never_complete() {
        let mut events = Vec::new();
        let out = cancelled_outcome("C:\\", "du-x", Instant::now(), |e| events.push(e.clone()));
        assert!(out.stats.cancelled);
        assert_eq!(out.stats.coverage, Coverage::Partial);
        assert_eq!(events.len(), 1);
        assert!(events[0].done && events[0].cancelled);
        let p = progress_event("du-x", 42, 10, 40, Instant::now());
        assert!(!p.done);
        assert_eq!(p.scanned_files, 42);
        assert_eq!(p.engine, ScanEngine::NtfsMft);
        assert_eq!(
            (p.mft_records_done, p.mft_records_total),
            (Some(10), Some(40))
        );
        // Never above 100 %, and no fields without a known total.
        let over = progress_event("du-x", 1, 50, 40, Instant::now());
        assert_eq!(over.mft_records_done, Some(40));
        let none = progress_event("du-x", 1, 0, 0, Instant::now());
        assert_eq!(
            (none.mft_records_done, none.mft_records_total),
            (None, None)
        );
    }

    #[test]
    fn depth_cap_marks_partial() {
        let mut events = Vec::new();
        let out = build_outcome(
            "C:\\",
            "C:\\",
            "du-test",
            &sample(),
            &info(),
            &ScanOptions { max_depth: Some(1) },
            Instant::now(),
            |ev| events.push(ev.clone()),
        )
        .unwrap();
        assert!(out.stats.depth_capped);
        assert_eq!(out.stats.coverage, Coverage::Partial);
        assert!(!out.snapshot.nodes.contains_key("C:\\Users\\alice"));
    }

    /// Parity with the regular walker on a real temp tree: synthesize the
    /// MFT view of the tree (what the helper would send) and compare
    /// per-node totals against `run_scan_serial_with_throttle`.
    #[test]
    fn parity_with_walker_on_temp_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        fs::create_dir_all(base.join("a").join("deep").join("er")).unwrap();
        fs::create_dir_all(base.join("b")).unwrap();
        fs::write(base.join("top.txt"), vec![1u8; 123]).unwrap();
        fs::write(base.join("a").join("one.bin"), vec![2u8; 4096 * 3 + 1]).unwrap();
        fs::write(base.join("a").join("deep").join("two"), vec![3u8; 10]).unwrap();
        fs::write(base.join("a").join("deep").join("er").join("x"), b"").unwrap();
        fs::write(base.join("b").join("y.dat"), vec![4u8; 70_000]).unwrap();

        let root = base.to_string_lossy().into_owned();
        let walker = run_scan_serial_with_throttle(
            &root,
            &ScanOptions::default(),
            &AtomicBool::new(false),
            "du-walk",
            None,
            |_| {},
        )
        .unwrap();

        // Synthesize entries from a plain walk of the same tree.
        let mut entries = Vec::new();
        let mut next = 100u64;
        let root_frn = 5 | (5 << 48);
        let mut stack = vec![(base.to_path_buf(), root_frn)];
        while let Some((dir, frn)) = stack.pop() {
            for item in fs::read_dir(&dir).unwrap() {
                let item = item.unwrap();
                let md = item.metadata().unwrap();
                let name = item.file_name().to_string_lossy().into_owned();
                next += 1;
                if md.is_dir() {
                    entries.push(e(next, frn, EntryKind::Dir, 0, &name));
                    stack.push((item.path(), next));
                } else {
                    let (alloc, ok) = crate::properties::allocated_size(&item.path(), md.len());
                    entries.push(WireEntry {
                        frn: next,
                        parent: frn,
                        kind: EntryKind::File,
                        logical: md.len(),
                        allocated: ok.then_some(alloc),
                        modified: 0,
                        name,
                    });
                }
            }
        }
        let fast = build_outcome(
            &root,
            &root,
            "du-fast",
            &entries,
            &MftScanInfo {
                root_frn,
                corrupt_records: 0,
            },
            &ScanOptions::default(),
            Instant::now(),
            |_| {},
        )
        .unwrap();
        let (w, f) = (&walker.snapshot, &fast.snapshot);
        assert_eq!(w.nodes.len(), f.nodes.len());
        for (id, wn) in &w.nodes {
            let fnode = f.nodes.get(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(wn.parent_id, fnode.parent_id, "{id}");
            assert_eq!(wn.name, fnode.name, "{id}");
            assert_eq!(wn.subtree_files, fnode.subtree_files, "{id}");
            assert_eq!(wn.subtree_dirs, fnode.subtree_dirs, "{id}");
            assert_eq!(
                wn.subtree_logical_bytes, fnode.subtree_logical_bytes,
                "{id}"
            );
            assert_eq!(
                wn.subtree_allocated_bytes, fnode.subtree_allocated_bytes,
                "{id}"
            );
            let mut wc = wn.children_ids.clone();
            let mut fc = fnode.children_ids.clone();
            wc.sort();
            fc.sort();
            assert_eq!(wc, fc, "{id}");
        }
        assert_eq!(walker.stats.logical_bytes, fast.stats.logical_bytes);
        assert_eq!(walker.stats.scanned_files, fast.stats.scanned_files);
        assert_eq!(walker.stats.scanned_dirs, fast.stats.scanned_dirs);
        assert_eq!(walker.stats.coverage, fast.stats.coverage);
    }
}
