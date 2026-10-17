//! `profiles` and `profile_revisions` (Automation design §4.3): CRUD with optimistic
//! concurrency (SP-6.2), revision bookkeeping (SP-6.1/SP-6.3) and the SP-6.4 dependants hook.
//! Multi-row changes run in one `BEGIN IMMEDIATE` transaction.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::StoreError;

/// Revisions kept per profile, newest first (SP-6.3: at least the last 50).
pub const REVISIONS_KEPT: i64 = 50;

/// One stored profile; `policy` is the JSON form of `ProfilePolicy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRecord {
    pub id: String,
    pub name: String,
    pub revision: i64,
    pub policy: String,
    pub digest: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// One stored revision of a profile; `policy` is JSON `ProfilePolicy`, `changes` is JSON
/// `[{field, from, to}]` with display values (SP-6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionRecord {
    pub revision: i64,
    pub policy: String,
    pub changes: String,
    pub changed_ms: i64,
}

pub struct NewProfile {
    pub id: String,
    pub name: String,
    pub policy: String,
    pub digest: String,
    pub now_ms: i64,
}

pub struct UpdateProfile {
    pub id: String,
    /// The revision the editor loaded (SP-6.2).
    pub expected_revision: i64,
    pub name: String,
    pub policy: String,
    pub digest: String,
    /// JSON `[{field, from, to}]` display values of this policy change.
    pub changes: String,
    /// True when a policy field changed (SP-6.1); a rename alone never bumps the revision.
    pub policy_changed: bool,
    pub now_ms: i64,
}

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<ProfileRecord> {
    Ok(ProfileRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        revision: row.get(2)?,
        policy: row.get(3)?,
        digest: row.get(4)?,
        created_ms: row.get(5)?,
        updated_ms: row.get(6)?,
    })
}

const COLUMNS: &str = "id, name, revision, policy, policy_digest, created_ms, updated_ms";

pub fn list(conn: &Connection) -> Result<Vec<ProfileRecord>, StoreError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM profiles ORDER BY name COLLATE NOCASE, id"
        ))
        .map_err(unavailable)?;
    let rows = stmt.query_map([], row_to_record).map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<ProfileRecord>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM profiles WHERE id = ?1"),
        [id],
        row_to_record,
    )
    .optional()
    .map_err(unavailable)
}

pub fn revisions(conn: &Connection, profile_id: &str) -> Result<Vec<RevisionRecord>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT revision, policy, changes, changed_ms FROM profile_revisions
             WHERE profile_id = ?1 ORDER BY revision DESC",
        )
        .map_err(unavailable)?;
    let rows = stmt
        .query_map([profile_id], |row| {
            Ok(RevisionRecord {
                revision: row.get(0)?,
                policy: row.get(1)?,
                changes: row.get(2)?,
                changed_ms: row.get(3)?,
            })
        })
        .map_err(unavailable)?;
    rows.map(|r| r.map_err(unavailable)).collect()
}

/// Uniqueness of the trimmed name, case-insensitive (SP-1.3), optionally excluding one profile
/// (itself during an update).
fn name_taken(conn: &Connection, name: &str, exclude_id: Option<&str>) -> Result<bool, StoreError> {
    let key = name.to_lowercase();
    let found: Option<String> = conn
        .query_row(
            "SELECT id FROM profiles WHERE name_key = ?1 AND (?2 IS NULL OR id != ?2) LIMIT 1",
            params![key, exclude_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(found.is_some())
}

pub fn create(conn: &mut Connection, new: &NewProfile) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    if name_taken(&tx, &new.name, None)? {
        return Err(StoreError::NameTaken);
    }
    let key = new.name.to_lowercase();
    tx.execute(
        "INSERT INTO profiles(id, name, name_key, revision, policy, policy_digest, created_ms, updated_ms)
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?6)",
        params![
            new.id,
            new.name,
            key,
            new.policy,
            new.digest,
            new.now_ms
        ],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)
}

