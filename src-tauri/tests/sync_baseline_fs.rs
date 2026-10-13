//! FER-414 (H2): bukti persistensi baseline dengan FILESYSTEM NYATA. Compare dan copy yang dipakai
//! adalah engine Rust sungguhan (`compare_dirs`, `run_copy`) pada direktori temporary milik tes;
//! penyimpanan yang diuji adalah `SyncBaselineStore` asli (file di disk, dibuka ulang = restart).
//!
//! Yang TIDAK dilakukan di sini: klasifikasi aksi (`syncPlan.ts`/`reconcileBaseline`) hidup di
//! TypeScript dan diuji di vitest (`src/lib/syncBaselineFlow.test.ts`, IPC tiruan berbasis
//! filesystem). Di sini dibuktikan fakta dasar yang dipakai perencana: baseline tersimpan
//! mencatat fakta kedua sisi, dan modifikasi/hapus nyata mengubah fakta itu seperti yang diharapkan.
//! Tidak ada tes yang menyentuh folder pengguna.

use ferrum_lib::compare::{CompareDirsReport, DirDiffState, compare_dirs};
use ferrum_lib::copy_engine::{ConflictPolicy, run_copy};
use ferrum_lib::domain::OperationProgress;
use ferrum_lib::ops::CancelToken;
use ferrum_lib::sync_baseline::{
    BASELINE_VERSION, BaselineDto, EntryDto, FactsDto, LoadResult, RootPair, SyncBaselineStore,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrum-fer414-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn silent(_: OperationProgress) {}

struct Fixture {
    base: PathBuf,
    left: PathBuf,
    right: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = temp(tag);
    let left = base.join("left");
    let right = base.join("right");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    Fixture { base, left, right }
}

fn pair(l: &Path, r: &Path) -> RootPair {
    RootPair {
        left_root: s(l),
        right_root: s(r),
    }
}

fn compare(l: &Path, r: &Path) -> CompareDirsReport {
    compare_dirs(&s(l), &s(r), true, 0).unwrap()
}

/// Hanya hasil TERVERIFIKASI (state Same pada compare segar) menjadi entri baseline: aturan
/// yang sama dengan `reconcileBaseline`.
fn baseline_from(p: &RootPair, report: &CompareDirsReport) -> BaselineDto {
    let mut entries = BTreeMap::new();
    for e in &report.entries {
        if e.state == DirDiffState::Same {
            entries.insert(
                e.name.clone(),
                EntryDto {
                    left: FactsDto {
                        size: e.left_size,
                        modified_ms: e.left_modified_ms,
                    },
                    right: FactsDto {
                        size: e.right_size,
                        modified_ms: e.right_modified_ms,
                    },
                },
            );
        }
    }
    BaselineDto {
        version: BASELINE_VERSION,
        left_root: p.left_root.clone(),
        right_root: p.right_root.clone(),
        entries,
        journal: None,
    }
}

fn copy_all_missing(f: &Fixture, report: &CompareDirsReport) {
    let cancel = CancelToken::default();
    for e in &report.entries {
        let (src, dst) = match e.state {
            DirDiffState::LeftOnly => (f.left.join(&e.name), &f.right),
            DirDiffState::RightOnly => (f.right.join(&e.name), &f.left),
            _ => continue,
        };
        let out = run_copy(&[s(&src)], &s(dst), ConflictPolicy::Ask, &cancel, &silent);
        assert!(out.failures.is_empty() && !out.cancelled);
    }
}

/// Aturan perubahan sisi yang sama dengan `sideChange` di TS (toleransi mtime 2 s).
fn changed(base: &FactsDto, size: u64, ms: i64) -> bool {
    base.size != size || (base.modified_ms - ms).abs() > 2000
}

/// Jalankan union first-run nyata dan simpan baseline dari compare segar.
fn first_run_and_persist(f: &Fixture, store: &SyncBaselineStore) -> RootPair {
    fs::write(f.left.join("only-left.txt"), b"from left").unwrap();
    fs::write(f.right.join("only-right.txt"), b"from right").unwrap();
    fs::write(f.left.join("both.txt"), b"identical").unwrap();
    fs::write(f.right.join("both.txt"), b"identical").unwrap();
    let p = pair(&f.left, &f.right);

    let report = compare(&f.left, &f.right);
    copy_all_missing(f, &report);
    let after = compare(&f.left, &f.right);
    assert!(after.entries.iter().all(|e| e.state == DirDiffState::Same));
    store.save(&p, &baseline_from(&p, &after)).unwrap();
    p
}

#[test]
fn first_run_union_then_restart_keeps_the_baseline() {
    let f = fixture("restart");
    let store_dir = f.base.join("store");
    let store = SyncBaselineStore::open(&store_dir);
    let p = first_run_and_persist(&f, &store);

    // Restart: store baru dari direktori yang sama.
    let reopened = SyncBaselineStore::open(&store_dir);
    let LoadResult::Ok { baseline } = reopened.load(&p).unwrap() else {
        panic!("baseline must survive a restart")
    };
    let names: Vec<&str> = baseline.entries.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["both.txt", "only-left.txt", "only-right.txt"]);
    let _ = fs::remove_dir_all(&f.base);
}

