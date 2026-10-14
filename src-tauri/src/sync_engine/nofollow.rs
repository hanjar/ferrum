//! Handle-relative, no-follow folder access for the profile engine (design §5.4 contract items
//! 1-3, SE-9).
//!
//! A path below a sync root or the versioning root is never resolved through the filesystem
//! namespace: it is walked component by component from an opened root handle
//! ([`archive_publish`](crate::archive_extract::publish) primitives), so a junction or symlink
//! planted in the tree is rejected instead of followed, and nothing is written through it.
//! While a handle of the chain is open the folder cannot be renamed or replaced (the handles are
//! opened without delete sharing), which also closes the swap window between creating a parent
//! and using it.

use std::path::Path;

use crate::archive_extract::publish::{self, DirHandle, PublishError};
use crate::mutations::validate_portable_name;
use crate::sync_engine::errors::{Classified, ErrorClass, Reason, classify_text};

/// Why a folder chain could not be opened or created.
#[derive(Debug)]
pub enum ChainError {
    /// A component is a junction or symlink.
    Link,
    /// A component exists but is a file.
    ParentIsFile,
    /// A component does not exist (and creation was not asked for).
    NotFound,
    Other(PublishError),
}

impl ChainError {
    pub fn classify(&self) -> Classified {
        match self {
            ChainError::Link => Classified {
                class: ErrorClass::PerItem,
                reason: Reason::LinkInPath,
            },
            ChainError::ParentIsFile => Classified {
                class: ErrorClass::PerItem,
                reason: Reason::ParentIsFile,
            },
            ChainError::NotFound => Classified {
                class: ErrorClass::PerItem,
                reason: Reason::NotFound,
            },
            ChainError::Other(e) => classify_publish(e),
        }
    }
}

/// Classify a failed no-follow primitive.
pub fn classify_publish(e: &PublishError) -> Classified {
    match e {
        PublishError::NotFound => Classified {
            class: ErrorClass::PerItem,
            reason: Reason::NotFound,
        },
        PublishError::Exists => Classified {
            class: ErrorClass::PerItem,
            reason: Reason::AlreadyExists,
        },
        PublishError::Reparse => Classified {
            class: ErrorClass::PerItem,
            reason: Reason::LinkInPath,
        },
        PublishError::WrongKind => Classified {
            class: ErrorClass::PerItem,
            reason: Reason::ParentIsFile,
        },
        PublishError::Io(text) => {
            if is_not_a_directory(text) {
                Classified {
                    class: ErrorClass::PerItem,
                    reason: Reason::ParentIsFile,
                }
            } else {
                classify_text(text, None, false)
            }
        }
    }
}

/// STATUS_NOT_A_DIRECTORY (0xC0000103): a directory open found a file.
fn is_not_a_directory(text: &str) -> bool {
    text.to_ascii_lowercase().contains("0xc0000103")
}

/// A chain end: either the root handle itself (no components) or an owned handle below it.
pub struct Held<'a> {
    root: &'a DirHandle,
    owned: Option<DirHandle>,
}

impl Held<'_> {
    pub fn dir(&self) -> &DirHandle {
        self.owned.as_ref().unwrap_or(self.root)
    }
}

/// Split a '/'-separated relative name into validated components. Anything that is not an
/// ordinary file name component (empty, `.`/`..`, separators, reserved device names, trailing
/// dot or space, invalid characters) is refused: a name from a plan never reaches a handle open
/// unchecked.
pub fn split_rel(name: &str) -> Result<Vec<&str>, Classified> {
    let invalid = Classified {
        class: ErrorClass::PerItem,
        reason: Reason::InvalidName,
    };
    if name.is_empty() {
        return Err(invalid);
    }
    let mut out = Vec::new();
    for comp in name.split('/') {
        if validate_portable_name(comp).is_err() {
            return Err(invalid);
        }
        out.push(comp);
    }
    Ok(out)
}

