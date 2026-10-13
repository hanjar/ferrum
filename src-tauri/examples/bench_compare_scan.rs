//! FER-472 manual benchmark: recursive compare_dirs scan time. Not part of `cargo test`.
//!
//!   cargo run --release --example bench_compare_scan -- gen <root> <n_files>
//!   cargo run --release --example bench_compare_scan -- run <left> <right> [runs]
//!   cargo run --release --example bench_compare_scan -- mft-synth <n_files>
//!
//! `mft-synth` times the in-memory half of tier B (MftSource::build + listing every folder) on a
//! synthetic volume of n files (50 per folder, 10 per sub-level). The elevated MFT read itself needs
//! admin and is measured by `bench_fast_scan`.
//!
//! `gen` writes `<root>\left` and `<root>\right` (same layout, ~1% of the right files differ in
//! size, ~1% in mtime, a few one-sided). `run` prints wall time per run, entry counts and the
//! number of non-same entries so before/after results can be checked for equality.
//! Numbers are host-specific (Defender, cache warmth); run each side a few times.

use std::fs;
use std::path::Path;
use std::time::Instant;

use ferrum_lib::compare::{Ctl, DirDiffState, DirOpts, compare_dirs_with};

fn gen_tree(root: &Path, n: usize, variant: bool) {
    let per_dir = 50;
    let mut made = 0usize;
    let mut d = 0usize;
    while made < n {
        let top = root.join(format!("top{:03}", d / 100));
        let dir = top.join(format!("sub{:04}", d));
        fs::create_dir_all(&dir).unwrap();
        for i in 0..per_dir.min(n - made) {
            let idx = made + i;
            let mut len = 64 + (idx % 7) * 10;
            if variant && idx % 100 == 7 {
                len += 1; // size differs
            }
            let p = dir.join(format!("f{idx:07}.bin"));
            fs::write(&p, vec![b'x'; len]).unwrap();
            let secs = 1_700_000_000u64 + (idx as u64 % 1000);
            let mut t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            if variant && idx % 100 == 13 {
                t += std::time::Duration::from_secs(60); // newer on the right
            }
            let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(t).unwrap();
        }
        made += per_dir.min(n - made);
        d += 1;
    }
    if variant {
        fs::write(root.join("only-right.txt"), b"r").unwrap();
    } else {
        fs::write(root.join("only-left.txt"), b"l").unwrap();
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("gen") => {
            let root = Path::new(&a[2]);
            let n: usize = a[3].parse().unwrap();
            let t = Instant::now();
            gen_tree(&root.join("left"), n, false);
            gen_tree(&root.join("right"), n, true);
            println!("generated 2 x {n} files in {:?}", t.elapsed());
        }
        Some("run") => {
            let runs: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(3);
            for i in 0..runs {
                let t = Instant::now();
                let r = compare_dirs_with(
                    &a[2],
                    &a[3],
                    DirOpts {
                        by_content: false,
                        recursive: true,
                        max_entries: 5_000_000,
                    },
                    &Ctl::none(),
                )
                .expect("compare");
                let el = t.elapsed();
                let non_same = r
                    .entries
                    .iter()
                    .filter(|e| e.state != DirDiffState::Same)
                    .count();
                println!(
                    "run {i}: {el:?}  entries={} nonSame={non_same} errors={} truncated={}",
                    r.entries.len(),
                    r.errors.len(),
                    r.truncated
                );
            }
        }
        Some("mft-synth") => mft_synth(a[2].parse().unwrap()),
        _ => eprintln!("usage: gen <root> <n_files> | run <left> <right> [runs] | mft-synth <n>"),
    }
}

fn mft_synth(n: usize) {
    use ferrum_lib::tree_source::{MftSource, TreeSource};
    use ferrum_ntfs_scan::protocol::{EntryKind, WireEntry};
    const ROOT: u64 = 5;
    let mk = |frn: u64, parent: u64, kind: EntryKind, name: String| WireEntry {
        frn,
        parent,
        kind,
        logical: 1234,
        allocated: None,
        modified: 133_000_000_000_000_000 + frn,
        name,
    };
    let mut entries = Vec::with_capacity(n + n / 40);
    let data = 100;
    entries.push(mk(data, ROOT, EntryKind::Dir, "data".into()));
    let mut next = 101u64;
    let (mut dirs, mut files) = (0usize, 0usize);
    let mut top = 0u64;
    while files < n {
        let t = next;
        next += 1;
        entries.push(mk(t, data, EntryKind::Dir, format!("t{top}")));
        top += 1;
        for d in 0..10 {
            let sub = next;
            next += 1;
            entries.push(mk(sub, t, EntryKind::Dir, format!("s{d}")));
            dirs += 1;
            for f in 0..50 {
                if files >= n {
                    break;
                }
                entries.push(mk(next, sub, EntryKind::File, format!("f{f:04}.bin")));
                next += 1;
                files += 1;
            }
        }
    }
    let t = Instant::now();
    let src = MftSource::build(r"C:\data", r"C:\", &entries, ROOT).unwrap();
    let built = t.elapsed();
    let t = Instant::now();
    let mut seen = 0usize;
    for ti in 0..top {
        seen += src.list(&format!("t{ti}")).unwrap().infos.len();
        for d in 0..10 {
            seen += src.list(&format!("t{ti}/s{d}")).unwrap().infos.len();
        }
    }
    println!(
        "mft-synth: {files} files, {dirs} dirs: build {built:?}, list all {:?} ({seen} names)",
        t.elapsed()
    );
}
