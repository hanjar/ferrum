//! The automation store: one SQLite database (WAL) under the local (non-roaming) app data dir
//! (Automation design §4). Opens fail closed (SE-5): the store directory and the database file
//! must be plain file-system entries — any reparse point makes the store `untrusted` — and an
//! older binary facing a newer schema gets a read-only connection (LC-3): reads work, every
//! write reports `store_read_only`.
//!
//! Errors never carry paths or SQL text; commands map them to the closed-set codes of design §7.

pub mod approvals;
pub mod automations;
pub mod baseline_gen;
pub mod hosts;
pub mod leases;
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

    /// A store that could not be set up at all (for example no local data folder could be
    /// resolved): every command reports `store_unavailable`.
    pub fn unavailable() -> Store {
        Store {
            inner: Mutex::new(Inner {
                conn: None,
                mode: OpenMode::Failed(StoreError::Unavailable),
            }),
        }
    }

    /// Whether writes are possible right now. Commands ask this BEFORE showing a native dialog,
    /// so the user is never prompted for a change that would be refused anyway (LC-3).
    pub fn check_writable(&self) -> Result<(), StoreError> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match guard.mode {
            OpenMode::Write => Ok(()),
            OpenMode::ReadOnly => Err(StoreError::ReadOnly),
            OpenMode::Failed(e) => Err(e),
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
    // SE-5 (threat T3): the store must live in plain folders - the `automation` directory and
    // every folder above it that exists may not be a reparse point, or a junction could move the
    // whole store elsewhere. Existing ancestors are checked BEFORE anything is created, so no
    // folder is ever made inside a junction's target.
    ensure_plain_path(dir)?;
    std::fs::create_dir_all(dir).map_err(|_| StoreError::Unavailable)?;
    ensure_plain_path(dir)?;
    ensure_plain_store_files(dir)?;
    let db = dir.join(DB_FILE);
    let conn = Connection::open(&db).map_err(|_| StoreError::Unavailable)?;
    // The wait for another opener (a second Ferrum process, the runner) comes first: the very
    // first statement already contends for the lock on a brand-new store.
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| StoreError::Unavailable)?;
    enable_wal(&conn)?;
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")
        .map_err(|_| StoreError::Unavailable)?;
    // SQLite has just created the sidecar files: they are checked too (re-check after open).
    ensure_plain_store_files(dir)?;
    let from: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| StoreError::Unavailable)?;
    let mode = if from > SCHEMA_VERSION {
        // LC-3: a newer store is read by this older binary but never written - the connection
        // itself refuses writes, not only the wrapper.
        conn.pragma_update(None, "query_only", true)
            .map_err(|_| StoreError::Unavailable)?;
        OpenMode::ReadOnly
    } else {
        if from < SCHEMA_VERSION {
            // `from` is only a hint: the migration re-reads the version under its lock.
            migrate::run(&conn, dir, from, app_version)?;
        }
        OpenMode::Write
    };
    Ok((conn, mode))
}

/// Switching a fresh database to WAL can answer "busy" immediately even with a busy timeout (two
/// openers racing to be the one that converts it), so the switch is retried within the same
/// patience as every other wait.
fn enable_wal(conn: &Connection) -> Result<(), StoreError> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0)) {
            Ok(_) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(
                    e.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Err(_) => return Err(StoreError::Unavailable),
        }
    }
}

const DB_FILE: &str = "automation.db";
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The database, its WAL/SHM sidecars, the migration lock and every migration backup must be
/// plain files: no reparse point, no extra hard link (SE-5, threat T3).
fn ensure_plain_store_files(dir: &Path) -> Result<(), StoreError> {
    for name in [
        DB_FILE,
        "automation.db-wal",
        "automation.db-shm",
        migrate::LOCK_FILE,
    ] {
        ensure_plain_file(&dir.join(name))?;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("automation.db.bak-v")
            {
                ensure_plain_file(&entry.path())?;
            }
        }
    }
    Ok(())
}

/// An existing file that is a reparse point or has more than one hard link makes the store
/// untrusted; a missing file is fine.
pub(crate) fn ensure_plain_file(path: &Path) -> Result<(), StoreError> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    if is_reparse(path) || link_count(path).is_some_and(|n| n > 1) {
        return Err(StoreError::Untrusted);
    }
    Ok(())
}

/// Number of hard links of an existing file (`None` when it cannot be asked).
#[cfg(windows)]
fn link_count(path: &Path) -> Option<u32> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // Share everything and ask for no access: this only reads attributes of a file another
    // connection may hold open.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(7)
        .open(path)
        .ok()?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle; zeroed POD out-parameter.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return None;
    }
    Some(info.nNumberOfLinks)
}