/// Save a changed profile in one transaction: optimistic concurrency (SP-6.2), name uniqueness,
/// a new `profile_revisions` row only when a policy field changed (SP-6.1), pruned to the newest
/// 50 (SP-6.3), and the SP-6.4 dependants hook in the same transaction. Returns the revision now
/// stored.
pub fn update(conn: &mut Connection, up: &UpdateProfile) -> Result<i64, StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let current: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision, name_key FROM profiles WHERE id = ?1",
            [&up.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((revision, name_key)) = current else {
        return Err(StoreError::NotFound);
    };
    if revision != up.expected_revision {
        return Err(StoreError::Stale);
    }
    let key = up.name.to_lowercase();
    if key != name_key && name_taken(&tx, &up.name, Some(&up.id))? {
        return Err(StoreError::NameTaken);
    }
    let new_revision = if up.policy_changed {
        revision + 1
    } else {
        revision
    };
    if up.policy_changed {
        tx.execute(
            "INSERT INTO profile_revisions(profile_id, revision, policy, changes, changed_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![up.id, new_revision, up.policy, up.changes, up.now_ms],
        )
        .map_err(unavailable)?;
        // SP-6.3: keep the newest 50 revisions per profile.
        tx.execute(
            "DELETE FROM profile_revisions WHERE profile_id = ?1 AND revision <= ?2",
            params![up.id, new_revision - REVISIONS_KEPT],
        )
        .map_err(unavailable)?;
    }
    tx.execute(
        "UPDATE profiles SET name = ?2, name_key = ?3, revision = ?4, policy = ?5,
         policy_digest = ?6, updated_ms = ?7 WHERE id = ?1",
        params![
            up.id,
            up.name,
            key,
            new_revision,
            up.policy,
            up.digest,
            up.now_ms
        ],
    )
    .map_err(unavailable)?;
    // SP-6.4: everything that runs this profile becomes Needs review, in this same transaction -
    // only for a policy change; a rename alone must not disturb the automations using it.
    if up.policy_changed {
        mark_dependants_needs_review(&tx, &up.id, new_revision, &up.changes, up.now_ms)?;
    }
    tx.commit().map_err(unavailable)?;
    Ok(new_revision)
}

pub fn delete(conn: &mut Connection, id: &str, expected_revision: i64) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let revision: Option<i64> = tx
        .query_row("SELECT revision FROM profiles WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(unavailable)?;
    let Some(revision) = revision else {
        return Err(StoreError::NotFound);
    };
    if revision != expected_revision {
        return Err(StoreError::Stale);
    }
    if profile_in_use(&tx, id)? {
        return Err(StoreError::InUse);
    }
    // profile_revisions rows go with the profile (ON DELETE CASCADE).
    tx.execute("DELETE FROM profiles WHERE id = ?1", [id])
        .map_err(unavailable)?;
    tx.commit().map_err(unavailable)
}

/// SP-9.5: a profile an automation still uses cannot be deleted (the foreign key refuses it as
/// well, but this check gives the closed `in_use` answer first).
pub fn profile_in_use(conn: &Connection, id: &str) -> Result<bool, StoreError> {
    Ok(super::automations::count_for_profile(conn, id)? > 0)
}

/// How many automations use this profile (list DTO `usedBy`).
pub fn used_by(conn: &Connection, profile_id: &str) -> Result<i64, StoreError> {
    super::automations::count_for_profile(conn, profile_id)
}

/// SP-6.4, invoked inside the save transaction right after a policy-changing revision: every
/// automation using this profile becomes Needs review, its queued runs are removed, its watch
/// state resets, and one event is logged per automation. "Needs review" itself is not a flag:
/// `automation::policy::verify` reads the new profile revision against each approval, so the
/// automation reports `profileChanged{from, to}` the moment this transaction commits. A run that
/// is already in progress finishes under the revision it started with (it is not touched).
/// Returns how many automations were affected.
pub fn mark_dependants_needs_review(
    conn: &Connection,
    profile_id: &str,
    new_revision: i64,
    changes: &str,
    now_ms: i64,
) -> Result<usize, StoreError> {
    use super::automations::{self, EventKind};
    #[cfg(test)]
    DEPENDANTS_HOOK_CALLS.with(|c| c.set(c.get() + 1));
    let mut stmt = conn
        .prepare("SELECT id FROM automations WHERE profile_id = ?1 ORDER BY id")
        .map_err(unavailable)?;
    let ids: Vec<String> = stmt
        .query_map([profile_id], |row| row.get(0))
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    drop(stmt);
    // Which fields changed, for the event line ("Profile changed to rev 4 (delete policy)").
    let fields = changed_field_labels(changes);
    for id in &ids {
        automations::clear_queue(conn, id)?;
        automations::reset_watch_state(conn, id, now_ms)?;
        let what = if fields.is_empty() {
            String::new()
        } else {
            format!(" ({fields})")
        };
        automations::add_event(
            conn,
            id,
            now_ms,
            EventKind::Warn,
            &format!("Profile changed to rev {new_revision}{what} \u{2014} held until you review"),
        )?;
    }
    if !ids.is_empty() {
        automations::bump_change_seq(conn)?;
    }
    Ok(ids.len())
}