/// Walk `comps` below `root` without following reparse points. With `create`, missing folders
/// are made (one `mkdir` each, never recursive); a name taken by a file is `ParentIsFile`.
pub fn open_chain<'a>(
    root: &'a DirHandle,
    comps: &[&str],
    create: bool,
) -> Result<Held<'a>, ChainError> {
    let mut cur: Option<DirHandle> = None;
    for comp in comps {
        let name = std::ffi::OsStr::new(comp);
        let parent: &DirHandle = cur.as_ref().unwrap_or(root);
        let next = match publish::child_dir(parent, name) {
            Ok(h) => h,
            Err(PublishError::NotFound) if create => match publish::create_dir_new(parent, name) {
                Ok(h) => h,
                // Created by someone between the two calls: open what is there now.
                Err(PublishError::Exists) => {
                    publish::child_dir(parent, name).map_err(map_open_error)?
                }
                Err(e) => return Err(map_open_error(e)),
            },
            Err(e) => return Err(map_open_error(e)),
        };
        cur = Some(next);
    }
    Ok(Held { root, owned: cur })
}

fn map_open_error(e: PublishError) -> ChainError {
    match e {
        PublishError::Reparse => ChainError::Link,
        PublishError::NotFound => ChainError::NotFound,
        PublishError::WrongKind => ChainError::ParentIsFile,
        PublishError::Io(ref text) if is_not_a_directory(text) => ChainError::ParentIsFile,
        other => ChainError::Other(other),
    }
}

/// Like [`open_chain`] but returns an owned handle (needs at least one component).
pub fn open_chain_owned(
    root: &DirHandle,
    comps: &[&str],
    create: bool,
) -> Result<DirHandle, ChainError> {
    if comps.is_empty() {
        return Err(ChainError::NotFound);
    }
    let held = open_chain(root, comps, create)?;
    held.owned.ok_or(ChainError::NotFound)
}

/// The comparable form of a folder path: no `\\?\` / `\\?\UNC\` prefix, no trailing separator,
/// lowercase.
fn comparable(path: &str) -> String {
    let mut p = path.to_string();
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        p = format!(r"\\{rest}");
    } else if let Some(rest) = p.strip_prefix(r"\\?\") {
        p = rest.to_string();
    }
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    p.to_lowercase()
}

/// Is the folder behind `handle` exactly the one named `expected`? A junction or symlink
/// anywhere on the path makes the OS resolve another folder, and the answer is "no" (T4/T9: the
/// path the profile recorded must be the folder that is opened, not a redirection of it).
/// `publish::open_root` checks the reparse attribute of the path AFTER following it, so this is
/// what actually catches a redirected root. The handle stays open afterwards, so the folder
/// cannot be swapped under it.
fn is_the_folder(expected: &str, handle: &DirHandle) -> bool {
    publish::is_directory(handle)
        && publish::final_path(handle)
            .is_some_and(|actual| comparable(&actual) == comparable(expected))
}

