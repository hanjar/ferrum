//! Manual copy benchmark (task 15). Not part of `cargo test`.
//!
//! Run: cargo run --release --example bench_copy
//! Measures one large file and many small files through the copy engine.
//! Numbers are host-specific (see docs/benchmarks.md).

use ferrum_lib::copy_engine::{ConflictPolicy, run_copy};
use ferrum_lib::ops::CancelToken;
use std::fs;
use std::time::Instant;

fn silent(_p: ferrum_lib::domain::OperationProgress) {}

fn main() {
    let root = std::env::temp_dir().join(format!("ferrum-copybench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    // 1) one large file: 256 MB
    let t0 = Instant::now();
    let big = src.join("big.bin");
    {
        use std::io::Write;
        let mut f = fs::File::create(&big).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..256 {
            f.write_all(&chunk).unwrap();
        }
    }
    let big_setup = t0.elapsed();

    // 2) many small files: 2000 × 1 KB
    let t1 = Instant::now();
    let small_dir = src.join("small");
    fs::create_dir_all(&small_dir).unwrap();
    for i in 0..2000 {
        fs::write(small_dir.join(format!("s{i:04}.txt")), vec![7u8; 1024]).unwrap();
    }
    let small_setup = t1.elapsed();

    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let t2 = Instant::now();
    let out = run_copy(
        &[
            big.to_string_lossy().into_owned(),
            small_dir.to_string_lossy().into_owned(),
        ],
        &dest.to_string_lossy(),
        ConflictPolicy::Ask,
        &CancelToken::default(),
        &silent,
    );
    let copy_time = t2.elapsed();
    println!(
        "copy: files={} bytes={}MB failures={} setup_big={big_setup:?} setup_small={small_setup:?} copy={copy_time:?} throughput={:?}/s",
        out.copied_files,
        out.copied_bytes / (1024 * 1024),
        out.failures.len(),
        out.copied_bytes / copy_time.as_secs().max(1)
    );
    let _ = fs::remove_dir_all(&root);
}
