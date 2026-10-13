//! FER-464: size and load time of the Disk usage scan cache for a big root.
//! Ignored by default (it builds a ~3.3 M file synthetic tree). Run with:
//!
//! ```text
//! cargo test --release --test disk_usage_cache_bench -- --ignored --nocapture
//! ```
//!
//! `FERRUM_BENCH_FILES` / `FERRUM_BENCH_DIRS_L4` scale the tree down for a
//! quick try. The tree has the proportions of a system drive (≈ 7 files per
//! folder, 4 levels, mixed-length names); numbers are in docs/perf.

use std::collections::HashMap;
use std::time::Instant;

use ferrum_lib::disk_usage::{
    Coverage, DiskUsageFileEntry, DiskUsageNode, DiskUsageSnapshot, DiskUsageStats, ScanEngine,
};
use ferrum_lib::disk_usage_cache::{load, save};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const WORDS: &[&str] = &[
    "Windows",
    "System32",
    "Program Files",
    "Microsoft",
    "Users",
    "AppData",
    "Local",
    "Packages",
    "WinSxS",
    "assembly",
    "Common",
    "Intel",
    "node_modules",
    "cache",
    "Temp",
    "Resources",
    "Drivers",
    "Fonts",
    "locale",
    "x64",
    "amd64",
    "Edge",
    "Google",
    "Chrome",
    "Adobe",
    "Steam",
];
const EXTS: &[&str] = &[
    "dll", "exe", "sys", "mui", "png", "json", "txt", "log", "dat", "xml", "js", "ttf", "cat",
    "manifest", "mp4", "jpg",
];

fn blank(id: &str, name: &str, parent: Option<&str>) -> DiskUsageNode {
    DiskUsageNode {
        id: id.into(),
        path: id.into(),
        name: name.into(),
        parent_id: parent.map(Into::into),
        children_ids: Vec::new(),
        direct_files: 0,
        direct_symlinks: 0,
        direct_logical_bytes: 0,
        direct_allocated_bytes: Some(0),
        subtree_files: 0,
        subtree_dirs: 0,
        subtree_symlinks: 0,
        subtree_logical_bytes: 0,
        subtree_allocated_bytes: Some(0),
        subtree_errors: 0,
        skipped_volume_boundaries: 0,
        subtree_skipped_volumes: 0,
    }
}

