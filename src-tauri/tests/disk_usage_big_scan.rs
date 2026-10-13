//! FER-479 — guards against the "white window" freeze after a disk usage
//! scan of a whole system drive (C:\ ≈ 3.8 million MFT entries).
//!
//! Root cause (measured with `big_scan_bench` below, before the fix):
//! `disk_usage_snapshot` serialized EVERY directory node of the snapshot
//! (ids, paths, names, children id lists) into one IPC response — hundreds
//! of MB of JSON built by a SYNC Tauri command, i.e. on the main thread
//! that also pumps the window's messages, then parsed in one go by the
//! WebView's main thread. `disk_usage_top_files` on the scan root also
//! materialized one candidate (with two path strings) per file of the
//! whole drive just to keep the top N.
//!
//! The guard test runs in the normal gate on a synthetic MFT stream
//! (no filesystem, no elevation) and pins what the frontend may receive:
//! a summary of bounded size, independent of the tree size. The ignored
//! bench reproduces the full 4 M-entry case:
//!
//! ```text
//! cargo test --release --test disk_usage_big_scan -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use ferrum_lib::disk_usage::{
    DiskUsageFilter, DiskUsageSort, ScanOptions, SizeMetric, fast, new_scan_id,
    query_children_by_scan, snapshot, snapshot_summary, store_snapshot_arc, top_files_by_scan,
};
use ferrum_ntfs_scan::protocol::{EntryKind, WireEntry};

