//! The `hosts` table (Automation design §4.3): one row per process (app or runner), heartbeat
//! based liveness (§3.3). The table ships with schema v1; P0.4 builds the cross-process lease
//! reclaim on top of it.

use rusqlite::{Connection, params};
use serde::Serialize;

use super::StoreError;

/// A host row is live when its heartbeat is at most this old (design §3.3: 15 s, beats every 5 s).
pub const LIVE_WINDOW_MS: i64 = 15_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRow {
    /// `app` or `runner` (checked by the schema).
    pub kind: String,
    pub pid: i64,
    /// Process creation time as FILETIME — `(pid, created)` names a process unambiguously (§3.3).
    pub created: i64,
    pub version: String,
    pub schema_version: i64,
    pub session_id: i64,
    pub last_beat_ms: i64,
}

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// Insert or refresh this process's own host row (heartbeat).
pub fn beat(conn: &Connection, row: &HostRow) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO hosts(kind, pid, created, version, schema_version, session_id, last_beat_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(kind) DO UPDATE SET
           pid = ?2, created = ?3, version = ?4, schema_version = ?5,
           session_id = ?6, last_beat_ms = ?7",
        params![
            row.kind,
            row.pid,
            row.created,
            row.version,
            row.schema_version,
            row.session_id,
            row.last_beat_ms
        ],
    )
    .map(|_| ())
    .map_err(unavailable)
}

pub fn all(conn: &Connection) -> Result<Vec<HostRow>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT kind, pid, created, version, schema_version, session_id, last_beat_ms
             FROM hosts ORDER BY kind",
        )
        .map_err(unavailable)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(HostRow {
                kind: row.get(0)?,
                pid: row.get(1)?,
                created: row.get(2)?,
                version: row.get(3)?,
                schema_version: row.get(4)?,
                session_id: row.get(5)?,
                last_beat_ms: row.get(6)?,
            })
        })
        .map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// Hosts whose heartbeat is still fresh (`now_ms - last_beat_ms <= LIVE_WINDOW_MS`).
pub fn live(conn: &Connection, now_ms: i64) -> Result<Vec<HostRow>, StoreError> {
    Ok(all(conn)?
        .into_iter()
        .filter(|h| now_ms - h.last_beat_ms <= LIVE_WINDOW_MS)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::store::Store;

    fn row(kind: &str, beat_ms: i64) -> HostRow {
        HostRow {
            kind: kind.into(),
            pid: 4242,
            created: 1,
            version: "0.2.0".into(),
            schema_version: 1,
            session_id: 7,
            last_beat_ms: beat_ms,
        }
    }

    #[test]
    fn heartbeat_upserts_and_liveness_expires() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        store
            .with_read(|conn| -> Result<(), StoreError> {
                beat(conn, &row("app", 1_000))?;
                beat(conn, &row("app", 2_000))?; // refresh wins, no duplicate row
                beat(conn, &row("runner", 1_000))?;
                let hosts = all(conn)?;
                assert_eq!(hosts.len(), 2);
                assert_eq!(
                    hosts.iter().find(|h| h.kind == "app").unwrap().last_beat_ms,
                    2_000
                );
                // Live window (15 s).
                assert_eq!(live(conn, 17_000)?.len(), 1);
                assert_eq!(live(conn, 17_000)?[0].kind, "app");
                assert_eq!(live(conn, 2_000)?.len(), 2);
                Ok(())
            })
            .unwrap();
    }
}
