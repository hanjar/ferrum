//! Manual listing benchmark (task 04). Not part of `cargo test`.
//!
//! Run:  cargo run --release --example bench_listing -- [n_entries]
//! Defaults to 10_000 entries; pass 100_000 for the stress size. Prints the
//! host-measured wall time — record numbers as host-specific, not universal.

use std::fs;
use std::time::Instant;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(10_000);

    let dir = std::env::temp_dir().join(format!("ferrum-bench-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("bench root");

    let t0 = Instant::now();
    for i in 0..n {
        if i % 10 == 0 {
            fs::create_dir_all(dir.join(format!("dir-{i:06}"))).unwrap();
        } else {
            fs::write(dir.join(format!("file-{i:06}.bin")), vec![0u8; 64]).unwrap();
        }
    }
    let setup = t0.elapsed();

    let t1 = Instant::now();
    let listing = ferrum_lib::fs_service::list_dir(dir.to_str().unwrap()).expect("listing");
    let list = t1.elapsed();

    println!(
        "entries={n} setup={setup:?} list={list:?} per-entry={:?}",
        list / listing.len() as u32
    );
    let _ = fs::remove_dir_all(&dir);
}