#[cfg(windows)]
mod procmem {
    use std::ffi::c_void;
    use std::mem::size_of;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged_pool: usize,
        quota_paged_pool: usize,
        quota_peak_non_paged_pool: usize,
        quota_non_paged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    /// Current working set in bytes (`None` when the call fails).
    pub fn working_set() -> Option<u64> {
        let mut c = ProcessMemoryCounters {
            cb: size_of::<ProcessMemoryCounters>() as u32,
            page_fault_count: 0,
            peak_working_set: 0,
            working_set: 0,
            quota_peak_paged_pool: 0,
            quota_paged_pool: 0,
            quota_peak_non_paged_pool: 0,
            quota_non_paged_pool: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        unsafe {
            if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
                Some(c.working_set as u64)
            } else {
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod procmem {
    pub fn working_set() -> Option<u64> {
        None
    }
}

const ROOT_FRN: u64 = 5;

/// Deterministic C:-like MFT stream: ~1 in 6 entries is a directory and
/// every entry hangs under a uniformly random EARLIER directory (a random
/// recursive tree: a few huge folders, a long tail of small ones, mean
/// depth ≈ ln(dirs) ≈ 13 — close to a real system drive). Sizes vary
/// 0..~1 MiB with an occasional 1 GiB file; names have typical lengths.
fn synthetic_volume(total: usize) -> Vec<WireEntry> {
    let mut out = Vec::with_capacity(total);
    let mut dirs: Vec<u64> = vec![ROOT_FRN];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for i in 0..total {
        let frn = 1000 + i as u64;
        let r = next();
        let parent = dirs[(r >> 8) as usize % dirs.len()];
        if r % 6 == 0 {
            out.push(WireEntry {
                frn,
                parent,
                kind: EntryKind::Dir,
                logical: 0,
                allocated: Some(0),
                modified: 0,
                name: format!("dir{i:07}"),
            });
            dirs.push(frn);
        } else {
            let size = (r >> 20) % 1_048_576 + if r % 997 == 0 { 1 << 30 } else { 0 };
            out.push(WireEntry {
                frn,
                parent,
                kind: EntryKind::File,
                logical: size,
                allocated: Some(size.div_ceil(4096) * 4096),
                modified: 0,
                name: format!("file_{i:07}.dat"),
            });
        }
    }
    out
}

struct Built {
    scan_id: String,
    root_id: String,
    build: Duration,
}

fn build_and_store(entries: &[WireEntry]) -> Built {
    let scan_id = new_scan_id();
    let t = Instant::now();
    let outcome = fast::build_outcome(
        "C:\\",
        "C:\\",
        &scan_id,
        entries,
        &fast::MftScanInfo {
            root_frn: ROOT_FRN,
            corrupt_records: 0,
        },
        &ScanOptions::default(),
        Instant::now(),
        |_| {},
    )
    .expect("synthetic volume builds");
    let build = t.elapsed();
    let root_id = outcome.snapshot.root_id.clone();
    store_snapshot_arc(outcome.snapshot);
    Built {
        scan_id,
        root_id,
        build,
    }
}

/// What the frontend receives after `done`: bounded by construction.
const SUMMARY_MAX_BYTES: usize = 16 * 1024;
/// One children page / top-N response: bounded by the limit, not the tree.
const PAGE_MAX_BYTES: usize = 256 * 1024;

#[test]
fn snapshot_response_stays_small_on_large_tree() {
    let entries = synthetic_volume(300_000);
    let b = build_and_store(&entries);

    let summary = snapshot_summary(&b.scan_id).expect("stored");
    let json = serde_json::to_vec(&summary).expect("serializes");
    assert!(
        json.len() <= SUMMARY_MAX_BYTES,
        "disk_usage_snapshot response is {} bytes — it must not scale with the tree",
        json.len()
    );
    assert_eq!(summary.root_node.id, b.root_id);
    assert!(
        summary.root_node.children_ids.is_empty(),
        "no id lists over IPC"
    );
    assert!(summary.stats.scanned_files > 200_000);

    let page = query_children_by_scan(
        &b.scan_id,
        &b.root_id,
        0,
        50,
        DiskUsageSort::LargestFirst,
        &DiskUsageFilter::default(),
    )
    .expect("children");
    let page_json = serde_json::to_vec(&page).expect("serializes");
    assert!(page_json.len() <= PAGE_MAX_BYTES);
    // Tree rows render counts/error badges from the page itself (no node map).
    let dir = page
        .items
        .iter()
        .find(|i| i.subtree_dirs.is_some())
        .expect("root has child dirs");
    assert!(dir.subtree_files.is_some() && dir.subtree_errors.is_some());

    let top = top_files_by_scan(&b.scan_id, &b.root_id, 20, &DiskUsageFilter::default())
        .expect("top files");
    assert_eq!(top.items.len(), 20);
    assert!(top.truncated);
    assert!(top.total_matching > 200_000);
    assert!(serde_json::to_vec(&top).expect("serializes").len() <= PAGE_MAX_BYTES);
    // Largest-first, stable.
    assert!(
        top.items
            .windows(2)
            .all(|w| w[0].logical_bytes >= w[1].logical_bytes)
    );
}

/// The bounded top-N selection must return exactly what a full sort of
/// every matching file would (metric desc, id asc), ties and unknown
/// allocated sizes included.
#[test]
fn bounded_top_files_matches_full_sort() {
    let mut entries = synthetic_volume(30_000);
    for e in &mut entries {
        if e.kind == EntryKind::File {
            e.logical = (e.frn % 7) * 1000; // heavy ties
            e.allocated = (e.frn % 11 != 0).then_some(e.logical + 96);
        }
    }
    let b = build_and_store(&entries);
    let snap = snapshot(&b.scan_id).expect("stored");
    for (metric, min_bytes, limit) in [
        (SizeMetric::Logical, None, 37u64),
        (SizeMetric::Allocated, None, 100),
        (SizeMetric::Logical, Some(3000), 5),
        (SizeMetric::Allocated, Some(1), 1000),
    ] {
        let filter = DiskUsageFilter {
            metric,
            min_bytes,
            ..DiskUsageFilter::default()
        };
        let got = top_files_by_scan(&b.scan_id, &b.root_id, limit, &filter).expect("top");
        let mut all: Vec<(u64, String)> = Vec::new();
        for (dir, files) in &snap.files_by_parent {
            for f in files.iter().filter(|f| !f.is_symlink) {
                let v = match metric {
                    SizeMetric::Logical => Some(f.logical_bytes),
                    SizeMetric::Allocated => f.allocated_bytes,
                };
                if min_bytes.is_some_and(|m| v.is_none_or(|v| v < m)) {
                    continue;
                }
                let id = std::path::Path::new(dir)
                    .join(&f.name)
                    .to_string_lossy()
                    .into_owned();
                all.push((v.unwrap_or(0), id));
            }
        }
        all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        assert_eq!(got.total_matching, all.len() as u64);
        assert_eq!(got.truncated, all.len() as u64 > limit);
        let want: Vec<&str> = all
            .iter()
            .take(limit as usize)
            .map(|(_, id)| id.as_str())
            .collect();
        let have: Vec<&str> = got.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(have, want, "{metric:?} min={min_bytes:?} limit={limit}");
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Full-size reproduction (≈ C:\ of the reporting host: 3.79 M entries).
#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn big_scan_bench() {
    let total: usize = std::env::var("FERRUM_BIG_SCAN_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_000_000);
    let ws0 = procmem::working_set().unwrap_or(0);
    let t = Instant::now();
    let entries = synthetic_volume(total);
    println!("synthetic entries: {} in {:?}", entries.len(), t.elapsed());
    let b = build_and_store(&entries);
    drop(entries);
    let ws1 = procmem::working_set().unwrap_or(0);
    println!(
        "build_outcome: {:?}; working set {} -> {} (snapshot held)",
        b.build,
        mb(ws0),
        mb(ws1)
    );

    let t = Instant::now();
    let summary = snapshot_summary(&b.scan_id).expect("stored");
    let json = serde_json::to_vec(&summary).expect("serializes");
    println!(
        "disk_usage_snapshot response: {} bytes, serialize {:?}, dirs {}",
        json.len(),
        t.elapsed(),
        summary.stats.scanned_dirs
    );

    let t = Instant::now();
    let page = query_children_by_scan(
        &b.scan_id,
        &b.root_id,
        0,
        50,
        DiskUsageSort::LargestFirst,
        &DiskUsageFilter::default(),
    )
    .expect("children");
    let pj = serde_json::to_vec(&page).expect("serializes");
    println!(
        "query_children(root, 50): {:?}, {} bytes",
        t.elapsed(),
        pj.len()
    );

    let ws_before_top = procmem::working_set().unwrap_or(0);
    let t = Instant::now();
    let top = top_files_by_scan(&b.scan_id, &b.root_id, 20, &DiskUsageFilter::default())
        .expect("top files");
    let ws_after_top = procmem::working_set().unwrap_or(0);
    println!(
        "top_files(root, 20): {:?}, matching {}, working set {} -> {}",
        t.elapsed(),
        top.total_matching,
        mb(ws_before_top),
        mb(ws_after_top)
    );
}
