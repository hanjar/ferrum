//! `approvals` (Automation design §4.3, §8): one row per automation holding the policy hash, the
//! profile revision it covers, the configuration revision and the MAC. Verification lives in
//! `automation::policy`; this file is plain storage.

use rusqlite::{Connection, OptionalExtension, params};

use super::StoreError;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub automation_id: String,
    pub hash_version: i64,
    pub policy_hash: String,
    pub profile_id: String,
    pub profile_revision: i64,
    pub config_rev: i64,
    pub approved_ms: i64,
    pub mac: String,
}

pub fn get(conn: &Connection, automation_id: &str) -> Result<Option<ApprovalRecord>, StoreError> {
    conn.query_row(
        "SELECT automation_id, hash_version, policy_hash, profile_id, profile_revision,
                config_rev, approved_ms, mac
         FROM approvals WHERE automation_id = ?1",
        [automation_id],
        |row| {
            Ok(ApprovalRecord {
                automation_id: row.get(0)?,
                hash_version: row.get(1)?,
                policy_hash: row.get(2)?,
                profile_id: row.get(3)?,
                profile_revision: row.get(4)?,
                config_rev: row.get(5)?,
                approved_ms: row.get(6)?,
                mac: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(unavailable)
}

/// Replace the approval of an automation (one row per automation).
pub fn put(conn: &Connection, a: &ApprovalRecord) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO approvals(automation_id, hash_version, policy_hash, profile_id,
                               profile_revision, config_rev, approved_ms, mac)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(automation_id) DO UPDATE SET
           hash_version = ?2, policy_hash = ?3, profile_id = ?4, profile_revision = ?5,
           config_rev = ?6, approved_ms = ?7, mac = ?8",
        params![
            a.automation_id,
            a.hash_version,
            a.policy_hash,
            a.profile_id,
            a.profile_revision,
            a.config_rev,
            a.approved_ms,
            a.mac
        ],
    )
    .map(|_| ())
    .map_err(unavailable)
}
