//! FER-471 copy benchmark against our own engine (no robocopy).
//!
//! Run: cargo run --release --example bench_copy_adaptive [-- <auto|1..16|stream1>]
//! `stream1` = the pre-FER-471 shape: one worker, portable buffered copy with
//! a flush per file (the "before" numbers).
//! Datasets: 20k small files, 20 large files, mixed. Results go to docs/perf.

use ferrum_lib::copy_engine::{ConflictPolicy, CopyOptions, run_copy_with_options};
use ferrum_lib::copy_pool::ThreadSetting;
use ferrum_lib::ops::CancelToken;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

fn silent(_p: ferrum_lib::domain::OperationProgress) {}

fn fill(path: &Path, size: usize, seed: u8) {
    let mut f = fs::File::create(path).unwrap();
    let chunk: Vec<u8> = (0..1024 * 1024)
        .map(|i| (i as u8).wrapping_add(seed))
        .collect();
    let mut left = size;
    while left > 0 {
        let n = left.min(chunk.len());
        f.write_all(&chunk[..n]).unwrap();
        left -= n;
    }
}

fn small_files(dir: &Path, count: usize) {
    for d in 0..(count / 500).max(1) {
        let sub = dir.join(format!("d{d:03}"));
        fs::create_dir_all(&sub).unwrap();
        for i in 0..500.min(count) {
            let size = 512 + ((i * 37 + d * 11) % 48) * 1024;
            fs::write(
                sub.join(format!("f{i:04}.dat")),
                vec![(i % 251) as u8; size],
            )
            .unwrap();
        }
    }
}

fn run(label: &str, src: &Path, options: CopyOptions) {
    let dest = src.parent().unwrap().join(format!("dest-{label}"));
    let _ = fs::remove_dir_all(&dest);
    fs::create_dir_all(&dest).unwrap();
    let t = Instant::now();
    let out = run_copy_with_options(
        &[src.to_string_lossy().into_owned()],
        &dest.to_string_lossy(),
        ConflictPolicy::Ask,
        options,
        &CancelToken::default(),
        &silent,
    );
    let secs = t.elapsed().as_secs_f64();
    let mb = out.copied_bytes as f64 / 1048576.0;
    println!(
        "{label}: files={} MB={mb:.0} failures={} time={secs:.2}s  {:.0} MB/s  {:.0} files/s",
        out.copied_files,
        out.failures.len(),
        mb / secs,
        out.copied_files as f64 / secs
    );
    let _ = fs::remove_dir_all(&dest);
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "auto".into());
    let root = std::env::temp_dir().join(format!("ferrum-bench471-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let small = root.join("small");
    let large = root.join("large");
    let mixed = root.join("mixed");
    fs::create_dir_all(&large).unwrap();
    fs::create_dir_all(&small).unwrap();
    fs::create_dir_all(&mixed).unwrap();
    small_files(&small, 20_000);
    for i in 0..20 {
        fill(
            &large.join(format!("big{i:02}.bin")),
            32 * 1024 * 1024,
            i as u8,
        );
    }
    small_files(&mixed, 5_000);
    for i in 0..5 {
        fill(
            &mixed.join(format!("big{i}.bin")),
            64 * 1024 * 1024,
            i as u8,
        );
    }
    println!("mode: {arg}");
    let options = apply(&arg);
    run("small-20k", &small, options);
    run("large-20", &large, options);
    run("mixed", &mixed, options);
    let _ = fs::remove_dir_all(&root);
}

fn apply(arg: &str) -> CopyOptions {
    match arg {
        "stream1" => CopyOptions {
            threads: ThreadSetting::Fixed(1),
            stream_only: true,
            ..CopyOptions::default()
        },
        "auto" => CopyOptions::default(),
        n => CopyOptions {
            threads: ThreadSetting::Fixed(n.parse().expect("threads: auto|1..16|stream1")),
            ..CopyOptions::default()
        },
    }
}
