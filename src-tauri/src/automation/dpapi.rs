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
//! ## When a key may be made or replaced (FER-522 rework 1)
//!
//! Reading never creates anything. [`ApprovalKeys::load_or_create`] is called by an APPROVAL only
//! (never by baseline signing) and:
//!
//! - creates a key when there is none (`Missing`);
//! - replaces a key ONLY when the blob was read completely and DPAPI then refused it or it has the
//!   wrong size (`Unusable`: another user, machine or entropy, or a corrupt blob). The old blob is
//!   copied to `approval.key.bak` first; without that backup nothing is replaced;
//! - never touches the key after an I/O problem (`Unreadable`: sharing violation, access denied,
//!   a reparse point, an oversized file). A transient error must not rotate the key, which would
//!   invalidate every approval; the approval simply fails and can be retried;
//! - takes a cross-process lock (`approval.key.lock`, exclusive, held while it decides and writes)
//!   so the app and the runner cannot create or replace the key at the same time.
//!
//! The first creation or any replacement raises a notice ([`ApprovalKeys::take_rotation_notice`])
//! that the approval turns into an event on every automation, because earlier approvals no longer
//! verify. Secrets are wiped: the DPAPI output buffer is zeroed before it is freed, temporary
//! copies are `Zeroizing`, and [`ApprovalKey`] zeroes itself on drop.
//!
//! Tauri-free: the runner links this module.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use zeroize::{Zeroize, Zeroizing};

/// Fixed DPAPI entropy (design §8.4). A different entropy simulates "another DPAPI scope" in tests.
pub const ENTROPY: &[u8] = b"ferrum.automation.approval.v1";
/// File name of the protected key blob inside the store directory.
pub const KEY_FILE: &str = "approval.key";
/// Backup of a blob that was replaced.
pub const KEY_BACKUP_FILE: &str = "approval.key.bak";
/// Cross-process lock around creating or replacing the key.
pub const KEY_LOCK_FILE: &str = "approval.key.lock";
/// Key length in bytes.
pub const KEY_LEN: usize = 32;
/// A blob larger than this is not ours (a DPAPI blob for 32 bytes is a few hundred bytes).
const MAX_BLOB_BYTES: u64 = 16 * 1024;
/// How long creating the key waits for another process holding the lock.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The secret. Never printed (Debug is redacted), not serializable, wiped when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalKey(pub(crate) [u8; KEY_LEN]);

impl std::fmt::Debug for ApprovalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalKey(<redacted>)")
    }
}

impl Drop for ApprovalKey {
    fn drop(&mut self) {
        self.0.zeroize();
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
    /// The blob was read in full but cannot be used: another user, machine or entropy, corrupt,
    /// wrong size. The only state an approval may replace.
    Unusable,
    /// The file could not be read reliably (I/O error, sharing violation, reparse point,
    /// oversized): possibly transient, NEVER replaced.
    Unreadable,
}

/// Creating a key failed (no entropy, DPAPI refused, an unreadable existing file, the lock was
/// busy, the file could not be written). The caller fails closed: nothing is approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyError;

/// Source of the approval key. Production uses [`DpapiKeyFile`]; tests use `MemoryKeys` where
/// DPAPI is not the subject.
pub trait ApprovalKeys: Send + Sync {
    /// Read the key without ever creating one (verification, gate, baseline signing).
    fn load(&self) -> KeyLoad;
    /// The key, creating it when there is none and replacing it when it is `Unusable`. For an
    /// approval only.
    fn load_or_create(&self) -> Result<ApprovalKey, KeyError>;
    /// The key, creating it ONLY when there is no key file at all (`Missing`). An existing key,
    /// whether usable or not, is never replaced here and an unreadable file is an error. For
    /// baseline signing, so a first manual profile run does not have to wait for an approval.
    fn create_if_missing(&self) -> Result<ApprovalKey, KeyError>;
    /// True once after a key was created or replaced: earlier approvals no longer verify and the
    /// approval records an event for them.
    fn take_rotation_notice(&self) -> bool {
        false
    }
}

/// The DPAPI-protected key file.
pub struct DpapiKeyFile {
    path: PathBuf,
    entropy: Vec<u8>,
    /// One creator at a time inside this process.
    create_lock: Mutex<()>,
    rotated: AtomicBool,
}

/// What looking at the key file found, with the raw blob when it was read in full.
enum Inspect {
    Key(ApprovalKey),
    Missing,
    Unusable(Vec<u8>),
    Unreadable,
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
            rotated: AtomicBool::new(false),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn backup_path(&self) -> PathBuf {
        self.path.with_file_name(KEY_BACKUP_FILE)
    }

