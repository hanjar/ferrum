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
//! Known limit: an older AUTHENTIC baseline file replayed over a newer one still verifies (a
//! store-side counter would be needed to detect rollback); it holds only facts that were once true.
//!
//! Tauri-free.

use std::path::Path;
use std::sync::Arc;

use crate::automation::dpapi::{ApprovalKeys, KeyLoad};
use crate::automation::policy;
use crate::sync_baseline::{
    BaselineAuth, BaselineError, FirstRunReason, LoadResult, RootPair, SyncBaselineStore,
};

/// Folder (inside the automation store directory) holding profile baselines.
pub const BASELINES_DIR: &str = "baselines";
/// Purpose string of the baseline subkey (domain separation from approvals).
const SUBKEY_PURPOSE: &str = "ferrum.automation.baseline.key.v1";

/// MACs baseline files with a subkey of the approval key.
pub struct KeyedAuth {
    keys: Arc<dyn ApprovalKeys>,
}

impl KeyedAuth {
    pub fn new(keys: Arc<dyn ApprovalKeys>) -> Self {
        Self { keys }
    }
}

impl BaselineAuth for KeyedAuth {
    fn sign(&self, payload: &[u8]) -> Option<String> {
        // Signing may create the key (first sync on a fresh profile); verification never does.
        let key = self.keys.load_or_create().ok()?;
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
}

/// The authenticated baseline store of profile and automation runs. Cheap to clone.
#[derive(Clone)]
pub struct ProfileBaselineStore {
    inner: SyncBaselineStore,
}

impl ProfileBaselineStore {
    /// `store_dir` is the automation store directory (already proven plain by the store); the
    /// baselines folder inside it must not be a reparse point either.
    pub fn open(store_dir: &Path, keys: Arc<dyn ApprovalKeys>) -> Self {
        let dir = store_dir.join(BASELINES_DIR);
        if is_reparse(&dir) {
            return Self::unavailable();
        }
        Self {
            inner: SyncBaselineStore::open_authenticated(&dir, Arc::new(KeyedAuth::new(keys))),
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

fn is_reparse(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .map(|m| m.file_attributes() & 0x0400 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
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