/// Open a root folder for the engine: an existing folder, never a redirection of the recorded
/// path. With `create` the missing tail is made below the nearest existing ancestor, folder by
/// folder, no-follow (the versioning root is "created on first use", SP-3.2).
pub fn open_root_folder(path: &str, create: bool) -> Result<DirHandle, ChainError> {
    let p = Path::new(path);
    match publish::open_root(p) {
        Ok(h) => {
            return if is_the_folder(path, &h) {
                Ok(h)
            } else {
                Err(ChainError::Link)
            };
        }
        Err(PublishError::NotFound) if create => {}
        Err(e) => return Err(map_open_error(e)),
    }
    // Find the deepest ancestor that exists, then create the rest below it.
    let mut missing: Vec<String> = Vec::new();
    let mut at = p.to_path_buf();
    loop {
        let Some(name) = at.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return Err(ChainError::NotFound);
        };
        missing.push(name);
        if !at.pop() {
            return Err(ChainError::NotFound);
        }
        match publish::open_root(&at) {
            Ok(base) => {
                if !is_the_folder(&at.to_string_lossy(), &base) {
                    return Err(ChainError::Link);
                }
                missing.reverse();
                let comps: Vec<&str> = missing.iter().map(String::as_str).collect();
                for comp in &comps {
                    if validate_portable_name(comp).is_err() {
                        return Err(ChainError::Other(PublishError::Io(
                            "invalid folder name".into(),
                        )));
                    }
                }
                return open_chain_owned(&base, &comps, true);
            }
            Err(PublishError::NotFound) => continue,
            Err(e) => return Err(map_open_error(e)),
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferrum-nofollow-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    use crate::sync_engine::test_support::make_junction;

    #[test]
    fn rel_names_are_validated_per_component() {
        assert_eq!(split_rel("a/b/c.txt").unwrap(), vec!["a", "b", "c.txt"]);
        for bad in [
            "", "a//b", "../x", "a/../b", "a/./b", "a\\b", "x/CON", "t.", "a:b",
        ] {
            assert!(split_rel(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn chain_creates_folders_and_is_idempotent() {
        let base = temp("create");
        let root = publish::open_root(&base).unwrap();
        let held = open_chain(&root, &["a", "b", "c"], true).unwrap();
        drop(held);
        assert!(base.join("a").join("b").join("c").is_dir());
        // A second walk finds the same folders.
        assert!(open_chain(&root, &["a", "b", "c"], false).is_ok());
        assert!(matches!(
            open_chain(&root, &["a", "zzz"], false),
            Err(ChainError::NotFound)
        ));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_junction_in_the_chain_is_rejected_and_nothing_is_written_through_it() {
        let base = temp("junction");
        let outside = temp("junction-out");
        let root = publish::open_root(&base).unwrap();
        fs::create_dir_all(base.join("real")).unwrap();
        if !make_junction(&base.join("link"), &outside) {
            eprintln!("SKIPPED: junction creation is not permitted here");
            return;
        }
        for create in [false, true] {
            let r = open_chain(&root, &["link", "child"], create);
            assert!(matches!(r, Err(ChainError::Link)), "create={create}");
        }
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        let _ = fs::remove_dir(base.join("link"));
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_file_where_a_folder_is_needed_fails_and_overwrites_nothing() {
        let base = temp("file-parent");
        fs::write(base.join("a"), b"keep").unwrap();
        let root = publish::open_root(&base).unwrap();
        for create in [false, true] {
            let r = open_chain(&root, &["a", "b"], create);
            assert!(
                matches!(r, Err(ChainError::ParentIsFile)),
                "create={create}"
            );
        }
        assert_eq!(fs::read(base.join("a")).unwrap(), b"keep");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_dir_removal_is_non_recursive_and_skips_links() {
        let base = temp("rmdir");
        let outside = temp("rmdir-out");
        fs::create_dir_all(base.join("empty")).unwrap();
        fs::create_dir_all(base.join("full")).unwrap();
        fs::write(base.join("full").join("keep.txt"), b"x").unwrap();
        fs::write(outside.join("keep.txt"), b"x").unwrap();
        let root = publish::open_root(&base).unwrap();
        let name = |n: &str| std::ffi::OsString::from(n);
        assert!(publish::remove_empty_dir(&root, &name("empty")).unwrap());
        assert!(!base.join("empty").exists());
        // Not empty: reported, nothing removed.
        assert!(!publish::remove_empty_dir(&root, &name("full")).unwrap());
        assert!(base.join("full").join("keep.txt").exists());
        // Gone: NotFound.
        assert!(matches!(
            publish::remove_empty_dir(&root, &name("empty")),
            Err(PublishError::NotFound)
        ));
        // A junction is neither followed nor removed.
        if make_junction(&base.join("link"), &outside) {
            assert!(matches!(
                publish::remove_empty_dir(&root, &name("link")),
                Err(PublishError::Reparse)
            ));
            assert!(outside.join("keep.txt").exists());
            let _ = fs::remove_dir(base.join("link"));
        } else {
            eprintln!("SKIPPED: junction creation is not permitted here");
        }
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn delete_file_handles_read_only_and_refuses_directories() {
        let base = temp("delfile");
        fs::write(base.join("ro.txt"), b"x").unwrap();
        let mut perm = fs::metadata(base.join("ro.txt")).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(base.join("ro.txt"), perm).unwrap();
        fs::create_dir_all(base.join("dir")).unwrap();
        let root = publish::open_root(&base).unwrap();
        publish::delete_file(&root, std::ffi::OsStr::new("ro.txt")).unwrap();
        assert!(!base.join("ro.txt").exists());
        // A directory is not a file: refused, left alone.
        assert!(publish::delete_file(&root, std::ffi::OsStr::new("dir")).is_err());
        assert!(base.join("dir").is_dir());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rename_into_never_replaces_and_keeps_the_mtime() {
        let base = temp("rename");
        fs::create_dir_all(base.join("from")).unwrap();
        fs::create_dir_all(base.join("to")).unwrap();
        fs::write(base.join("from").join("a.txt"), b"one").unwrap();
        fs::write(base.join("to").join("taken.txt"), b"two").unwrap();
        let root = publish::open_root(&base).unwrap();
        let from = open_chain_owned(&root, &["from"], false).unwrap();
        let to = open_chain_owned(&root, &["to"], false).unwrap();
        let os = std::ffi::OsStr::new;
        let before = fs::metadata(base.join("from").join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        publish::rename_file_into(&from, os("a.txt"), &to, os("b.txt")).unwrap();
        assert!(!base.join("from").join("a.txt").exists());
        assert_eq!(fs::read(base.join("to").join("b.txt")).unwrap(), b"one");
        assert_eq!(
            fs::metadata(base.join("to").join("b.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        // The destination exists: refused, both files intact.
        fs::write(base.join("from").join("c.txt"), b"three").unwrap();
        let r = publish::rename_file_into(&from, os("c.txt"), &to, os("taken.txt"));
        assert!(matches!(r, Err(publish::MoveError::Exists)));
        assert_eq!(fs::read(base.join("to").join("taken.txt")).unwrap(), b"two");
        assert!(base.join("from").join("c.txt").exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// Plain (no `\\?\` prefix) real path of an existing folder.
    fn plain(p: &Path) -> String {
        let s = fs::canonicalize(p).unwrap().to_string_lossy().into_owned();
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
    }

    // T4/T9: a root path that a junction redirects is NOT the folder the profile recorded -
    // refused, not followed. (`publish::open_root` looks at the reparse attribute of the path
    // AFTER following it, so the engine proves the final path instead.)
    #[test]
    fn a_root_reached_through_a_junction_is_refused() {
        let base = temp("rootlink");
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        if !make_junction(&link, &real) {
            eprintln!("SKIPPED: junction creation is not permitted here");
            return;
        }
        assert!(open_root_folder(&plain(&real), false).is_ok());
        let via_link = format!("{}\\link", plain(&base));
        assert!(matches!(
            open_root_folder(&via_link, false),
            Err(ChainError::Link)
        ));
        // A missing tail below a redirected ancestor is refused too.
        assert!(matches!(
            open_root_folder(&format!("{via_link}\\new"), true),
            Err(ChainError::Link)
        ));
        assert!(!real.join("new").exists());
        let _ = fs::remove_dir(&link);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_tail_is_created_below_the_existing_ancestor_and_a_file_is_not_a_root() {
        let base = temp("roottail");
        let base_str = plain(&base);
        assert!(open_root_folder(&format!("{base_str}\\a\\b"), true).is_ok());
        assert!(base.join("a").join("b").is_dir());
        // Without `create` a missing root is NotFound.
        assert!(matches!(
            open_root_folder(&format!("{base_str}\\nope"), false),
            Err(ChainError::NotFound)
        ));
        // A file is not a folder.
        fs::write(base.join("f.txt"), b"x").unwrap();
        assert!(open_root_folder(&format!("{base_str}\\f.txt"), false).is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