    /// Open the file WITHOUT following a reparse point and read it through that one handle, so
    /// there is no gap between "is it a link?" and "read it" (SE-5).
    fn read_blob(&self) -> Result<Option<Vec<u8>>, ()> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_OPEN_REPARSE_POINT: open the link itself, never its target.
            options.custom_flags(0x0020_0000);
        }
        let file = match options.open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        };
        let meta = file.metadata().map_err(|_| ())?;
        if is_reparse_meta(&meta) || !meta.is_file() || meta.len() > MAX_BLOB_BYTES {
            return Err(());
        }
        let mut blob = Vec::with_capacity(meta.len() as usize);
        file.take(MAX_BLOB_BYTES + 1)
            .read_to_end(&mut blob)
            .map_err(|_| ())?;
        if blob.len() as u64 > MAX_BLOB_BYTES {
            return Err(());
        }
        Ok(Some(blob))
    }

    fn inspect(&self) -> Inspect {
        match self.read_blob() {
            Ok(None) => Inspect::Missing,
            Err(()) => Inspect::Unreadable,
            Ok(Some(blob)) => match unprotect(&blob, &self.entropy) {
                Some(plain) if plain.len() == KEY_LEN => {
                    let mut bytes = [0u8; KEY_LEN];
                    bytes.copy_from_slice(&plain);
                    let key = ApprovalKey(bytes);
                    bytes.zeroize();
                    Inspect::Key(key)
                }
                _ => Inspect::Unusable(blob),
            },
        }
    }

    /// Write `blob` as the key file. `replace` = overwrite an existing unusable file (its old
    /// content was backed up by the caller); otherwise the file is created only if nobody else
    /// created it first (`Ok(false)`).
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
            // Atomic "create if absent".
            match std::fs::hard_link(&tmp, &self.path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        };
        let _ = std::fs::remove_file(&tmp);
        outcome.map_err(|_| KeyError)
    }

    /// Keep the blob that is about to be replaced. No backup, no replacement. An earlier backup is
    /// never overwritten (FER-524, review P1.1 #6): the first one is `approval.key.bak`, later
    /// ones get a timestamp (and a counter when the clock repeats), so the oldest blob - possibly
    /// the only one a restored profile can still use - survives repeated replacements.
    fn back_up(&self, old: &[u8]) -> Result<(), KeyError> {
        use std::io::Write;
        let mut candidates = vec![self.backup_path()];
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        for n in 0..100u32 {
            candidates.push(
                self.path
                    .with_file_name(format!("{KEY_BACKUP_FILE}.{stamp}-{n}")),
            );
        }
        for backup in candidates {
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup);
            match created {
                Ok(mut f) => {
                    f.write_all(old).map_err(|_| KeyError)?;
                    return f.sync_all().map_err(|_| KeyError);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(KeyError),
            }
        }
        Err(KeyError)
    }
}

impl ApprovalKeys for DpapiKeyFile {
    fn load(&self) -> KeyLoad {
        match self.inspect() {
            Inspect::Key(k) => KeyLoad::Key(k),
            Inspect::Missing => KeyLoad::Missing,
            Inspect::Unusable(_) => KeyLoad::Unusable,
            Inspect::Unreadable => KeyLoad::Unreadable,
        }
    }

    fn load_or_create(&self) -> Result<ApprovalKey, KeyError> {
        self.obtain(true)
    }

    fn create_if_missing(&self) -> Result<ApprovalKey, KeyError> {
        self.obtain(false)
    }

    fn take_rotation_notice(&self) -> bool {
        self.rotated.swap(false, Ordering::SeqCst)
    }
}

impl DpapiKeyFile {
    /// Shared create path: lock, judge under the lock, create atomically; `allow_replace` lets an
    /// approval replace an `Unusable` blob (after a backup), signing never may.
    fn obtain(&self, allow_replace: bool) -> Result<ApprovalKey, KeyError> {
        let _one = self.create_lock.lock().unwrap_or_else(|p| p.into_inner());
        // Across processes: nobody else creates or replaces the key while we decide.
        let dir = self.path.parent().ok_or(KeyError)?;
        let _lock = KeyLock::acquire(dir)?;
        // The state is judged under the lock, so a key another process just made is simply used.
        let replace_old = match self.inspect() {
            Inspect::Key(key) => return Ok(key),
            // Possibly transient: never rotate on it.
            Inspect::Unreadable => return Err(KeyError),
            Inspect::Missing => None,
            // Signing never replaces an existing key.
            Inspect::Unusable(_) if !allow_replace => return Err(KeyError),
            Inspect::Unusable(old) => Some(old),
        };
        let mut bytes = [0u8; KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|_| KeyError)?;
        let made = ApprovalKey(bytes);
        bytes.zeroize();
        let blob = protect(made.as_bytes(), &self.entropy).ok_or(KeyError)?;
        if let Some(old) = &replace_old {
            self.back_up(old)?;
        }
        if !self.publish(&blob, replace_old.is_some())? {
            // Lost a race that the lock should have prevented (a process without the lock): keep
            // the winner's key rather than overwrite it.
            return match self.inspect() {
                Inspect::Key(key) => Ok(key),
                _ => Err(KeyError),
            };
        }
        log::warn!(
            "approval key {}; earlier approvals must be reviewed",
            if replace_old.is_some() {
                "replaced"
            } else {
                "created"
            }
        );
        self.rotated.store(true, Ordering::SeqCst);
        Ok(made)
    }
}

