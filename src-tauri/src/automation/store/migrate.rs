//! Forward-only store migrations (Automation design §4.5, LC-5).
//!
//! A migration runs under `BEGIN EXCLUSIVE`, takes a backup of the previous schema file first
//! (`automation.db.bak-v<from>`, after a `wal_checkpoint(TRUNCATE)` so the copy is complete),
//! applies the steps `from+1..=SCHEMA_VERSION`, and deletes the backup only on success. On
//! failure the transaction rolls back, the backup stays and the original store is untouched.

use std::path::Path;

use rusqlite::{Connection, params};

use super::StoreError;

/// Schema version this binary was compiled with. Version 1 ships with the store foundation;
/// later packages bump it and add one step each.
pub const SCHEMA_VERSION: i64 = 1;

/// Migrate `from` → [`SCHEMA_VERSION`]. A no-op when already current.
pub fn run(conn: &Connection, dir: &Path, from: i64, app_version: &str) -> Result<(), StoreError> {
    run_with_failure_at(conn, dir, from, app_version, None)
}

/// Core migration; `fail_at` injects a failure right before the given schema step is applied
/// (LC-5 backup test).
pub fn run_with_failure_at(
    conn: &Connection,
    dir: &Path,
    from: i64,
    app_version: &str,
    fail_at: Option<i64>,
) -> Result<(), StoreError> {
    if from >= SCHEMA_VERSION {
        return Ok(());
    }
    let db = dir.join("automation.db");
    let backup = dir.join(format!("automation.db.bak-v{from}"));
    // The backup must not carry un-checkpointed WAL frames.
    let _: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap_or(0);
    std::fs::copy(&db, &backup).map_err(|_| StoreError::Unavailable)?;

    conn.execute_batch("BEGIN EXCLUSIVE")
        .map_err(|_| StoreError::Unavailable)?;
    let outcome = (from + 1..=SCHEMA_VERSION).try_for_each(|version| {
        if fail_at == Some(version) {
            return Err(StoreError::Unavailable);
        }
        apply_step(conn, version, app_version)?;
        conn.pragma_update(None, "user_version", version)
            .map_err(|_| StoreError::Unavailable)
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
