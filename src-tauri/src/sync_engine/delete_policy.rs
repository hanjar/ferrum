//! Delete policies and the run gate (Automation design §5.5, §10.4 step 7-8, SP-3).
//!
//! - `RecycleBin`: one file at a time through the shell, ONLY when the receiving root is on the
//!   volume root `X:\` of a `DRIVE_FIXED` drive. The check runs again at run time with a fresh
//!   probe (it is also made when a profile is saved); a failure here stops the run before any
//!   operation. A Recycle Bin request never silently becomes a permanent delete.
//! - `Versioning`: [`VersionStore`](crate::sync_engine::versioning::VersionStore).
//! - `Permanent`: the file is deleted through its own handle, no-follow. Nothing in the engine
//!   removes a folder recursively: folders only ever go through `remove_empty_dir`.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::archive_extract::publish::{self, DirHandle};
use crate::automation::model::{
    DeletePolicy, DriveKind, ProfilePolicy, RootIdentity, Side, receiving_sides,
};
use crate::ops::CancelToken;
use crate::sync_engine::env::{BinState, EngineEnv};
use crate::sync_engine::errors::{Classified, ErrorClass, Reason, classify_fs, classify_io};
use crate::sync_engine::nofollow::{classify_publish, open_chain};
use crate::sync_engine::roots::{self, RootError, RootProbe};
use crate::sync_engine::versioning::{Expect, Moved, VersionStore};

/// Why a run could not start (before any operation). Codes are the closed vocabulary shown to
/// the user; no variant carries a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// The root's path form is no longer valid.
    RootInvalid(Side),
    /// The root cannot be opened now (offline, missing).
    RootUnavailable(Side),
    /// The root is another folder than the one the profile recorded (SP-7.1, T8).
    IdentityMismatch(Side),
    /// The Recycle Bin is not available on the receiving side (SP-3.1).
    RecycleBinUnavailable(Side),
    /// The versioning root is invalid, a file, nested in a sync root or contains one (SP-3.2).
    VersioningRootInvalid,
}

impl GateError {
    pub fn code(self) -> &'static str {
        match self {
            GateError::RootInvalid(_) => "root_invalid",
            GateError::RootUnavailable(_) => "root_unavailable",
            GateError::IdentityMismatch(_) => "identity_mismatch",
            GateError::RecycleBinUnavailable(_) => "recycle_bin_unavailable",
            GateError::VersioningRootInvalid => "versioning_root_invalid",
        }
    }

    /// The run error text (documented: it may name the profile's own root).
    pub fn text(self, profile: &ProfilePolicy) -> String {
        let root = |s: Side| match s {
            Side::Left => profile.left.path.as_str(),
            Side::Right => profile.right.path.as_str(),
        };
        match self {
            GateError::RootInvalid(s) => format!("{} is not a valid folder path.", root(s)),
            GateError::RootUnavailable(s) => {
                format!("{} is not available. Nothing was changed.", root(s))
            }
            GateError::IdentityMismatch(s) => format!(
                "{} is not the folder this profile was created for. Nothing was changed.",
                root(s)
            ),
            GateError::RecycleBinUnavailable(s) => format!(
                "Recycle Bin isn't available on {} — choose a versioning folder or permanent delete.",
                root(s)
            ),
            GateError::VersioningRootInvalid => {
                "The versioning folder is not usable. Nothing was deleted or overwritten."
                    .to_string()
            }
        }
    }
}

/// The canonical folders a run works on (and leases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRoots {
    pub left: String,
    pub right: String,
    pub versioning: Option<String>,
}

impl RunRoots {
    /// Every root the lease must cover (design §3.4).
    pub fn lease_roots(&self) -> Vec<String> {
        let mut v = vec![self.left.clone(), self.right.clone()];
        v.extend(self.versioning.clone());
        v
    }
}

