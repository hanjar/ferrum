//! AI-10 (FER-212/213/214): bukti perilaku nyata untuk primitif engine sync
//! dua arah pada filesystem temporary milik tes. Perencana (syncPlan.ts) ada
//! di frontend dan dites di vitest; di sini yang dibuktikan adalah sisi Rust:
//! compare read-only, penyalinan union converge, dan jaminan overwrite.
//! Tidak ada tes yang menyentuh folder pengguna.

use ferrum_lib::compare::{DirDiffState, compare_dirs, validate_sync_roots};
use ferrum_lib::copy_engine::{ConflictPolicy, CopyOptions, run_copy, run_copy_with_options};
use ferrum_lib::domain::OperationProgress;
use ferrum_lib::ops::CancelToken;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrum-ai10-{tag}-{}-{}",
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

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Spec bagian 6, first-run: union tanpa delete. Pemetaan state -> arah di
/// bawah adalah aturan tabel itu sendiri; yang diuji adalah bahwa engine
/// compare + copy menghasilkan dua tree identik tanpa kehilangan satu byte.
#[test]
fn first_run_union_converges_without_deleting_anything() {
    let left = temp("union-l");
    let right = temp("union-r");
    fs::write(left.join("only-left.txt"), b"from left").unwrap();
    fs::write(right.join("only-right.txt"), b"from right").unwrap();
    fs::write(left.join("both.txt"), b"identical").unwrap();
    fs::write(right.join("both.txt"), b"identical").unwrap();

    assert!(validate_sync_roots(&s(&left), &s(&right)).is_ok());
    let report = compare_dirs(&s(&left), &s(&right), true, 0).unwrap();
    let cancel = CancelToken::default();
    for e in &report.entries {
        match e.state {
            DirDiffState::LeftOnly => {
                let out = run_copy(
                    &[s(&left.join(&e.name))],
                    &s(&right),
                    ConflictPolicy::Ask,
                    &cancel,
                    &silent,
                );
                assert!(out.failures.is_empty() && !out.cancelled);
            }
            DirDiffState::RightOnly => {
                let out = run_copy(
                    &[s(&right.join(&e.name))],
                    &s(&left),
                    ConflictPolicy::Ask,
                    &cancel,
                    &silent,
                );
                assert!(out.failures.is_empty() && !out.cancelled);
            }
            DirDiffState::Same => {}
            other => panic!("unexpected state in a union fixture: {other:?}"),
        }
    }

    assert_eq!(tree(&left), tree(&right), "both trees must hold the union");
    assert_eq!(tree(&left).len(), 3, "nothing deleted, nothing lost");
    let after = compare_dirs(&s(&left), &s(&right), true, 0).unwrap();
    assert!(after.entries.iter().all(|e| e.state == DirDiffState::Same));
    let _ = fs::remove_dir_all(&left);
    let _ = fs::remove_dir_all(&right);
}

/// Spec bagian 6 butir 1: root same/nested/alias ditolak SEBELUM ada job
/// yang boleh menulis. Tree tidak berubah oleh pengecekan itu.
#[test]
fn rejected_roots_leave_both_trees_untouched() {
    let root = temp("rej");
    fs::write(root.join("a.txt"), b"keep").unwrap();
    let inner = root.join("inner");
    fs::create_dir(&inner).unwrap();
    fs::write(inner.join("b.txt"), b"keep too").unwrap();
    let before = (tree(&root), tree(&inner));
    assert!(validate_sync_roots(&s(&root), &s(&root)).is_err());
    assert!(validate_sync_roots(&s(&root), &s(&inner)).is_err());
    assert!(validate_sync_roots(&s(&inner), &s(&root)).is_err());
    assert_eq!((tree(&root), tree(&inner)), before);
    let _ = fs::remove_dir_all(&root);
}

/// Jaminan engine: pekerjaan yang di-cancel di tengah overwrite tidak boleh
/// menghilangkan isi tujuan lama. Pada base AI-10 (`64fc71e`) copy engine
/// memotong tujuan lebih dulu lalu menghapusnya saat cancel, sehingga tes ini
/// MERAH; perbaikannya (staging, FER-385.B/FER-393 di copy_engine.rs) sudah
/// ter-merge dan tes ini hijau, jadi `#[ignore]` dicabut.
#[test]
fn cancelled_overwrite_keeps_the_previous_target_content() {
    let root = temp("ow-cancel");
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    fs::write(src.join("data.bin"), vec![9u8; 96 * 1024 * 1024]).unwrap();
    fs::write(dst.join("data.bin"), b"OLD TARGET CONTENT").unwrap();

    // FER-459: cancel from the engine's own progress report instead of polling the destination
    // folder from a second thread. The old watcher raced the copy: on Windows a directory listing
    // reports a stale size for a file still open for writing, so it could miss the staged file
    // until the copy had already finished. The first report is emitted as soon as the workers
    // start, long before the file can be written, so the job is cancelled mid-flight every time;
    // the exact mid-chunk cancel is pinned in copy_engine's tests with the chunk hook.
    let token = CancelToken::default();
    let cancel_on_report = token.clone();
    let out = run_copy_with_options(
        &[s(&src.join("data.bin"))],
        &s(&dst),
        ConflictPolicy::Overwrite,
        CopyOptions {
            stream_only: true,
            buffer_size: 64 * 1024,
            ..CopyOptions::default()
        },
        &token,
        &move |_: OperationProgress| cancel_on_report.cancel(),
    );
    assert!(out.cancelled);
    assert_eq!(
        fs::read(dst.join("data.bin")).ok().as_deref(),
        Some(&b"OLD TARGET CONTENT"[..]),
        "cancel must leave the previous target intact"
    );
    let left: Vec<String> = fs::read_dir(&dst)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        vec!["data.bin".to_string()],
        "no staging file left behind"
    );
    assert_eq!(
        fs::read(src.join("data.bin")).unwrap().len(),
        96 * 1024 * 1024
    );
    let _ = fs::remove_dir_all(&root);
}