fn build(target_files: u64, l4: u64) -> DiskUsageSnapshot {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let root = "C:\\".to_string();
    let fan = [24u64, 20, 30, l4];
    let mut order: Vec<String> = vec![root.clone()];
    let mut nodes: HashMap<String, DiskUsageNode> = HashMap::new();
    nodes.insert(root.clone(), blank(&root, "C:\\", None));
    let mut level = vec![root.clone()];
    for f in fan {
        let mut next = Vec::new();
        for parent in &level {
            for i in 0..f {
                let word = WORDS[(rng.next() % WORDS.len() as u64) as usize];
                let name = format!("{word}{i:x}{:04x}", rng.next() & 0xffff);
                let id = format!(
                    "{}{}{}",
                    parent,
                    if parent.ends_with('\\') { "" } else { "\\" },
                    name
                );
                nodes.insert(id.clone(), blank(&id, &name, Some(parent)));
                nodes.get_mut(parent).unwrap().children_ids.push(id.clone());
                order.push(id.clone());
                next.push(id);
            }
        }
        level = next;
    }
    let dirs = order.len() as u64;
    let per_dir = (target_files / dirs).max(1);
    let mut files_by_parent: HashMap<String, Vec<DiskUsageFileEntry>> = HashMap::new();
    let mut total_files = 0u64;
    let mut total_logical = 0u64;
    for id in &order {
        let n = per_dir + (rng.next() % 5).saturating_sub(2);
        let mut list = Vec::with_capacity(n as usize);
        let mut logical = 0u64;
        for _ in 0..n {
            let size = (rng.next() % 4_000_000) >> (rng.next() % 14);
            let ext = EXTS[(rng.next() % EXTS.len() as u64) as usize];
            let name = format!(
                "{}-{:x}.{ext}",
                WORDS[(rng.next() % WORDS.len() as u64) as usize],
                rng.next() & 0xff_ffff
            );
            list.push(DiskUsageFileEntry {
                name,
                logical_bytes: size,
                allocated_bytes: Some((size + 4095) & !4095),
                is_symlink: false,
            });
            logical += size;
        }
        let node = nodes.get_mut(id).unwrap();
        node.direct_files = n;
        node.direct_logical_bytes = logical;
        node.direct_allocated_bytes = Some(logical + n * 2048);
        total_files += n;
        total_logical += logical;
        files_by_parent.insert(id.clone(), list);
    }
    // Roll subtree totals up (children were created after their parents).
    for id in order.iter().rev() {
        let (f, d, l, a, parent) = {
            let n = &nodes[id];
            (
                n.direct_files + n.subtree_files,
                1 + n.subtree_dirs,
                n.direct_logical_bytes + n.subtree_logical_bytes,
                n.direct_allocated_bytes.unwrap_or(0) + n.subtree_allocated_bytes.unwrap_or(0),
                n.parent_id.clone(),
            )
        };
        let n = nodes.get_mut(id).unwrap();
        n.subtree_files = f;
        n.subtree_dirs = d;
        n.subtree_logical_bytes = l;
        n.subtree_allocated_bytes = Some(a);
        if let Some(p) = parent {
            let pn = nodes.get_mut(&p).unwrap();
            pn.subtree_files += f;
            pn.subtree_dirs += d;
            pn.subtree_logical_bytes += l;
            pn.subtree_allocated_bytes = Some(pn.subtree_allocated_bytes.unwrap_or(0) + a);
        }
    }
    DiskUsageSnapshot {
        scan_id: "bench".into(),
        root_path: root.clone(),
        root_id: root,
        version: 1,
        scanned_at_ms: 1_800_000_000_000,
        nodes,
        files_by_parent,
        stats: DiskUsageStats {
            scanned_files: total_files,
            scanned_dirs: dirs,
            scanned_symlinks: 0,
            skipped_volume_boundaries: 0,
            logical_bytes: total_logical,
            allocated_available_files: total_files,
            allocated_unavailable_files: 0,
            known_allocated_bytes: Some(total_logical),
            total_errors: 0,
            coverage: Coverage::Complete,
            cancelled: false,
            depth_capped: false,
            duration_ms: 0,
        },
    }
}

#[test]
#[ignore = "builds a ~3.3 M file synthetic tree; run on demand"]
fn cache_size_and_load_time_for_a_big_root() {
    let files: u64 = std::env::var("FERRUM_BENCH_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_300_000);
    let l4: u64 = std::env::var("FERRUM_BENCH_DIRS_L4")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(33);
    let t = Instant::now();
    let snap = build(files, l4);
    println!(
        "built synthetic tree: {} dirs, {} files in {:?}",
        snap.stats.scanned_dirs,
        snap.stats.scanned_files,
        t.elapsed()
    );
    let dir = std::env::temp_dir().join(format!("ferrum-du-cache-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let t = Instant::now();
    let size = save(&dir, &snap, ScanEngine::NtfsMft).expect("save");
    let save_ms = t.elapsed();
    let t = Instant::now();
    let loaded = load(&dir, "C:\\", "bench-2".into(), 2).expect("load");
    let load_ms = t.elapsed();
    assert_eq!(loaded.snapshot.stats, snap.stats);
    assert_eq!(loaded.snapshot.nodes.len(), snap.nodes.len());
    println!(
        "RESULT files={} dirs={} cache_bytes={} ({:.1} MiB) save={:?} load={:?}",
        snap.stats.scanned_files,
        snap.stats.scanned_dirs,
        size,
        size as f64 / 1_048_576.0,
        save_ms,
        load_ms
    );
    let _ = std::fs::remove_dir_all(&dir);
}
