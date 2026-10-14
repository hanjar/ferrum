//! The automation store: one SQLite database (WAL) under the local (non-roaming) app data dir
//! (Automation design §4). Opens fail closed (SE-5): the store directory and the database file
//! must be plain file-system entries — any reparse point makes the store `untrusted` — and an
//! older binary facing a newer schema gets a read-only connection (LC-3): reads work, every
//! write reports `store_read_only`.
//!
//! Errors never carry paths or SQL text; commands map them to the closed-set codes of design §7.

pub mod hosts;
pub mod migrate;
pub mod profiles;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

pub use migrate::SCHEMA_VERSION;

/// Store failures that cross to commands as closed-set codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// Cannot open or create the store (or a migration failed; LC-5 keeps the backup).
    Unavailable,
    /// The store location contains a reparse point (SE-5, threat T3).
    Untrusted,
    /// The store schema is newer than this binary (LC-3).
    ReadOnly,
    /// The row does not exist.
    NotFound,
    /// The profile name is already in use (case-insensitive, SP-1.3).
    NameTaken,
    /// `expectedRevision` does not match the stored revision (SP-6.2).
    Stale,
    /// The profile is referenced by an automation (SP-9.5).
    InUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    Write,
    ReadOnly,
    Failed(StoreError),
}

/// A handle to one opened store. Cheap to clone through [`std::sync::Arc`]; all access is
/// serialized on one connection (the runner opens its own).
pub struct Store {
    inner: Mutex<Inner>,
}

struct Inner {
    conn: Option<Connection>,
    mode: OpenMode,
}

impl Store {
    /// Open (creating or migrating) the store in `dir`. Never panics; an unusable store is
    /// remembered and reported by every command that touches it.
    pub fn open(dir: &Path, app_version: &str) -> Store {
        match open_connection(dir, app_version) {
            Ok((conn, mode)) => Store {
                inner: Mutex::new(Inner {
                    conn: Some(conn),
                    mode,
                }),
            },
            Err(e) => Store {
                inner: Mutex::new(Inner {
                    conn: None,
                    mode: OpenMode::Failed(e),
                }),
            },
        }
    }

    /// Read access (also available on a read-only, newer-schema store, LC-3). The closure may
    /// use its own error type; store failures convert into it.
    pub fn with_read<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E>
    where
        StoreError: Into<E>,
    {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match (&guard.conn, guard.mode) {
            (Some(conn), _) => f(conn),
            (None, OpenMode::Failed(e)) => Err(e.into()),
            (None, _) => Err(StoreError::Unavailable.into()),
        }
    }

    /// Write access. Refused with [`StoreError::ReadOnly`] on a newer-schema store (LC-3).
    pub fn with_write<T, E>(&self, f: impl FnOnce(&mut Connection) -> Result<T, E>) -> Result<T, E>
    where
        StoreError: Into<E>,
    {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let OpenMode::Failed(e) = guard.mode {
            return Err(e.into());
        }
        if guard.mode == OpenMode::ReadOnly {
            return Err(StoreError::ReadOnly.into());
        }
        match guard.conn.as_mut() {
            Some(conn) => f(conn),
            None => Err(StoreError::Unavailable.into()),
        }
    }
}

fn open_connection(dir: &Path, app_version: &str) -> Result<(Connection, OpenMode), StoreError> {
    std::fs::create_dir_all(dir).map_err(|_| StoreError::Unavailable)?;
    // SE-5 (threat T3): the store must live in plain folders — the `automation` directory (and
    // every folder above it that exists), plus the database file itself, may not be a reparse
    // point, or a junction could move the whole store elsewhere.
    ensure_plain_path(dir)?;
    let db = dir.join("automation.db");
    if db.exists() && is_reparse(&db) {
        return Err(StoreError::Untrusted);
    }
    let conn = Connection::open(&db).map_err(|_| StoreError::Unavailable)?;
    let _: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(|_| StoreError::Unavailable)?;
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
    )
    .map_err(|_| StoreError::Unavailable)?;
    let from: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| StoreError::Unavailable)?;
    let mode = if from > SCHEMA_VERSION {
        // LC-3: a newer store is read by this older binary but never written.
        OpenMode::ReadOnly
    } else {
        if from < SCHEMA_VERSION {
            migrate::run(&conn, dir, from, app_version)?;
        }
        OpenMode::Write
    };
    Ok((conn, mode))
}

/// Refuse when `path` or any existing folder above it is a reparse point.
fn ensure_plain_path(path: &Path) -> Result<(), StoreError> {
    let mut cur = path.to_path_buf();
    loop {
        if is_reparse(&cur) {
            return Err(StoreError::Untrusted);
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent.to_path_buf(),
            _ => return Ok(()),
        }
    }
}

