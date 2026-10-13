//! What gets indexed, per privacy mode (FER-475).
//!
//! - `high_performance`: everything on every local fixed NTFS volume.
//! - `shared`: the content of every user profile directory (the
//!   `ProfileImagePath` of each `ProfileList` entry on that volume) is NOT
//!   indexed — the profile folder itself stays (its name is visible to every
//!   user in Explorer anyway), nothing below it is stored, read from the
//!   journal into the index, or persisted. Every result S3 returns in shared
//!   mode must additionally pass the client's AccessCheck (`query::DirAccess`).
//!
//!   Also excluded in `shared` (review M1): the content of `$Recycle.Bin` on
//!   every volume (a folder deleted from a profile keeps its names there),
//!   and a whole volume that is mounted inside a profile (the platform passes
//!   its root, `E:\`, as a profile path). The platform resolves every
//!   profile to its FINAL path (junctions, symlinks, relocated `C:\Users`),
//!   and a profile list that cannot be read or resolved fails closed: no
//!   volume is built in shared mode (review M2/M3, see `engine::discover`).
//!
//! A [`Scope`] has a fingerprint; a saved index whose fingerprint differs
//! (mode changed, profile added/removed) is discarded and rebuilt.

use crate::proto::Mode;

/// Exclusions for one volume. Paths are volume-relative components
/// (`["Users", "bob"]`), compared case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub mode: Mode,
    pub excluded: Vec<Vec<String>>,
    /// The whole volume lies inside a profile (shared mode): not indexed.
    pub whole_volume: bool,
}

/// Recycle bin folder at every volume root.
pub const RECYCLE_BIN: &str = "$Recycle.Bin";

impl Scope {
    pub fn everything() -> Scope {
        Scope {
            mode: Mode::HighPerformance,
            excluded: Vec::new(),
            whole_volume: false,
        }
    }

    /// Scope for `volume` (drive letter) in `mode`, given absolute profile
    /// paths (`C:\Users\bob`). Profiles on other volumes and malformed paths
    /// are ignored; a profile path that IS this volume's root (`E:\`) means
    /// the whole volume is excluded.
    pub fn for_volume(mode: Mode, volume: char, profiles: &[String]) -> Scope {
        let mut excluded: Vec<Vec<String>> = Vec::new();
        let mut whole_volume = false;
        if mode == Mode::Shared {
            excluded.push(vec![RECYCLE_BIN.to_string()]);
            for p in profiles {
                if is_volume_root(volume, p) {
                    whole_volume = true;
                }
                if let Some(comps) = volume_relative(volume, p)
                    && !excluded.iter().any(|e| eq_path(e, &comps))
                {
                    excluded.push(comps);
                }
            }
            excluded.sort_by_key(|c| c.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>());
        }
        Scope {
            mode,
            excluded,
            whole_volume,
        }
    }

    /// Stable 64-bit FNV-1a over the mode and the lowercased exclusions.
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for x in b {
                h ^= *x as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        eat(self.mode.as_str().as_bytes());
        if self.whole_volume {
            eat(b"\x01whole");
        }
        for e in &self.excluded {
            eat(b"\x00");
            for c in e {
                eat(b"\\");
                eat(c.to_lowercase().as_bytes());
            }
        }
        h
    }
}

/// `E:\` or `E:` for volume `E`.
fn is_volume_root(volume: char, path: &str) -> bool {
    let mut c = path.chars();
    match (c.next(), c.next()) {
        (Some(d), Some(':')) => d.eq_ignore_ascii_case(&volume) && matches!(c.as_str(), "" | "\\"),
        _ => false,
    }
}

fn eq_path(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.to_lowercase() == y.to_lowercase())
}

/// `C:\Users\bob` on volume `C` → `["Users", "bob"]`. `None` for another
/// volume, the root, UNC / device paths, or `.`/`..` components.
pub fn volume_relative(volume: char, path: &str) -> Option<Vec<String>> {
    let mut chars = path.chars();
    let drive = chars.next()?;
    if !drive.eq_ignore_ascii_case(&volume) || chars.next()? != ':' {
        return None;
    }
    let rest = chars.as_str();
    if !rest.starts_with('\\') {
        return None;
    }
    let comps: Vec<String> = rest
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect();
    if comps.is_empty()
        || comps
            .iter()
            .any(|c| c == "." || c == ".." || c.contains('/'))
    {
        return None;
    }
    Some(comps)
}