/// `deletePolicy, mode` -> "delete policy, direction" (at most 4 labels).
fn changed_field_labels(changes: &str) -> String {
    let parsed: Vec<crate::automation::model::FieldChange> =
        serde_json::from_str(changes).unwrap_or_default();
    let mut labels: Vec<&str> = Vec::new();
    for change in &parsed {
        let label = crate::automation::model::field_label(&change.field);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels.truncate(4);
    labels.join(", ")
}

#[cfg(test)]
thread_local! {
    static DEPENDANTS_HOOK_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test probe: how often the SP-6.4 hook ran on this thread since the last reset.
#[cfg(test)]
pub fn dependants_hook_calls() -> usize {
    DEPENDANTS_HOOK_CALLS.with(|c| c.get())
}

#[cfg(test)]
pub fn reset_dependants_hook_calls() {
    DEPENDANTS_HOOK_CALLS.with(|c| c.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::store::Store;

    fn full_policy(right: &str) -> String {
        let root = |p: &str| {
            format!(
                r#"{{"path":"{p}","identity":{{"kind":"local","volumeSerial":"00000001","fileId":"aa","fsName":"NTFS","drive":"fixed"}}}}"#
            )
        };
        format!(
            r#"{{"left":{l},"right":{r},"mode":{{"kind":"oneWay","from":"left"}},"deletePolicy":{{"kind":"recycleBin"}},"compare":"sizeAndDate","filters":{{"include":[],"exclude":[]}},"guard":{{"enabled":true,"maxCount":50,"maxPercent":20}}}}"#,
            l = root(r"C:\a"),
            r = root(right)
        )
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "0.2.0");
        (dir, store)
    }

    fn create_profile(store: &Store, id: &str, name: &str, policy: &str) {
        store
            .with_write(|conn| {
                create(
                    conn,
                    &NewProfile {
                        id: id.into(),
                        name: name.into(),
                        policy: policy.into(),
                        digest: "d".into(),
                        now_ms: 1_000,
                    },
                )
            })
            .unwrap();
    }

    fn save_policy(
        store: &Store,
        id: &str,
        expected: i64,
        name: &str,
        policy: &str,
        changes: &str,
    ) -> Result<i64, StoreError> {
        store.with_write(|conn| {
            update(
                conn,
                &UpdateProfile {
                    id: id.into(),
                    expected_revision: expected,
                    name: name.into(),
                    policy: policy.into(),
                    digest: "d2".into(),
                    changes: changes.into(),
                    policy_changed: true,
                    now_ms: 2_000,
                },
            )
        })
    }

    fn stored(store: &Store, id: &str) -> ProfileRecord {
        store.with_read(|conn| get(conn, id)).unwrap().unwrap()
    }

    /// SP-6.1: the revision starts at 1 and increases by exactly 1 per policy-changing save; a
    /// rename alone keeps the revision.
    #[test]
    fn sp_6_1_revision_bumps_only_on_policy_fields() {
        let (_dir, store) = store();
        create_profile(&store, "id-1", "P", &full_policy(r"D:\b"));
        assert_eq!(stored(&store, "id-1").revision, 1);
        // Rename only: no bump, name and key change.
        store
            .with_write(|conn| {
                update(
                    conn,
                    &UpdateProfile {
                        id: "id-1".into(),
                        expected_revision: 1,
                        name: "Renamed".into(),
                        policy: full_policy(r"D:\b"),
                        digest: "d".into(),
                        changes: "[]".into(),
                        policy_changed: false,
                        now_ms: 1_500,
                    },
                )
            })
            .unwrap();
        let rec = stored(&store, "id-1");
        assert_eq!(rec.revision, 1);
        assert_eq!(rec.name, "Renamed");
        // Each policy change: exactly +1.
        assert_eq!(
            save_policy(
                &store,
                "id-1",
                1,
                "Renamed",
                &full_policy(r"E:\b"),
                r#"[{"field":"rightRoot","from":"D:\\b","to":"E:\\b"}]"#
            )
            .unwrap(),
            2
        );
        assert_eq!(
            save_policy(&store, "id-1", 2, "Renamed", &full_policy(r"F:\b"), "[]").unwrap(),
            3
        );
    }

    /// SP-6.2: a save with the wrong `expectedRevision` is `stale` and changes nothing.
    #[test]
    fn sp_6_2_stale_on_expected_revision_mismatch() {
        let (_dir, store) = store();
        create_profile(&store, "id-1", "P", &full_policy(r"D:\b"));
        let err = save_policy(&store, "id-1", 99, "P", &full_policy(r"E:\b"), "[]").unwrap_err();
        assert_eq!(err, StoreError::Stale);
        assert_eq!(stored(&store, "id-1").revision, 1);
        assert_eq!(stored(&store, "id-1").policy, full_policy(r"D:\b"));
    }

    /// SP-1.3: names are unique case-insensitively, on create and on rename.
    #[test]
    fn sp_1_3_names_are_unique_case_insensitively() {
        let (_dir, store) = store();
        create_profile(&store, "id-1", "Backup", &full_policy(r"D:\b"));
        let err = store
            .with_write(|conn| {
                create(
                    conn,
                    &NewProfile {
                        id: "id-2".into(),
                        name: "BACKUP".into(),
                        policy: full_policy(r"E:\b"),
                        digest: "d".into(),
                        now_ms: 1,
                    },
                )
            })
            .unwrap_err();
        assert_eq!(err, StoreError::NameTaken);
        // Renaming id-2 to a fresh name works; renaming to the taken name is refused.
        create_profile(&store, "id-2", "Other", &full_policy(r"E:\b"));
        let err = store
            .with_write(|conn| {
                update(
                    conn,
                    &UpdateProfile {
                        id: "id-2".into(),
                        expected_revision: 1,
                        name: "backup".into(),
                        policy: full_policy(r"E:\b"),
                        digest: "d".into(),
                        changes: "[]".into(),
                        policy_changed: false,
                        now_ms: 1,
                    },
                )
            })
            .unwrap_err();
        assert_eq!(err, StoreError::NameTaken);
        // Renaming to itself (same profile) is fine.
        store
            .with_write(|conn| {
                update(
                    conn,
                    &UpdateProfile {
                        id: "id-2".into(),
                        expected_revision: 1,
                        name: "Other".into(),
                        policy: full_policy(r"E:\b"),
                        digest: "d".into(),
                        changes: "[]".into(),
                        policy_changed: false,
                        now_ms: 1,
                    },
                )
            })
            .unwrap();
    }

    /// SP-6.3: every policy revision stores its from → to display values and the list is pruned
    /// to the newest 50.
    #[test]
    fn sp_6_3_changes_list_is_kept_and_pruned_to_fifty() {
        let (_dir, store) = store();
        create_profile(&store, "id-1", "P", &full_policy(r"D:\b"));
        for i in 0..55 {
            let expected = 1 + i;
            let next = format!(r"D:\b{i}");
            save_policy(
                &store,
                "id-1",
                expected,
                "P",
                &full_policy(&next),
                &format!(r#"["v{i}"]"#),
            )
            .unwrap();
        }
        assert_eq!(stored(&store, "id-1").revision, 56);
        let revs = store.with_read(|conn| revisions(conn, "id-1")).unwrap();
        assert_eq!(revs.len(), 50, "pruned to 50");
        // Newest first: revision 56 with its changes, oldest kept is 7 (56 - 50 + 1).
        assert_eq!(revs[0].revision, 56);
        assert_eq!(revs[0].changes, r#"["v54"]"#);
        assert_eq!(revs.last().unwrap().revision, 7);
        // The changes JSON persists verbatim and parses back.
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&revs[0].changes).unwrap();
        assert_eq!(parsed, vec![serde_json::json!("v54")]);
    }

    /// SP-9.5 (P0.3 side): the in-use check is wired into delete; today nothing can reference a
    /// profile, so deletion succeeds after the revision check.
    #[test]
    fn delete_refuses_stale_and_removes_the_profile_with_revisions() {
        let (_dir, store) = store();
        create_profile(&store, "id-1", "P", &full_policy(r"D:\b"));
        save_policy(&store, "id-1", 1, "P", &full_policy(r"E:\b"), "[]").unwrap();
        assert_eq!(
            store
                .with_write(|conn| delete(conn, "id-1", 99))
                .unwrap_err(),
            StoreError::Stale
        );
        store.with_write(|conn| delete(conn, "id-1", 2)).unwrap();
        assert!(store.with_read(|conn| get(conn, "id-1")).unwrap().is_none());
        assert!(
            store
                .with_read(|conn| revisions(conn, "id-1"))
                .unwrap()
                .is_empty()
        );
        // Deleting again: not found.
        assert_eq!(
            store
                .with_write(|conn| delete(conn, "id-1", 2))
                .unwrap_err(),
            StoreError::NotFound
        );
    }

    /// SP-6.4 (call-site test; full effect lands in P1.1): the dependants hook runs inside the
    /// save transaction and reports zero dependants while no automations table exists.
    #[test]
    fn sp_6_4_hook_runs_inside_the_save_transaction() {
        let (_dir, store) = store();
        create_profile(&store, "id-1", "P", &full_policy(r"D:\b"));
        // The hook itself is callable and honest about "no dependants yet".
        store
            .with_read(|conn| -> Result<(), StoreError> {
                assert_eq!(
                    mark_dependants_needs_review(conn, "id-1", 2, "[]", 1).unwrap(),
                    0
                );
                Ok(())
            })
            .unwrap();
        // A policy-changing save passes through the hook without error.
        assert_eq!(
            save_policy(&store, "id-1", 1, "P", &full_policy(r"E:\b"), "[]").unwrap(),
            2
        );
        // And the usedBy counter (P1.1 call site) is honestly zero for now.
        assert_eq!(store.with_read(|conn| used_by(conn, "id-1")).unwrap(), 0);
    }
}