#[cfg(not(windows))]
fn link_count(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| m.nlink() as u32)
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
                for table in [
                    "profiles",
                    "profile_revisions",
                    "leases",
                    "hosts",
                    "automations",
                    "approvals",
                    "automation_runtime",
                    "queue",
                    "runs",
                    "run_ops",
                    "events",
                    "controls",
                    "baseline_gen",
                ] {
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

    /// A v1 store as the previous release left it, with one profile in it.
    fn v1_store(dir: &Path) {
        let conn = Connection::open(dir.join("automation.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        migrate::create_v1_for_tests(&conn, "0.1.0").unwrap();
        conn.execute(
            "INSERT INTO profiles(id, name, name_key, revision, policy, policy_digest, created_ms, updated_ms) VALUES ('id-1', 'Old profile', 'old profile', 3, ?1, 'd', 1, 2)",
            [empty_policy_json()],
        )
        .unwrap();
    }

    /// LC-5 (FER-522): the v1 -> v2 fixture migration keeps the v1 data, adds the automation
    /// tables, mirrors the version in `meta`, and leaves no backup behind.
    #[test]
    fn lc_5_migration_v1_to_v2_keeps_the_data_and_adds_the_tables() {
        let dir = tempfile::tempdir().unwrap();
        v1_store(dir.path());
        let store = Store::open(dir.path(), "0.2.0");
        store
            .with_write(|conn| -> Result<(), StoreError> {
                let version: i64 = conn
                    .pragma_query_value(None, "user_version", |r| r.get(0))
                    .unwrap();
                assert_eq!(version, 2);
                let mirrored: String = conn
                    .query_row(
                        "SELECT value FROM meta WHERE key = 'schema_version'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(mirrored, "2");
                let (name, revision): (String, i64) = conn
                    .query_row(
                        "SELECT name, revision FROM profiles WHERE id = 'id-1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!((name.as_str(), revision), ("Old profile", 3));
                // The new tables work: an automation needs an existing profile (foreign key) and
                // an approval needs its automation.
                assert!(
                    conn.execute(
                        "INSERT INTO automations(id, name, kind, profile_id, policy, enabled, config_rev, created_ms, updated_ms) VALUES ('a', 'A', 'watch', 'missing', '{}', 1, 1, 1, 1)",
                        [],
                    )
                    .is_err(),
                    "automations.profile_id references profiles"
                );
                conn.execute(
                    "INSERT INTO automations(id, name, kind, profile_id, policy, enabled, config_rev, created_ms, updated_ms) VALUES ('a', 'A', 'watch', 'id-1', '{}', 1, 1, 1, 1)",
                    [],
                )
                .unwrap();
                assert!(
                    conn.execute(
                        "INSERT INTO automations(id, name, kind, profile_id, policy, enabled, config_rev, created_ms, updated_ms) VALUES ('b', 'B', 'bogus', 'id-1', '{}', 1, 1, 1, 1)",
                        [],
                    )
                    .is_err(),
                    "kind is checked"
                );
                // A profile used by an automation cannot be removed by a stray DELETE.
                assert!(conn.execute("DELETE FROM profiles WHERE id = 'id-1'", []).is_err());
                Ok(())
            })
            .unwrap();
        assert!(names(dir.path()).iter().all(|n| !n.contains(".bak-")));
    }

    /// LC-5 (FER-522): a v2 step that fails keeps the v1 backup, leaves the v1 store untouched and
    /// a later good open migrates from it.
    #[test]
    fn lc_5_a_failing_v2_migration_keeps_the_backup_and_the_v1_original() {
        let dir = tempfile::tempdir().unwrap();
        v1_store(dir.path());
        let db = dir.path().join("automation.db");
        let result = migrate::run_with_failure_at(
            &Connection::open(&db).unwrap(),
            dir.path(),
            1,
            "0.2.0",
            Some(2),
        );
        assert_eq!(result, Err(StoreError::Unavailable));
        assert!(dir.path().join("automation.db.bak-v1").exists());
        let conn = Connection::open(&db).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='automations'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "the v1 store is untouched");
        let profiles: i64 = conn
            .query_row("SELECT COUNT(*) FROM profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(profiles, 1);
        drop(conn);
        let store = Store::open(dir.path(), "0.2.0");
        store.check_writable().unwrap();
        assert!(!dir.path().join("automation.db.bak-v1").exists());
    }

    // ── Rework 1 ──

    fn names(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// P2-6: two openers of a brand-new store at the same moment both succeed, one waits for the
    /// other, the schema is created exactly once, and no backup or lock file is left behind
    /// (a migration that really happened cleans up its own backup).
    #[test]
    fn p2_6_two_openers_of_a_new_store_both_succeed_and_leave_no_backup() {
        for round in 0..15 {
            let dir = tempfile::tempdir().unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = dir.path().to_path_buf();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        Store::open(&path, "0.2.0")
                    })
                })
                .collect();
            let stores: Vec<Store> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            for store in &stores {
                store
                    .check_writable()
                    .unwrap_or_else(|e| panic!("round {round}: {e:?}"));
                store
                    .with_read(|conn| -> Result<(), StoreError> {
                        let version: i64 = conn
                            .pragma_query_value(None, "user_version", |r| r.get(0))
                            .map_err(|_| StoreError::Unavailable)?;
                        assert_eq!(version, SCHEMA_VERSION);
                        let metas: i64 = conn
                            .query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))
                            .map_err(|_| StoreError::Unavailable)?;
                        assert_eq!(metas, 4, "the schema was created exactly once");
                        Ok(())
                    })
                    .unwrap();
            }
            let left = names(dir.path());
            assert!(
                left.iter()
                    .all(|n| !n.contains(".bak-") && !n.ends_with(".lock")),
                "round {round}: {left:?}"
            );
        }
    }

    /// P2-6: an opener whose pre-read said "old" but who finds the store already current under
    /// the lock does nothing at all: no backup, no second CREATE TABLE.
    #[test]
    fn p2_6_a_stale_version_hint_under_the_lock_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path(), "0.2.0");
        let conn = Connection::open(dir.path().join("automation.db")).unwrap();
        // The caller still believes the store is at version 0.
        migrate::run(&conn, dir.path(), 0, "0.2.0").unwrap();
        assert!(names(dir.path()).iter().all(|n| !n.contains(".bak-")));
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='profiles'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    /// P3 item 1: a hard link on the database file makes the store untrusted.
    #[cfg(windows)]
    #[test]
    fn a_hard_linked_database_file_is_untrusted() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("automation");
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "0.2.0");
        std::fs::hard_link(dir.join("automation.db"), base.path().join("elsewhere.db")).unwrap();
        assert_eq!(
            Store::open(&dir, "0.2.0").check_writable().unwrap_err(),
            StoreError::Untrusted
        );
    }

    /// P3 item 1: the WAL/SHM sidecars and a migration backup as reparse points are untrusted too.
    #[cfg(windows)]
    #[test]
    fn reparse_sidecars_and_backups_are_untrusted() {
        for name in [
            "automation.db-wal",
            "automation.db-shm",
            "automation.db.bak-v0",
            migrate::LOCK_FILE,
        ] {
            let base = tempfile::tempdir().unwrap();
            let dir = base.path().join("automation");
            std::fs::create_dir_all(&dir).unwrap();
            let target = base.path().join("elsewhere");
            std::fs::create_dir_all(&target).unwrap();
            assert!(junction(&dir.join(name), &target), "junction {name}");
            assert_eq!(
                Store::open(&dir, "0.2.0").check_writable().unwrap_err(),
                StoreError::Untrusted,
                "{name}"
            );
        }
    }

    /// P3 item 2: an existing ancestor that is a junction is rejected BEFORE anything is created,
    /// so no folder appears inside the junction's target.
    #[cfg(windows)]
    #[test]
    fn nothing_is_created_below_a_junction_ancestor() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.path().join("via-junction");
        assert!(junction(&link, &real), "junction created");
        let store = Store::open(&link.join("deeper").join("automation"), "0.2.0");
        assert_eq!(store.check_writable().unwrap_err(), StoreError::Untrusted);
        assert!(
            std::fs::read_dir(&real).unwrap().next().is_none(),
            "the junction target stays empty"
        );
    }

    /// P3 item 8 + LC-3: on a newer-schema store the connection itself refuses writes, so even a
    /// closure that went through `with_read` cannot change anything.
    #[test]
    fn a_read_only_store_connection_cannot_write_at_all() {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path(), "0.2.0");
        {
            let conn = Connection::open(dir.path().join("automation.db")).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        let store = Store::open(dir.path(), "0.2.0");
        assert_eq!(store.check_writable().unwrap_err(), StoreError::ReadOnly);
        let wrote = store.with_read(|conn| -> Result<usize, StoreError> {
            Ok(conn
                .execute("INSERT INTO meta(key, value) VALUES ('sneaky', '1')", [])
                .unwrap_or(0))
        });
        assert_eq!(
            wrote.unwrap(),
            0,
            "the insert was refused by the connection"
        );
    }
}