fn same_path(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Gate checks that need no operation yet: roots exist and are the recorded folders, the
/// Recycle Bin is allowed on the receiving sides, the versioning root is usable.
pub fn check_gate(profile: &ProfilePolicy, env: &EngineEnv) -> Result<RunRoots, GateError> {
    let probe: &dyn RootProbe = &*env.probe;
    let mut fresh: Vec<(Side, String, RootIdentity)> = Vec::new();
    for (side, spec) in [(Side::Left, &profile.left), (Side::Right, &profile.right)] {
        let canonical = match probe.canonical(&spec.path) {
            Ok(c) => c,
            Err(RootError::Invalid) => return Err(GateError::RootInvalid(side)),
            Err(_) => return Err(GateError::RootUnavailable(side)),
        };
        if !same_path(&canonical, &spec.path) {
            // The path now resolves somewhere else (junction retargeted, drive letter moved).
            return Err(GateError::IdentityMismatch(side));
        }
        let identity = match probe.identity(&canonical) {
            Ok(id) => id,
            Err(RootError::Invalid) => return Err(GateError::RootInvalid(side)),
            Err(_) => return Err(GateError::RootUnavailable(side)),
        };
        if identity != spec.identity || is_unknown(&identity) {
            return Err(GateError::IdentityMismatch(side));
        }
        fresh.push((side, canonical, identity));
    }

    if matches!(profile.delete_policy, DeletePolicy::RecycleBin) {
        for side in receiving_sides(&profile.mode) {
            let identity = fresh
                .iter()
                .find(|(s, _, _)| *s == side)
                .map(|(_, _, id)| id);
            let fixed = matches!(
                identity,
                Some(RootIdentity::Local {
                    drive: DriveKind::Fixed,
                    ..
                })
            );
            if !fixed {
                return Err(GateError::RecycleBinUnavailable(side));
            }
            // The volume's own Recycle Bin settings: "don't move files to the Recycle Bin", a
            // bin size of 0 or the no-recycle policy turn a delete into a permanent one; an
            // unreadable setting counts as unusable (SP-3.1, fail closed).
            let root = match side {
                Side::Left => &profile.left.path,
                Side::Right => &profile.right.path,
            };
            let volume_root: String = root.chars().take(3).collect();
            if env.bin.state(&volume_root) != BinState::Usable {
                return Err(GateError::RecycleBinUnavailable(side));
            }
        }
    }

    let versioning = match &profile.delete_policy {
        DeletePolicy::Versioning { root } => Some(check_versioning_root(root, &fresh, probe)?),
        _ => None,
    };
    Ok(RunRoots {
        left: fresh[0].1.clone(),
        right: fresh[1].1.clone(),
        versioning,
    })
}

/// A network identity recorded while the share was offline carries no ids: never trusted.
fn is_unknown(identity: &RootIdentity) -> bool {
    matches!(
        identity,
        RootIdentity::Network {
            volume_serial: None,
            file_id: None,
            ..
        }
    )
}

fn check_versioning_root(
    stored: &str,
    sync_roots: &[(Side, String, RootIdentity)],
    probe: &dyn RootProbe,
) -> Result<String, GateError> {
    let bad = GateError::VersioningRootInvalid;
    if stored.is_empty() {
        return Err(bad);
    }
    let canonical = probe.canonical(stored).map_err(|_| bad)?;
    if !same_path(&canonical, stored) {
        return Err(bad);
    }
    for (_, root, _) in sync_roots {
        if same_path(root, &canonical) || roots::overlap(root, &canonical) {
            return Err(bad);
        }
    }
    // A FILE named like the versioning root passes canonicalization; refuse it
    // here. A folder that does not exist yet is created on first use.
    match std::fs::metadata(&canonical) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(bad),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(bad),
    }
    Ok(canonical)
}

/// How a delete ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Done,
    /// The file no longer matches what the plan saw: nothing was deleted or moved.
    Changed,
}

/// Deletes (and moves aside, for overwrites) single files under the profile's policy.
pub struct Deleter<'a> {
    pub policy: &'a DeletePolicy,
    pub versions: Option<&'a VersionStore>,
    pub env: &'a EngineEnv,
    pub cancel: &'a CancelToken,
}

