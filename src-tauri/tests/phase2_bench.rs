//! F2-28 evidence harness — wall-clock timings for the phase-2 engine paths.
//!
//! These are NOT correctness tests; they generate measurement evidence on the
//! current host and print a table. They are `#[ignore]`d so the normal gate
//! stays fast:
//!
//! ```text
//! cargo test --test phase2_bench --release -- --ignored --nocapture
//! ```
//!
//! Everything drives the same functions the Tauri commands call —
//! `create_zip`/`create_7z`/`test_zip`/`test_7z`, `ZipProvider::{list,
//! extract}` (which runs the F2-03 publish pipeline) and `run_search`/
//! `list_branch` — so numbers reflect shipping code paths.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ferrum_lib::archive_contract::{
    ArchiveConflictPolicy, ArchiveExtractRequest, ArchiveLimits, ArchiveListRequest,
    ArchiveProvider, Location, MemberPath,
};
use ferrum_lib::archive_create_test::{create_7z, create_zip, test_7z, test_zip};
use ferrum_lib::archive_zip::ZipProvider;
use ferrum_lib::search::SearchOptions;

fn bench_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ferrum-bench-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("bench root");
    dir
}

fn limits(entries: u64) -> ArchiveLimits {
    ArchiveLimits {
        max_entries: entries,
        max_total_bytes: u64::MAX,
        max_entry_bytes: u64::MAX,
        max_depth: 32,
    }
}

/// Deterministic pseudo-random bytes (xorshift) so runs are reproducible and
/// incompressible enough to exercise the deflate path honestly.
fn fill_random(buf: &mut [u8], seed: u64) {
    let mut s = seed | 1;
    for b in buf.iter_mut() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *b = s as u8;
    }
}

/// A tree with `files` files of `file_size` bytes spread over
/// `dirs` subdirectories. Returns the tree root.
fn make_tree(root: &Path, dirs: usize, files: usize, file_size: usize) -> PathBuf {
    let mut buf = vec![0u8; file_size];
    let per_dir = files / dirs.max(1);
    for d in 0..dirs.max(1) {
        let sub = root.join(format!("dir-{d:03}"));
        fs::create_dir_all(&sub).expect("subdir");
        for i in 0..per_dir {
            fill_random(&mut buf, (d * 1_000_003 + i) as u64);
            let p = sub.join(format!("file-{i:05}.log"));
            let mut f = BufWriter::new(File::create(p).expect("file"));
            f.write_all(&buf).expect("write");
        }
    }
    root.to_path_buf()
}

