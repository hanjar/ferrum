//! FER-460 benchmark: regular walker vs MFT scan on one real volume.
//! Not part of `cargo test`. The MFT half needs an ELEVATED terminal
//! (raw volume read); run it yourself, never from an unattended agent:
//!
//! ```text
//! cargo run --release --example bench_fast_scan -- C            # both engines
//! cargo run --release --example bench_fast_scan -- C mft-only   # skip the walk
//! ```
//!
//! The MFT half calls the helper's in-process scan (same parser, same
//! read-only volume access, no pipe) and then builds the Disk usage snapshot
//! exactly like the app does. The walker half is the shipping DSK-09 pool.
//! Run the walker FIRST on a cold cache if you want a conservative ratio —
//! the order below favors the walker (it runs second, on a warm cache).

#[cfg(windows)]
fn main() {
    use ferrum_lib::disk_usage::{self, ScanOptions, fast};
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    let mut args = std::env::args().skip(1);
    let letter = args
        .next()
        .and_then(|a| a.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('C');
    let mft_only = args.next().as_deref() == Some("mft-only");
    let root = format!("{letter}:\\");

    let t0 = Instant::now();
    match ferrum_ntfs_scan::helper::scan_local(letter) {
        Ok((entries, summary)) => {
            let read = t0.elapsed();
            let t1 = Instant::now();
            let out = fast::build_outcome(
                &root,
                &root,
                "bench-mft",
                &entries,
                &fast::MftScanInfo {
                    root_frn: summary.root_frn,
                    corrupt_records: summary.corrupt_records,
                },
                &ScanOptions::default(),
                Instant::now(),
                |_| {},
            )
            .expect("build");
            let build = t1.elapsed();
            println!(
                "mft: records={} entries={} corrupt={} read+parse={read:?} build={build:?} total={:?} files={} dirs={} logical={} B",
                summary.records_scanned,
                entries.len(),
                summary.corrupt_records,
                t0.elapsed(),
                out.stats.scanned_files,
                out.stats.scanned_dirs,
                out.stats.logical_bytes,
            );
        }
        Err(e) => println!("mft: unavailable ({e}) — run from an elevated terminal"),
    }

    if !mft_only {
        let t2 = Instant::now();
        let out = disk_usage::run_scan_with_throttle(
            &root,
            &ScanOptions::default(),
            &AtomicBool::new(false),
            "bench-walk",
            None,
            |_| {},
        )
        .expect("walk");
        println!(
            "walk: total={:?} files={} dirs={} logical={} B errors={} coverage={:?}",
            t2.elapsed(),
            out.stats.scanned_files,
            out.stats.scanned_dirs,
            out.stats.logical_bytes,
            out.stats.total_errors,
            out.stats.coverage,
        );
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only (FER-460)");
}
