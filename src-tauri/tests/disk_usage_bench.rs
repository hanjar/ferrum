//! DSK-07.3 (FER-325) evidence harness — wall-clock / memory / cancel-latency
//! benchmarks for the disk usage engine on SYNTHETIC trees, run on the host
//! under measurement (host specs are recorded next to the numbers, not here).
//!
//! These are NOT correctness tests; like `phase2_bench.rs` they are
//! `#[ignore]`d so the normal gate stays fast:
//!
//! ```text
//! cargo test --test disk_usage_bench --release -- --ignored --nocapture
//! ```
//!
//! Everything drives the same functions the Tauri commands call —
//! `run_scan_with_throttle` (worker path), `store_snapshot` +
//! `query_children_by_scan`/`top_files_by_scan` (DSK-03 commands) and the
//! `active_scans`/`cancel_scan` registry — so numbers reflect shipping paths.
//!
//! Measurement honesty notes:
//! - Scan/entry throughput walks REAL filesystem entries in the temp dir
//!   (files pre-sized via `File::set_len`, i.e. sparse — build cost is
//!   metadata-only and reported separately from the scan).
//! - Memory is the Windows working set via `K32GetProcessMemoryInfo`
//!   (kernel32 psapi forward): the closest std-free proxy for RSS; the peak
//!   value is process-lifetime, so it is read as before/delta context, not an
//!   exact per-scan peak.
//! - Cancel latency = `cancel_scan()` call → `run_scan_with_throttle`
//!   returning (worker thread joined) — the full acknowledgement path,
//!   measured once per run (scheduler noise included, stated as such).

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ferrum_lib::disk_usage::{
    DiskUsageFilter, DiskUsageSort, ScanOptions, active_scans, cancel_scan, default_worker_count,
    new_scan_id, query_children_by_scan, run_scan_serial_with_throttle, run_scan_with_throttle,
    store_snapshot, top_files_by_scan,
};

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

    /// (current, peak) working set in bytes — the closest std-free proxy for
    /// process RSS on Windows. `None` when the API call fails.
    pub fn working_set() -> Option<(u64, u64)> {
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
            // GetCurrentProcess returns the pseudo-handle (never fails).
            if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
                Some((c.working_set as u64, c.peak_working_set as u64))
            } else {
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod procmem {
    /// Memory sampling is Windows-only in this harness; other hosts report
    /// None (stated honestly instead of faking a number).
    pub fn working_set() -> Option<(u64, u64)> {
        None
    }
}

fn bench_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ferrum-du-bench-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("bench root");
    dir
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

fn pct95(sorted: &[Duration]) -> Duration {
    let i = sorted.len().saturating_sub(1).min(sorted.len() * 95 / 100);
    sorted[i]
}

fn avg(durs: &[Duration]) -> Duration {
    durs.iter().sum::<Duration>() / durs.len().max(1) as u32
}

/// Nested deterministic tree: `branches/subs/leaves` directories with
/// `files_per_leaf` files each. File logical sizes cycle 0..4 KiB with every
/// 97th file carrying +64 KiB, all via `set_len` (sparse — no payload write).
/// Returns (entries created, total logical bytes).
fn build_tree(
    root: &Path,
    branches: usize,
    subs: usize,
    leaves: usize,
    files_per_leaf: usize,
    build_budget: Option<Duration>,
) -> io::Result<(u64, u64)> {
    let started = Instant::now();
    let mut seq: u64 = 0;
    let mut total_bytes: u64 = 0;
    for b in 0..branches {
        for s in 0..subs {
            for l in 0..leaves {
                let lp = root
                    .join(format!("branch-{b:03}"))
                    .join(format!("sub-{s:03}"))
                    .join(format!("leaf-{l:03}"));
                fs::create_dir_all(&lp)?;
                for i in 0..files_per_leaf {
                    let size = (seq.wrapping_mul(7919) % 4096)
                        + if seq.is_multiple_of(97) { 64 * 1024 } else { 0 };
                    let f = File::create(lp.join(format!("file-{i:05}.log")))?;
                    f.set_len(size)?;
                    total_bytes += size;
                    seq += 1;
                }
            }
        }
        // Watchdog for the 1M attempt: an honest aborted attempt, not a hang.
        if let Some(budget) = build_budget
            && started.elapsed() > budget
        {
            return Err(io::Error::other(format!(
                "build watchdog: exceeded {:?} after branch {b} ({} entries so far)",
                budget, seq
            )));
        }
    }
    Ok((seq, total_bytes))
}

fn print_header(title: &str) {
    println!("\n### {title}\n");
    println!("| measurement | value |");
    println!("| --- | --- |");
}

#[test]
#[ignore = "benchmark — run explicitly with --ignored --nocapture"]
fn bench_disk_usage_100k() {
    const BRANCHES: usize = 10;
    const SUBS: usize = 10;
    const LEAVES: usize = 10;
    const FILES_PER_LEAF: usize = 100; // 10*10*10*100 = 100_000 files

    let work = bench_dir("100k");
    let tree = work.join("tree");

    let t0 = Instant::now();
    let (expected_entries, expected_logical) =
        build_tree(&tree, BRANCHES, SUBS, LEAVES, FILES_PER_LEAF, None).expect("build tree");
    let build = t0.elapsed();

    let (ws_before, peak_before) = procmem::working_set().unwrap_or((0, 0));

    // ── scan wall-time + throughput (real walk, BOTH engines, one run) ──
    // BEFORE (DSK-07 baseline engine): the pre-DSK-09 single-threaded
    // walker, driven directly so before/after share process, tree and warm
    // cache state in this run.
    let serial_events = Arc::new(AtomicUsize::new(0));
    let se_sink = serial_events.clone();
    let ts = Instant::now();
    let serial = run_scan_serial_with_throttle(
        tree.to_string_lossy().as_ref(),
        &ScanOptions::default(),
        &AtomicBool::new(false),
        &new_scan_id(),
        Some(Duration::from_millis(50)),
        move |_ev| {
            se_sink.fetch_add(1, Ordering::Relaxed);
        },
    )
    .expect("serial scan 100k");
    let serial_scan = ts.elapsed();
    assert_eq!(serial.stats.scanned_files, expected_entries);
    assert_eq!(serial.stats.logical_bytes, expected_logical);

    // AFTER (DSK-09): the worker pool at default parallelism — the shipping
    // `run_scan_with_throttle` path. `outcome` feeds the store/query/cancel
    // measurements below.
    let events = Arc::new(AtomicUsize::new(0));
    let ev_sink = events.clone();
    let t1 = Instant::now();
    let outcome = run_scan_with_throttle(
        tree.to_string_lossy().as_ref(),
        &ScanOptions::default(),
        &AtomicBool::new(false),
        &new_scan_id(),
        Some(Duration::from_millis(50)),
        move |_ev| {
            ev_sink.fetch_add(1, Ordering::Relaxed);
        },
    )
    .expect("scan 100k");
    let scan = t1.elapsed();
    let scanned_entries = outcome.stats.scanned_files + outcome.stats.scanned_dirs;
    // Sanity: the synthetic tree is deterministic — both engines must agree.
    assert_eq!(outcome.stats.scanned_files, expected_entries);
    assert_eq!(outcome.stats.logical_bytes, expected_logical);

    store_snapshot((*outcome.snapshot).clone());
    let (ws_after, peak_after) = procmem::working_set().unwrap_or((0, 0));

    print_header("Disk usage engine — synthetic 100k tree (release, this host)");
    println!(
        "| tree build ({} files, sparse set_len) | {:.2} s |",
        expected_entries,
        build.as_secs_f64()
    );
    println!(
        "| scan BEFORE — single-thread ({} entries, {} events) | {:.2} s ({:.0} entries/s) |",
        scanned_entries,
        serial_events.load(Ordering::Relaxed),
        serial_scan.as_secs_f64(),
        scanned_entries as f64 / serial_scan.as_secs_f64()
    );
    println!(
        "| scan AFTER — pool {} workers ({} progress events) | {:.2} s ({:.0} entries/s) |",
        default_worker_count(),
        events.load(Ordering::Relaxed),
        scan.as_secs_f64(),
        scanned_entries as f64 / scan.as_secs_f64()
    );
    println!(
        "| speedup AFTER / BEFORE | {:.2}× |",
        serial_scan.as_secs_f64() / scan.as_secs_f64()
    );
    println!(
        "| logical bytes | {} ({}) |",
        outcome.stats.logical_bytes,
        mb(outcome.stats.logical_bytes)
    );
    println!(
        "| working set before → after scan | {} → {} (Δ {}) |",
        mb(ws_before),
        mb(ws_after),
        mb(ws_after.saturating_sub(ws_before))
    );
    println!(
        "| process peak working set context | {} (before run: {}) |",
        mb(peak_after),
        mb(peak_before)
    );

    // ── query latency: DSK-03 commands off the stored snapshot ──────────
    const ITERS: usize = 300;
    let root_id = outcome.snapshot.root_id.clone();
    let mut child_times = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        let page = query_children_by_scan(
            &outcome.scan_id,
            &root_id,
            0,
            50,
            DiskUsageSort::LargestFirst,
            &DiskUsageFilter::default(),
        )
        .expect("query children");
        child_times.push(t.elapsed());
        assert_eq!(page.total, 10); // root lists the 10 branch dirs, no loose files
    }
    child_times.sort();
    let mut file_times = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        let top = top_files_by_scan(&outcome.scan_id, &root_id, 50, &DiskUsageFilter::default())
            .expect("top files");
        file_times.push(t.elapsed());
        assert_eq!(top.total_matching, expected_entries);
        assert!(top.truncated);
    }
    file_times.sort();
    println!(
        "| query_children(root, 50) avg / p95 of {ITERS} | {:.3} / {:.3} ms |",
        avg(&child_times).as_secs_f64() * 1e3,
        pct95(&child_times).as_secs_f64() * 1e3
    );
    println!(
        "| top_files(root, 50, recursive) avg / p95 of {ITERS} | {:.3} / {:.3} ms |",
        avg(&file_times).as_secs_f64() * 1e3,
        pct95(&file_times).as_secs_f64() * 1e3
    );

    // ── cancel latency: registry cancel → worker acknowledged ───────────
    let cancel_id = new_scan_id();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    active_scans()
        .lock()
        .expect("registry")
        .insert(cancel_id.clone(), cancel_flag.clone());
    let cancel_events = Arc::new(AtomicUsize::new(0));
    let ce_sink = cancel_events.clone();
    let scan_root = tree.to_string_lossy().into_owned();
    let worker_scan_id = cancel_id.clone();
    let worker = std::thread::spawn(move || {
        run_scan_with_throttle(
            &scan_root,
            &ScanOptions::default(),
            &cancel_flag,
            &worker_scan_id,
            Some(Duration::from_millis(50)),
            move |_ev| {
                ce_sink.fetch_add(1, Ordering::Relaxed);
            },
        )
        .expect("cancelled scan")
    });
    // Wait until the walk is provably mid-flight, then cancel.
    let deadline = Instant::now() + Duration::from_secs(10);
    while cancel_events.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let t2 = Instant::now();
    let accepted = cancel_scan(&cancel_id);
    let done = worker.join().expect("worker joins");
    let cancel_latency = t2.elapsed();
    assert!(accepted);
    assert!(done.stats.cancelled);
    assert_eq!(
        done.stats.coverage,
        ferrum_lib::disk_usage::Coverage::Partial
    );
    println!(
        "| cancel latency (cancel_scan → pool worker returned, cancelled after {} events) | {:.2} ms |",
        cancel_events.load(Ordering::Relaxed),
        cancel_latency.as_secs_f64() * 1e3
    );

    let _ = fs::remove_dir_all(&work);
}

