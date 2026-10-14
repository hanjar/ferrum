//! Forward-only store migrations (Automation design §4.5, LC-5).
//!
//! Openers are serialized by an exclusive lock FILE (`automation.migrate.lock`, held for the
//! whole check-backup-migrate sequence, so two processes opening a fresh store take turns). Under
//! that lock the schema version is read AGAIN: an opener that lost the race finds the store
//! current and does nothing (no backup, no second `CREATE TABLE`). Only a real migration takes a
//! backup of the previous schema (`automation.db.bak-v<from>`, a consistent `VACUUM INTO`
//! snapshot), applies the steps `from+1..=SCHEMA_VERSION` inside `BEGIN EXCLUSIVE` (version
//! re-read once more inside it), and deletes the backup only on success. On failure the
//! transaction rolls back, the backup stays and the original store is untouched.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use super::StoreError;

/// Schema version this binary was compiled with. Version 1 ships with the store foundation;
/// later packages bump it and add one step each.
pub const SCHEMA_VERSION: i64 = 1;

/// Name of the migration lock file inside the store directory.
pub const LOCK_FILE: &str = "automation.migrate.lock";

/// How long an opener waits for another opener's migration.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Migrate to [`SCHEMA_VERSION`]. `hint_from` is the version the caller saw before locking; the
/// migration re-reads the real version under its lock. A no-op when already current.
pub fn run(
    conn: &Connection,
    dir: &Path,
    hint_from: i64,
    app_version: &str,
) -> Result<(), StoreError> {
    run_inner(conn, dir, hint_from, app_version, None)
}

/// Core migration with a failure injected right before the given schema step (LC-5 backup test).
#[cfg(test)]
pub(super) fn run_with_failure_at(
    conn: &Connection,
    dir: &Path,
    hint_from: i64,
    app_version: &str,
    fail_at: Option<i64>,
) -> Result<(), StoreError> {
    run_inner(conn, dir, hint_from, app_version, fail_at)
}

fn user_version(conn: &Connection) -> Result<i64, StoreError> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| StoreError::Unavailable)
}

fn run_inner(
    conn: &Connection,
    dir: &Path,
    hint_from: i64,
    app_version: &str,
    fail_at: Option<i64>,
) -> Result<(), StoreError> {
    if hint_from >= SCHEMA_VERSION {
        return Ok(());
    }
    let _lock = MigrationLock::acquire(dir)?;
    // Under the lock: the version another opener may just have written.
    let from = user_version(conn)?;
    if from >= SCHEMA_VERSION {
        return Ok(());
    }
    let backup = dir.join(format!("automation.db.bak-v{from}"));
    take_backup(conn, &backup)?;

    conn.execute_batch("BEGIN EXCLUSIVE")
        .map_err(|_| StoreError::Unavailable)?;
    let outcome = user_version(conn).and_then(|inside| {
        if inside != from {
            // Someone changed the schema between the snapshot and the lock: never migrate on a
            // stale view. The caller's next open sees the new state.
            return Err(StoreError::Unavailable);
        }
        (from + 1..=SCHEMA_VERSION).try_for_each(|version| {
            if fail_at == Some(version) {
                return Err(StoreError::Unavailable);
            }
            apply_step(conn, version, app_version)?;
            conn.pragma_update(None, "user_version", version)
                .map_err(|_| StoreError::Unavailable)
        })
    });
    match outcome {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .map_err(|_| StoreError::Unavailable)?;
            // Success only now: the previous schema backup goes away (LC-5).
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            // The backup stays and the original is intact (LC-5).
            Err(e)
        }
    }
}

/// A consistent snapshot of the committed state (`VACUUM INTO` cannot run in a transaction and
/// refuses an existing target, so a leftover backup of an earlier failed run is replaced).
fn take_backup(conn: &Connection, backup: &Path) -> Result<(), StoreError> {
    super::ensure_plain_file(backup)?;
    let _ = std::fs::remove_file(backup);
    let target: PathBuf = backup.to_path_buf();
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
        .map(|_| ())
        .map_err(|_| StoreError::Unavailable)
}

/// Exclusive lock file for the migration sequence; deleted when released.
struct MigrationLock {
    _file: Option<std::fs::File>,
}

impl MigrationLock {
    #[cfg(windows)]
    fn acquire(dir: &Path) -> Result<Self, StoreError> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let path = dir.join(LOCK_FILE);
        super::ensure_plain_file(&path)?;
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            // share_mode(0): while one opener holds the handle no other can open the file.
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .share_mode(0)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
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
                Err(_) => return Err(StoreError::Unavailable),
            }
        }
    }

    #[cfg(not(windows))]
    fn acquire(_dir: &Path) -> Result<Self, StoreError> {
        Ok(Self { _file: None })
    }
}

fn apply_step(conn: &Connection, version: i64, app_version: &str) -> Result<(), StoreError> {
    match version {
        1 => {
            conn.execute_batch(V1)
                .map_err(|_| StoreError::Unavailable)?;
            for (key, value) in [
                ("schema_version", "1".to_string()),
                ("created_by_version", app_version.to_string()),
                ("min_writer_version", app_version.to_string()),
                ("change_seq", "0".to_string()),
            ] {
                conn.execute(
                    "INSERT INTO meta(key, value) VALUES (?1, ?2)",
                    params![key, value],
                )
                .map_err(|_| StoreError::Unavailable)?;
            }
            Ok(())
        }
        // Later packages add `n => ...` steps here.
        _ => Err(StoreError::Unavailable),
    }
}

/// Schema v1 (Automation design §4.3), verbatim. All timestamps are Unix epoch milliseconds;
/// `leases` is created now but only used from P0.4 on.
const V1: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE profiles(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  name_key TEXT NOT NULL UNIQUE,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  policy TEXT NOT NULL,
  policy_digest TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL);

CREATE TABLE profile_revisions(
  profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL,
  policy TEXT NOT NULL,
  changes TEXT NOT NULL,
  changed_ms INTEGER NOT NULL,
  PRIMARY KEY(profile_id, revision));

CREATE TABLE leases(
  id TEXT PRIMARY KEY,
  holder_kind TEXT NOT NULL,
  holder_pid INTEGER NOT NULL,
  holder_created INTEGER NOT NULL,
  run_id INTEGER,
  roots TEXT NOT NULL,
  acquired_ms INTEGER NOT NULL,
  beat_ms INTEGER NOT NULL);

CREATE TABLE hosts(
  kind TEXT PRIMARY KEY CHECK(kind IN ('app','runner')),
  pid INTEGER NOT NULL,
  created INTEGER NOT NULL,
  version TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  session_id INTEGER NOT NULL,
  last_beat_ms INTEGER NOT NULL);
";
