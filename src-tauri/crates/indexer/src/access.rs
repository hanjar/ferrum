//! Shared-mode result filter (FER-476): can THIS client list a directory?
//!
//! The directory is opened BY FILE ID (`OpenFileById` with the indexed file
//! reference, sequence number included) — never by path, so a junction or a
//! rename between indexing and the check cannot redirect it, and a reused
//! record (new sequence) simply fails to open. Only `READ_CONTROL` is
//! requested (a read right; unaffected by the write-restricted service
//! token). Its security descriptor is then evaluated with `AccessCheck`
//! against the CLIENT's token (the identification-level impersonation
//! token captured at hello) for `FILE_LIST_DIRECTORY`.
//!
//! Every failure (cannot open the volume root or the directory, cannot read
//! the SD, AccessCheck error) means "not listable" — fail closed.
//!
//! Results are cached per connection and directory; only DEFINITE outcomes
//! are stored (`Yes`/`No`; `Unknown` stays out and is re-evaluated —
//! FER-503, so a denied directory is answered from the cache instead of
//! re-running OpenFileById+AccessCheck on every `changes` query). The cache
//! is dropped when the security epoch moves (a journal batch carried
//! `SECURITY_CHANGE`, review I5) and is capped in size.

use crate::query::{Access, DirAccess};
use crate::win::{OwnedHandle, wide};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    AccessCheck, DACL_SECURITY_INFORMATION, GENERIC_MAPPING, GROUP_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PRIVILEGE_SET, PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ALL_ACCESS, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType, OPEN_EXISTING, OpenFileById, READ_CONTROL,
};

/// Most cached decisions per token identity (review N3).
pub const MAX_CACHE: usize = 200_000;

/// Can `token` (an impersonation token, at least identification level)
/// list the directory `frn` on the volume whose root is open as `root`?
pub fn token_can_list(token: &OwnedHandle, root: &OwnedHandle, frn: u64) -> bool {
    token_check(token, root, frn) == Access::Yes
}

