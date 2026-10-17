//! The approval key (Automation design §8.4): 32 random bytes, protected with Windows DPAPI for
//! the CURRENT USER (`CryptProtectData`, no machine scope, `CRYPTPROTECT_UI_FORBIDDEN`) plus a
//! fixed entropy string, stored as an opaque blob in `approval.key` next to the automation store.
//!
//! Why: an approval MAC keyed with this secret binds approvals to this Windows user on this
//! machine. A store restored from a backup, copied from another PC or edited by anything that
//! cannot call DPAPI as the user no longer verifies, and the automations fall back to
//! "Needs review" (fail closed, threat T3/T7). It is NOT a defence against malware running as the
//! same user (out of scope, §10.1).
//!
//! Missing, unreadable or undecryptable key = [`KeyLoad::Missing`] / [`KeyLoad::Unusable`]: no MAC
//! can be computed or verified. Only [`ApprovalKeys::load_or_create`] ever makes a key, and only
//! when approving (or when a profile baseline is signed); a corrupt key is replaced then.
//!
//! Tauri-free: the runner links this module.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Fixed DPAPI entropy (design §8.4). A different entropy simulates "another DPAPI scope" in tests.
pub const ENTROPY: &[u8] = b"ferrum.automation.approval.v1";
/// File name of the protected key blob inside the store directory.
pub const KEY_FILE: &str = "approval.key";
/// Key length in bytes.
pub const KEY_LEN: usize = 32;
/// A blob larger than this is not ours (a DPAPI blob for 32 bytes is a few hundred bytes).
const MAX_BLOB_BYTES: u64 = 16 * 1024;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The secret. Never printed (Debug is redacted) and not serializable.
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalKey(pub(crate) [u8; KEY_LEN]);

impl std::fmt::Debug for ApprovalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalKey(<redacted>)")
    }
}

impl ApprovalKey {
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// A fixed key for tests that do not exercise DPAPI.
    #[cfg(test)]
    pub fn for_tests(byte: u8) -> Self {
        ApprovalKey([byte; KEY_LEN])
    }
}

/// What reading the key gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyLoad {
    Key(ApprovalKey),
    /// No key file (first use, or it was deleted).
    Missing,
    /// A key file exists but cannot be used: other user/machine/entropy, corrupt, a reparse point.
    Unusable,
}

/// Creating a key failed (no entropy, DPAPI refused, the file could not be written). The caller
/// fails closed: nothing is approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyError;

/// Source of the approval key. Production uses [`DpapiKeyFile`]; tests use [`MemoryKeys`] where
/// DPAPI is not the subject.
pub trait ApprovalKeys: Send + Sync {
    /// Read the key without ever creating one (verification, gate).
    fn load(&self) -> KeyLoad;
    /// The key, creating it when there is none or the stored one is unusable (approval).
    fn load_or_create(&self) -> Result<ApprovalKey, KeyError>;
}

/// The DPAPI-protected key file.
pub struct DpapiKeyFile {
    path: PathBuf,
    entropy: Vec<u8>,
    /// One creator at a time inside this process.
    create_lock: Mutex<()>,
}

impl DpapiKeyFile {
    /// `dir` is the automation store directory (already verified plain by the store).
    pub fn new(dir: &Path) -> Self {
        Self::with_entropy(dir, ENTROPY)
    }