/// Exclusive cross-process lock file (`approval.key.lock`), deleted when released. A holder that
/// dies releases it with its handle.
struct KeyLock {
    _file: Option<std::fs::File>,
}

impl KeyLock {
    #[cfg(windows)]
    fn acquire(dir: &Path) -> Result<Self, KeyError> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let path = dir.join(KEY_LOCK_FILE);
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            // share_mode(0): while one process holds the handle no other can open the file.
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .share_mode(0)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
            {
                Ok(file) => return Ok(Self { _file: Some(file) }),
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(ERROR_SHARING_VIOLATION) | Some(ERROR_ACCESS_DENIED)
                    ) && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(ERROR_SHARING_VIOLATION) | Some(ERROR_ACCESS_DENIED)
                    ) =>
                {
                    log::warn!(
                        "approval key lock stayed busy for {} s (os error {:?})",
                        LOCK_WAIT.as_secs(),
                        e.raw_os_error()
                    );
                    return Err(KeyError);
                }
                Err(e) => {
                    // Diagnostic (FER-524, review P1.1 #6): the approval cannot be saved and the
                    // user should be able to find out why (a read-only folder, a security
                    // product, a reparse point where the lock file belongs).
                    log::warn!(
                        "approval key lock could not be created (os error {:?}, {:?})",
                        e.raw_os_error(),
                        e.kind()
                    );
                    return Err(KeyError);
                }
            }
        }
    }

    #[cfg(not(windows))]
    fn acquire(_dir: &Path) -> Result<Self, KeyError> {
        Ok(Self { _file: None })
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

    fn create_if_missing(&self) -> Result<ApprovalKey, KeyError> {
        Err(KeyError)
    }
}

/// An in-memory key source (tests of code that signs and verifies but is not about DPAPI).
#[cfg(test)]
pub struct MemoryKeys {
    key: Mutex<KeyLoad>,
    rotated: AtomicBool,
}

#[cfg(test)]
impl MemoryKeys {
    pub fn with_key(byte: u8) -> Self {
        Self {
            key: Mutex::new(KeyLoad::Key(ApprovalKey::for_tests(byte))),
            rotated: AtomicBool::new(false),
        }
    }

    pub fn missing() -> Self {
        Self {
            key: Mutex::new(KeyLoad::Missing),
            rotated: AtomicBool::new(false),
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
            KeyLoad::Unreadable => Err(KeyError),
            _ => {
                let key = ApprovalKey::for_tests(7);
                *guard = KeyLoad::Key(key.clone());
                self.rotated.store(true, Ordering::SeqCst);
                Ok(key)
            }
        }
    }

    fn create_if_missing(&self) -> Result<ApprovalKey, KeyError> {
        let mut guard = self.key.lock().unwrap();
        match &*guard {
            KeyLoad::Key(key) => Ok(key.clone()),
            KeyLoad::Missing => {
                let key = ApprovalKey::for_tests(7);
                *guard = KeyLoad::Key(key.clone());
                self.rotated.store(true, Ordering::SeqCst);
                Ok(key)
            }
            _ => Err(KeyError),
        }
    }

    fn take_rotation_notice(&self) -> bool {
        self.rotated.swap(false, Ordering::SeqCst)
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
    // allocated `out.pbData`, which `take_output` copies, zeroes and frees with `LocalFree`.
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
    take_output(out).map(|z| z.to_vec())
}

/// Decrypt a blob made by [`protect`]. `None` = other user/machine/entropy or corrupt. The
/// plaintext is wiped when the returned value drops.
#[cfg(windows)]
fn unprotect(blob: &[u8], entropy: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};
    let input = blob_of(blob)?;
    let entropy_blob = blob_of(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: same contract as `protect`: live input slices, null for the unused optional
    // parameters, a valid out-parameter that `take_output` copies, zeroes and frees.
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

/// Copy the system-allocated output, WIPE the system buffer (it may hold the plaintext key) and
/// free it. The copy wipes itself on drop.
#[cfg(windows)]
fn take_output(
    out: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> Option<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::Foundation::LocalFree;
    if out.pbData.is_null() {
        return None;
    }
    // SAFETY: on success DPAPI returns a buffer of `cbData` bytes at `pbData`, valid, writable and
    // exclusively ours until freed.
    let buffer = unsafe { std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) };
    let copy = Zeroizing::new(buffer.to_vec());
    // `zeroize` writes volatilely, so the compiler cannot drop the wipe before the free.
    buffer.zeroize();
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
fn unprotect(_blob: &[u8], _entropy: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    None
}

#[cfg(test)]
mod tests;