/// FER-477 review P2-4: the tri-state form. `Unknown` = the decision could
/// not be evaluated (the service cannot open the directory or read its
/// security descriptor, or AccessCheck itself failed); `No` = AccessCheck
/// ran and denied. Search/subtree fold `Unknown` into "no" (fail closed);
/// the `changes` query answers rescan for it.
pub fn token_check(token: &OwnedHandle, root: &OwnedHandle, frn: u64) -> Access {
    let id = FILE_ID_DESCRIPTOR {
        dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
    };
    let h = unsafe {
        OpenFileById(
            root.raw(),
            &id,
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return Access::Unknown;
    }
    let dir = OwnedHandle(h);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let rc = unsafe {
        GetSecurityInfo(
            dir.raw(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS || sd.is_null() {
        return Access::Unknown;
    }
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    // PRIVILEGE_SET with room for a few privileges (AccessCheck may report
    // privileges used; none are expected for FILE_LIST_DIRECTORY).
    let mut privs = [0u64; 16];
    let mut privs_len = std::mem::size_of_val(&privs) as u32;
    let mut granted = 0u32;
    let mut status = 0i32;
    let ok = unsafe {
        AccessCheck(
            sd,
            token.raw(),
            FILE_LIST_DIRECTORY,
            &mapping,
            privs.as_mut_ptr() as *mut PRIVILEGE_SET,
            &mut privs_len,
            &mut granted,
            &mut status,
        )
    };
    unsafe { LocalFree(sd) };
    if ok == 0 {
        Access::Unknown
    } else if status != 0 && granted & FILE_LIST_DIRECTORY != 0 {
        Access::Yes
    } else {
        Access::No
    }
}

/// Handle to `X:\` used as the volume hint for `OpenFileById`
/// (attributes only; no data access, no file on the volume opened).
fn open_volume_root(letter: u8) -> Option<OwnedHandle> {
    let path = wide(format!("{}:\\", letter as char));
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then(|| OwnedHandle(h))
}

/// How long one AccessCheck decision is reused (review L5): an ACL change
/// the journal did not report (paused service, rebuild) is honoured within
/// this time at the latest.
pub const CACHE_TTL: Duration = Duration::from_secs(60);
/// Most token identities with cached decisions (any users).
pub const MAX_KEYS: usize = 64;
/// Most identities ONE user SID may hold (review N4): a fifth identity of
/// the same user evicts that user's least recently used one.
pub const MAX_KEYS_PER_USER: usize = 4;
/// Most decisions in the whole cache (review N4): beyond this, whole
/// identities are evicted least-recently-used first.
pub const MAX_TOTAL: usize = 400_000;

/// Cache key: the token's AccessCheck identity ([`token_fingerprint`]).
pub type TokenKey = Arc<[u8]>;

/// User SID inside a key made by `token_fingerprint` (magic 5, logon 8,
/// SID length 1, SID); other keys (tests) are their own user.
fn user_of(key: &[u8]) -> &[u8] {
    if key.len() > 14 && key.starts_with(b"FRXK") {
        let n = key[13] as usize;
        if key.len() >= 14 + n {
            return &key[14..14 + n];
        }
    }
    key
}

/// AccessCheck decisions shared by all connections whose client tokens are
/// IDENTICAL for AccessCheck (review N1/N1b: the key is the full token
/// fingerprint, not just the logon id). Shared so that the visibility of
/// every directory, evaluated BEFORE any name is matched (review L3), is
/// not re-checked for every search.
///
/// Bounded (review N3/N4):
/// - per identity [`MAX_CACHE`] decisions (expired first, then only that
///   identity's map);
/// - [`MAX_KEYS_PER_USER`] identities per user SID, [`MAX_KEYS`] overall,
///   least recently used identity evicted;
/// - [`MAX_TOTAL`] decisions overall, least recently used identities
///   evicted first (never the one being written);
/// - expired decisions are swept every [`CACHE_TTL`].
///
/// One identity can therefore not empty another user's decisions, and the
/// memory is bounded whatever clients do. Everything is dropped when the
/// security epoch moves (`SECURITY_CHANGE`, rebuild, resume).
#[derive(Default)]
pub struct SharedCache {
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    epoch: u64,
    keys: HashMap<TokenKey, PerKey>,
    total: usize,
    last_sweep: Option<Instant>,
}

struct PerKey {
    /// (letter, dir frn) → (DEFINITE outcome, when). `true` = AccessCheck
    /// granted, `false` = AccessCheck denied. An `Unknown` (could not be
    /// evaluated) is never stored — FER-503, so a stored `false` can be
    /// trusted as a denial.
    map: HashMap<(u8, u64), (bool, Instant)>,
    used: Instant,
}

impl CacheInner {
    fn remove_key(&mut self, key: &TokenKey) {
        if let Some(k) = self.keys.remove(key) {
            self.total -= k.map.len();
        }
    }

    /// Least recently used key among those `filter` accepts, never `keep`.
    fn lru(&self, keep: &TokenKey, filter: impl Fn(&TokenKey) -> bool) -> Option<TokenKey> {
        self.keys
            .iter()
            .filter(|(k, _)| *k != keep && filter(k))
            .min_by_key(|(_, v)| v.used)
            .map(|(k, _)| k.clone())
    }

    fn sweep(&mut self, now: Instant) {
        let mut total = 0;
        for k in self.keys.values_mut() {
            k.map
                .retain(|_, (_, at)| now.saturating_duration_since(*at) < CACHE_TTL);
            total += k.map.len();
        }
        self.keys.retain(|_, k| !k.map.is_empty());
        self.total = total;
        self.last_sweep = Some(now);
    }
}

impl SharedCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop everything when `epoch` differs from the cached one.
    pub fn sync_epoch(&self, epoch: u64) {
        let mut c = self.lock();
        if c.epoch != epoch {
            c.keys.clear();
            c.total = 0;
            c.epoch = epoch;
        }
    }

    fn get(&self, key: &TokenKey, letter: u8, frn: u64, now: Instant) -> Option<bool> {
        let mut c = self.lock();
        let k = c.keys.get_mut(key)?;
        k.used = now;
        k.map
            .get(&(letter, frn))
            .filter(|(_, at)| now.saturating_duration_since(*at) < CACHE_TTL)
            .map(|(v, _)| *v)
    }

    fn put(&self, key: &TokenKey, letter: u8, frn: u64, v: bool, now: Instant) {
        let mut c = self.lock();
        // Periodic sweep of expired decisions (review N4).
        if c.last_sweep
            .is_none_or(|at| now.saturating_duration_since(at) >= CACHE_TTL)
        {
            c.sweep(now);
        }
        if !c.keys.contains_key(key) {
            // A new identity: per-user cap, then the global identity cap.
            let user = user_of(key).to_vec();
            let same_user = c
                .keys
                .keys()
                .filter(|k| user_of(k) == user.as_slice())
                .count();
            if same_user >= MAX_KEYS_PER_USER
                && let Some(old) = c.lru(key, |k| user_of(k) == user.as_slice())
            {
                c.remove_key(&old);
            }
            while c.keys.len() >= MAX_KEYS {
                match c.lru(key, |_| true) {
                    Some(old) => c.remove_key(&old),
                    None => break,
                }
            }
            c.keys.insert(
                key.clone(),
                PerKey {
                    map: HashMap::new(),
                    used: now,
                },
            );
        }
        // Global cap: evict whole least-recently-used identities first.
        while c.total >= MAX_TOTAL {
            match c.lru(key, |_| true) {
                Some(old) => c.remove_key(&old),
                None => break,
            }
        }
        let mut removed = 0;
        let mut added = 0;
        {
            let k = c.keys.get_mut(key).expect("inserted above");
            k.used = now;
            if k.map.len() >= MAX_CACHE || k.map.len() >= MAX_TOTAL {
                let before = k.map.len();
                k.map
                    .retain(|_, (_, at)| now.saturating_duration_since(*at) < CACHE_TTL);
                if k.map.len() >= MAX_CACHE.min(MAX_TOTAL) {
                    k.map.clear();
                }
                removed = before - k.map.len();
            }
            if k.map.insert((letter, frn), (v, now)).is_none() {
                added = 1;
            }
        }
        c.total = c.total - removed + added;
    }

    /// Decisions cached for all identities.
    pub fn len(&self) -> usize {
        self.lock().total
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Decisions cached for one identity.
    pub fn len_for(&self, key: &TokenKey) -> usize {
        self.lock().keys.get(key).map_or(0, |k| k.map.len())
    }

    /// Identities currently cached.
    pub fn identities(&self) -> usize {
        self.lock().keys.len()
    }
}

/// AccessCheck filter of one connection (its client token), backed by the
/// shared cache under the token's own identity.
pub struct TokenAccess {
    token: Arc<OwnedHandle>,
    key: TokenKey,
    roots: HashMap<u8, Option<OwnedHandle>>,
    cache: Arc<SharedCache>,
}

impl TokenAccess {
    pub fn new(token: Arc<OwnedHandle>, key: TokenKey, cache: Arc<SharedCache>) -> TokenAccess {
        TokenAccess {
            token,
            key,
            roots: HashMap::new(),
            cache,
        }
    }

    /// Drop cached decisions when the security epoch moved.
    pub fn refresh(&mut self, epoch: u64) {
        self.cache.sync_epoch(epoch);
    }

    /// Volume root handles are only kept for one request (no long-lived
    /// handle on a volume between requests).
    pub fn release_volumes(&mut self) {
        self.roots.clear();
    }

    pub fn cached_count(&self) -> usize {
        self.cache.len_for(&self.key)
    }
}

impl DirAccess for TokenAccess {
    fn cached(&mut self, letter: u8, dir_frn: u64) -> Option<bool> {
        self.cache.get(&self.key, letter, dir_frn, Instant::now())
    }

    fn can_list(&mut self, letter: u8, dir_frn: u64) -> bool {
        let now = Instant::now();
        if let Some(v) = self.cache.get(&self.key, letter, dir_frn, now) {
            return v;
        }
        let root = self
            .roots
            .entry(letter)
            .or_insert_with(|| open_volume_root(letter));
        let a = match root {
            Some(r) => token_check(&self.token, r, dir_frn),
            None => Access::Unknown,
        };
        // Only a definite outcome is cached (FER-503); `Unknown` stays out
        // and is evaluated again next time until it resolves either way.
        if a != Access::Unknown {
            self.cache
                .put(&self.key, letter, dir_frn, a == Access::Yes, now);
        }
        a == Access::Yes
    }

    /// FER-503 (a): the cache holds only definite outcomes (an `Unknown` is
    /// never stored), so a cached `false` IS a denial and is reported — the
    /// `changes` query reuses it instead of re-running the AccessCheck for
    /// every changed directory the client may not list.
    fn cached_access(&mut self, letter: u8, dir_frn: u64) -> Option<Access> {
        self.cached(letter, dir_frn)
            .map(|v| if v { Access::Yes } else { Access::No })
    }

    /// Review P2-4, FER-503 (a): a definite answer comes from the cache
    /// (`Yes` and, since FER-503, also `No` — it used to be re-evaluated on
    /// every `changes` query, thousands of redundant OpenFileById +
    /// AccessCheck calls a non-admin could trigger at will); `Unknown` is
    /// evaluated fresh and never cached.
    fn check(&mut self, letter: u8, dir_frn: u64) -> Access {
        let now = Instant::now();
        match self.cache.get(&self.key, letter, dir_frn, now) {
            Some(true) => Access::Yes,
            Some(false) => Access::No,
            None => {
                let root = self
                    .roots
                    .entry(letter)
                    .or_insert_with(|| open_volume_root(letter));
                let a = match root {
                    Some(r) => token_check(&self.token, r, dir_frn),
                    None => Access::Unknown,
                };
                if a != Access::Unknown {
                    self.cache
                        .put(&self.key, letter, dir_frn, a == Access::Yes, now);
                }
                a
            }
        }
    }
}

/// An identification-level impersonation copy of this process's token
/// (tests and the console: stands in for a pipe client's token).
pub fn own_identification_token() -> std::io::Result<OwnedHandle> {
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, SecurityIdentification, TOKEN_DUPLICATE, TOKEN_QUERY, TokenImpersonation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut base = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut base,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let base = OwnedHandle(base);
    let mut imp = std::ptr::null_mut();
    if unsafe {
        DuplicateTokenEx(
            base.raw(),
            TOKEN_QUERY,
            std::ptr::null(),
            SecurityIdentification,
            TokenImpersonation,
            &mut imp,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(OwnedHandle(imp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::{SecurityDescriptor, current_user_sid, sid_string, token_fingerprint};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, GetFileInformationByHandle,
    };

    fn frn_of(path: &std::path::Path) -> u64 {
        let w = wide(path);
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(h, INVALID_HANDLE_VALUE);
        let h = OwnedHandle(h);
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        assert_ne!(unsafe { GetFileInformationByHandle(h.raw(), &mut info) }, 0);
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64
    }

    /// FER-503 review P3: a temp tree whose subfolders carry locked ACLs.
    /// Dropped — also when the test panics half-way — it restores the
    /// inherited ACLs (`icacls /reset /T`, as the other ACL tests do) and
    /// then deletes the tree, so no unlistable folder stays in %TEMP%.
    struct AclTempDir(std::path::PathBuf);

    impl AclTempDir {
        fn new(name: &str) -> AclTempDir {
            let root =
                std::env::temp_dir().join(format!("ferrum-access-{name}-{}", std::process::id()));
            let dir = AclTempDir(root);
            dir.cleanup();
            std::fs::create_dir_all(&dir.0).unwrap();
            dir
        }

        fn cleanup(&self) {
            if !self.0.exists() {
                return;
            }
            let _ = std::process::Command::new("icacls")
                .arg(&self.0)
                .args(["/reset", "/T", "/C", "/Q"])
                .output();
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Drop for AclTempDir {
        fn drop(&mut self) {
            self.cleanup();
        }
    }

    fn mkdir_sd(path: &std::path::Path, sddl: &str) {
        let sd = SecurityDescriptor::from_sddl(sddl).unwrap();
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let w = wide(path);
        assert_ne!(unsafe { CreateDirectoryW(w.as_ptr(), &sa) }, 0);
    }

    /// Real AccessCheck without admin rights: a directory the user may
    /// list, one where the user may traverse/read attributes but NOT list,
    /// a stale file reference (wrong sequence) and the cache behaviour.
    #[test]
    fn access_check_by_file_id_with_the_client_token() {
        let root = std::env::temp_dir().join(format!("ferrum-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let user = sid_string(&current_user_sid().unwrap()).unwrap();
        let open = root.join("open");
        let closed = root.join("closed");
        std::fs::create_dir(&open).unwrap();
        // Owner keeps READ_CONTROL/WRITE_DAC implicitly; the DACL grants
        // the user traverse + read attributes + read control + delete only.
        mkdir_sd(
            &closed,
            &format!("D:P(A;;0x1200a0;;;{user})(A;;FA;;;SY)(A;;FA;;;BA)(A;;SD;;;{user})"),
        );
        let letter = root.to_string_lossy().as_bytes()[0].to_ascii_uppercase();
        let token = Arc::new(own_identification_token().unwrap());
        let vol = open_volume_root(letter).expect("volume root");
        assert!(token_can_list(&token, &vol, frn_of(&open)));
        assert!(!token_can_list(&token, &vol, frn_of(&closed)));
        // Wrong sequence number: the record is not that file → denied.
        let stale = frn_of(&open) ^ (0x7FFF << 48);
        assert!(!token_can_list(&token, &vol, stale));
        // FER-477 review P2-4: the tri-state form tells "denied by
        // AccessCheck" from "could not be evaluated" (cannot open by id).
        assert_eq!(token_check(&token, &vol, frn_of(&open)), Access::Yes);
        assert_eq!(token_check(&token, &vol, frn_of(&closed)), Access::No);
        assert_eq!(token_check(&token, &vol, stale), Access::Unknown);

        let cache = Arc::new(SharedCache::default());
        cache.sync_epoch(1);
        let key: TokenKey = Arc::from(token_fingerprint(token.raw()).unwrap());
        let mut acc = TokenAccess::new(token.clone(), key.clone(), cache.clone());
        assert_eq!(
            DirAccess::cached(&mut acc, letter, frn_of(&open)),
            None,
            "cold"
        );
        assert!(acc.can_list(letter, frn_of(&open)));
        assert!(!acc.can_list(letter, frn_of(&closed)));
        assert_eq!(
            DirAccess::cached(&mut acc, letter, frn_of(&open)),
            Some(true)
        );
        assert_eq!(acc.cached_count(), 2);
        // FER-503 (a): the cache stores only definite outcomes, so the
        // cached `false` of the denied directory IS reported by
        // `cached_access` (it used to be indistinguishable from an
        // unevaluable one, hence untrusted); `Unknown` is not cached and
        // `check` evaluates it fresh every time.
        assert_eq!(acc.cached_access(letter, frn_of(&closed)), Some(Access::No));
        assert_eq!(acc.cached_access(letter, frn_of(&open)), Some(Access::Yes));
        assert_eq!(acc.check(letter, frn_of(&closed)), Access::No);
        assert_eq!(acc.check(letter, stale), Access::Unknown);
        assert_eq!(acc.cached_count(), 2, "Unknown is not cached");
        // Another connection with the same token identity reuses them; a
        // different identity gets its own.
        let mut same = TokenAccess::new(token.clone(), key.clone(), cache.clone());
        assert!(same.can_list(letter, frn_of(&open)));
        assert_eq!(cache.len(), 2);
        let other_key: TokenKey = Arc::from(&b"other"[..]);
        let mut other = TokenAccess::new(token, other_key, cache.clone());
        assert!(other.can_list(letter, frn_of(&open)));
        assert_eq!(cache.len(), 3);
        acc.refresh(1);
        assert_eq!(cache.len(), 3, "same epoch keeps the cache");
        acc.refresh(2);
        assert_eq!(cache.len(), 0, "SECURITY_CHANGE epoch drops it");
        // TTL: an entry older than CACHE_TTL is not used.
        let old = Instant::now() - CACHE_TTL - Duration::from_secs(1);
        cache.put(&key, letter, 42, true, old);
        assert_eq!(cache.get(&key, letter, 42, Instant::now()), None);
        assert!(!acc.can_list(b'Q', 5), "no such volume: fail closed");
        let _ = std::process::Command::new("cmd")
            .args(["/C", "icacls"])
            .arg(&closed)
            .args(["/reset"])
            .output();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// FER-503 (a): a definite denial is served from the cache — a second
    /// `changes` round (the `cached_access` lookup under the index lock,
    /// then `check` only for what is missing) never re-runs the AccessCheck
    /// — while an `Unknown` keeps being re-checked and never lands in the
    /// cache. Another token identity shares nothing (review N1/N1b).
    #[test]
    fn denied_decision_is_reused_unknown_is_not() {
        let tmp = AclTempDir::new("503");
        let root = &tmp.0;
        let user = sid_string(&current_user_sid().unwrap()).unwrap();
        let closed = root.join("closed");
        // The user may not list this directory (definite `No` from
        // AccessCheck — not an evaluation failure).
        mkdir_sd(
            &closed,
            &format!("D:P(A;;0x1200a0;;;{user})(A;;FA;;;SY)(A;;FA;;;BA)(A;;SD;;;{user})"),
        );
        let letter = root.to_string_lossy().as_bytes()[0].to_ascii_uppercase();
        let closed_frn = frn_of(&closed);
        let stale = closed_frn ^ (0x7FFF << 48);
        let token = Arc::new(own_identification_token().unwrap());
        let cache = Arc::new(SharedCache::default());
        let key: TokenKey = Arc::from(token_fingerprint(token.raw()).unwrap());
        let mut acc = TokenAccess::new(token.clone(), key.clone(), cache.clone());

        /// Counts the `check` calls of one `changes`-like flow.
        struct Counted<'a> {
            inner: &'a mut TokenAccess,
            checks: std::cell::Cell<usize>,
        }
        impl DirAccess for Counted<'_> {
            fn cached(&mut self, l: u8, f: u64) -> Option<bool> {
                DirAccess::cached(self.inner, l, f)
            }
            fn cached_access(&mut self, l: u8, f: u64) -> Option<Access> {
                DirAccess::cached_access(self.inner, l, f)
            }
            fn can_list(&mut self, l: u8, f: u64) -> bool {
                self.inner.can_list(l, f)
            }
            fn check(&mut self, l: u8, f: u64) -> Access {
                self.checks.set(self.checks.get() + 1);
                self.inner.check(l, f)
            }
        }

        let mut c = Counted {
            inner: &mut acc,
            checks: std::cell::Cell::new(0),
        };
        // Round 1: nothing cached — the lookup misses and the check runs.
        assert_eq!(DirAccess::cached_access(&mut c, letter, closed_frn), None);
        assert_eq!(DirAccess::check(&mut c, letter, closed_frn), Access::No);
        assert_eq!(c.checks.get(), 1);
        // Round 2: the cached `No` answers under the lock; the query needs
        // no `check` at all — the AccessCheck is NOT run again.
        assert_eq!(
            DirAccess::cached_access(&mut c, letter, closed_frn),
            Some(Access::No)
        );
        assert_eq!(c.checks.get(), 1, "the denial came from the cache");
        // `Unknown` (unevaluable) is re-checked every round and not cached.
        assert_eq!(DirAccess::check(&mut c, letter, stale), Access::Unknown);
        assert_eq!(DirAccess::check(&mut c, letter, stale), Access::Unknown);
        assert_eq!(c.checks.get(), 3);
        assert_eq!(
            cache.len_for(&key),
            1,
            "only the definite denial; the Unknowns left nothing"
        );
        // A different token identity shares nothing (the cache key is the
        // full token fingerprint).
        let other: TokenKey = Arc::from(&b"other-identity"[..]);
        let mut o = TokenAccess::new(token, other, cache.clone());
        assert_eq!(
            DirAccess::cached_access(&mut o, letter, closed_frn),
            None,
            "no cross-identity reuse"
        );
        assert_eq!(DirAccess::check(&mut o, letter, closed_frn), Access::No);
        // `tmp` restores the ACLs and deletes the tree when dropped.
    }

    /// Review N1 regression: a folder listable ONLY through Authenticated
    /// Users. The full token may list it; a derived token of the same logon
    /// with S-1-5-11 deny-only may not — even after the full token warmed
    /// the shared cache.
    #[test]
    fn deny_only_token_never_reuses_the_full_tokens_decision() {
        let root = std::env::temp_dir().join(format!("ferrum-access-n1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let au_only = root.join("au-only");
        // List only via AU; the owner (this user) keeps READ_CONTROL.
        mkdir_sd(&au_only, "D:P(A;;0x1200a9;;;AU)(A;;FA;;;SY)");
        let letter = root.to_string_lossy().as_bytes()[0].to_ascii_uppercase();
        let frn = frn_of(&au_only);
        let full = Arc::new(own_identification_token().unwrap());
        let deny = Arc::new(crate::win::deny_only_token(&["S-1-5-11"]).unwrap());
        let vol = open_volume_root(letter).unwrap();
        assert!(token_can_list(&full, &vol, frn), "AU grants list");
        assert!(!token_can_list(&deny, &vol, frn), "deny-only AU does not");
        let cache = Arc::new(SharedCache::default());
        let fk: TokenKey = Arc::from(token_fingerprint(full.raw()).unwrap());
        let dk: TokenKey = Arc::from(token_fingerprint(deny.raw()).unwrap());
        assert_ne!(fk, dk);
        let mut a = TokenAccess::new(full, fk, cache.clone());
        assert!(a.can_list(letter, frn), "cache warmed by the full token");
        let mut b = TokenAccess::new(deny, dk, cache.clone());
        assert_eq!(DirAccess::cached(&mut b, letter, frn), None);
        assert!(
            !b.can_list(letter, frn),
            "the derived token is still denied"
        );
        let _ = std::process::Command::new("icacls")
            .arg(&au_only)
            .args(["/reset"])
            .output();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review N3: one identity filling its cap never empties another one's
    /// decisions; beyond MAX_KEYS the least recently used identity goes.
    #[test]
    fn cache_is_bounded_per_identity_with_lru_identities() {
        let c = SharedCache::default();
        let now = Instant::now();
        let a: TokenKey = Arc::from(&b"a"[..]);
        let b: TokenKey = Arc::from(&b"b"[..]);
        c.put(&b, b'C', 1, true, now);
        for i in 0..(MAX_CACHE as u64 + 10) {
            c.put(&a, b'C', i, true, now);
        }
        assert!(c.len_for(&a) <= MAX_CACHE);
        assert_eq!(c.get(&b, b'C', 1, now), Some(true), "b untouched");
        for i in 0..MAX_KEYS {
            let k: TokenKey = Arc::from(format!("k{i}").into_bytes());
            c.put(&k, b'C', 1, true, now + Duration::from_millis(1 + i as u64));
        }
        assert!(c.lock().keys.len() <= MAX_KEYS);
        assert_eq!(c.len_for(&a), 0, "the least recently used identity went");
    }

    /// Review N1b regression: a folder listable ONLY by the user SID. A
    /// derived token of the same logon whose user SID is deny-only must
    /// stay denied even after the full token warmed the shared cache.
    #[test]
    fn deny_only_user_sid_never_reuses_the_full_tokens_decision() {
        let root = std::env::temp_dir().join(format!("ferrum-access-n1b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let user = sid_string(&current_user_sid().unwrap()).unwrap();
        let user_only = root.join("user-only");
        mkdir_sd(
            &user_only,
            &format!("D:P(A;;0x1200a9;;;{user})(A;;FA;;;SY)"),
        );
        let letter = root.to_string_lossy().as_bytes()[0].to_ascii_uppercase();
        let frn = frn_of(&user_only);
        let full = Arc::new(own_identification_token().unwrap());
        let deny = Arc::new(crate::win::deny_only_token(&[&user]).unwrap());
        let vol = open_volume_root(letter).unwrap();
        assert!(token_can_list(&full, &vol, frn));
        assert!(!token_can_list(&deny, &vol, frn), "deny-only user SID");
        let cache = Arc::new(SharedCache::default());
        let fk: TokenKey = Arc::from(token_fingerprint(full.raw()).unwrap());
        let dk: TokenKey = Arc::from(token_fingerprint(deny.raw()).unwrap());
        assert_ne!(fk, dk);
        assert!(TokenAccess::new(full, fk, cache.clone()).can_list(letter, frn));
        let mut b = TokenAccess::new(deny, dk, cache.clone());
        assert!(!b.can_list(letter, frn), "still denied with a warm cache");
        let _ = std::process::Command::new("icacls")
            .arg(&user_only)
            .args(["/reset"])
            .output();
        let _ = std::fs::remove_dir_all(&root);
    }

    fn fake_key(user: u8, n: u8) -> TokenKey {
        // Same layout as token_fingerprint: magic, logon, SID len, SID.
        let mut k = b"FRXK2".to_vec();
        k.extend_from_slice(&[0u8; 8]);
        k.push(2);
        k.extend_from_slice(&[user, user]);
        k.push(n);
        Arc::from(k)
    }

    /// Review N4: at most MAX_KEYS_PER_USER identities per user SID; the
    /// fifth evicts that user's least recently used one, not another user's.
    #[test]
    fn identities_per_user_are_capped() {
        let c = SharedCache::default();
        let t0 = Instant::now();
        let other = fake_key(9, 0);
        c.put(&other, b'C', 1, true, t0);
        for i in 0..=MAX_KEYS_PER_USER as u8 {
            c.put(
                &fake_key(1, i),
                b'C',
                1,
                true,
                t0 + Duration::from_millis(1 + i as u64),
            );
        }
        assert_eq!(
            c.len_for(&fake_key(1, 0)),
            0,
            "oldest identity of user 1 evicted"
        );
        assert_eq!(c.len_for(&fake_key(1, MAX_KEYS_PER_USER as u8)), 1);
        assert_eq!(c.len_for(&other), 1, "another user's identity untouched");
        assert_eq!(c.identities(), 1 + MAX_KEYS_PER_USER);
        assert_eq!(c.len(), 1 + MAX_KEYS_PER_USER);
    }

    /// Review N4: the total stays at most MAX_TOTAL; whole least recently
    /// used identities go first, never the one being written.
    #[test]
    fn total_decisions_are_capped_globally() {
        let c = SharedCache::default();
        let t0 = Instant::now();
        let per = MAX_CACHE as u64 - 1;
        let (a, b, d) = (fake_key(1, 0), fake_key(2, 0), fake_key(3, 0));
        for i in 0..per {
            c.put(&a, b'C', i, true, t0);
        }
        for i in 0..per {
            c.put(&b, b'C', i, true, t0 + Duration::from_millis(1));
        }
        assert!(c.len() <= MAX_TOTAL);
        for i in 0..(MAX_TOTAL as u64 / 2) {
            c.put(&d, b'C', i, true, t0 + Duration::from_millis(2));
        }
        assert!(c.len() <= MAX_TOTAL, "{}", c.len());
        assert_eq!(c.len_for(&a), 0, "least recently used identity evicted");
        assert!(c.len_for(&d) > 0);
    }

    /// Review N4: expired decisions are swept periodically, also for
    /// identities nobody uses any more.
    #[test]
    fn expired_decisions_are_swept() {
        let c = SharedCache::default();
        let t0 = Instant::now();
        let (a, b) = (fake_key(1, 0), fake_key(2, 0));
        for i in 0..100 {
            c.put(&a, b'C', i, true, t0);
        }
        assert_eq!(c.len(), 100);
        let later = t0 + CACHE_TTL + Duration::from_secs(1);
        c.put(&b, b'C', 1, true, later);
        assert_eq!(c.len_for(&a), 0, "a's expired decisions swept");
        assert_eq!(c.identities(), 1, "empty identity dropped");
        assert_eq!(c.len(), 1);
    }
}