#[test]
fn both_changed_is_visible_from_stored_facts() {
    let f = fixture("both-changed");
    let store = SyncBaselineStore::open(&f.base.join("store"));
    let p = first_run_and_persist(&f, &store);
    fs::write(f.left.join("both.txt"), b"left edit, longer").unwrap();
    fs::write(f.right.join("both.txt"), b"right edit!!").unwrap();

    let report = compare(&f.left, &f.right);
    let e = report
        .entries
        .iter()
        .find(|e| e.name == "both.txt")
        .unwrap();
    assert_ne!(
        e.state,
        DirDiffState::Same,
        "real compare sees the difference"
    );
    let LoadResult::Ok { baseline } = store.load(&p).unwrap() else {
        panic!("load")
    };
    let base = &baseline.entries["both.txt"];
    assert!(
        changed(&base.left, e.left_size, e.left_modified_ms),
        "left changed vs baseline"
    );
    assert!(
        changed(&base.right, e.right_size, e.right_modified_ms),
        "right changed vs baseline"
    );
    let _ = fs::remove_dir_all(&f.base);
}

#[test]
fn modify_vs_delete_is_visible_and_nothing_is_deleted_by_the_check() {
    let f = fixture("modify-delete");
    let store = SyncBaselineStore::open(&f.base.join("store"));
    let p = first_run_and_persist(&f, &store);
    fs::write(f.left.join("both.txt"), b"left modified after baseline").unwrap();
    fs::remove_file(f.right.join("both.txt")).unwrap();

    let report = compare(&f.left, &f.right);
    let e = report
        .entries
        .iter()
        .find(|e| e.name == "both.txt")
        .unwrap();
    assert_eq!(e.state, DirDiffState::LeftOnly);
    let LoadResult::Ok { baseline } = store.load(&p).unwrap() else {
        panic!("load")
    };
    let base = &baseline.entries["both.txt"];
    assert!(
        changed(&base.left, e.left_size, e.left_modified_ms),
        "left was modified"
    );
    assert!(f.left.join("both.txt").exists(), "nothing was deleted");
    let _ = fs::remove_dir_all(&f.base);
}

#[test]
fn one_sided_delete_keeps_the_survivor_and_the_baseline_entry() {
    let f = fixture("no-delete");
    let store = SyncBaselineStore::open(&f.base.join("store"));
    let p = first_run_and_persist(&f, &store);
    fs::remove_file(f.right.join("only-left.txt")).unwrap();

    let report = compare(&f.left, &f.right);
    let e = report
        .entries
        .iter()
        .find(|e| e.name == "only-left.txt")
        .unwrap();
    assert_eq!(e.state, DirDiffState::LeftOnly);
    let LoadResult::Ok { baseline } = store.load(&p).unwrap() else {
        panic!("load")
    };
    let base = &baseline.entries["only-left.txt"];
    assert!(
        !changed(&base.left, e.left_size, e.left_modified_ms),
        "survivor unchanged"
    );
    // Pasangan baseline + 'hilang di kanan' = deletedRight (konflik di perencana), bukan delete.
    assert_eq!(
        fs::read(f.left.join("only-left.txt")).unwrap(),
        b"from left"
    );
    let _ = fs::remove_dir_all(&f.base);
}