// The S2 result-filter hook (`ResultFilter`, fail-closed in shared mode)
// is replaced in FER-476 by `query::DirAccess`: every result in shared
// mode passes an AccessCheck with the client's token (`access.rs`).

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn volume_relative_paths() {
        assert_eq!(
            volume_relative('C', r"C:\Users\bob"),
            Some(s(&["Users", "bob"]))
        );
        assert_eq!(
            volume_relative('c', r"C:\Users\bob\"),
            Some(s(&["Users", "bob"]))
        );
        assert_eq!(volume_relative('D', r"C:\Users\bob"), None);
        for bad in [
            r"C:\",
            "C:",
            r"C:Users",
            r"\\srv\share\x",
            r"\\?\C:\x",
            r"C:\a\..\b",
            "",
        ] {
            assert_eq!(volume_relative('C', bad), None, "{bad}");
        }
    }

    #[test]
    fn shared_mode_excludes_profiles_on_this_volume_only() {
        let profiles = vec![
            r"C:\Users\bob".to_string(),
            r"C:\users\BOB".to_string(),
            r"C:\Users\alice".to_string(),
            r"D:\Profiles\carol".to_string(),
            r"C:\Windows\ServiceProfiles\LocalService".to_string(),
        ];
        let c = Scope::for_volume(Mode::Shared, 'C', &profiles);
        assert_eq!(
            c.excluded,
            vec![
                s(&["$Recycle.Bin"]),
                s(&["Users", "alice"]),
                s(&["Users", "bob"]),
                s(&["Windows", "ServiceProfiles", "LocalService"]),
            ]
        );
        let d = Scope::for_volume(Mode::Shared, 'D', &profiles);
        assert_eq!(
            d.excluded,
            vec![s(&["$Recycle.Bin"]), s(&["Profiles", "carol"])]
        );
        assert!(!d.whole_volume);
        let hp = Scope::for_volume(Mode::HighPerformance, 'C', &profiles);
        assert!(hp.excluded.is_empty());
    }

    #[test]
    fn recycle_bin_is_always_excluded_in_shared_mode() {
        let e = Scope::for_volume(Mode::Shared, 'E', &[]);
        assert_eq!(e.excluded, vec![s(&["$Recycle.Bin"])]);
        assert!(
            Scope::for_volume(Mode::HighPerformance, 'E', &[])
                .excluded
                .is_empty()
        );
    }

    #[test]
    fn a_volume_mounted_in_a_profile_is_wholly_excluded() {
        let p = vec![r"C:\Users\bob".to_string(), r"E:\".to_string()];
        assert!(Scope::for_volume(Mode::Shared, 'E', &p).whole_volume);
        assert!(Scope::for_volume(Mode::Shared, 'e', &["e:".to_string()]).whole_volume);
        assert!(!Scope::for_volume(Mode::Shared, 'C', &p).whole_volume);
        assert!(!Scope::for_volume(Mode::HighPerformance, 'E', &p).whole_volume);
        assert_ne!(
            Scope::for_volume(Mode::Shared, 'E', &p).fingerprint(),
            Scope::for_volume(Mode::Shared, 'E', &[]).fingerprint()
        );
    }

    #[test]
    fn fingerprint_tracks_mode_and_exclusions() {
        let p = vec![r"C:\Users\bob".to_string()];
        let a = Scope::for_volume(Mode::Shared, 'C', &p);
        let b = Scope::for_volume(Mode::Shared, 'C', &[r"C:\USERS\BOB".to_string()]);
        assert_eq!(a.fingerprint(), b.fingerprint(), "case-insensitive");
        let c = Scope::for_volume(Mode::HighPerformance, 'C', &p);
        assert_ne!(a.fingerprint(), c.fingerprint());
        let d = Scope::for_volume(Mode::Shared, 'C', &[]);
        assert_ne!(a.fingerprint(), d.fingerprint());
        assert_ne!(d.fingerprint(), Scope::everything().fingerprint());
    }
}
