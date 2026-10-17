//! The `hosts` table (Automation design §4.3): one row per host KIND (`app` or `runner`), heartbeat
//! based liveness (§3.3), and the election built on it (FER-524, RT-4): the first process that
//! claims a kind hosts; every other process of that kind only reads and sends controls.
//!
//! Election rules (all inside one `BEGIN IMMEDIATE` transaction, so two processes cannot both
//! win):
//!
//! - no row, or the row is this very process: it hosts (the row is written or refreshed);
//! - another process's row is **live** when its last beat is at most [`LIVE_WINDOW_MS`] old (a
//!   beat far in the future counts as damaged, not live) AND the process `(pid, creation time)`
//!   still exists (the injected probe): the claim is refused;
//! - otherwise (stale beat, or the process is gone) the claimant takes the row over. The previous
//!   host cannot beat any more without noticing: its next [`claim`] answers [`Claim::Held`].

use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

use super::StoreError;
use super::leases::{Holder, ProcessProbe};

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

/// A beat this far ahead of the reader's clock is a damaged or forged row, not a live host.
pub const FUTURE_SKEW_MS: i64 = 60_000;

/// The host kinds the schema allows.
pub const KIND_APP: &str = "app";
pub const KIND_RUNNER: &str = "runner";

/// What a [`claim`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// This process already hosted; its beat was refreshed.
    Refreshed,
    /// This process is the host now. `previous` is the row it replaced (a dead or stale host),
    /// `None` for a first claim.
    Acquired { previous: Option<HostRow> },
    /// Another live process hosts.
    Held { by: Holder },
}

/// Whether `row` still counts as a live host at `now_ms`: a fresh beat AND an existing process.
pub fn is_live(row: &HostRow, now_ms: i64, probe: &dyn ProcessProbe) -> bool {
    let age = now_ms.saturating_sub(row.last_beat_ms);
    let fresh = (-FUTURE_SKEW_MS..=LIVE_WINDOW_MS).contains(&age);
    fresh
        && probe.is_alive(Holder {
            pid: row.pid,
            created: row.created,
        })
}

/// Claim (or keep) the host role of `row.kind` for the process named by `row`. One immediate
/// transaction; the row is written with `row.last_beat_ms` as the beat.
pub fn claim(
    conn: &mut Connection,
    row: &HostRow,
    probe: &dyn ProcessProbe,
) -> Result<Claim, StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let current = all(&tx)?.into_iter().find(|h| h.kind == row.kind);
    let outcome = match &current {
        Some(cur) if cur.pid == row.pid && cur.created == row.created => Claim::Refreshed,
        Some(cur) if is_live(cur, row.last_beat_ms, probe) => {
            // Another live process hosts: nothing is written.
            return Ok(Claim::Held {
                by: Holder {
                    pid: cur.pid,
                    created: cur.created,
                },
            });
        }
        previous => Claim::Acquired {
            previous: previous.clone(),
        },
    };
    beat(&tx, row)?;
    tx.commit().map_err(unavailable)?;
    Ok(outcome)
}

/// Give the host role up (clean exit). Only the row's own process can: a row that names another
/// process is left alone. Returns whether a row was removed.
pub fn release(conn: &Connection, kind: &str, holder: Holder) -> Result<bool, StoreError> {
    conn.execute(
        "DELETE FROM hosts WHERE kind = ?1 AND pid = ?2 AND created = ?3",
        params![kind, holder.pid, holder.created],
    )
    .map(|n| n > 0)
    .map_err(unavailable)
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

    // ── Election (FER-524, RT-4 / LC-6) ──

    use crate::automation::store::leases::test_support::FakeProbe;

    fn me(pid: i64, beat: i64) -> HostRow {
        HostRow {
            pid,
            created: pid * 10,
            ..row("app", beat)
        }
    }

    fn holder(pid: i64) -> Holder {
        Holder {
            pid,
            created: pid * 10,
        }
    }

    fn claim_in(store: &Store, row: &HostRow, probe: &FakeProbe) -> Claim {
        store.with_write(|conn| claim(conn, row, probe)).unwrap()
    }

    #[test]
    fn only_one_process_hosts_and_a_live_host_is_not_displaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        let probe = FakeProbe::default();
        assert_eq!(
            claim_in(&store, &me(1, 1_000), &probe),
            Claim::Acquired { previous: None }
        );
        // The second instance is refused and writes nothing.
        assert_eq!(
            claim_in(&store, &me(2, 2_000), &probe),
            Claim::Held { by: holder(1) }
        );
        let rows = store.with_read(all).unwrap();
        assert_eq!((rows.len(), rows[0].pid), (1, 1));
        // The host refreshes its own beat.
        assert_eq!(claim_in(&store, &me(1, 6_000), &probe), Claim::Refreshed);
        // Still refused 14 s after the last beat.
        assert_eq!(
            claim_in(&store, &me(2, 6_000 + 14_000), &probe),
            Claim::Held { by: holder(1) }
        );
    }

    /// Killing the host (the row is left behind, no release): the other instance takes over once
    /// the beat is older than 15 s.
    #[test]
    fn a_host_that_stopped_beating_is_replaced_after_fifteen_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        let probe = FakeProbe::default();
        claim_in(&store, &me(1, 1_000), &probe);
        assert!(matches!(
            claim_in(&store, &me(2, 1_000 + 15_000), &probe),
            Claim::Held { .. }
        ));
        let taken = claim_in(&store, &me(2, 1_000 + 15_001), &probe);
        assert!(
            matches!(&taken, Claim::Acquired { previous: Some(p) } if p.pid == 1),
            "{taken:?}"
        );
        // The old host, if it wakes up, is told it lost the role.
        assert_eq!(
            claim_in(&store, &me(1, 1_000 + 16_000), &probe),
            Claim::Held { by: holder(2) }
        );
    }

    /// A host whose process is gone is replaced at once, however fresh its last beat is.
    #[test]
    fn a_dead_process_is_replaced_without_waiting_for_the_beat() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        let probe = FakeProbe::default();
        claim_in(&store, &me(1, 1_000), &probe);
        probe.kill(holder(1));
        assert!(matches!(
            claim_in(&store, &me(2, 2_000), &probe),
            Claim::Acquired { previous: Some(_) }
        ));
    }

    #[test]
    fn a_beat_from_the_far_future_is_damage_not_a_live_host() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        let probe = FakeProbe::default();
        claim_in(&store, &me(1, 1_000_000_000), &probe);
        assert!(matches!(
            claim_in(&store, &me(2, 1_000), &probe),
            Claim::Acquired { .. }
        ));
    }

    #[test]
    fn release_removes_only_the_own_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        let probe = FakeProbe::default();
        claim_in(&store, &me(1, 1_000), &probe);
        assert!(!store.with_read(|c| release(c, "app", holder(2))).unwrap());
        assert!(store.with_read(|c| release(c, "app", holder(1))).unwrap());
        assert!(store.with_read(all).unwrap().is_empty());
        // After a clean release anyone can host at once.
        assert!(matches!(
            claim_in(&store, &me(2, 1_001), &probe),
            Claim::Acquired { previous: None }
        ));
    }
}