#[test]
fn corrupt_baseline_on_disk_is_first_run() {
    let f = fixture("corrupt");
    let store_dir = f.base.join("store");
    let store = SyncBaselineStore::open(&store_dir);
    let p = first_run_and_persist(&f, &store);
    for entry in fs::read_dir(&store_dir).unwrap().flatten() {
        fs::write(entry.path(), b"\0\0garbage").unwrap();
    }
    assert!(matches!(
        store.load(&p).unwrap(),
        LoadResult::FirstRun { .. }
    ));
    let _ = fs::remove_dir_all(&f.base);
}

#[test]
fn two_real_pairs_are_independent() {
    let f1 = fixture("pair1");
    let f2 = fixture("pair2");
    let store_dir = f1.base.join("store");
    let store = SyncBaselineStore::open(&store_dir);
    let p1 = first_run_and_persist(&f1, &store);
    // Pasangan 2 hanya punya satu file; disimpan di store yang sama.
    fs::write(f2.left.join("solo.txt"), b"x").unwrap();
    fs::write(f2.right.join("solo.txt"), b"x").unwrap();
    let p2 = pair(&f2.left, &f2.right);
    let after = compare(&f2.left, &f2.right);
    store.save(&p2, &baseline_from(&p2, &after)).unwrap();

    let LoadResult::Ok { baseline: b1 } = store.load(&p1).unwrap() else {
        panic!("p1")
    };
    let LoadResult::Ok { baseline: b2 } = store.load(&p2).unwrap() else {
        panic!("p2")
    };
    assert_eq!(b1.entries.len(), 3);
    assert_eq!(b2.entries.keys().collect::<Vec<_>>(), vec!["solo.txt"]);
    store.clear(&p2).unwrap();
    assert!(matches!(store.load(&p1).unwrap(), LoadResult::Ok { .. }));
    let _ = fs::remove_dir_all(&f1.base);
    let _ = fs::remove_dir_all(&f2.base);
}

#[test]
fn nested_and_same_roots_are_rejected_before_any_baseline_io() {
    let f = fixture("reject");
    let store_dir = f.base.join("store");
    let store = SyncBaselineStore::open(&store_dir);
    let inner = f.left.join("inner");
    fs::create_dir_all(&inner).unwrap();
    let dto = |p: &RootPair| BaselineDto {
        version: BASELINE_VERSION,
        left_root: p.left_root.clone(),
        right_root: p.right_root.clone(),
        entries: BTreeMap::new(),
        journal: None,
    };
    for p in [
        pair(&f.left, &f.left),
        pair(&f.left, &inner),
        pair(&inner, &f.left),
    ] {
        assert_eq!(store.load(&p).unwrap_err().code, "invalid_roots");
        assert_eq!(store.save(&p, &dto(&p)).unwrap_err().code, "invalid_roots");
    }
    let files = fs::read_dir(&store_dir).unwrap().flatten().count();
    assert_eq!(files, 0, "rejected roots write nothing");
    let _ = fs::remove_dir_all(&f.base);
}

/// Alias junction (Windows): jalur lewat junction = pasangan yang sama dengan jalur asli.
/// Dilewati bila junction tidak dapat dibuat di lingkungan ini.
#[cfg(windows)]
#[test]
fn junction_alias_of_a_root_finds_the_same_baseline() {
    let f = fixture("junction");
    let store = SyncBaselineStore::open(&f.base.join("store"));
    let p = first_run_and_persist(&f, &store);
    let alias = f.base.join("left-alias");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J", &s(&alias), &s(&f.left)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !made {
        eprintln!("junction could not be created here; skipping");
        let _ = fs::remove_dir_all(&f.base);
        return;
    }
    let aliased = pair(&alias, &f.right);
    let LoadResult::Ok { baseline } = store.load(&aliased).unwrap() else {
        panic!("alias of the same root must resolve to the same baseline")
    };
    assert_eq!(baseline.entries.len(), 3);
    // Alias dan aslinya sebagai pasangan sama-sama ditolak.
    assert_eq!(
        store.load(&pair(&alias, &f.left)).unwrap_err().code,
        "invalid_roots"
    );
    let _ = p;
    let _ = fs::remove_dir(&alias);
    let _ = fs::remove_dir_all(&f.base);
}