impl Deleter<'_> {
    /// Delete the file `name` in `rel_parent` below `root` (handle `root`, path `root_path`),
    /// provided it still matches `expect`. For `permanent` and `versioning` the check is made on
    /// the file's own handle, atomically with the delete; the shell's Recycle Bin call has no such
    /// hook, so there the just-in-time check made right before is all there is.
    pub fn delete_file(
        &self,
        side: Side,
        root: &DirHandle,
        root_path: &str,
        rel_parent: &[&str],
        name: &str,
        expect: Expect,
    ) -> Result<Removal, Classified> {
        let held = open_chain(root, rel_parent, false).map_err(|e| e.classify())?;
        let parent = held.dir();
        let mut path = PathBuf::from(root_path);
        for comp in rel_parent {
            path.push(comp);
        }
        path.push(name);
        match self.policy {
            DeletePolicy::RecycleBin => self.recycle(parent, name, &path, expect),
            DeletePolicy::Permanent => {
                let check = |size, ms| expect.matches(size, ms);
                match publish::delete_file_if(parent, OsStr::new(name), &check) {
                    Ok(true) => Ok(Removal::Done),
                    Ok(false) => Ok(Removal::Changed),
                    Err(e) => Err(classify_publish(&e)),
                }
            }
            DeletePolicy::Versioning { .. } => {
                let Some(store) = self.versions else {
                    // Cannot happen (the run opens the store first); fail closed.
                    return Err(Classified {
                        class: crate::sync_engine::errors::ErrorClass::PerItem,
                        reason: crate::sync_engine::errors::Reason::Unknown,
                    });
                };
                store
                    .move_in(
                        side,
                        parent,
                        &path,
                        rel_parent,
                        name,
                        expect,
                        self.env,
                        self.cancel,
                    )
                    .map(|moved| match moved {
                        Moved::Changed => Removal::Changed,
                        Moved::Renamed | Moved::Copied => Removal::Done,
                    })
            }
        }
    }

    /// One file to the Recycle Bin, observed through a handle opened BEFORE the shell is called:
    /// it must be a plain file that still matches the plan (a name swapped for a folder or a link
    /// never reaches the shell), and afterwards the same object must report a path inside the
    /// Recycle Bin. Anything else - gone, or somewhere else - means the shell deleted it for good:
    /// `recycle_bypassed` stops the run (at most this one file is affected).
    fn recycle(
        &self,
        parent: &DirHandle,
        name: &str,
        path: &std::path::Path,
        expect: Expect,
    ) -> Result<Removal, Classified> {
        #[cfg(windows)]
        {
            let probe = publish::open_file_probe(parent, OsStr::new(name))
                .map_err(|e| classify_publish(&e))?;
            match probe.facts() {
                Ok((size, ms)) if expect.matches(size, ms) => {}
                Ok(_) => return Ok(Removal::Changed),
                Err(e) => return Err(classify_io(&e, false)),
            }
            self.env
                .trash
                .trash_file(path)
                .map_err(|e| classify_fs(&e, false))?;
            // A classic (non-POSIX) delete - FAT/exFAT volumes - keeps the name while our probe is
            // open, but the object is gone for good as soon as it closes.
            if probe.delete_pending() {
                return Err(Classified {
                    class: ErrorClass::Systemic,
                    reason: Reason::RecycleBypassed,
                });
            }
            let original = path.to_string_lossy().to_lowercase();
            match probe.final_path() {
                Some(p) if self.env.trash.is_in_bin(&p) => Ok(Removal::Done),
                // Still where it was: the shell did nothing.
                Some(p) if p.trim_start_matches(r"\\?\").to_lowercase() == original => {
                    Err(Classified {
                        class: ErrorClass::PerItem,
                        reason: Reason::Unknown,
                    })
                }
                _ => Err(Classified {
                    class: ErrorClass::Systemic,
                    reason: Reason::RecycleBypassed,
                }),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (parent, name, path, expect);
            Err(Classified {
                class: ErrorClass::Systemic,
                reason: Reason::RecycleBypassed,
            })
        }
    }

    /// Keep the previous version of an overwritten file (versioning only): move the existing
    /// target aside; the caller copies the new file afterwards.
    pub fn keep_overwritten(
        &self,
        side: Side,
        root: &DirHandle,
        root_path: &str,
        rel_parent: &[&str],
        name: &str,
        expect: Expect,
    ) -> Result<Removal, Classified> {
        debug_assert!(matches!(self.policy, DeletePolicy::Versioning { .. }));
        self.delete_file(side, root, root_path, rel_parent, name, expect)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The REAL shell and the REAL Recycle Bin on a temp file owned by this test: the file ends
    /// up inside `\$Recycle.Bin\` and the engine's check says so. Ignored by default because it
    /// leaves one entry in the current user's bin; run by hand:
    /// `cargo test real_recycle_bin -- --ignored`.
    #[test]
    #[ignore = "puts one temp file into the real Recycle Bin"]
    fn real_recycle_bin_is_verified_by_the_final_path() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("ferrum-p02-bin-check-{}.txt", std::process::id());
        let file = dir.path().join(&name);
        std::fs::write(&file, b"temp").unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        let root = publish::open_root(dir.path()).unwrap();
        let env = EngineEnv::production();
        let cancel = CancelToken::default();
        let deleter = Deleter {
            policy: &DeletePolicy::RecycleBin,
            versions: None,
            env: &env,
            cancel: &cancel,
        };
        let expect = Expect {
            size: meta.len(),
            mtime_ms: crate::fs_service::modified_ms(&meta),
        };
        let out = deleter.delete_file(
            Side::Left,
            &root,
            &dir.path().to_string_lossy(),
            &[],
            &name,
            expect,
        );
        assert_eq!(out.unwrap(), Removal::Done);
        assert!(!file.exists());
    }
}
