//! Baselines of sync PROFILES and automations (Automation FER-522 design decision, threat T1/T3).
//!
//! The manual "Synchronize Dirs" baseline is written by the webview through `sync_baseline_save`
//! (under a lease). A baseline the webview can write must never decide what an UNATTENDED run
//! deletes: a forged baseline that claims files were in sync makes a two-way run propagate a
//! "deletion" it never saw. So profile and automation runs use a SEPARATE store:
//!
//! - its own folder (`<store dir>/baselines`), reachable only through [`ProfileBaselineStore`],
//!   a type no manual command holds;
//! - every file carries a MAC (HMAC-SHA256 under a subkey of the DPAPI approval key, domain
//!   separated from approval MACs). A file without a valid MAC - written by anything else,
//!   edited, copied from another user or machine, or key lost - loads as `FirstRun { Untrusted }`:
//!   no entries, hence no deletion is ever propagated from it;
//! - [`verify_profile_baseline`] lets the runner (P1.3) ask "is there a baseline I may trust"
//!   before it plans.
//!
//! - each pair has a save COUNTER in the automation store (`baseline_gen`); the file holds the
//!   counter it was written under (covered by the MAC) and loads only when it equals the current
//!   one, so an older authentic file put back over a newer one is refused (replay);
//! - signing never creates or replaces the approval key; only an approval does.
//!
//! Tauri-free.

use std::path::Path;
use std::sync::Arc;

use crate::automation::dpapi::{ApprovalKeys, KeyLoad};
use crate::automation::policy;
use crate::automation::store::{Store, baseline_gen};
use crate::sync_baseline::{
    BaselineAuth, BaselineError, FirstRunReason, LoadResult, RootPair, SyncBaselineStore,
};

/// Folder (inside the automation store directory) holding profile baselines.
pub const BASELINES_DIR: &str = "baselines";
/// Purpose string of the baseline subkey (domain separation from approvals).
const SUBKEY_PURPOSE: &str = "ferrum.automation.baseline.key.v1";

/// MACs baseline files with a subkey of the approval key and keeps the save counters in the
/// automation store.
pub struct KeyedAuth {
    keys: Arc<dyn ApprovalKeys>,
    store: Arc<Store>,
}

impl KeyedAuth {
    pub fn new(keys: Arc<dyn ApprovalKeys>, store: Arc<Store>) -> Self {
        Self { keys, store }
    }
}

impl BaselineAuth for KeyedAuth {
    fn sign(&self, payload: &[u8]) -> Option<String> {
        // Signing NEVER creates or replaces the approval key (FER-522 rework 1): only an approval
        // does. Without a usable key a baseline simply cannot be signed, so it is not saved.
        let KeyLoad::Key(key) = self.keys.load() else {
            return None;
        };
        let sub = policy::derive_subkey(&key, SUBKEY_PURPOSE)?;
        policy::hmac_hex(&sub, payload)
    }

    fn verify(&self, payload: &[u8], mac: &str) -> bool {
        let KeyLoad::Key(key) = self.keys.load() else {
            return false;
        };
        policy::derive_subkey(&key, SUBKEY_PURPOSE)
            .is_some_and(|sub| policy::hmac_matches(&sub, payload, mac))
    }

    fn generation(&self, pair_key: &str) -> Option<u64> {
        self.store
            .with_read(|conn| baseline_gen::current(conn, pair_key))
            .ok()
            .flatten()
    }

    fn advance_generation(&self, pair_key: &str) -> Option<u64> {
        self.store
            .with_write(|conn| baseline_gen::advance(conn, pair_key))
            .ok()
    }
}

/// The authenticated baseline store of profile and automation runs. Cheap to clone.
#[derive(Clone)]
pub struct ProfileBaselineStore {
    inner: SyncBaselineStore,
}

impl ProfileBaselineStore {
    /// `store_dir` is the automation store directory (already proven plain by the store); the
    /// baselines folder inside it must not be a reparse point either, which is checked through an
    /// open handle of the folder itself (not a separate path lookup).
    pub fn open(store_dir: &Path, keys: Arc<dyn ApprovalKeys>, store: Arc<Store>) -> Self {
        let dir = store_dir.join(BASELINES_DIR);
        if std::fs::create_dir_all(&dir).is_err() || !is_plain_dir(&dir) {
            return Self::unavailable();
        }
        Self {
            inner: SyncBaselineStore::open_authenticated(
                &dir,
                Arc::new(KeyedAuth::new(keys, store)),
            ),
        }
    }

    /// No usable location: loads are first-run, saves fail honestly.
    pub fn unavailable() -> Self {
        Self {
            inner: SyncBaselineStore::unavailable(),
        }
    }

    /// The store the engine reads and writes (`EngineRequest::new(.., baselines, ..)`). Writes
    /// through it still require a run lease (`sync_baseline::save_with_guard`).
    pub fn engine_store(&self) -> &SyncBaselineStore {
        &self.inner
    }
}

/// True when `path` is a real directory that is not a reparse point, judged on one handle opened
/// WITHOUT following links (`FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`).
fn is_plain_dir(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(0x0020_0000 | 0x0200_0000)
            .share_mode(7);
        match options.open(path).and_then(|f| f.metadata()) {
            Ok(meta) => meta.is_dir() && meta.file_attributes() & 0x0400 == 0,
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    }
}

/// What the runner may rely on for a folder pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineTrust {
    /// An authentic baseline exists: two-way deletion propagation may use it.
    Trusted,
    /// No baseline yet: a first run (nothing is propagated as a deletion).
    Absent,
    /// A baseline file exists but cannot be trusted (no or wrong MAC, corrupt, wrong version,
    /// wrong pair): treated as a first run. The next settled run replaces it.
    Untrusted,
    /// A root is missing or detached: nothing can be said.
    RootsUnavailable,
}

/// `verify_profile_baseline` (FER-522): the runner calls this before it plans a profile run, and
/// treats everything but [`BaselineTrust::Trusted`] as "no baseline". Invalid (nested/identical)
/// roots are an error.
pub fn verify_profile_baseline(
    store: &ProfileBaselineStore,
    pair: &RootPair,
) -> Result<BaselineTrust, BaselineError> {
    Ok(match store.engine_store().load(pair)? {
        LoadResult::Ok { .. } => BaselineTrust::Trusted,
        LoadResult::FirstRun { reason } => match reason {
            FirstRunReason::None => BaselineTrust::Absent,
            FirstRunReason::RootsUnavailable => BaselineTrust::RootsUnavailable,
            FirstRunReason::Corrupt
            | FirstRunReason::UnknownVersion
            | FirstRunReason::Mismatch
            | FirstRunReason::TooLarge
            | FirstRunReason::Untrusted => BaselineTrust::Untrusted,
        },
    })
}

#[cfg(test)]
mod tests;