/// One honest attempt at 1M entries. The tree BUILD is the bottleneck on
/// Windows; a build watchdog aborts past ~7 minutes so the run reports
/// "belum feasible" instead of hanging — never force-fit.
#[test]
#[ignore = "benchmark — run explicitly with --ignored --nocapture"]
fn bench_disk_usage_1m_attempt() {
    const BRANCHES: usize = 100;
    const SUBS: usize = 10;
    const LEAVES: usize = 10;
    const FILES_PER_LEAF: usize = 100; // 100*10*10*100 = 1_000_000 files

    let work = bench_dir("1m");
    let tree = work.join("tree");

    let t0 = Instant::now();
    let built = build_tree(
        &tree,
        BRANCHES,
        SUBS,
        LEAVES,
        FILES_PER_LEAF,
        Some(Duration::from_secs(420)),
    );
    let build = t0.elapsed();
    let (expected_entries, expected_logical) = match built {
        Ok(ok) => ok,
        Err(e) => {
            print_header("Disk usage engine — 1M entries attempt");
            println!(
                "| status | BELUM FEASIBLE — build watchdog after {:.1} s: {} |",
                build.as_secs_f64(),
                e
            );
            let _ = fs::remove_dir_all(&work);
            return;
        }
    };

    // BEFORE (DSK-07 baseline engine): single-threaded walker, same run.
    let ts = Instant::now();
    let serial = run_scan_serial_with_throttle(
        tree.to_string_lossy().as_ref(),
        &ScanOptions::default(),
        &AtomicBool::new(false),
        &new_scan_id(),
        Some(Duration::from_millis(100)),
        |_| {},
    )
    .expect("serial scan 1m");
    let serial_scan = ts.elapsed();
    assert_eq!(serial.stats.scanned_files, expected_entries);
    assert_eq!(serial.stats.logical_bytes, expected_logical);

    // AFTER (DSK-09): the worker pool at default parallelism.
    let t1 = Instant::now();
    let outcome = run_scan_with_throttle(
        tree.to_string_lossy().as_ref(),
        &ScanOptions::default(),
        &AtomicBool::new(false),
        &new_scan_id(),
        Some(Duration::from_millis(100)),
        |_| {},
    )
    .expect("scan 1m");
    let scan = t1.elapsed();
    assert_eq!(outcome.stats.scanned_files, expected_entries);
    assert_eq!(outcome.stats.logical_bytes, expected_logical);
    store_snapshot((*outcome.snapshot).clone());

    let t2 = Instant::now();
    let root_id = outcome.snapshot.root_id.clone();
    let page = query_children_by_scan(
        &outcome.scan_id,
        &root_id,
        0,
        50,
        DiskUsageSort::LargestFirst,
        &DiskUsageFilter::default(),
    )
    .expect("query children 1m");
    let query = t2.elapsed();
    let t3 = Instant::now();
    let top = top_files_by_scan(&outcome.scan_id, &root_id, 50, &DiskUsageFilter::default())
        .expect("top files 1m");
    let top_t = t3.elapsed();

    print_header("Disk usage engine — synthetic 1M tree (release, this host)");
    println!(
        "| tree build ({} files) | {:.1} s |",
        expected_entries,
        build.as_secs_f64()
    );
    println!(
        "| scan BEFORE — single-thread ({} dirs + {} files) | {:.2} s ({:.0} entries/s) |",
        outcome.stats.scanned_dirs,
        outcome.stats.scanned_files,
        serial_scan.as_secs_f64(),
        (outcome.stats.scanned_files + outcome.stats.scanned_dirs) as f64
            / serial_scan.as_secs_f64()
    );
    println!(
        "| scan AFTER — pool {} workers | {:.2} s ({:.0} entries/s) |",
        default_worker_count(),
        scan.as_secs_f64(),
        (outcome.stats.scanned_files + outcome.stats.scanned_dirs) as f64 / scan.as_secs_f64()
    );
    println!(
        "| speedup AFTER / BEFORE | {:.2}× |",
        serial_scan.as_secs_f64() / scan.as_secs_f64()
    );
    println!(
        "| query_children(root, 50) | {:.3} ms ({} children) |",
        query.as_secs_f64() * 1e3,
        page.total
    );
    println!(
        "| top_files(root, 50) over 1M-entry snapshot | {:.3} ms (matched {}) |",
        top_t.as_secs_f64() * 1e3,
        top.total_matching
    );

    let _ = fs::remove_dir_all(&work);
}