/// A ZIP with `members` stored entries (fast to build) across 100 dirs.
fn make_zip_with_members(path: &Path, members: usize) {
    let file = File::create(path).expect("zip file");
    let mut zw = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = Default::default();
    let per_dir = members / 100;
    for d in 0..100 {
        zw.add_directory(format!("d{d:03}"), opts).expect("dir");
        for i in 0..per_dir {
            zw.start_file(format!("d{d:03}/m{i:06}.txt"), opts)
                .expect("start");
            zw.write_all(b"x").expect("write");
        }
    }
    zw.finish().expect("finish");
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

fn report(label: &str, elapsed: std::time::Duration, extra: &str) {
    println!(
        "| {:<42} | {:>9.2} s | {}",
        label,
        elapsed.as_secs_f32(),
        extra
    );
}

#[test]
#[ignore = "benchmark — run explicitly with --ignored --nocapture"]
fn bench_archive_listing() {
    println!("\n### Archive listing (ZipProvider::list, central-directory read)\n");
    for n in [10_000usize, 100_000] {
        let dir = bench_dir("list");
        let z = dir.join(format!("members-{n}.zip"));
        let t0 = Instant::now();
        make_zip_with_members(&z, n);
        let build = t0.elapsed();
        let provider = ZipProvider::new();
        let req = ArchiveListRequest {
            location: Location::Archive {
                archive_path: z.to_string_lossy().into_owned(),
                member_path: MemberPath::parse("").unwrap(),
            },
            request_token: 1,
            password: None,
        };
        let t1 = Instant::now();
        let resp = provider.list(&req).expect("list");
        let t = t1.elapsed();
        report(
            &format!("list ZIP with {n} members"),
            t,
            &format!(
                "{} entries returned; zip build {} s",
                resp.entries.len(),
                build.as_secs_f32()
            ),
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
#[ignore = "benchmark — run explicitly with --ignored --nocapture"]
fn bench_pack_extract_small_many() {
    let work = bench_dir("small-many");
    println!("\n### Pack/extract, many small files (5,000 x 1 KiB in 50 dirs)\n");
    let tree = make_tree(&work.join("tree"), 50, 5_000, 1_024);

    let t = Instant::now();
    create_zip(
        &[tree.to_string_lossy().into_owned()],
        &work.join("small.zip"),
        &limits(10_000),
    )
    .expect("pack zip");
    report("pack ZIP (5k files, deflate)", t.elapsed(), "");

    let t = Instant::now();
    test_zip(&work.join("small.zip"), None).expect("test zip");
    report("test ZIP (full decode + CRC)", t.elapsed(), "");

    let req = ArchiveExtractRequest {
        archive_path: work.join("small.zip").to_string_lossy().into_owned(),
        member_paths: vec![],
        destination_path: work.join("out-zip").to_string_lossy().into_owned(),
        conflict: ArchiveConflictPolicy::Overwrite,
        limits: limits(10_000),
        password: None,
    };
    fs::create_dir_all(work.join("out-zip")).expect("dest");
    let t = Instant::now();
    ZipProvider::new()
        .extract(&req, &mut |_| true)
        .expect("extract zip");
    report("extract ZIP (F2-03 publish pipeline)", t.elapsed(), "");

    let t = Instant::now();
    create_7z(
        &[tree.to_string_lossy().into_owned()],
        &work.join("small.7z"),
    )
    .expect("pack 7z");
    report("pack 7z (5k files, LZMA2)", t.elapsed(), "");

    let t = Instant::now();
    test_7z(&work.join("small.7z"), None).expect("test 7z");
    report("test 7z (full decode)", t.elapsed(), "");

    println!(
        "| {:<42} | {:>9} | {}",
        "artifact sizes",
        "",
        &format!(
            "zip {}, 7z {}",
            mb(fs::metadata(work.join("small.zip")).unwrap().len()),
            mb(fs::metadata(work.join("small.7z")).unwrap().len())
        )
    );
    let _ = fs::remove_dir_all(&work);
}

#[test]
#[ignore = "benchmark — run explicitly with --ignored --nocapture"]
fn bench_pack_extract_large() {
    let work = bench_dir("large");
    println!("\n### Pack/extract, one large incompressible file (200 MiB)\n");
    let big = work.join("big.bin");
    let mut f = BufWriter::new(File::create(&big).expect("big file"));
    let mut buf = vec![0u8; 1 << 20];
    for chunk in 0..200 {
        fill_random(&mut buf, chunk as u64 + 7);
        f.write_all(&buf).expect("write chunk");
    }
    drop(f);

    let t = Instant::now();
    create_zip(
        &[big.to_string_lossy().into_owned()],
        &work.join("big.zip"),
        &limits(10),
    )
    .expect("pack zip");
    report("pack ZIP (200 MiB, deflate)", t.elapsed(), "");

    let t = Instant::now();
    test_zip(&work.join("big.zip"), None).expect("test zip");
    report("test ZIP (full decode + CRC)", t.elapsed(), "");

    let req = ArchiveExtractRequest {
        archive_path: work.join("big.zip").to_string_lossy().into_owned(),
        member_paths: vec![],
        destination_path: work.join("out-big").to_string_lossy().into_owned(),
        conflict: ArchiveConflictPolicy::Overwrite,
        limits: limits(10),
        password: None,
    };
    fs::create_dir_all(work.join("out-big")).expect("dest");
    let t = Instant::now();
    ZipProvider::new()
        .extract(&req, &mut |_| true)
        .expect("extract zip");
    report("extract ZIP (200 MiB)", t.elapsed(), "");

    let t = Instant::now();
    create_7z(&[big.to_string_lossy().into_owned()], &work.join("big.7z")).expect("pack 7z");
    report("pack 7z (200 MiB, LZMA2)", t.elapsed(), "");

    let t = Instant::now();
    test_7z(&work.join("big.7z"), None).expect("test 7z");
    report("test 7z (full decode)", t.elapsed(), "");

    let _ = fs::remove_dir_all(&work);
}

#[test]
#[ignore = "benchmark — run explicitly with --ignored --nocapture"]
fn bench_search_and_branch_view() {
    let work = bench_dir("search");
    println!("\n### Recursive search + branch view (5,000-file tree)\n");
    let tree = make_tree(&work.join("tree"), 50, 5_000, 1_024);

    let options = SearchOptions {
        pattern: "*.log".into(),
        mode: "glob".into(),
        case_sensitive: false,
        recursive: true,
        min_size: None,
        max_size: None,
        modified_after_ms: None,
        modified_before_ms: None,
        content: None,
        max_results: 20_000,
        content_max_file_bytes: 0,
        max_depth: 32,
    };
    let cancel = AtomicBool::new(false);
    let mut hits = 0usize;
    let t = Instant::now();
    let stats =
        ferrum_lib::search::run_search(&tree.to_string_lossy(), &options, &cancel, |batch| {
            hits += batch.len();
            true
        })
        .expect("search");
    report(
        "search '*.log' over 5k-file tree",
        t.elapsed(),
        &format!("{} hits, {} visited", hits, stats.visited),
    );

    let t = Instant::now();
    let branch =
        ferrum_lib::branch_view::list_branch(&tree.to_string_lossy(), 100_000).expect("branch");
    report(
        "branch view (flattened subtree)",
        t.elapsed(),
        &format!("{} entries", branch.entries.len()),
    );
    let _ = fs::remove_dir_all(&work);
}