    /// Same file, another entropy: how tests simulate a key protected in another DPAPI scope.
    pub fn with_entropy(dir: &Path, entropy: &[u8]) -> Self {
        Self {
            path: dir.join(KEY_FILE),
            entropy: entropy.to_vec(),
            create_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_blob(&self) -> Result<Option<Vec<u8>>, ()> {
        match std::fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
            Ok(meta) => {
                // SE-5: a junction/symlink at the key path is never followed.
                if is_reparse_meta(&meta) || !meta.is_file() || meta.len() > MAX_BLOB_BYTES {
                    return Err(());
                }
            }
        }
        std::fs::read(&self.path).map(Some).map_err(|_| ())
    }

    fn decode(&self, blob: &[u8]) -> KeyLoad {
        match unprotect(blob, &self.entropy) {
            Some(plain) if plain.len() == KEY_LEN => {
                let mut key = [0u8; KEY_LEN];
                key.copy_from_slice(&plain);
                KeyLoad::Key(ApprovalKey(key))
            }
            _ => KeyLoad::Unusable,
        }
    }

    /// Write `blob` as a new key file. `replace` = overwrite an existing unusable file;
    /// otherwise the file is created only if nobody else created it first (`Ok(false)`).
    fn publish(&self, blob: &[u8], replace: bool) -> Result<bool, KeyError> {
        use std::io::Write;
        let dir = self.path.parent().ok_or(KeyError)?;
        let tmp = dir.join(format!(
            "{KEY_FILE}.tmp-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let write = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(blob)?;
            f.sync_all()
        })();
        if write.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err(KeyError);
        }
        let outcome = if replace {
            std::fs::rename(&tmp, &self.path).map(|()| true)
        } else {
            // Atomic "create if absent": the loser of a race (the runner and the app creating
            // the first key at once) keeps the winner's key instead of replacing it.
            match std::fs::hard_link(&tmp, &self.path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        };
        let _ = std::fs::remove_file(&tmp);
        outcome.map_err(|_| KeyError)
    }
}

impl ApprovalKeys for DpapiKeyFile {
    fn load(&self) -> KeyLoad {
        match self.read_blob() {
            Ok(Some(blob)) => self.decode(&blob),
            Ok(None) => KeyLoad::Missing,
            Err(()) => KeyLoad::Unusable,
        }
    }

    fn load_or_create(&self) -> Result<ApprovalKey, KeyError> {
        let _one = self.create_lock.lock().unwrap_or_else(|p| p.into_inner());
        // At most two rounds: a lost creation race re-reads the winner's key.
        for _ in 0..2 {
            let replace = match self.load() {
                KeyLoad::Key(key) => return Ok(key),
                KeyLoad::Missing => false,
                KeyLoad::Unusable => true,
            };
            let mut bytes = [0u8; KEY_LEN];
            getrandom::fill(&mut bytes).map_err(|_| KeyError)?;
            let blob = protect(&bytes, &self.entropy).ok_or(KeyError)?;
            if self.publish(&blob, replace)? {
                return Ok(ApprovalKey(bytes));
            }
        }
        Err(KeyError)
    }
}

fn is_reparse_meta(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        meta.file_attributes() & 0x0400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// No key store at all (no resolvable local data folder): every approval stays unverifiable and
/// nothing can be approved (fail closed).
pub struct NoKeys;

impl ApprovalKeys for NoKeys {
    fn load(&self) -> KeyLoad {
        KeyLoad::Missing
    }

    fn load_or_create(&self) -> Result<ApprovalKey, KeyError> {
        Err(KeyError)
    }
}

/// An in-memory key source (tests of code that signs and verifies but is not about DPAPI).
#[cfg(test)]
pub struct MemoryKeys {
    key: Mutex<KeyLoad>,
}

#[cfg(test)]
impl MemoryKeys {
    pub fn with_key(byte: u8) -> Self {
        Self {
            key: Mutex::new(KeyLoad::Key(ApprovalKey::for_tests(byte))),
        }
    }

    pub fn missing() -> Self {
        Self {
            key: Mutex::new(KeyLoad::Missing),
        }
    }

    pub fn set(&self, state: KeyLoad) {
        *self.key.lock().unwrap() = state;
    }
}

#[cfg(test)]
impl ApprovalKeys for MemoryKeys {
    fn load(&self) -> KeyLoad {
        self.key.lock().unwrap().clone()
    }

    fn load_or_create(&self) -> Result<ApprovalKey, KeyError> {
        let mut guard = self.key.lock().unwrap();
        match &*guard {
            KeyLoad::Key(key) => Ok(key.clone()),
            _ => {
                let key = ApprovalKey::for_tests(7);
                *guard = KeyLoad::Key(key.clone());
                Ok(key)
            }
        }
    }
}

// ── DPAPI ──

/// Encrypt for the current Windows user with `entropy`. `None` = DPAPI refused (fail closed).
#[cfg(windows)]
fn protect(plain: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};
    let input = blob_of(plain)?;
    let entropy_blob = blob_of(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `input` and `entropy_blob` point at live slices for the whole call; the optional
    // pointers that are unused are null; `out` is a valid out-parameter. On success the system
    // allocated `out.pbData`, which `take_output` copies and frees with `LocalFree`.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy_blob,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return None;
    }
    take_output(out)
}

/// Decrypt a blob made by [`protect`]. `None` = other user/machine/entropy or corrupt.
#[cfg(windows)]
fn unprotect(blob: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};
    let input = blob_of(blob)?;
    let entropy_blob = blob_of(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: same contract as `protect`: live input slices, null for the unused optional
    // parameters, a valid out-parameter that `take_output` copies and frees.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy_blob,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return None;
    }
    take_output(out)
}

