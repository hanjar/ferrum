//! DPAPI key file tests (design §8.4; FER-522 rework 1: no rotation on transient errors).

use super::*;

fn windows_only() -> bool {
    cfg!(windows)
}

#[test]
fn a_key_is_created_once_and_read_back() {
    if !windows_only() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = DpapiKeyFile::new(dir.path());
    assert_eq!(keys.load(), KeyLoad::Missing);
    let made = keys.load_or_create().unwrap();
    assert_eq!(keys.load(), KeyLoad::Key(made.clone()));
    // Creating again returns the same key (no silent rotation).
    assert_eq!(keys.load_or_create().unwrap(), made);
    // The file holds a DPAPI blob, not the key bytes.
    let blob = std::fs::read(keys.path()).unwrap();
    assert!(blob.len() > KEY_LEN);
    assert!(!blob.windows(KEY_LEN).any(|w| w == made.as_bytes()));
    // The lock file does not outlive the call.
    assert!(!dir.path().join(KEY_LOCK_FILE).exists());
}

#[test]
fn a_deleted_key_is_missing_and_a_new_one_differs() {
    if !windows_only() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = DpapiKeyFile::new(dir.path());
    let first = keys.load_or_create().unwrap();
    std::fs::remove_file(keys.path()).unwrap();
    assert_eq!(keys.load(), KeyLoad::Missing);
    assert_ne!(keys.load_or_create().unwrap(), first);
}

/// Another DPAPI scope (simulated by another entropy) cannot read the key - and is a blob that
/// was read in full, so it is the one case that may be replaced.
#[test]
fn another_entropy_cannot_decrypt() {
    if !windows_only() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    DpapiKeyFile::new(dir.path()).load_or_create().unwrap();
    let other = DpapiKeyFile::with_entropy(dir.path(), b"someone else");
    assert_eq!(other.load(), KeyLoad::Unusable);
}

/// An unusable blob is replaced only after it was backed up, and the rotation is announced once.
#[test]
fn an_unusable_blob_is_backed_up_then_replaced_and_announced() {
    if !windows_only() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = DpapiKeyFile::new(dir.path());
    std::fs::write(keys.path(), b"not a dpapi blob").unwrap();
    assert_eq!(keys.load(), KeyLoad::Unusable);
    assert!(!keys.take_rotation_notice());
    let made = keys.load_or_create().unwrap();
    assert_eq!(keys.load(), KeyLoad::Key(made));
    assert_eq!(
        std::fs::read(dir.path().join(KEY_BACKUP_FILE)).unwrap(),
        b"not a dpapi blob",
        "the old blob is kept"
    );
    assert!(keys.take_rotation_notice(), "announced");
    assert!(!keys.take_rotation_notice(), "only once");
}

/// FER-522 rework 1: an I/O error is NOT a reason to rotate. The file is held open exclusively by
/// someone else (sharing violation): reading says `Unreadable`, creating fails, and the key file
/// is byte-for-byte what it was - then everything works again once the other handle is gone.
#[cfg(windows)]
#[test]
fn a_sharing_violation_does_not_rotate_the_key() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let keys = DpapiKeyFile::new(dir.path());
    let made = keys.load_or_create().unwrap();
    let before = std::fs::read(keys.path()).unwrap();
    {
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(keys.path())
            .unwrap();
        assert_eq!(keys.load(), KeyLoad::Unreadable);
        assert_eq!(keys.load_or_create(), Err(KeyError));
        assert!(!dir.path().join(KEY_BACKUP_FILE).exists());
    }
    assert_eq!(std::fs::read(keys.path()).unwrap(), before);
    assert_eq!(keys.load(), KeyLoad::Key(made.clone()));
    assert_eq!(keys.load_or_create().unwrap(), made);
}

/// An oversized file or a key path that is a directory is `Unreadable` and is never replaced.
#[test]
fn an_oversized_or_non_file_key_path_is_never_replaced() {
    if !windows_only() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = DpapiKeyFile::new(dir.path());
    std::fs::write(keys.path(), vec![7u8; (MAX_BLOB_BYTES + 1) as usize]).unwrap();
    assert_eq!(keys.load(), KeyLoad::Unreadable);
    assert_eq!(keys.load_or_create(), Err(KeyError));
    assert_eq!(
        std::fs::metadata(keys.path()).unwrap().len(),
        MAX_BLOB_BYTES + 1
    );
    std::fs::remove_file(keys.path()).unwrap();
    std::fs::create_dir(keys.path()).unwrap();
    assert_eq!(keys.load(), KeyLoad::Unreadable);
    assert_eq!(keys.load_or_create(), Err(KeyError));
}

/// SE-5: a junction at the key path is opened as itself (never followed) and is not replaced.
#[cfg(windows)]
#[test]
fn a_key_file_that_is_a_junction_is_never_followed_or_replaced() {
    let base = tempfile::tempdir().unwrap();
    let target = base.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    let dir = base.path().join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let linked = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(dir.join(KEY_FILE))
        .arg(&target)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(linked, "junction created");
    let keys = DpapiKeyFile::new(&dir);
    assert_eq!(keys.load(), KeyLoad::Unreadable);
    assert_eq!(keys.load_or_create(), Err(KeyError));
    assert!(std::fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn debug_never_prints_the_key() {
    let key = ApprovalKey::for_tests(0xAB);
    assert!(!format!("{key:?}").contains("171"));
    assert!(format!("{key:?}").contains("redacted"));
}

/// The key wipes itself when dropped.
#[test]
fn the_key_is_zeroed_on_drop() {
    let mut key = std::mem::ManuallyDrop::new(ApprovalKey::for_tests(0xAB));
    assert!(key.as_bytes().iter().all(|b| *b == 0xAB));
    // SAFETY: the value is dropped exactly once (it is in ManuallyDrop and never dropped again);
    // its storage stays valid, so reading the wiped bytes afterwards is sound.
    unsafe { std::ptr::drop_in_place(&mut *key) };
    assert!(key.as_bytes().iter().all(|b| *b == 0));
}

/// Two creators in two "processes" (two instances, so no shared in-process mutex) end with one key:
/// the file lock serializes them.
#[test]
fn two_creators_end_with_one_key() {
    if !windows_only() {
        return;
    }
    for _ in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let a = DpapiKeyFile::new(dir.path());
        let b = DpapiKeyFile::new(dir.path());
        let (ka, kb) = std::thread::scope(|s| {
            let ha = s.spawn(|| a.load_or_create().unwrap());
            let hb = s.spawn(|| b.load_or_create().unwrap());
            (ha.join().unwrap(), hb.join().unwrap())
        });
        assert_eq!(ka, kb, "the loser keeps the winner's key");
        // Exactly one of them created it.
        assert_eq!(
            u8::from(a.take_rotation_notice()) + u8::from(b.take_rotation_notice()),
            1
        );
    }
}

/// A held lock makes a creator wait, not rotate: with the lock taken by someone else for longer
/// than the wait, creation fails and nothing is written.
#[cfg(windows)]
#[test]
fn a_held_lock_blocks_creation() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let _held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(dir.path().join(KEY_LOCK_FILE))
        .unwrap();
    let keys = DpapiKeyFile::new(dir.path());
    assert_eq!(keys.load_or_create(), Err(KeyError));
    assert!(!keys.path().exists());
}