/// True when the entry itself is a junction, symlink or other reparse point.
fn is_reparse(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
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

#[cfg(test)]
mod tests {
    use super::*;

    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn empty_policy_json() -> String {
        // A minimal but valid ProfilePolicy JSON; the store only carries the text.
        r#"{"left":{"path":"C:\\a","identity":{"kind":"local","volumeSerial":"00000001","fileId":"aa","fsName":"NTFS","drive":"fixed"}},"right":{"path":"D:\\b","identity":{"kind":"local","volumeSerial":"00000001","fileId":"bb","fsName":"NTFS","drive":"fixed"}},"mode":{"kind":"oneWay","from":"left"},"deletePolicy":{"kind":"recycleBin"},"compare":"sizeAndDate","filters":{"include":[],"exclude":[]},"guard":{"enabled":true,"maxCount":50,"maxPercent":20}}"#.into()
    }

    /// LC-5 / design §4.5: a fresh directory migrates v0 → v1; the schema is mirrored in
    /// `meta.schema_version`.
    #[test]
    fn migration_v0_to_v1_on_an_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        store
            .with_write(|conn| -> Result<(), StoreError> {
                let version: i64 = conn
                    .pragma_query_value(None, "user_version", |r| r.get(0))
                    .map_err(|_| StoreError::Unavailable)?;
                assert_eq!(version, SCHEMA_VERSION);
                let mirrored: String = conn
                    .query_row(
                        "SELECT value FROM meta WHERE key = 'schema_version'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(mirrored, SCHEMA_VERSION.to_string());
                for table in ["profiles", "profile_revisions", "leases", "hosts"] {
                    let n: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                            [table],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(n, 1, "{table} exists");
                }
                Ok(())
            })
            .unwrap();
    }

    /// Reopening a current store is a no-op: no second migration, no backup file, data kept.
    #[test]
    fn reopening_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path(), "0.2.0");
        first
            .with_write(|conn| -> Result<(), StoreError> {
                conn.execute(
                    "INSERT INTO profiles(id, name, name_key, revision, policy, policy_digest, created_ms, updated_ms) VALUES ('id-1', 'P', 'p', 1, ?1, 'd', 1, 1)",
                    [empty_policy_json()],
                )
                .map(|_| ())
                .map_err(|_| StoreError::Unavailable)
            })
            .unwrap();
        drop(first);
        let second = Store::open(dir.path(), "0.2.0");
        second
            .with_read(|conn| -> Result<(), StoreError> {
                let n: i64 = conn
                    .query_row("SELECT COUNT(*) FROM profiles", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(n, 1);
                Ok(())
            })
            .unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().all(|n| !n.contains(".bak-")),
            "no backup left behind: {entries:?}"
        );
    }

    /// LC-3: a store written by a newer binary is opened read-only; every write reports
    /// `store_read_only` and nothing changes.
    #[test]
    fn a_newer_schema_store_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path(), "0.2.0");
        // Simulate a future version: schema 99.
        {
            let conn = Connection::open(dir.path().join("automation.db")).unwrap();
            let _: i64 = conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
                .unwrap_or(0);
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        let store = Store::open(dir.path(), "0.2.0");
        let err = store
            .with_write(|conn| -> Result<(), StoreError> {
                conn.execute("DELETE FROM profiles", [])
                    .map(|_| ())
                    .map_err(|_| StoreError::Unavailable)
            })
            .unwrap_err();
        assert_eq!(err, StoreError::ReadOnly);
        // Reads still work.
        store
            .with_read(|conn| -> Result<(), StoreError> {
                let version: i64 = conn
                    .pragma_query_value(None, "user_version", |r| r.get(0))
                    .map_err(|_| StoreError::Unavailable)?;
                assert_eq!(version, 99);
                Ok(())
            })
            .unwrap();
    }

    /// LC-5: a migration that fails mid-way keeps the backup and leaves the original store
    /// intact (failure injected at the v1 step).
    #[test]
    fn a_failed_migration_keeps_the_backup_and_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("automation.db");
        // A v0 store: an empty database file, user_version 0.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("PRAGMA user_version = 0;").unwrap();
        }
        let result = migrate::run_with_failure_at(
            &Connection::open(&db).unwrap(),
            dir.path(),
            0,
            "0.2.0",
            Some(1),
        );
        assert_eq!(result, Err(StoreError::Unavailable));
        // The backup of the previous schema exists and the original is still v0 with no tables.
        assert!(dir.path().join("automation.db.bak-v0").exists());
        let conn = Connection::open(&db).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 0);
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "the original store is untouched");
        // A later good open migrates normally from the intact original.
        drop(conn);
        Store::open(dir.path(), "0.2.0");
    }

    /// SE-5 (threat T3): the store directory as a junction → `untrusted`, fail closed.
    #[cfg(windows)]
    #[test]
    fn a_junction_as_the_store_directory_is_untrusted() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir_all(real.join("automation")).unwrap();
        let link = base.path().join("via-junction");
        assert!(junction(&link, &real), "junction created");
        let store = Store::open(&link.join("automation"), "0.2.0");
        assert_eq!(
            store
                .with_read(|_| -> Result<(), StoreError> { Ok(()) })
                .unwrap_err(),
            StoreError::Untrusted
        );
        assert_eq!(
            store
                .with_write(|_| -> Result<(), StoreError> { Ok(()) })
                .unwrap_err(),
            StoreError::Untrusted
        );
    }

    /// SE-5 (threat T3): the database file itself as a reparse point → `untrusted`.
    #[cfg(windows)]
    #[test]
    fn a_junction_as_the_database_file_is_untrusted() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("automation");
        std::fs::create_dir_all(&dir).unwrap();
        let target = base.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::remove_file(dir.join("automation.db")).ok();
        assert!(
            junction(&dir.join("automation.db"), &target),
            "junction created"
        );
        let store = Store::open(&dir, "0.2.0");
        assert_eq!(
            store
                .with_read(|_| -> Result<(), StoreError> { Ok(()) })
                .unwrap_err(),
            StoreError::Untrusted
        );
    }

    /// SE-5 (threat T3): a store whose directory cannot be created is `unavailable`, never
    /// silently relocated.
    #[test]
    fn an_uncreatable_directory_is_unavailable() {
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        // `automation` below a regular file cannot be created.
        let store = Store::open(&file.join("automation"), "0.2.0");
        assert_eq!(
            store
                .with_read(|_| -> Result<(), StoreError> { Ok(()) })
                .unwrap_err(),
            StoreError::Unavailable
        );
    }
}