/// `CRYPTPROTECT_UI_FORBIDDEN`: DPAPI must never show a prompt.
#[cfg(windows)]
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

#[cfg(windows)]
fn blob_of(bytes: &[u8]) -> Option<windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB> {
    Some(
        windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(bytes.len()).ok()?,
            pbData: bytes.as_ptr() as *mut u8,
        },
    )
}

/// Copy the system-allocated output and free it.
#[cfg(windows)]
fn take_output(
    out: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    if out.pbData.is_null() {
        return None;
    }
    // SAFETY: on success DPAPI returns a buffer of `cbData` bytes at `pbData`, valid until freed.
    let copy = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    // SAFETY: `pbData` was allocated by the system with LocalAlloc and is freed exactly once.
    unsafe {
        LocalFree(out.pbData as _);
    }
    Some(copy)
}

/// Off Windows nothing can be protected: no key ever exists (fail closed).
#[cfg(not(windows))]
fn protect(_plain: &[u8], _entropy: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn unprotect(_blob: &[u8], _entropy: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_created_once_and_read_back() {
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
    }

    #[test]
    fn a_deleted_key_is_missing_and_a_new_one_differs() {
        let dir = tempfile::tempdir().unwrap();
        let keys = DpapiKeyFile::new(dir.path());
        let first = keys.load_or_create().unwrap();
        std::fs::remove_file(keys.path()).unwrap();
        assert_eq!(keys.load(), KeyLoad::Missing);
        assert_ne!(keys.load_or_create().unwrap(), first);
    }

    /// Another DPAPI scope (simulated by another entropy) cannot read the key.
    #[test]
    fn another_entropy_cannot_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        DpapiKeyFile::new(dir.path()).load_or_create().unwrap();
        let other = DpapiKeyFile::with_entropy(dir.path(), b"someone else");
        assert_eq!(other.load(), KeyLoad::Unusable);
    }

    #[test]
    fn a_corrupt_blob_is_unusable_and_is_replaced_on_create() {
        let dir = tempfile::tempdir().unwrap();
        let keys = DpapiKeyFile::new(dir.path());
        std::fs::write(keys.path(), b"not a dpapi blob").unwrap();
        assert_eq!(keys.load(), KeyLoad::Unusable);
        let made = keys.load_or_create().unwrap();
        assert_eq!(keys.load(), KeyLoad::Key(made));
    }

    #[test]
    fn a_key_file_that_is_a_junction_is_never_followed() {
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
        assert_eq!(DpapiKeyFile::new(&dir).load(), KeyLoad::Unusable);
    }

    #[test]
    fn debug_never_prints_the_key() {
        let key = ApprovalKey::for_tests(0xAB);
        assert!(!format!("{key:?}").contains("171"));
        assert!(format!("{key:?}").contains("redacted"));
    }

    #[test]
    fn two_creators_end_with_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let a = DpapiKeyFile::new(dir.path());
        let b = DpapiKeyFile::new(dir.path());
        let (ka, kb) = std::thread::scope(|s| {
            let ha = s.spawn(|| a.load_or_create().unwrap());
            let hb = s.spawn(|| b.load_or_create().unwrap());
            (ha.join().unwrap(), hb.join().unwrap())
        });
        assert_eq!(ka, kb, "the loser of the race keeps the winner's key");
    }
}
