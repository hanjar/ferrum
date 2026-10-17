use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const SCHEMA_VERSION: i64 = 13;
const SEARCH_POSTINGS_PER_GRAM: i64 = 100;
/// Word-token entries live in `activity_search_grams` under a gram that starts
/// with this control character (real grams are alphanumeric, so no clash).
/// They give indexed prefix seeks for short and prefix queries.
const TOKEN_MARK: char = '\u{1}';
const TOKEN_MAX_CHARS: usize = 64;
/// Initials entries (`Quarterly Business Review` -> `qbr`) use this mark.
const INITIALS_MARK: char = match char::from_u32(2) {
    Some(c) => c,
    None => '?',
};
/// Query words that get a prefix seek (the rest are covered by grams).
const SEARCH_PREFIX_WORDS: usize = 4;
/// Seeks per query; 8 keeps 1–2-edit typos of 8-gram queries fully covered
/// while longer queries sample evenly (documented recall bound).
const SEARCH_GRAMS_PER_QUERY: usize = 8;
const EPISODE_WINDOW_MS: i64 = 30_000;
const RANK_CANDIDATE_SLACK: i64 = 50;
const DAY_MS: i64 = 86_400_000;
const HALF_LIFE_DAYS: f64 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    NewEpisode,
    ContinuedEpisode,
    Duplicate,
    Ignored,
    StaleEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRow {
    pub entity: ActivityEntity,
    pub last_utc_ms: u64,
    pub episode_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentCursor {
    pub from_utc_ms: u64,
    pub until_utc_ms: u64,
    pub kind: ActivityKind,
    pub privacy_epoch: u64,
    pub revision: u64,
    pub last_utc_ms: u64,
    pub last_entity_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPage {
    pub rows: Vec<RecentRow>,
    pub next_cursor: Option<RecentCursor>,
}

/// Usage of one entity inside a window, from the per-day buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Popularity {
    pub episodes: u64,
    pub active_days: u64,
    pub last_utc_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrequentRow {
    pub entity: ActivityEntity,
    pub last_utc_ms: u64,
    pub episode_count: u64,
    pub active_days: u64,
    pub score: f64,
}

/// How far the store trusts that an entity still denotes the same filesystem
/// object it was first recorded for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityConfidence {
    /// Identified by (volume, path) only; the common case.
    Path,
    /// A platform file id was observed at the locator and has matched since.
    Verified,
    /// A rename/move was suspected but evidence was insufficient; never merged.
    Unresolved,
    /// The object is known gone (deleted, or its path now holds another
    /// object). The row is kept as history, never re-attached to a locator.
    Ended,
}

impl IdentityConfidence {
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Verified => "verified",
            Self::Unresolved => "unresolved",
            Self::Ended => "ended",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "verified" => Self::Verified,
            "unresolved" => Self::Unresolved,
            "ended" => Self::Ended,
            _ => Self::Path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsChangeKind {
    Rename,
    Move,
    Copy,
    Delete,
}

/// Receipt of a filesystem change Ferrum itself performed successfully.
/// `action_id` makes retries idempotent. `to`/`to_volume_id` are required for
/// everything except `Delete`. Locators may be spelled with either separator,
/// trailing separators or an extended-length prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsChange {
    pub action_id: String,
    pub kind: FsChangeKind,
    pub from_volume_id: String,
    pub from: String,
    pub to_volume_id: Option<String>,
    pub to: Option<String>,
    pub occurred_utc_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// Rename/move: this many entities (the entity plus any subtree) kept their ids.
    Relocated { entities: u64 },
    /// Delete: entities are Missing, detached from their locators, history kept.
    Deleted { entities: u64 },
    /// Copy: the source is untouched and the destination inherits nothing.
    CopyNotInherited,
    /// The destination is under an exclusion: the moved history was forgotten.
    Forgotten { entities: u64 },
    /// The source locator is not tracked: nothing to transfer.
    NotTracked,
    /// This `action_id` was already applied.
    Duplicate,
}

/// Result of a stat/open the caller performed (with its own timeout). The
/// store never touches the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationProbe {
    /// Exists. `platform_id` is the platform file id (e.g. NTFS file id, or
    /// inode) when the caller could read one.
    Present { platform_id: Option<String> },
    /// The volume is reachable but the path does not exist.
    NotFound,
    /// Exists or may exist but access is denied.
    AccessDenied,
    /// The volume/share is not mounted or reachable.
    VolumeUnavailable,
    /// Timeout, cancelled or any other inconclusive result.
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationOutcome {
    Updated(Availability),
    /// The locator now holds a different object than the entity denotes. The
    /// entity is marked Missing/Ended and detached; its history is not reused.
    IdentityMismatch,
    UnknownEntity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityState {
    pub entity: ActivityEntity,
    pub confidence: IdentityConfidence,
    pub platform_id: Option<String>,
    pub checked_utc_ms: Option<u64>,
    /// False once the entity was detached from its locator (deleted/ended).
    pub attached: bool,
}

impl IdentityState {
    /// Lazy validation policy: the caller revalidates only when this says so
    /// (never per keystroke). A receipt counts as a fresh check.
    pub fn is_stale(&self, now_utc_ms: u64, max_age_ms: u64) -> bool {
        self.checked_utc_ms
            .is_none_or(|checked| now_utc_ms.saturating_sub(checked) > max_age_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalCandidate {
    pub volume_id: String,
    pub locator: String,
    pub kind: EntityKind,
    pub platform_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// Only entities attached to a locator and currently Missing can be merged.
    SourceNotMissing,
    KindMismatch,
    VolumeMismatch,
    /// Name/size/time similarity is not evidence: a platform id is required
    /// on both sides.
    NoPlatformEvidence,
    PlatformIdMismatch,
    /// Another tracked entity claims the same platform id.
    Ambiguous,
    /// The candidate locator already belongs to another tracked entity.
    TargetTracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Merged,
    /// The candidate is under an exclusion; the history was forgotten.
    Excluded,
    Unresolved(UnresolvedReason),
    UnknownEntity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOutcome {
    /// Entities whose locators were re-prefixed because the volume came back
    /// under another mount point.
    pub rebased: u64,
    /// Entities of another volume that lost this mount point and went Offline.
    pub offlined_other: u64,
    pub previous_mount: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasOutcome {
    Linked,
    AlreadyLinked,
    /// The alias path is tracked as a separate entity.
    Conflict,
    /// The alias lies in an excluded root; the target was forgotten and excluded.
    ExcludedTarget,
    UnknownEntity,
}

pub struct ActivityStore {
    connection: Connection,
}

impl ActivityStore {
    /// Caller supplies an OS app-data path; never put this database in a repository.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| rusqlite::Error::InvalidPath(parent.to_owned()))?;
        }
        let connection = Connection::open(path)?;
        Self::initialize(connection)
    }

    /// Like `open`, but a database that is corrupt (not a database / malformed)
    /// is moved aside to a timestamped quarantine name and replaced by a fresh
    /// one. Returns `true` when that recovery happened. A database written by a
    /// *newer* schema is never touched: the error is returned and the file is
    /// left intact. Other failures (permissions, I/O) are returned unchanged.
    pub fn open_recovering(path: &Path) -> rusqlite::Result<(Self, bool)> {
        match Self::open(path) {
            Ok(store) => Ok((store, false)),
            Err(error) if is_corruption(&error) => {
                quarantine(path)?;
                Self::open(path).map(|store| (store, true))
            }
            Err(error) => Err(error),
        }
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(mut connection: Connection) -> rusqlite::Result<Self> {
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON; PRAGMA busy_timeout = 3000;",
        )?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !(0..=SCHEMA_VERSION).contains(&version) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if version == 0 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE TABLE activity_state (id INTEGER PRIMARY KEY CHECK (id = 1), privacy_epoch INTEGER NOT NULL DEFAULT 0 CHECK (privacy_epoch >= 0), recording_enabled INTEGER NOT NULL DEFAULT 1 CHECK (recording_enabled IN (0, 1)));
                 INSERT INTO activity_state (id) VALUES (1);
                 CREATE TABLE activity_entities (id TEXT PRIMARY KEY, kind TEXT NOT NULL, volume_id TEXT NOT NULL, locator TEXT NOT NULL, parent_path TEXT NOT NULL, display_name TEXT NOT NULL, availability TEXT NOT NULL, metadata TEXT NOT NULL);
                 CREATE TABLE activity_episodes (id INTEGER PRIMARY KEY, entity_id TEXT NOT NULL REFERENCES activity_entities(id) ON DELETE CASCADE, kind TEXT NOT NULL, start_utc_ms INTEGER NOT NULL, last_utc_ms INTEGER NOT NULL, CHECK (last_utc_ms >= start_utc_ms));
                 CREATE INDEX activity_episode_lookup ON activity_episodes(entity_id, kind, last_utc_ms DESC);
                 CREATE TABLE activity_receipts (event_id TEXT PRIMARY KEY, action_id TEXT NOT NULL UNIQUE, episode_id INTEGER NOT NULL REFERENCES activity_episodes(id) ON DELETE CASCADE);
                 CREATE TABLE activity_exclusions (root TEXT PRIMARY KEY);
                 CREATE TABLE activity_hidden (entity_id TEXT PRIMARY KEY);
                 CREATE TABLE activity_vocabulary (entity_id TEXT PRIMARY KEY REFERENCES activity_entities(id) ON DELETE CASCADE, search_key TEXT NOT NULL);
                 PRAGMA user_version = 1;",
            )?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "ALTER TABLE activity_state ADD COLUMN retention_cutoff_utc_ms INTEGER NOT NULL DEFAULT 0 CHECK (retention_cutoff_utc_ms >= 0);
                 PRAGMA user_version = 2;",
            )?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE INDEX activity_recent_kind_time ON activity_episodes(kind, last_utc_ms DESC, entity_id);
                 PRAGMA user_version = 3;",
            )?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "ALTER TABLE activity_state ADD COLUMN data_revision INTEGER NOT NULL DEFAULT 0 CHECK (data_revision >= 0);
                 PRAGMA user_version = 4;",
            )?;
            tx.commit()?;
        }
        if version < 5 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE TABLE activity_search_grams (
                   kind TEXT NOT NULL,
                   gram TEXT NOT NULL,
                   entity_id TEXT NOT NULL REFERENCES activity_entities(id) ON DELETE CASCADE,
                   PRIMARY KEY (kind, gram, entity_id)
                 );
                 CREATE INDEX activity_search_entity ON activity_search_grams(entity_id);",
            )?;
            {
                let mut statement = tx.prepare(
                    "SELECT v.entity_id, e.kind, v.search_key FROM activity_vocabulary v
                     JOIN activity_entities e ON e.id = v.entity_id",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                for row in rows {
                    let (id, kind, name) = row?;
                    index_name(&tx, &id, &kind, &name)?;
                }
            }
            tx.pragma_update(None, "user_version", 5)?;
            tx.commit()?;
        }
        if version < 6 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS activity_vocabulary_name ON activity_vocabulary(search_key, entity_id);",
            )?;
            tx.pragma_update(None, "user_version", 6)?;
            tx.commit()?;
        }
        if version < 7 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS activity_favorites (
                   path TEXT PRIMARY KEY,
                   label TEXT NOT NULL,
                   position INTEGER NOT NULL,
                   created_utc_ms INTEGER NOT NULL
                 );",
            )?;
            tx.pragma_update(None, "user_version", 7)?;
            tx.commit()?;
        }
        if version < 8 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Files can be pinned too; existing rows were folder pins.
            let has_kind: i64 = tx.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('activity_favorites') WHERE name = 'kind'",
                [],
                |row| row.get(0),
            )?;
            if has_kind == 0 {
                tx.execute(
                    "ALTER TABLE activity_favorites ADD COLUMN kind TEXT NOT NULL DEFAULT 'folder'",
                    [],
                )?;
            }
            tx.pragma_update(None, "user_version", 8)?;
            tx.commit()?;
        }
        if version < 9 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Materialized rollups so sidebar queries never re-aggregate the
            // full episode table: per-entity latest (Recent) and per-day
            // buckets (Frequent). Both are derived — rebuilt on migration.
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS activity_entity_summary (
                   entity_id TEXT NOT NULL REFERENCES activity_entities(id) ON DELETE CASCADE,
                   kind TEXT NOT NULL,
                   last_utc_ms INTEGER NOT NULL,
                   episodes INTEGER NOT NULL,
                   PRIMARY KEY (entity_id, kind)
                 );
                 CREATE INDEX IF NOT EXISTS activity_summary_recent
                   ON activity_entity_summary(kind, last_utc_ms DESC, entity_id);
                 CREATE TABLE IF NOT EXISTS activity_daily (
                   entity_id TEXT NOT NULL REFERENCES activity_entities(id) ON DELETE CASCADE,
                   kind TEXT NOT NULL,
                   utc_day INTEGER NOT NULL,
                   episodes INTEGER NOT NULL,
                   last_utc_ms INTEGER NOT NULL,
                   PRIMARY KEY (entity_id, kind, utc_day)
                 );
                 INSERT OR REPLACE INTO activity_entity_summary(entity_id, kind, last_utc_ms, episodes)
                   SELECT entity_id, kind, MAX(last_utc_ms), COUNT(*)
                   FROM activity_episodes GROUP BY entity_id, kind;
                 INSERT OR REPLACE INTO activity_daily(entity_id, kind, utc_day, episodes, last_utc_ms)
                   SELECT entity_id, kind, last_utc_ms / 86400000, COUNT(*), MAX(last_utc_ms)
                   FROM activity_episodes GROUP BY entity_id, kind, last_utc_ms / 86400000;",
            )?;
            tx.pragma_update(None, "user_version", 9)?;
            tx.commit()?;
        }
        if version < 10 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Covering index so Frequent range-scans buckets without table lookups
            // or a sort (the query no longer orders by entity).
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS activity_daily_frequent
                   ON activity_daily(kind, last_utc_ms, entity_id, utc_day, episodes);
                 CREATE TABLE IF NOT EXISTS activity_entity_rank (
                   entity_id TEXT NOT NULL REFERENCES activity_entities(id) ON DELETE CASCADE,
                   kind TEXT NOT NULL,
                   rank_key REAL NOT NULL,
                   active_days INTEGER NOT NULL,
                   last_utc_ms INTEGER NOT NULL,
                   PRIMARY KEY (entity_id, kind)
                 );
                 CREATE INDEX IF NOT EXISTS activity_rank_frequent
                   ON activity_entity_rank(kind, rank_key DESC, last_utc_ms DESC, entity_id)
                   WHERE active_days >= 2;",
            )?;
            rebuild_rank(&tx)?;
            tx.pragma_update(None, "user_version", 10)?;
            tx.commit()?;
        }
        if version < 11 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Identity reconciliation (ACT-06). Every step is idempotent so a
            // database whose user_version was lowered by hand migrates cleanly.
            for (column, ddl) in [
                (
                    "locator_key",
                    "ALTER TABLE activity_entities ADD COLUMN locator_key TEXT",
                ),
                (
                    "platform_id",
                    "ALTER TABLE activity_entities ADD COLUMN platform_id TEXT",
                ),
                (
                    "identity",
                    "ALTER TABLE activity_entities ADD COLUMN identity TEXT NOT NULL DEFAULT 'path'",
                ),
                (
                    "relocated",
                    "ALTER TABLE activity_entities ADD COLUMN relocated INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "checked_utc_ms",
                    "ALTER TABLE activity_entities ADD COLUMN checked_utc_ms INTEGER",
                ),
            ] {
                let present: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('activity_entities') WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )?;
                if present == 0 {
                    tx.execute(ddl, [])?;
                }
            }
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS activity_volumes (
                   volume_id TEXT PRIMARY KEY,
                   mount TEXT NOT NULL,
                   mount_key TEXT NOT NULL,
                   mounted INTEGER NOT NULL CHECK (mounted IN (0, 1)),
                   last_seen_utc_ms INTEGER NOT NULL
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS activity_volume_mount
                   ON activity_volumes(mount_key) WHERE mounted = 1;
                 CREATE TABLE IF NOT EXISTS activity_aliases (
                   volume_id TEXT NOT NULL,
                   alias_key TEXT NOT NULL,
                   alias_locator TEXT NOT NULL,
                   entity_id TEXT NOT NULL REFERENCES activity_entities(id) ON DELETE CASCADE,
                   PRIMARY KEY (volume_id, alias_key)
                 );
                 CREATE INDEX IF NOT EXISTS activity_alias_entity ON activity_aliases(entity_id);
                 CREATE TABLE IF NOT EXISTS activity_changes (
                   action_id TEXT PRIMARY KEY,
                   applied_utc_ms INTEGER NOT NULL
                 );",
            )?;
            // Backfill the normalized lookup key. A collision (two legacy rows
            // for one location) leaves the later row detached rather than
            // failing the migration.
            let rows = {
                let mut statement = tx.prepare(
                    "SELECT id, volume_id, locator FROM activity_entities WHERE locator_key IS NULL ORDER BY id",
                )?;
                statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut seen = BTreeSet::new();
            for (id, volume, locator) in rows {
                let key = locator_key(&locator);
                if seen.insert((volume, key.clone())) {
                    tx.execute(
                        "UPDATE activity_entities SET locator_key = ?1 WHERE id = ?2",
                        params![key, id],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE activity_entities SET identity = 'unresolved' WHERE id = ?1",
                        [&id],
                    )?;
                }
            }
            tx.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS activity_entity_locator
                   ON activity_entities(volume_id, locator_key) WHERE locator_key IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS activity_entity_platform
                   ON activity_entities(volume_id, platform_id) WHERE platform_id IS NOT NULL;",
            )?;
            tx.pragma_update(None, "user_version", 11)?;
            tx.commit()?;
        }
        if version < 12 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // activity_receipts.episode_id is a cascading FK with no index, so
            // every episode deleted by purge/forget scanned the whole receipts
            // table (O(episodes x receipts)).
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS activity_receipt_episode ON activity_receipts(episode_id);",
            )?;
            tx.pragma_update(None, "user_version", 12)?;
            tx.commit()?;
        }
        if version < 13 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Word-token prefix entries (see TOKEN_MARK); index_name rewrites
            // every entity's grams, so the backfill is idempotent.
            {
                let mut statement = tx.prepare(
                    "SELECT v.entity_id, e.kind, v.search_key FROM activity_vocabulary v
                     JOIN activity_entities e ON e.id = v.entity_id",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (id, kind, name) in rows {
                    index_name(&tx, &id, &kind, &name)?;
                }
            }
            tx.pragma_update(None, "user_version", 13)?;
            tx.commit()?;
        }
        Ok(Self { connection })
    }

    /// Manual pins, ordered stably by pin time. Deliberately NOT touched by
    /// forget/purge/clear — removing a favorite is always the user's explicit
    /// remove_favorite call (spec §7).
    pub fn favorites(&self) -> rusqlite::Result<Vec<(String, String, String)>> {
        let mut statement = self
            .connection
            .prepare("SELECT path, label, kind FROM activity_favorites ORDER BY position ASC")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    }

    /// `Ok(false)` means the path was already pinned.
    pub fn add_favorite(&mut self, path: &str, label: &str, kind: &str) -> rusqlite::Result<bool> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM activity_favorites",
            [],
            |row| row.get(0),
        )?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO activity_favorites(path, label, position, created_utc_ms, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, label, position, now_ms(), kind],
        )? == 1;
        tx.commit()?;
        Ok(inserted)
    }

    /// `Ok(false)` means the path was not pinned.
    pub fn remove_favorite(&mut self, path: &str) -> rusqlite::Result<bool> {
        let removed = self
            .connection
            .execute("DELETE FROM activity_favorites WHERE path = ?1", [path])?
            == 1;
        Ok(removed)
    }

    pub fn privacy_epoch(&self) -> rusqlite::Result<u64> {
        self.connection.query_row(
            "SELECT privacy_epoch FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
    }

    pub fn recording_enabled(&self) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT recording_enabled FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
    }

    pub fn set_recording_enabled(&mut self, enabled: bool) -> rusqlite::Result<u64> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE activity_state SET recording_enabled = ?1, privacy_epoch = privacy_epoch + 1 WHERE id = 1 AND recording_enabled != ?1",
            [enabled],
        )?;
        let epoch = tx.query_row(
            "SELECT privacy_epoch FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(epoch)
    }

    pub fn record(&mut self, receipt: &ActivityReceipt) -> rusqlite::Result<RecordOutcome> {
        if let Some(outcome) = Self::precheck(receipt)? {
            return Ok(outcome);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = Self::record_in(&tx, receipt)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Applies receipts in one transaction (one commit/fsync). Each receipt runs
    /// in its own savepoint, so a failing receipt is rolled back alone and
    /// reported at its index while the others still commit. The privacy epoch
    /// is checked per receipt, exactly as in `record`.
    pub fn record_batch(
        &mut self,
        receipts: &[ActivityReceipt],
    ) -> rusqlite::Result<Vec<rusqlite::Result<RecordOutcome>>> {
        let mut tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcomes = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            match Self::precheck(receipt) {
                Ok(Some(outcome)) => {
                    outcomes.push(Ok(outcome));
                    continue;
                }
                Err(error) => {
                    outcomes.push(Err(error));
                    continue;
                }
                Ok(None) => {}
            }
            let result = match tx.savepoint() {
                Ok(sp) => match Self::record_in(&sp, receipt) {
                    Ok(outcome) => sp.commit().map(|()| outcome),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            outcomes.push(result);
        }
        tx.commit()?;
        Ok(outcomes)
    }

    /// Validation and kind/provenance filters that need no database access.
    fn precheck(receipt: &ActivityReceipt) -> rusqlite::Result<Option<RecordOutcome>> {
        receipt
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        if !matches!(
            receipt.kind,
            ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
        ) {
            return Ok(Some(RecordOutcome::Ignored));
        }
        // Chat-originated receipts must never raise Recent/Frequent (FER-250):
        // no entity, episode or rollup is created for them.
        if receipt.provenance != ferrum_activity_contract::Provenance::Manual {
            return Ok(Some(RecordOutcome::Ignored));
        }
        i64::try_from(receipt.occurred_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        Ok(None)
    }

    fn record_in(tx: &Connection, receipt: &ActivityReceipt) -> rusqlite::Result<RecordOutcome> {
        let occurred =
            i64::try_from(receipt.occurred_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let (epoch, enabled, cutoff): (u64, bool, u64) = tx.query_row(
            "SELECT privacy_epoch, recording_enabled, retention_cutoff_utc_ms FROM activity_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if epoch != receipt.privacy_epoch {
            return Ok(RecordOutcome::StaleEpoch);
        }
        if !enabled || receipt.occurred_utc_ms < cutoff || excluded(tx, &receipt.entity.locator)? {
            return Ok(RecordOutcome::Ignored);
        }
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM activity_receipts WHERE event_id = ?1 OR action_id = ?2)",
            params![receipt.event_id, receipt.action_id],
            |row| row.get(0),
        )?;
        if duplicate {
            return Ok(RecordOutcome::Duplicate);
        }
        // Resolve the stored identity for this location first: a renamed or
        // moved entity keeps its id, a recycled id/path gets a fresh one.
        let entity = resolve_entity(tx, &receipt.entity, occurred)?;
        upsert_entity(tx, &entity, occurred)?;
        let kind = kind_name(&receipt.kind);
        let latest: Option<(i64, i64, i64)> = tx.query_row(
            "SELECT id, start_utc_ms, last_utc_ms FROM activity_episodes WHERE entity_id = ?1 AND kind = ?2 ORDER BY last_utc_ms DESC LIMIT 1",
            params![entity.id, kind], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        // A receipt joins the open episode when it lies inside it, after it by
        // less than the window, or before its start by less than the window
        // (cross-pane receipts can arrive out of timestamp order, FER-252).
        let (episode_id, outcome, old_day) = if let Some((id, _, old_last)) =
            latest.filter(|(_, start, last)| {
                if occurred >= *last {
                    occurred - *last < EPISODE_WINDOW_MS
                } else if occurred >= *start {
                    true
                } else {
                    *start - occurred < EPISODE_WINDOW_MS
                }
            }) {
            tx.execute(
                "UPDATE activity_episodes SET last_utc_ms = MAX(last_utc_ms, ?1), start_utc_ms = MIN(start_utc_ms, ?1) WHERE id = ?2",
                params![occurred, id],
            )?;
            (id, RecordOutcome::ContinuedEpisode, Some(old_last / DAY_MS))
        } else {
            tx.execute("INSERT INTO activity_episodes(entity_id, kind, start_utc_ms, last_utc_ms) VALUES (?1, ?2, ?3, ?3)", params![entity.id, kind, occurred])?;
            (tx.last_insert_rowid(), RecordOutcome::NewEpisode, None)
        };
        // Maintain the materialized rollups in the same transaction so queries
        // never re-aggregate raw episodes (see schema v9). A continued episode
        // moves its bucket's timestamp without adding an episode count; when
        // it crosses a UTC midnight the episode moves between daily buckets.
        // A continued episode keeps its newest timestamp, so an out-of-order
        // (older) receipt must not move its daily bucket backwards.
        let new_day = match old_day {
            Some(_) => occurred.max(latest.map_or(occurred, |(_, _, last)| last)) / DAY_MS,
            None => occurred / DAY_MS,
        };
        if outcome == RecordOutcome::NewEpisode {
            tx.execute(
                "INSERT INTO activity_entity_summary(entity_id, kind, last_utc_ms, episodes)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(entity_id, kind) DO UPDATE SET
                   last_utc_ms = MAX(last_utc_ms, excluded.last_utc_ms), episodes = episodes + 1",
                params![entity.id, kind, occurred],
            )?;
            tx.execute(
                "INSERT INTO activity_daily(entity_id, kind, utc_day, episodes, last_utc_ms)
                 VALUES (?1, ?2, ?3, 1, ?4)
                 ON CONFLICT(entity_id, kind, utc_day) DO UPDATE SET
                   episodes = episodes + 1,
                   last_utc_ms = MAX(last_utc_ms, excluded.last_utc_ms)",
                params![entity.id, kind, new_day, occurred],
            )?;
        } else {
            tx.execute(
                "UPDATE activity_entity_summary SET last_utc_ms = MAX(last_utc_ms, ?1)
                 WHERE entity_id = ?2 AND kind = ?3",
                params![occurred, entity.id, kind],
            )?;
            let old_day = old_day.expect("continued episode carries its previous day");
            if old_day == new_day {
                tx.execute(
                    "UPDATE activity_daily SET last_utc_ms = MAX(last_utc_ms, ?1)
                     WHERE entity_id = ?2 AND kind = ?3 AND utc_day = ?4",
                    params![occurred, entity.id, kind, new_day],
                )?;
            } else {
                tx.execute(
                    "UPDATE activity_daily SET episodes = episodes - 1
                     WHERE entity_id = ?1 AND kind = ?2 AND utc_day = ?3",
                    params![entity.id, kind, old_day],
                )?;
                tx.execute(
                    "DELETE FROM activity_daily
                     WHERE entity_id = ?1 AND kind = ?2 AND utc_day = ?3 AND episodes <= 0",
                    params![entity.id, kind, old_day],
                )?;
                tx.execute(
                    "INSERT INTO activity_daily(entity_id, kind, utc_day, episodes, last_utc_ms)
                     VALUES (?1, ?2, ?3, 1, ?4)
                     ON CONFLICT(entity_id, kind, utc_day) DO UPDATE SET
                       episodes = episodes + 1,
                       last_utc_ms = MAX(last_utc_ms, excluded.last_utc_ms)",
                    params![entity.id, kind, new_day, occurred],
                )?;
            }
        }
        refresh_rank(tx, &entity.id, kind)?;
        tx.execute(
            "INSERT INTO activity_receipts(event_id, action_id, episode_id) VALUES (?1, ?2, ?3)",
            params![receipt.event_id, receipt.action_id, episode_id],
        )?;
        tx.execute(
            "UPDATE activity_state SET data_revision = data_revision + 1 WHERE id = 1",
            [],
        )?;
        Ok(outcome)
    }

    /// Returns None when caller's privacy epoch is stale; rows are read in one snapshot.
    pub fn recent(
        &mut self,
        from_utc_ms: u64,
        until_utc_ms: u64,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
    ) -> rusqlite::Result<Option<Vec<RecentRow>>> {
        self.recent_in(from_utc_ms, until_utc_ms, kind, privacy_epoch, limit, None)
    }

    /// `recent` restricted to entities at or below `root` (an absolute
    /// workspace root path supplied by the caller). The filter is part of the
    /// query, so it applies before `limit`.
    pub fn recent_in(
        &mut self,
        from_utc_ms: u64,
        until_utc_ms: u64,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
        root: Option<&str>,
    ) -> rusqlite::Result<Option<Vec<RecentRow>>> {
        if limit == 0 || limit > 100 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let scope = root_scope(root)?;
        let (scope_key, scope_low, scope_high) = (
            scope.as_ref().map(|s| s.key.as_str()),
            scope.as_ref().map(|s| s.low.as_str()),
            scope.as_ref().map(|s| s.high.as_str()),
        );
        let from = i64::try_from(from_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let until = i64::try_from(until_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if !matches!(
            kind,
            ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.transaction()?;
        let (epoch, cutoff): (u64, u64) = tx.query_row(
            "SELECT privacy_epoch, retention_cutoff_utc_ms FROM activity_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if epoch != privacy_epoch {
            return Ok(None);
        }
        let mut result = Vec::new();
        if from < until {
            let cutoff = i64::try_from(cutoff).map_err(|_| rusqlite::Error::InvalidQuery)?;
            let mut statement = tx.prepare(
                &
                // Reads the materialized per-entity rollup (schema v9): an
                // indexed top-N instead of aggregating every episode.
                "SELECT e.metadata, s.last_utc_ms, s.episodes
                 FROM activity_entity_summary s
                 JOIN activity_entities e ON e.id = s.entity_id
                 WHERE s.kind = ?1 AND s.last_utc_ms >= ?2 AND s.last_utc_ms < ?3
                   AND s.last_utc_ms >= ?4
                   AND (?6 IS NULL OR {scope})
                 ORDER BY s.last_utc_ms DESC, s.entity_id ASC
                 LIMIT ?5"
                    .replace("{scope}", &scope_clause(6, 7, 8)),
            )?;
            let rows = statement.query_map(
                params![
                    kind_name(&kind),
                    from,
                    until,
                    cutoff,
                    limit,
                    scope_key,
                    scope_low,
                    scope_high
                ],
                |row| {
                    let metadata: String = row.get(0)?;
                    let entity = serde_json::from_str(&metadata).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(RecentRow {
                        entity,
                        last_utc_ms: row.get(1)?,
                        episode_count: row.get(2)?,
                    })
                },
            )?;
            for row in rows {
                result.push(row?);
            }
        }
        tx.commit()?;
        Ok(Some(result))
    }

    /// A changed revision invalidates the next page rather than mixing two snapshots.
    pub fn recent_page(
        &mut self,
        from_utc_ms: u64,
        until_utc_ms: u64,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
        cursor: Option<&RecentCursor>,
    ) -> rusqlite::Result<Option<RecentPage>> {
        if !(1..=100).contains(&limit) || from_utc_ms >= until_utc_ms {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let from = i64::try_from(from_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let until = i64::try_from(until_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if !matches!(
            kind,
            ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.transaction()?;
        let (epoch, cutoff, revision): (u64, u64, u64) = tx.query_row(
            "SELECT privacy_epoch, retention_cutoff_utc_ms, data_revision FROM activity_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if epoch != privacy_epoch {
            return Ok(None);
        }
        let cutoff = i64::try_from(cutoff).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let (last, last_id) = if let Some(cursor) = cursor {
            if cursor.from_utc_ms != from_utc_ms
                || cursor.until_utc_ms != until_utc_ms
                || cursor.kind != kind
                || cursor.privacy_epoch != epoch
                || cursor.revision != revision
                || cursor.last_entity_id.is_empty()
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let last =
                i64::try_from(cursor.last_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
            let anchor: bool = tx.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM activity_entity_summary
                   WHERE kind = ?1 AND last_utc_ms >= ?2 AND last_utc_ms < ?3
                     AND last_utc_ms >= ?4 AND entity_id = ?5 AND last_utc_ms = ?6
                 )",
                params![
                    kind_name(&kind),
                    from,
                    until,
                    cutoff,
                    cursor.last_entity_id,
                    last
                ],
                |row| row.get(0),
            )?;
            if !anchor {
                return Err(rusqlite::Error::InvalidQuery);
            }
            (Some(last), Some(cursor.last_entity_id.as_str()))
        } else {
            (None, None)
        };
        let mut statement = tx.prepare(
            "SELECT e.metadata, s.last_utc_ms, s.episodes
             FROM activity_entity_summary s
             JOIN activity_entities e ON e.id = s.entity_id
             WHERE s.kind = ?1 AND s.last_utc_ms >= ?2 AND s.last_utc_ms < ?3
               AND s.last_utc_ms >= ?4
               AND (?5 IS NULL OR s.last_utc_ms < ?5
                 OR (s.last_utc_ms = ?5 AND s.entity_id > ?6))
             ORDER BY s.last_utc_ms DESC, s.entity_id ASC
             LIMIT ?7",
        )?;
        let rows = statement.query_map(
            params![
                kind_name(&kind),
                from,
                until,
                cutoff,
                last,
                last_id,
                i64::from(limit) + 1
            ],
            |row| {
                let metadata: String = row.get(0)?;
                let entity = serde_json::from_str(&metadata).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(RecentRow {
                    entity,
                    last_utc_ms: row.get(1)?,
                    episode_count: row.get(2)?,
                })
            },
        )?;
        let mut result = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = result.len() > usize::from(limit);
        result.truncate(usize::from(limit));
        let next_cursor = if more {
            result.last().map(|row| RecentCursor {
                from_utc_ms,
                until_utc_ms,
                kind,
                privacy_epoch: epoch,
                revision,
                last_utc_ms: row.last_utc_ms,
                last_entity_id: row.entity.id.clone(),
            })
        } else {
            None
        };
        drop(statement);
        tx.commit()?;
        Ok(Some(RecentPage {
            rows: result,
            next_cursor,
        }))
    }

    /// Ranking uses one UTC query clock; raw episode counts remain uncapped.
    pub fn frequent(
        &mut self,
        from_utc_ms: u64,
        now_utc_ms: u64,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
    ) -> rusqlite::Result<Option<Vec<FrequentRow>>> {
        self.frequent_in(from_utc_ms, now_utc_ms, kind, privacy_epoch, limit, None)
    }

    /// `frequent` restricted to entities at or below `root` (absolute
    /// workspace root path); the filter applies before `limit`.
    pub fn frequent_in(
        &mut self,
        from_utc_ms: u64,
        now_utc_ms: u64,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
        root: Option<&str>,
    ) -> rusqlite::Result<Option<Vec<FrequentRow>>> {
        if !(1..=100).contains(&limit) || from_utc_ms > now_utc_ms {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let scope = root_scope(root)?;
        let from = i64::try_from(from_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let now = i64::try_from(now_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if !matches!(
            kind,
            ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.transaction()?;
        let (epoch, cutoff): (u64, u64) = tx.query_row(
            "SELECT privacy_epoch, retention_cutoff_utc_ms FROM activity_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if epoch != privacy_epoch {
            return Ok(None);
        }
        let cutoff = i64::try_from(cutoff).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let kind_str = kind_name(&kind);
        let today = now.div_euclid(DAY_MS);
        let mut grouped: std::collections::BTreeMap<String, (i64, u64, u64, f64)> =
            std::collections::BTreeMap::new();
        let mut accumulate = |entity_id: String, day: i64, last: i64, count: u64| {
            let entry = grouped
                .entry(entity_id)
                .or_insert_with(|| (last, 0, 0, 0.0));
            entry.0 = entry.0.max(last);
            entry.1 += count;
            entry.2 += 1;
            let age_days = today.saturating_sub(day).max(0) as f64;
            entry.3 += (count.min(5) as f64) * 2.0_f64.powf(-age_days / HALF_LIFE_DAYS);
        };
        // Fast path: when the window excludes no bucket (none future/purged) the
        // precomputed rank key orders entities, so only the top candidates (plus
        // a float-tie margin) are re-scored exactly from their buckets.
        // Two single-aggregate queries so SQLite answers each from the index ends.
        let min_last: Option<i64> = tx.query_row(
            "SELECT MIN(last_utc_ms) FROM activity_daily WHERE kind = ?1",
            [kind_str],
            |row| row.get(0),
        )?;
        let max_last: Option<i64> = tx.query_row(
            "SELECT MAX(last_utc_ms) FROM activity_daily WHERE kind = ?1",
            [kind_str],
            |row| row.get(0),
        )?;
        // Every existing bucket must fall inside the window, i.e. the filters
        // below would not exclude anything.
        let fast = min_last.is_none_or(|min| min >= cutoff && min >= from)
            && max_last.is_none_or(|max| max <= now);
        if fast {
            // Ties on the (rounded) rank key are broken by recency then id, like the
            // final comparator, so a bounded prefix is enough; the extra slack
            // absorbs float noise between the key and the exact score.
            let candidates = if let Some(scope) = &scope {
                let mut query = tx.prepare(&format!(
                    "SELECT r.entity_id FROM activity_entity_rank r
                     JOIN activity_entities e ON e.id = r.entity_id
                     WHERE r.kind = ?1 AND r.active_days >= 2 AND {}
                     ORDER BY r.rank_key DESC, r.last_utc_ms DESC, r.entity_id ASC
                     LIMIT ?2",
                    scope_clause(3, 4, 5)
                ))?;
                let rows = query.query_map(
                    params![
                        kind_str,
                        i64::from(limit) + RANK_CANDIDATE_SLACK,
                        scope.key,
                        scope.low,
                        scope.high
                    ],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                let mut query = tx.prepare(
                    "SELECT entity_id FROM activity_entity_rank
                     WHERE kind = ?1 AND active_days >= 2
                     ORDER BY rank_key DESC, last_utc_ms DESC, entity_id ASC
                     LIMIT ?2",
                )?;
                let rows = query.query_map(
                    params![kind_str, i64::from(limit) + RANK_CANDIDATE_SLACK],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut buckets = tx.prepare(
                "SELECT utc_day, last_utc_ms, episodes FROM activity_daily
                 WHERE entity_id = ?1 AND +kind = ?2",
            )?;
            for id in candidates {
                let rows = buckets.query_map(params![id, kind_str], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                })?;
                for row in rows {
                    let (day, last, count) = row?;
                    accumulate(id.clone(), day, last, count);
                }
            }
        } else {
            // Reads pre-aggregated per-day buckets (schema v9): no episode scan,
            // no per-row entity join — metadata is fetched only for final rows.
            // Entities of this kind inside the workspace root (when scoped).
            let allowed: Option<std::collections::HashSet<String>> = match &scope {
                Some(scope) => {
                    let mut query = tx.prepare(&format!(
                        "SELECT e.id FROM activity_entities e WHERE e.kind = ?1 AND {}",
                        scope_clause(2, 3, 4)
                    ))?;
                    let rows = query.query_map(
                        params![entity_kind_name(&kind), scope.key, scope.low, scope.high],
                        |row| row.get::<_, String>(0),
                    )?;
                    Some(rows.collect::<rusqlite::Result<_>>()?)
                }
                None => None,
            };
            let mut statement = tx.prepare(
                "SELECT entity_id, utc_day, last_utc_ms, episodes
                 FROM activity_daily
                 WHERE kind = ?1 AND last_utc_ms >= ?2 AND last_utc_ms >= ?3
                   AND last_utc_ms <= ?4",
            )?;
            let rows = statement.query_map(params![kind_str, from, cutoff, now], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?;
            for row in rows {
                let (entity_id, day, last, count) = row?;
                if allowed
                    .as_ref()
                    .is_some_and(|set| !set.contains(&entity_id))
                {
                    continue;
                }
                accumulate(entity_id, day, last, count);
            }
        }
        // Score/sort on plain rows first, fetch metadata only for the survivors.
        let mut scored: Vec<(String, i64, u64, u64, f64)> = grouped
            .into_iter()
            .map(|(id, (last, count, active_days, score))| (id, last, count, active_days, score))
            .filter(|(_, _, _, active_days, _)| *active_days >= 2)
            .collect();
        scored.sort_by(|a, b| {
            b.4.total_cmp(&a.4)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(usize::from(limit));
        tx.commit()?;
        let mut result = Vec::new();
        for (entity_id, last, episode_count, active_days, score) in scored {
            let metadata: String = self.connection.query_row(
                "SELECT metadata FROM activity_entities WHERE id = ?1",
                [&entity_id],
                |row| row.get(0),
            )?;
            let entity: ActivityEntity = serde_json::from_str(&metadata).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            result.push(FrequentRow {
                entity,
                last_utc_ms: u64::try_from(last).map_err(|_| rusqlite::Error::InvalidQuery)?,
                episode_count,
                active_days,
                score,
            });
        }
        Ok(Some(result))
    }

    /// Bounded name candidates for caller-side matching, not a complete search or ranker.
    /// Sources, in priority order: exact name, indexed word-prefix seeks for the
    /// first four query words (so short and prefix queries do not depend on
    /// grams), then at most eight gram seeks with 100 postings each. Path-only
    /// matches and names sharing only omitted grams may not appear. No file
    /// content is indexed.
    pub fn search_candidates(
        &mut self,
        query: &str,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
    ) -> rusqlite::Result<Option<Vec<ActivityEntity>>> {
        self.search_candidates_in(query, kind, privacy_epoch, limit, None)
    }

    /// `search_candidates` restricted to entities at or below `root` (absolute
    /// workspace root path); the filter is part of every seek, so the posting
    /// windows and the limit are workspace-scoped.
    pub fn search_candidates_in(
        &mut self,
        query: &str,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
        root: Option<&str>,
    ) -> rusqlite::Result<Option<Vec<ActivityEntity>>> {
        if !(1..=100).contains(&limit)
            || query.trim().is_empty()
            || query.len() > 255
            || !matches!(
                kind,
                ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
            )
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let scope = root_scope(root)?;
        let tx = self.connection.transaction()?;
        let epoch: u64 = tx.query_row(
            "SELECT privacy_epoch FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if epoch != privacy_epoch {
            return Ok(None);
        }
        let kind_str = entity_kind_name(&kind);
        let grams = search_grams(query);
        let grams: Vec<_> = if grams.len() > SEARCH_GRAMS_PER_QUERY {
            (0..SEARCH_GRAMS_PER_QUERY)
                .map(|index| {
                    grams[index * (grams.len() - 1) / (SEARCH_GRAMS_PER_QUERY - 1)].clone()
                })
                .collect()
        } else {
            grams
        };
        let mut counts = BTreeMap::<String, u16>::new();
        {
            let mut plain = tx.prepare(
                "SELECT entity_id FROM activity_search_grams
                 WHERE kind = ?1 AND gram = ?2 ORDER BY entity_id LIMIT ?3",
            )?;
            let mut scoped = tx.prepare(&format!(
                "SELECT g.entity_id FROM activity_search_grams g
                 JOIN activity_entities e ON e.id = g.entity_id
                 WHERE g.kind = ?1 AND g.gram = ?2 AND {}
                 ORDER BY g.entity_id LIMIT ?3",
                scope_clause(4, 5, 6)
            ))?;
            for gram in grams {
                let rows = match &scope {
                    Some(scope) => scoped.query_map(
                        params![
                            kind_str,
                            gram,
                            SEARCH_POSTINGS_PER_GRAM,
                            scope.key,
                            scope.low,
                            scope.high
                        ],
                        id_column,
                    )?,
                    None => plain
                        .query_map(params![kind_str, gram, SEARCH_POSTINGS_PER_GRAM], id_column)?,
                };
                for id in rows {
                    *counts.entry(id?).or_default() += 1;
                }
            }
        }
        let normalized = query.trim().to_lowercase();
        let exact_ids = {
            let mut exact = tx.prepare(&format!(
                "SELECT v.entity_id FROM activity_vocabulary v
                 JOIN activity_entities e ON e.id = v.entity_id
                 WHERE v.search_key = ?1 AND e.kind = ?2 AND (?4 IS NULL OR {})
                 ORDER BY v.entity_id LIMIT ?3",
                scope_clause(4, 5, 6)
            ))?;
            exact
                .query_map(
                    params![
                        normalized,
                        kind_str,
                        limit,
                        scope.as_ref().map(|s| s.key.as_str()),
                        scope.as_ref().map(|s| s.low.as_str()),
                        scope.as_ref().map(|s| s.high.as_str())
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        // Indexed word-prefix seeks: the range [mark+word, mark+word+MAX).
        let mut prefix_ids: Vec<String> = Vec::new();
        {
            let mut plain = tx.prepare(
                "SELECT entity_id FROM activity_search_grams
                 WHERE kind = ?1 AND gram >= ?2 AND gram < ?3
                 ORDER BY gram, entity_id LIMIT ?4",
            )?;
            let mut scoped = tx.prepare(&format!(
                "SELECT g.entity_id FROM activity_search_grams g
                 JOIN activity_entities e ON e.id = g.entity_id
                 WHERE g.kind = ?1 AND g.gram >= ?2 AND g.gram < ?3 AND {}
                 ORDER BY g.gram, g.entity_id LIMIT ?4",
                scope_clause(5, 6, 7)
            ))?;
            for word in search_tokens(query).into_iter().take(SEARCH_PREFIX_WORDS) {
                let low = format!("{TOKEN_MARK}{word}");
                let high = format!("{low}{}", char::MAX);
                let rows = match &scope {
                    Some(scope) => scoped.query_map(
                        params![
                            kind_str,
                            low,
                            high,
                            SEARCH_POSTINGS_PER_GRAM,
                            scope.key,
                            scope.low,
                            scope.high
                        ],
                        id_column,
                    )?,
                    None => plain.query_map(
                        params![kind_str, low, high, SEARCH_POSTINGS_PER_GRAM],
                        id_column,
                    )?,
                };
                for id in rows {
                    let id = id?;
                    if !prefix_ids.contains(&id) {
                        prefix_ids.push(id);
                    }
                }
            }
        }
        // Abbreviation lookup: one equality seek on the initials key.
        let abbreviation: String = query
            .trim()
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        if !abbreviation.is_empty() && abbreviation.chars().count() <= TOKEN_MAX_CHARS {
            let key = format!("{INITIALS_MARK}{abbreviation}");
            let mut statement = tx.prepare(&format!(
                "SELECT g.entity_id FROM activity_search_grams g
                 JOIN activity_entities e ON e.id = g.entity_id
                 WHERE g.kind = ?1 AND g.gram = ?2 AND (?4 IS NULL OR {})
                 ORDER BY g.entity_id LIMIT ?3",
                scope_clause(4, 5, 6)
            ))?;
            let rows = statement.query_map(
                params![
                    kind_str,
                    key,
                    SEARCH_POSTINGS_PER_GRAM,
                    scope.as_ref().map(|s| s.key.as_str()),
                    scope.as_ref().map(|s| s.low.as_str()),
                    scope.as_ref().map(|s| s.high.as_str())
                ],
                id_column,
            )?;
            for id in rows {
                let id = id?;
                if !prefix_ids.contains(&id) {
                    prefix_ids.push(id);
                }
            }
        }
        let mut ranked: Vec<_> = counts.into_iter().collect();
        ranked.sort_by(|(a_id, a_hits), (b_id, b_hits)| {
            b_hits.cmp(a_hits).then_with(|| a_id.cmp(b_id))
        });
        let mut ids = exact_ids.clone();
        ids.extend(prefix_ids.into_iter().filter(|id| !exact_ids.contains(id)));
        let mut seen: std::collections::HashSet<String> = ids.iter().cloned().collect();
        ids.extend(
            ranked
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| seen.insert(id.clone())),
        );
        let mut statement =
            tx.prepare("SELECT metadata FROM activity_entities WHERE id = ?1 AND kind = ?2")?;
        let mut result = Vec::new();
        for id in ids {
            if result.len() >= usize::from(limit) {
                break;
            }
            let metadata: Option<String> = statement
                .query_row(params![id, kind_str], |row| row.get(0))
                .optional()?;
            if let Some(metadata) = metadata {
                let entity = serde_json::from_str(&metadata).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                result.push(entity);
            }
        }
        drop(statement);
        tx.commit()?;
        Ok(Some(result))
    }

    /// Usage of the given entities inside `[from, now]` (UTC ms), read from the
    /// per-day buckets. Entities with no bucket in the window are absent.
    pub fn entity_popularity(
        &mut self,
        ids: &[String],
        kind: ActivityKind,
        from_utc_ms: u64,
        now_utc_ms: u64,
    ) -> rusqlite::Result<BTreeMap<String, Popularity>> {
        let from = i64::try_from(from_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let now = i64::try_from(now_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if !matches!(
            kind,
            ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.transaction()?;
        let mut result = BTreeMap::new();
        {
            // `+kind` keeps SQLite on the (entity_id, kind, utc_day) primary key.
            let mut statement = tx.prepare(
                "SELECT COALESCE(SUM(episodes), 0), COUNT(*), MAX(last_utc_ms)
                 FROM activity_daily
                 WHERE entity_id = ?1 AND +kind = ?2
                   AND last_utc_ms >= ?3 AND last_utc_ms <= ?4",
            )?;
            for id in ids {
                let (episodes, days, last): (u64, u64, Option<i64>) = statement
                    .query_row(params![id, kind_name(&kind), from, now], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?;
                if let Some(last) = last {
                    result.insert(
                        id.clone(),
                        Popularity {
                            episodes,
                            active_days: days,
                            last_utc_ms: u64::try_from(last).unwrap_or(0),
                        },
                    );
                }
            }
        }
        tx.commit()?;
        Ok(result)
    }

    pub fn episode_count(&self) -> rusqlite::Result<u64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM activity_episodes", [], |row| {
                row.get(0)
            })
    }

    pub fn forget_entity(&mut self, entity_id: &str) -> rusqlite::Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM activity_entities WHERE id = ?1", [entity_id])?;
        tx.execute(
            "DELETE FROM activity_hidden WHERE entity_id = ?1",
            [entity_id],
        )?;
        tx.execute(
            "DELETE FROM activity_entity_summary WHERE entity_id = ?1",
            [entity_id],
        )?;
        tx.execute(
            "DELETE FROM activity_daily WHERE entity_id = ?1",
            [entity_id],
        )?;
        tx.execute(
            "DELETE FROM activity_entity_rank WHERE entity_id = ?1",
            [entity_id],
        )?;
        tx.execute(
            "UPDATE activity_state SET privacy_epoch = privacy_epoch + 1 WHERE id = 1",
            [],
        )?;
        tx.commit()
    }

    /// Candidates whose locator contains `needle` (case-insensitive for
    /// ASCII), for explicit path-like queries that have no name grams.
    /// `needle` must already be escaped by the caller (`%`/`_`).
    pub fn locator_candidates(
        &mut self,
        needle: &str,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
    ) -> rusqlite::Result<Option<Vec<ActivityEntity>>> {
        self.locator_candidates_in(needle, kind, privacy_epoch, limit, None)
    }

    /// `locator_candidates` restricted to a workspace root (absolute path).
    pub fn locator_candidates_in(
        &mut self,
        needle: &str,
        kind: ActivityKind,
        privacy_epoch: u64,
        limit: u16,
        root: Option<&str>,
    ) -> rusqlite::Result<Option<Vec<ActivityEntity>>> {
        let scope = root_scope(root)?;
        if !(1..=100).contains(&limit)
            || needle.trim().is_empty()
            || needle.len() > 255
            || !matches!(
                kind,
                ActivityKind::FolderVisit | ActivityKind::FileOpenRequested
            )
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.transaction()?;
        let epoch: u64 = tx.query_row(
            "SELECT privacy_epoch FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if epoch != privacy_epoch {
            return Ok(None);
        }
        let mut statement = tx.prepare(&format!(
            "SELECT e.metadata FROM activity_entities e
             WHERE e.kind = ?1 AND e.locator LIKE '%' || ?2 || '%' ESCAPE '!'
               AND (?4 IS NULL OR {})
             ORDER BY e.locator LIMIT ?3",
            scope_clause(4, 5, 6)
        ))?;
        let rows = statement.query_map(
            params![
                entity_kind_name(&kind),
                needle,
                limit,
                scope.as_ref().map(|s| s.key.as_str()),
                scope.as_ref().map(|s| s.low.as_str()),
                scope.as_ref().map(|s| s.high.as_str())
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut result = Vec::new();
        for row in rows {
            let metadata: String = row?;
            let entity = serde_json::from_str(&metadata).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            result.push(entity);
        }
        drop(statement);
        tx.commit()?;
        Ok(Some(result))
    }

    pub fn purge_before(&mut self, cutoff_utc_ms: u64) -> rusqlite::Result<usize> {
        let cutoff = i64::try_from(cutoff_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE activity_state SET retention_cutoff_utc_ms = MAX(retention_cutoff_utc_ms, ?1) WHERE id = 1",
            [cutoff],
        )?;
        // Everything below is set-based: per-(entity, kind) deleted counts go
        // into a temp table, then each derived table is fixed with one
        // statement instead of one statement per pair.
        tx.execute_batch(
            "DROP TABLE IF EXISTS temp.purge_pairs;
             CREATE TEMP TABLE purge_pairs (
               entity_id TEXT NOT NULL, kind TEXT NOT NULL, deleted INTEGER NOT NULL,
               PRIMARY KEY (entity_id, kind)
             ) WITHOUT ROWID;
             INSERT INTO purge_pairs(entity_id, kind, deleted)
               SELECT entity_id, kind, COUNT(*)
               FROM activity_episodes
               WHERE last_utc_ms < (SELECT retention_cutoff_utc_ms FROM activity_state WHERE id = 1)
               GROUP BY entity_id, kind;",
        )?;
        let removed = tx.execute(
            "DELETE FROM activity_episodes WHERE last_utc_ms < (SELECT retention_cutoff_utc_ms FROM activity_state WHERE id = 1)",
            [],
        )?;
        tx.execute(
            "UPDATE activity_entity_summary SET episodes = episodes - p.deleted
             FROM purge_pairs p
             WHERE activity_entity_summary.entity_id = p.entity_id
               AND activity_entity_summary.kind = p.kind",
            [],
        )?;
        // Buckets whose latest episode is older than the cutoff are gone whole.
        tx.execute(
            "DELETE FROM activity_daily WHERE last_utc_ms < (SELECT retention_cutoff_utc_ms FROM activity_state WHERE id = 1)",
            [],
        )?;
        refresh_rank_for_purged(&tx)?;
        tx.execute(
            "DELETE FROM activity_entity_summary
             WHERE episodes <= 0
                OR NOT EXISTS(
                   SELECT 1 FROM activity_episodes e
                   WHERE e.entity_id = activity_entity_summary.entity_id
                     AND e.kind = activity_entity_summary.kind
                )",
            [],
        )?;
        tx.execute(
            "DELETE FROM activity_entities
             WHERE NOT EXISTS (SELECT 1 FROM activity_episodes e WHERE e.entity_id = activity_entities.id)",
            [],
        )?;
        tx.execute_batch("DROP TABLE temp.purge_pairs;")?;
        tx.execute(
            "DELETE FROM activity_changes WHERE applied_utc_ms < ?1",
            [cutoff],
        )?;
        if removed > 0 {
            tx.execute(
                "UPDATE activity_state SET data_revision = data_revision + 1 WHERE id = 1",
                [],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn exclude_root(&mut self, root: &str) -> rusqlite::Result<u64> {
        if !is_absolute(root)
            || root.len() > 4096
            || root.contains('\0')
            || root.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO activity_exclusions(root) VALUES (?1)",
            [root],
        )?;
        let mut ids = Vec::new();
        {
            let mut query = tx.prepare("SELECT id, locator FROM activity_entities")?;
            let rows = query.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, locator) = row?;
                if within_root(&locator, root) {
                    ids.push(id);
                }
            }
        }
        // A verified alias inside the root makes its target forbidden too:
        // exclude the canonical locator so the real path stays excluded after
        // the alias rows are gone with the entity.
        {
            let mut query = tx.prepare(
                "SELECT a.entity_id, a.alias_locator, e.locator
                 FROM activity_aliases a JOIN activity_entities e ON e.id = a.entity_id",
            )?;
            let rows = query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut targets = Vec::new();
            for (id, alias, canonical) in rows {
                if within_root(&alias, root) {
                    tx.execute(
                        "INSERT OR IGNORE INTO activity_exclusions(root) VALUES (?1)",
                        [&canonical],
                    )?;
                    targets.push(canonical);
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            if !targets.is_empty() {
                let mut query = tx.prepare("SELECT id, locator FROM activity_entities")?;
                let rows = query
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (id, locator) in rows {
                    if targets.iter().any(|target| within_root(&locator, target))
                        && !ids.contains(&id)
                    {
                        ids.push(id);
                    }
                }
            }
        }
        for id in ids {
            tx.execute("DELETE FROM activity_entities WHERE id = ?1", [&id])?;
            tx.execute("DELETE FROM activity_hidden WHERE entity_id = ?1", [&id])?;
            tx.execute(
                "DELETE FROM activity_entity_summary WHERE entity_id = ?1",
                [&id],
            )?;
            tx.execute("DELETE FROM activity_daily WHERE entity_id = ?1", [&id])?;
            tx.execute(
                "DELETE FROM activity_entity_rank WHERE entity_id = ?1",
                [&id],
            )?;
        }
        tx.execute(
            "DELETE FROM activity_hidden WHERE entity_id NOT IN (SELECT id FROM activity_entities)",
            [],
        )?;
        tx.execute(
            "UPDATE activity_state SET privacy_epoch = privacy_epoch + 1 WHERE id = 1",
            [],
        )?;
        let epoch = tx.query_row(
            "SELECT privacy_epoch FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(epoch)
    }

    pub fn clear_all(&mut self) -> rusqlite::Result<u64> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("DELETE FROM activity_receipts; DELETE FROM activity_episodes; DELETE FROM activity_vocabulary; DELETE FROM activity_entities; DELETE FROM activity_hidden; DELETE FROM activity_entity_summary; DELETE FROM activity_daily; DELETE FROM activity_entity_rank; DELETE FROM activity_aliases; DELETE FROM activity_changes; DELETE FROM activity_volumes;")?;
        tx.execute(
            "UPDATE activity_state SET privacy_epoch = privacy_epoch + 1 WHERE id = 1",
            [],
        )?;
        let epoch = tx.query_row(
            "SELECT privacy_epoch FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(epoch)
    }
}

/// Identity reconciliation (ACT-06). The store never touches the filesystem:
/// callers pass receipts of changes Ferrum made, and results of stats they
/// performed themselves (lazy validation).
impl ActivityStore {
    /// Entity currently reachable at this location: by its locator, else by a
    /// verified alias. Windows-style locators compare case-insensitively and
    /// across separator spellings; POSIX locators keep case.
    pub fn entity_at_locator(
        &self,
        volume_id: &str,
        locator: &str,
    ) -> rusqlite::Result<Option<ActivityEntity>> {
        check_id(volume_id)?;
        check_locator(locator)?;
        let key = locator_key(locator);
        let id = match entity_at(&self.connection, volume_id, &key)? {
            Some(id) => Some(id),
            None => alias_at(&self.connection, volume_id, &key)?,
        };
        match id {
            Some(id) => Ok(load(&self.connection, &id)?.map(|stored| stored.entity)),
            None => Ok(None),
        }
    }

    pub fn identity_state(&self, entity_id: &str) -> rusqlite::Result<Option<IdentityState>> {
        Ok(
            load(&self.connection, entity_id)?.map(|stored| IdentityState {
                attached: stored.key.is_some(),
                entity: stored.entity,
                confidence: stored.confidence,
                platform_id: stored.platform_id,
                checked_utc_ms: stored.checked.and_then(|value| u64::try_from(value).ok()),
            }),
        )
    }

    /// Applies a rename/move/copy/delete receipt. Rename/move transfer the
    /// entity (and a folder's whole subtree, on path-segment boundaries) to the
    /// new locator keeping ids, history and rank rows. Delete marks the subtree
    /// Missing and detaches it so a new object at the same path starts fresh.
    /// Copy never inherits. Structural updates do not depend on the recording
    /// switch or privacy epoch: they only modify rows that already exist.
    pub fn apply_change(&mut self, change: &FsChange) -> rusqlite::Result<ChangeOutcome> {
        check_id(&change.action_id)?;
        check_id(&change.from_volume_id)?;
        check_locator(&change.from)?;
        let destination = match (change.kind, &change.to_volume_id, &change.to) {
            (FsChangeKind::Delete, None, None) => None,
            (FsChangeKind::Delete, _, _) => return Err(rusqlite::Error::InvalidQuery),
            (_, Some(volume), Some(to)) => {
                check_id(volume)?;
                check_locator(to)?;
                Some((volume.as_str(), to.as_str()))
            }
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        let now =
            i64::try_from(change.occurred_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let from_key = locator_key(&change.from);
        if let Some((volume, to)) = destination
            && matches!(change.kind, FsChangeKind::Rename | FsChangeKind::Move)
        {
            let to_key = locator_key(to);
            if volume == change.from_volume_id && to_key != from_key && inside(&to_key, &from_key) {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM activity_changes WHERE action_id = ?1)",
            [&change.action_id],
            |row| row.get(0),
        )?;
        if seen {
            return Ok(ChangeOutcome::Duplicate);
        }
        tx.execute(
            "INSERT INTO activity_changes(action_id, applied_utc_ms) VALUES (?1, ?2)",
            params![change.action_id, now],
        )?;
        let outcome = match (change.kind, destination) {
            (FsChangeKind::Delete, _) => {
                let rows = subtree_rows(&tx, &change.from_volume_id, &from_key)?;
                if rows.is_empty() {
                    ChangeOutcome::NotTracked
                } else {
                    for row in &rows {
                        detach(&tx, &row.id, now)?;
                    }
                    bump_revision(&tx)?;
                    ChangeOutcome::Deleted {
                        entities: rows.len() as u64,
                    }
                }
            }
            (FsChangeKind::Copy, Some((volume, to))) => {
                // The destination keeps path identity, but whatever platform id
                // it had belonged to the content that was just overwritten.
                if let Some(id) = entity_at(&tx, volume, &locator_key(to))? {
                    tx.execute(
                        "UPDATE activity_entities SET platform_id = NULL, identity = 'path' WHERE id = ?1",
                        [&id],
                    )?;
                }
                ChangeOutcome::CopyNotInherited
            }
            (_, Some((volume, to))) => {
                let rows = subtree_rows(&tx, &change.from_volume_id, &from_key)?;
                if rows.is_empty() {
                    ChangeOutcome::NotTracked
                } else {
                    let (moved, forgotten) = relocate_subtree(
                        &tx,
                        &rows,
                        &Relocation {
                            old_root_key: &from_key,
                            new_volume: volume,
                            new_root: display_norm(to),
                            root_availability: Some(Availability::Available),
                            descendant_availability: Some(Availability::Available),
                            rename_root: true,
                            keep_platform: volume == change.from_volume_id,
                            now,
                        },
                    )?;
                    bump_revision(&tx)?;
                    if forgotten > 0 {
                        tx.execute(
                            "UPDATE activity_state SET privacy_epoch = privacy_epoch + 1 WHERE id = 1",
                            [],
                        )?;
                        ChangeOutcome::Forgotten {
                            entities: forgotten,
                        }
                    } else {
                        ChangeOutcome::Relocated { entities: moved }
                    }
                }
            }
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// Records the result of a caller-side stat (lazy validation). Offline,
    /// Missing, Inaccessible and Unknown are distinct and never destructive:
    /// only a platform-id mismatch ends an identity.
    pub fn apply_validation(
        &mut self,
        entity_id: &str,
        probe: &ValidationProbe,
        now_utc_ms: u64,
    ) -> rusqlite::Result<ValidationOutcome> {
        let now = i64::try_from(now_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(stored) = load(&tx, entity_id)? else {
            return Ok(ValidationOutcome::UnknownEntity);
        };
        let before = stored.entity.availability.clone();
        let outcome = match probe {
            ValidationProbe::Present { platform_id } => {
                if stored.key.is_none() {
                    // Ended entities are never re-attached by whatever lives there now.
                    return Ok(ValidationOutcome::IdentityMismatch);
                }
                match (&stored.platform_id, platform_id) {
                    (Some(known), Some(seen)) if known != seen => {
                        detach(&tx, entity_id, now)?;
                        bump_revision(&tx)?;
                        tx.commit()?;
                        return Ok(ValidationOutcome::IdentityMismatch);
                    }
                    (Some(_), Some(_)) => {
                        tx.execute(
                            "UPDATE activity_entities SET identity = 'verified' WHERE id = ?1",
                            [entity_id],
                        )?;
                    }
                    (None, Some(seen)) => {
                        tx.execute(
                            "UPDATE activity_entities SET platform_id = ?2, identity = 'verified' WHERE id = ?1",
                            params![entity_id, seen],
                        )?;
                    }
                    _ => {}
                }
                Availability::Available
            }
            ValidationProbe::NotFound => Availability::Missing,
            ValidationProbe::AccessDenied => Availability::Inaccessible,
            ValidationProbe::VolumeUnavailable => Availability::Offline,
            ValidationProbe::Indeterminate => Availability::Unknown,
        };
        set_availability(&tx, entity_id, outcome.clone())?;
        tx.execute(
            "UPDATE activity_entities SET checked_utc_ms = ?2 WHERE id = ?1",
            params![entity_id, now],
        )?;
        if before != outcome {
            bump_revision(&tx)?;
        }
        tx.commit()?;
        Ok(ValidationOutcome::Updated(outcome))
    }

    /// An entity known Missing may be matched to a new location only with
    /// platform-id evidence on both sides, the same volume and kind, no other
    /// claimant of the id and a free target. Anything weaker (name, size,
    /// timestamps) leaves the entity unmerged and flags it Unresolved.
    pub fn reconcile_external(
        &mut self,
        entity_id: &str,
        candidate: &ExternalCandidate,
        now_utc_ms: u64,
    ) -> rusqlite::Result<ReconcileOutcome> {
        check_id(&candidate.volume_id)?;
        check_locator(&candidate.locator)?;
        let now = i64::try_from(now_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(stored) = load(&tx, entity_id)? else {
            return Ok(ReconcileOutcome::UnknownEntity);
        };
        let (Some(old_key), Availability::Missing) =
            (stored.key.clone(), stored.entity.availability.clone())
        else {
            return Ok(ReconcileOutcome::Unresolved(
                UnresolvedReason::SourceNotMissing,
            ));
        };
        let mut verdict = Ok(());
        if candidate.kind != stored.entity.kind {
            verdict = Err(UnresolvedReason::KindMismatch);
        } else if candidate.volume_id != stored.entity.volume_id {
            verdict = Err(UnresolvedReason::VolumeMismatch);
        } else if let (Some(known), Some(seen)) = (&stored.platform_id, &candidate.platform_id) {
            if known != seen {
                verdict = Err(UnresolvedReason::PlatformIdMismatch);
            } else {
                let others: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM activity_entities WHERE volume_id = ?1 AND platform_id = ?2 AND id != ?3",
                    params![candidate.volume_id, seen, entity_id],
                    |row| row.get(0),
                )?;
                if others > 0 {
                    verdict = Err(UnresolvedReason::Ambiguous);
                }
            }
        } else {
            verdict = Err(UnresolvedReason::NoPlatformEvidence);
        }
        let new_key = locator_key(&candidate.locator);
        if verdict.is_ok()
            && new_key != old_key
            && entity_at(&tx, &candidate.volume_id, &new_key)?.is_some()
        {
            verdict = Err(UnresolvedReason::TargetTracked);
        }
        if let Err(reason) = verdict {
            tx.execute(
                "UPDATE activity_entities SET identity = 'unresolved' WHERE id = ?1",
                [entity_id],
            )?;
            tx.commit()?;
            return Ok(ReconcileOutcome::Unresolved(reason));
        }
        if new_key != old_key {
            let rows = subtree_rows(&tx, &stored.entity.volume_id, &old_key)?;
            let (_, forgotten) = relocate_subtree(
                &tx,
                &rows,
                &Relocation {
                    old_root_key: &old_key,
                    new_volume: &candidate.volume_id,
                    new_root: display_norm(&candidate.locator),
                    root_availability: Some(Availability::Available),
                    descendant_availability: Some(Availability::Unknown),
                    rename_root: true,
                    keep_platform: true,
                    now,
                },
            )?;
            if forgotten > 0 {
                tx.execute(
                    "UPDATE activity_state SET privacy_epoch = privacy_epoch + 1, data_revision = data_revision + 1 WHERE id = 1",
                    [],
                )?;
                tx.commit()?;
                return Ok(ReconcileOutcome::Excluded);
            }
        } else {
            set_availability(&tx, entity_id, Availability::Available)?;
        }
        tx.execute(
            "UPDATE activity_entities SET identity = 'verified', checked_utc_ms = ?2 WHERE id = ?1",
            params![entity_id, now],
        )?;
        bump_revision(&tx)?;
        tx.commit()?;
        Ok(ReconcileOutcome::Merged)
    }

    /// Declares that `volume_id` is currently mounted at `mount` (e.g. `E:`),
    /// as established by the caller from the OS. A volume that reappears under
    /// another mount point has its entity locators re-prefixed (same ids); the
    /// previous owner of this mount point goes Offline, so a recycled drive
    /// letter can never retarget the old volume's history.
    pub fn mount_volume(
        &mut self,
        volume_id: &str,
        mount: &str,
        now_utc_ms: u64,
    ) -> rusqlite::Result<MountOutcome> {
        check_id(volume_id)?;
        check_locator(&if mount.len() == 2 && mount.ends_with(':') {
            format!("{mount}\\")
        } else {
            mount.to_owned()
        })?;
        let now = i64::try_from(now_utc_ms).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let display = normalize_mount(mount);
        let key = locator_key(&display);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT mount, mount_key FROM activity_volumes WHERE volume_id = ?1",
                [volume_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let other: Option<String> = tx
            .query_row(
                "SELECT volume_id FROM activity_volumes WHERE mount_key = ?1 AND mounted = 1 AND volume_id != ?2",
                params![key, volume_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut offlined_other = 0;
        if let Some(other) = other {
            tx.execute(
                "UPDATE activity_volumes SET mounted = 0 WHERE volume_id = ?1",
                [&other],
            )?;
            offlined_other = bulk_availability(&tx, &other, None, Availability::Offline, true)?;
        }
        let mut rebased = 0;
        if let Some((_, previous_key)) = &previous
            && *previous_key != key
        {
            let rows = subtree_rows(&tx, volume_id, previous_key)?;
            rebased = relocate_subtree(
                &tx,
                &rows,
                &Relocation {
                    old_root_key: previous_key,
                    new_volume: volume_id,
                    new_root: display.clone(),
                    root_availability: None,
                    descendant_availability: None,
                    rename_root: false,
                    keep_platform: true,
                    now,
                },
            )?
            .0;
        }
        // Back online but unverified: Unknown until a lazy check says otherwise.
        let revived = bulk_availability(
            &tx,
            volume_id,
            Some(Availability::Offline),
            Availability::Unknown,
            false,
        )?;
        tx.execute(
            "INSERT INTO activity_volumes(volume_id, mount, mount_key, mounted, last_seen_utc_ms)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(volume_id) DO UPDATE SET
               mount = excluded.mount, mount_key = excluded.mount_key, mounted = 1,
               last_seen_utc_ms = excluded.last_seen_utc_ms",
            params![volume_id, display, key, now],
        )?;
        if offlined_other + rebased + revived > 0 {
            bump_revision(&tx)?;
        }
        tx.commit()?;
        Ok(MountOutcome {
            rebased,
            offlined_other,
            previous_mount: previous.map(|(mount, _)| mount),
        })
    }

    /// The caller learned the volume is gone (ejected, share unreachable):
    /// its entities become Offline, never Missing. Returns how many changed.
    pub fn unmount_volume(&mut self, volume_id: &str) -> rusqlite::Result<u64> {
        check_id(volume_id)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE activity_volumes SET mounted = 0 WHERE volume_id = ?1",
            [volume_id],
        )?;
        let changed = bulk_availability(&tx, volume_id, None, Availability::Offline, true)?;
        if changed > 0 {
            bump_revision(&tx)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Re-tags entities recorded under a placeholder volume id (the previous
    /// service used the drive letter, e.g. `C:`) with a real volume identity.
    /// The caller asserts the legacy label currently denotes that volume.
    pub fn adopt_legacy_volume(
        &mut self,
        legacy_volume_id: &str,
        volume_id: &str,
    ) -> rusqlite::Result<u64> {
        check_id(legacy_volume_id)?;
        check_id(volume_id)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statement =
                tx.prepare("SELECT id FROM activity_entities WHERE volume_id = ?1")?;
            statement
                .query_map([legacy_volume_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut adopted = 0;
        for id in ids {
            let changed = tx.execute(
                "UPDATE OR IGNORE activity_entities SET volume_id = ?2 WHERE id = ?1",
                params![id, volume_id],
            )?;
            if changed == 1
                && let Some(mut stored) = load(&tx, &id)?
            {
                stored.entity.volume_id = volume_id.to_owned();
                save_entity(&tx, &stored.entity, stored.key.as_deref(), false)?;
                adopted += 1;
            }
        }
        if adopted > 0 {
            bump_revision(&tx)?;
        }
        tx.commit()?;
        Ok(adopted)
    }

    /// Records that `alias_locator` (symlink/junction spelling) denotes the
    /// entity, as verified by the caller. Aliases resolve to the same entity
    /// and exclusions covering an alias also cover the target.
    pub fn link_alias(
        &mut self,
        entity_id: &str,
        alias_locator: &str,
    ) -> rusqlite::Result<AliasOutcome> {
        check_locator(alias_locator)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(stored) = load(&tx, entity_id)? else {
            return Ok(AliasOutcome::UnknownEntity);
        };
        let Some(own_key) = stored.key.clone() else {
            return Ok(AliasOutcome::UnknownEntity);
        };
        let volume = stored.entity.volume_id.clone();
        let key = locator_key(alias_locator);
        if key == own_key || alias_at(&tx, &volume, &key)?.as_deref() == Some(entity_id) {
            return Ok(AliasOutcome::AlreadyLinked);
        }
        if entity_at(&tx, &volume, &key)?.is_some() || alias_at(&tx, &volume, &key)?.is_some() {
            return Ok(AliasOutcome::Conflict);
        }
        if excluded(&tx, alias_locator)? {
            tx.execute(
                "INSERT OR IGNORE INTO activity_exclusions(root) VALUES (?1)",
                [&stored.entity.locator],
            )?;
            tx.execute("DELETE FROM activity_entities WHERE id = ?1", [entity_id])?;
            tx.execute(
                "DELETE FROM activity_hidden WHERE entity_id = ?1",
                [entity_id],
            )?;
            tx.execute(
                "UPDATE activity_state SET privacy_epoch = privacy_epoch + 1, data_revision = data_revision + 1 WHERE id = 1",
                [],
            )?;
            tx.commit()?;
            return Ok(AliasOutcome::ExcludedTarget);
        }
        tx.execute(
            "INSERT INTO activity_aliases(volume_id, alias_key, alias_locator, entity_id) VALUES (?1, ?2, ?3, ?4)",
            params![volume, key, display_norm(alias_locator), entity_id],
        )?;
        tx.commit()?;
        Ok(AliasOutcome::Linked)
    }
}

/// Rank key = log2 of the capped daily counts weighted 2^(day/half-life).
/// Independent of the query day, so it orders entities by Frequent score.
fn is_corruption(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
            )
    )
}

/// Moves the database (and any journal/WAL sidecars) to `<name>.corrupt-<ms>[-n]`.
fn quarantine(path: &Path) -> rusqlite::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| rusqlite::Error::InvalidPath(path.to_owned()))?;
    let stamp = now_ms();
    let mut suffix = format!("corrupt-{stamp}");
    let mut attempt = 0_u32;
    while path.with_file_name(format!("{name}.{suffix}")).exists() {
        attempt += 1;
        suffix = format!("corrupt-{stamp}-{attempt}");
    }
    let target = path.with_file_name(format!("{name}.{suffix}"));
    std::fs::rename(path, &target).map_err(|_| rusqlite::Error::InvalidPath(path.to_owned()))?;
    for ext in ["-journal", "-wal", "-shm"] {
        let side = path.with_file_name(format!("{name}{ext}"));
        if side.exists() {
            let _ = std::fs::rename(&side, path.with_file_name(format!("{name}.{suffix}{ext}")));
        }
    }
    Ok(())
}

fn rank_key(days: &[(i64, u64)]) -> (f64, u64) {
    let reference = days.iter().map(|(day, _)| *day).max().unwrap_or(0);
    let weighted: f64 = days
        .iter()
        .map(|(day, count)| {
            (*count.min(&5) as f64) * 2.0_f64.powf((day - reference) as f64 / HALF_LIFE_DAYS)
        })
        .sum();
    let key = weighted.log2() + reference as f64 / HALF_LIFE_DAYS;
    // Rounded so float noise cannot split genuine ties.
    ((key * 1e9).round() / 1e9, days.len() as u64)
}

fn refresh_rank(tx: &Connection, entity_id: &str, kind: &str) -> rusqlite::Result<()> {
    let days = {
        let mut query = tx.prepare_cached(
            "SELECT utc_day, episodes, last_utc_ms FROM activity_daily WHERE entity_id = ?1 AND +kind = ?2",
        )?;
        let rows = query.query_map(params![entity_id, kind], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let last = days.iter().map(|(_, _, last)| *last).max().unwrap_or(0);
    let days: Vec<(i64, u64)> = days.into_iter().map(|(d, c, _)| (d, c)).collect();
    if days.is_empty() {
        tx.execute(
            "DELETE FROM activity_entity_rank WHERE entity_id = ?1 AND kind = ?2",
            params![entity_id, kind],
        )?;
        return Ok(());
    }
    let (key, active_days) = rank_key(&days);
    tx.execute(
        "INSERT INTO activity_entity_rank(entity_id, kind, rank_key, active_days, last_utc_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(entity_id, kind) DO UPDATE SET
           rank_key = excluded.rank_key, active_days = excluded.active_days,
           last_utc_ms = excluded.last_utc_ms",
        params![entity_id, kind, key, active_days, last],
    )?;
    Ok(())
}

/// Recomputes rank rows for every pair listed in `temp.purge_pairs` with one
/// ordered read of their remaining daily buckets and one insert pass.
fn refresh_rank_for_purged(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    let mut query = tx.prepare(
        "SELECT p.entity_id, p.kind, d.utc_day, d.episodes, d.last_utc_ms
         FROM purge_pairs p
         JOIN activity_daily d ON d.entity_id = p.entity_id AND +d.kind = p.kind
         ORDER BY p.entity_id, p.kind, d.utc_day",
    )?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(query);
    tx.execute(
        "DELETE FROM activity_entity_rank
         WHERE EXISTS (SELECT 1 FROM purge_pairs p
                       WHERE p.entity_id = activity_entity_rank.entity_id
                         AND p.kind = activity_entity_rank.kind)",
        [],
    )?;
    let mut insert = tx.prepare(
        "INSERT INTO activity_entity_rank(entity_id, kind, rank_key, active_days, last_utc_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut flush =
        |id: &str, kind: &str, days: &[(i64, u64)], last: i64| -> rusqlite::Result<()> {
            let (key, active) = rank_key(days);
            insert.execute(params![id, kind, key, active, last])?;
            Ok(())
        };
    let mut current: Option<(String, String)> = None;
    let mut days: Vec<(i64, u64)> = Vec::new();
    let mut last = 0i64;
    for (id, kind, day, count, bucket_last) in rows {
        if current
            .as_ref()
            .is_none_or(|(cid, ckind)| *cid != id || *ckind != kind)
        {
            if let Some((cid, ckind)) = current.take() {
                flush(&cid, &ckind, &days, last)?;
            }
            days.clear();
            last = 0;
            current = Some((id, kind));
        }
        days.push((day, count));
        last = last.max(bucket_last);
    }
    if let Some((cid, ckind)) = current {
        flush(&cid, &ckind, &days, last)?;
    }
    Ok(())
}

/// Rebuilds the derived rank table from the daily buckets (migration, tests).
pub fn rebuild_rank(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM activity_entity_rank", [])?;
    let mut query = tx.prepare(
        "SELECT entity_id, kind, utc_day, episodes, last_utc_ms FROM activity_daily ORDER BY entity_id, kind",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, u64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut insert = tx.prepare(
        "INSERT INTO activity_entity_rank(entity_id, kind, rank_key, active_days, last_utc_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut current: Option<(String, String)> = None;
    let mut days: Vec<(i64, u64)> = Vec::new();
    let mut last = 0i64;
    for row in rows {
        let (id, kind, day, count, bucket_last) = row?;
        if current.as_ref() != Some(&(id.clone(), kind.clone())) {
            if let Some((cid, ckind)) = current.take() {
                let (key, active) = rank_key(&days);
                insert.execute(params![cid, ckind, key, active, last])?;
            }
            days.clear();
            last = 0;
            current = Some((id, kind));
        }
        days.push((day, count));
        last = last.max(bucket_last);
    }
    if let Some((cid, ckind)) = current {
        let (key, active) = rank_key(&days);
        insert.execute(params![cid, ckind, key, active, last])?;
    }
    Ok(())
}

struct Stored {
    entity: ActivityEntity,
    key: Option<String>,
    platform_id: Option<String>,
    confidence: IdentityConfidence,
    checked: Option<i64>,
}

struct SubtreeRow {
    id: String,
    locator: String,
    key: String,
}

struct Relocation<'a> {
    old_root_key: &'a str,
    new_volume: &'a str,
    /// Display form (see `display_norm`) of the new root locator.
    new_root: String,
    /// `None` keeps the stored availability.
    root_availability: Option<Availability>,
    descendant_availability: Option<Availability>,
    rename_root: bool,
    keep_platform: bool,
    now: i64,
}

fn parse_entity(metadata: &str) -> rusqlite::Result<ActivityEntity> {
    serde_json::from_str(metadata).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn check_id(value: &str) -> rusqlite::Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.contains('\0') {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

fn check_locator(value: &str) -> rusqlite::Result<()> {
    if !is_absolute(value)
        || value.len() > 4096
        || value.contains('\0')
        || value.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

fn bump_revision(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE activity_state SET data_revision = data_revision + 1 WHERE id = 1",
        [],
    )?;
    Ok(())
}

fn load(conn: &Connection, id: &str) -> rusqlite::Result<Option<Stored>> {
    let row = conn
        .query_row(
            "SELECT metadata, locator_key, platform_id, identity, checked_utc_ms
             FROM activity_entities WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|(metadata, key, platform_id, identity, checked)| {
        Ok(Stored {
            entity: parse_entity(&metadata)?,
            key,
            platform_id,
            confidence: IdentityConfidence::parse(&identity),
            checked,
        })
    })
    .transpose()
}

fn entity_at(conn: &Connection, volume: &str, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM activity_entities WHERE volume_id = ?1 AND locator_key = ?2",
        params![volume, key],
        |row| row.get(0),
    )
    .optional()
}

fn alias_at(conn: &Connection, volume: &str, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT entity_id FROM activity_aliases WHERE volume_id = ?1 AND alias_key = ?2",
        params![volume, key],
        |row| row.get(0),
    )
    .optional()
}

/// Writes the entity's mutable columns and its JSON snapshot together.
fn save_entity(
    conn: &Connection,
    entity: &ActivityEntity,
    key: Option<&str>,
    name_changed: bool,
) -> rusqlite::Result<()> {
    let metadata = serde_json::to_string(entity).map_err(|_| rusqlite::Error::InvalidQuery)?;
    conn.execute(
        "UPDATE activity_entities
         SET volume_id = ?2, locator = ?3, parent_path = ?4, display_name = ?5,
             availability = ?6, metadata = ?7, locator_key = ?8
         WHERE id = ?1",
        params![
            entity.id,
            entity.volume_id,
            entity.locator,
            entity.parent_path,
            entity.display_name,
            format!("{:?}", entity.availability),
            metadata,
            key
        ],
    )?;
    if name_changed {
        conn.execute(
            "INSERT INTO activity_vocabulary(entity_id, search_key) VALUES (?1, ?2)
             ON CONFLICT(entity_id) DO UPDATE SET search_key = excluded.search_key",
            params![entity.id, entity.display_name.to_lowercase()],
        )?;
        index_name(
            conn,
            &entity.id,
            &format!("{:?}", entity.kind),
            &entity.display_name,
        )?;
    }
    Ok(())
}

fn set_availability(
    conn: &Connection,
    id: &str,
    availability: Availability,
) -> rusqlite::Result<()> {
    if let Some(mut stored) = load(conn, id)?
        && stored.entity.availability != availability
    {
        stored.entity.availability = availability;
        save_entity(conn, &stored.entity, stored.key.as_deref(), false)?;
    }
    Ok(())
}

/// Sets availability for a volume's entities. `only_from` restricts to one
/// current state; `attached_only` skips detached (ended) entities.
fn bulk_availability(
    conn: &Connection,
    volume: &str,
    only_from: Option<Availability>,
    to: Availability,
    attached_only: bool,
) -> rusqlite::Result<u64> {
    let ids = {
        let mut statement = conn.prepare(
            "SELECT id FROM activity_entities WHERE volume_id = ?1
               AND (?2 IS NULL OR availability = ?2)
               AND (?3 = 0 OR locator_key IS NOT NULL)",
        )?;
        let from = only_from.map(|value| format!("{value:?}"));
        statement
            .query_map(params![volume, from, attached_only], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut changed = 0;
    for id in ids {
        if let Some(mut stored) = load(conn, &id)?
            && stored.entity.availability != to
        {
            stored.entity.availability = to.clone();
            save_entity(conn, &stored.entity, stored.key.as_deref(), false)?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Ends an entity: Missing, detached from its locator (so a new object at the
/// same path starts fresh), platform id dropped (file ids can be recycled).
/// History rows stay; they are removed only by forget/exclude/retention.
fn detach(conn: &Connection, id: &str, now: i64) -> rusqlite::Result<()> {
    set_availability(conn, id, Availability::Missing)?;
    conn.execute(
        "UPDATE activity_entities
         SET locator_key = NULL, platform_id = NULL, identity = 'ended', checked_utc_ms = ?2
         WHERE id = ?1",
        params![id, now],
    )?;
    conn.execute("DELETE FROM activity_aliases WHERE entity_id = ?1", [id])?;
    Ok(())
}

fn subtree_rows(conn: &Connection, volume: &str, key: &str) -> rusqlite::Result<Vec<SubtreeRow>> {
    let (low, high) = subtree_bounds(key);
    let mut statement = conn.prepare(
        "SELECT id, locator, locator_key FROM activity_entities
         WHERE volume_id = ?1 AND locator_key IS NOT NULL
           AND (locator_key = ?2 OR (locator_key >= ?3 AND locator_key < ?4))
         ORDER BY locator_key",
    )?;
    statement
        .query_map(params![volume, key, low, high], |row| {
            Ok(SubtreeRow {
                id: row.get(0)?,
                locator: row.get(1)?,
                key: row.get(2)?,
            })
        })?
        .collect()
}

/// Moves a located subtree to a new root, keeping every entity id. Entities
/// that currently occupy a destination locator (and are not part of the move)
/// are detached as Missing: they were overwritten, but their history stays.
/// Returns (relocated, forgotten); forgotten entities landed in an excluded root.
fn relocate_subtree(
    conn: &Connection,
    rows: &[SubtreeRow],
    plan: &Relocation<'_>,
) -> rusqlite::Result<(u64, u64)> {
    let new_root_trimmed = plan.new_root.trim_end_matches(['\\', '/']).to_owned();
    let skip = plan.old_root_key.chars().count();
    let moving: BTreeSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        let locator = if row.key == plan.old_root_key {
            plan.new_root.clone()
        } else {
            let rest: String = display_norm(&row.locator).chars().skip(skip).collect();
            format!("{new_root_trimmed}{rest}")
        };
        let key = locator_key(&locator);
        targets.push((row, locator, key));
    }
    for (_, _, key) in &targets {
        if let Some(occupant) = entity_at(conn, plan.new_volume, key)?
            && !moving.contains(occupant.as_str())
        {
            detach(conn, &occupant, plan.now)?;
        }
    }
    for row in rows {
        conn.execute(
            "UPDATE activity_entities SET locator_key = NULL WHERE id = ?1",
            [&row.id],
        )?;
    }
    let (mut moved, mut forgotten) = (0, 0);
    for (row, locator, key) in targets {
        let Some(mut stored) = load(conn, &row.id)? else {
            continue;
        };
        let is_root = row.key == plan.old_root_key;
        let old_name = stored.entity.display_name.clone();
        let changed_place = key != row.key || stored.entity.volume_id != plan.new_volume;
        stored.entity.volume_id = plan.new_volume.to_owned();
        stored.entity.parent_path = parent_of(&locator);
        stored.entity.locator = locator.clone();
        if is_root && plan.rename_root {
            stored.entity.display_name = base_name_of(&locator);
        }
        let wanted = if is_root {
            &plan.root_availability
        } else {
            &plan.descendant_availability
        };
        if let Some(availability) = wanted {
            stored.entity.availability = availability.clone();
        }
        let name_changed = old_name != stored.entity.display_name;
        save_entity(conn, &stored.entity, Some(&key), name_changed)?;
        conn.execute(
            "UPDATE activity_entities SET relocated = relocated | ?2 WHERE id = ?1",
            params![row.id, changed_place],
        )?;
        if is_root && matches!(plan.root_availability, Some(Availability::Available)) {
            conn.execute(
                "UPDATE activity_entities SET checked_utc_ms = ?2 WHERE id = ?1",
                params![row.id, plan.now],
            )?;
        }
        if !plan.keep_platform {
            conn.execute(
                "UPDATE activity_entities SET platform_id = NULL, identity = 'path' WHERE id = ?1",
                [&row.id],
            )?;
        }
        conn.execute(
            "DELETE FROM activity_aliases WHERE entity_id = ?1",
            [&row.id],
        )?;
        if excluded(conn, &locator)? {
            conn.execute("DELETE FROM activity_entities WHERE id = ?1", [&row.id])?;
            conn.execute(
                "DELETE FROM activity_hidden WHERE entity_id = ?1",
                [&row.id],
            )?;
            forgotten += 1;
        } else {
            moved += 1;
        }
    }
    Ok((moved, forgotten))
}

/// Chooses the entity a receipt belongs to. Same location (after case /
/// separator folding) => same entity even if the id differs (renamed/moved
/// earlier). A verified alias resolves to its target. A new object whose
/// proposed id is already owned by another identity (moved away, ended, other
/// volume) gets a fresh id so history is never merged by accident.
fn resolve_entity(
    conn: &Connection,
    entity: &ActivityEntity,
    now: i64,
) -> rusqlite::Result<ActivityEntity> {
    let key = locator_key(&entity.locator);
    let kind = format!("{:?}", entity.kind);
    if let Some(id) = entity_at(conn, &entity.volume_id, &key)? {
        let stored_kind: String = conn.query_row(
            "SELECT kind FROM activity_entities WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )?;
        if stored_kind == kind {
            let mut resolved = entity.clone();
            resolved.id = id;
            return Ok(resolved);
        }
        // A file replaced a folder (or vice versa) at this path.
        detach(conn, &id, now)?;
    } else if let Some(id) = alias_at(conn, &entity.volume_id, &key)?
        && let Some(stored) = load(conn, &id)?
        && stored.entity.kind == entity.kind
    {
        let mut resolved = stored.entity;
        resolved.availability = entity.availability.clone();
        return Ok(resolved);
    }
    let holder: Option<(String, Option<String>, bool)> = conn
        .query_row(
            "SELECT volume_id, locator_key, relocated FROM activity_entities WHERE id = ?1",
            [&entity.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((volume, holder_key, relocated)) = holder else {
        return Ok(entity.clone());
    };
    // A fresh identity is minted only with positive evidence that the id's
    // previous owner is gone from that spot: it ended, it was moved away, or
    // its volume is registered as no longer mounted (drive letter handed to
    // another volume). Anything else is an unexplained conflict: refuse.
    let displaced_volume = volume != entity.volume_id && {
        let mounted: Option<bool> = conn
            .query_row(
                "SELECT mounted FROM activity_volumes WHERE volume_id = ?1",
                [&volume],
                |row| row.get(0),
            )
            .optional()?;
        mounted == Some(false)
    };
    if !(holder_key.is_none() || relocated || displaced_volume) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let base: String = entity.id.chars().take(100).collect();
    for n in 1_u32.. {
        let candidate = format!("{base}~{n}");
        let taken: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM activity_entities WHERE id = ?1)",
            [&candidate],
            |row| row.get(0),
        )?;
        if !taken {
            let mut fresh = entity.clone();
            fresh.id = candidate;
            return Ok(fresh);
        }
    }
    Err(rusqlite::Error::InvalidQuery)
}

fn upsert_entity(
    tx: &Connection,
    entity: &ActivityEntity,
    observed_ms: i64,
) -> rusqlite::Result<()> {
    let metadata = serde_json::to_string(entity).map_err(|_| rusqlite::Error::InvalidQuery)?;
    tx.execute(
        "INSERT INTO activity_entities(id, kind, volume_id, locator, parent_path, display_name, availability, metadata, locator_key, checked_utc_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET locator=excluded.locator, parent_path=excluded.parent_path, display_name=excluded.display_name,
           availability=excluded.availability, metadata=excluded.metadata, locator_key=excluded.locator_key,
           checked_utc_ms=excluded.checked_utc_ms",
        params![entity.id, format!("{:?}", entity.kind), entity.volume_id, entity.locator, entity.parent_path, entity.display_name, format!("{:?}", entity.availability), metadata, locator_key(&entity.locator), observed_ms],
    )?;
    tx.execute("INSERT INTO activity_vocabulary(entity_id, search_key) VALUES (?1, ?2) ON CONFLICT(entity_id) DO UPDATE SET search_key=excluded.search_key", params![entity.id, entity.display_name.to_lowercase()])?;
    index_name(
        tx,
        &entity.id,
        &format!("{:?}", entity.kind),
        &entity.display_name,
    )?;
    Ok(())
}

// ---- locator normalization -------------------------------------------

fn windows_like(path: &str) -> bool {
    path.starts_with("\\\\")
        || (path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
}

fn fold_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// Canonical *display* spelling: no extended-length prefix, backslashes for
/// Windows-style paths, no trailing separator (drive roots keep `C:\`). Case is
/// preserved, so `key == fold(display_norm)` except a drive root's trailing `\`.
fn display_norm(locator: &str) -> String {
    let stripped = if let Some(rest) = locator.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = locator.strip_prefix("\\\\?\\") {
        rest.to_owned()
    } else {
        locator.to_owned()
    };
    if windows_like(&stripped) {
        let mut text = stripped.replace('/', "\\");
        while text.len() > 3 && text.ends_with('\\') {
            text.pop();
        }
        if text.len() == 2 && text.ends_with(':') {
            text.push('\\');
        }
        text
    } else {
        let trimmed = stripped.trim_end_matches('/');
        if trimmed.is_empty() {
            "/".to_owned()
        } else {
            trimmed.to_owned()
        }
    }
}

/// Lookup key of a location. Windows-style locators (drive or UNC) fold case
/// and separator spelling because NTFS/SMB are case-insensitive by default;
/// POSIX locators keep case. Per-directory case-sensitivity on Windows is not
/// detected (documented limitation).
fn locator_key(locator: &str) -> String {
    let display = display_norm(locator);
    if windows_like(&display) {
        display
            .trim_end_matches('\\')
            .chars()
            .map(fold_char)
            .collect()
    } else {
        display
    }
}

fn key_separator(key: &str) -> (char, char) {
    if windows_like(key) {
        ('\\', ']')
    } else {
        ('/', '0')
    }
}

/// Half-open key range holding exactly the strict descendants of `key`.
fn subtree_bounds(key: &str) -> (String, String) {
    let (separator, next) = key_separator(key);
    let mut low = key.to_owned();
    if !low.ends_with(separator) {
        low.push(separator);
    }
    let mut high = low.clone();
    high.pop();
    high.push(next);
    (low, high)
}

fn inside(key: &str, root: &str) -> bool {
    let (low, high) = subtree_bounds(root);
    key >= low.as_str() && key < high.as_str()
}

fn parent_of(locator: &str) -> String {
    let separator = if windows_like(locator) { '\\' } else { '/' };
    match locator.rfind(separator) {
        Some(index) if windows_like(locator) && index <= 2 => {
            locator[..3.min(locator.len())].to_owned()
        }
        Some(0) => "/".to_owned(),
        Some(index) => locator[..index].to_owned(),
        None => locator.to_owned(),
    }
}

fn base_name_of(locator: &str) -> String {
    let trimmed = locator.trim_end_matches(['\\', '/']);
    match trimmed.rsplit(['\\', '/']).next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => locator.to_owned(),
    }
}

fn normalize_mount(mount: &str) -> String {
    let mut display = display_norm(mount);
    if display.as_bytes().get(1) == Some(&b':') {
        display = display[..1].to_uppercase() + &display[1..];
    }
    // `E:\` and `E:` are the same mount; keep the bare drive form.
    if display.len() == 3 && display.ends_with(":\\") {
        display.pop();
    }
    display
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn search_grams(text: &str) -> Vec<String> {
    let name = text.trim().to_lowercase();
    let mut grams = BTreeSet::new();
    for word in name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() < 3 {
            grams.insert(word.to_owned());
        } else {
            for window in chars.windows(3) {
                grams.insert(window.iter().collect());
            }
        }
    }
    grams.into_iter().collect()
}

fn id_column(row: &rusqlite::Row<'_>) -> rusqlite::Result<String> {
    row.get(0)
}

/// Lowercased alphanumeric words of a name, bounded in length.
fn search_tokens(text: &str) -> BTreeSet<String> {
    text.trim()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().take(TOKEN_MAX_CHARS).collect())
        .collect()
}

/// Abbreviation keys of a multi-word name: the word initials, and the initials
/// of the words before a trailing number followed by that number (the shapes
/// the matcher accepts as `Initialism`).
fn search_initials(text: &str) -> BTreeSet<String> {
    let lowered = text.trim().to_lowercase();
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut keys = BTreeSet::new();
    if words.len() < 2 {
        return keys;
    }
    let initials =
        |slice: &[&str]| -> String { slice.iter().filter_map(|w| w.chars().next()).collect() };
    keys.insert(initials(&words));
    if let Some((number, preceding)) = words.split_last()
        && !preceding.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
    {
        keys.insert(format!("{}{number}", initials(preceding)));
    }
    keys
}

/// Workspace filter: `locator_key` equals the root key or lies in its subtree.
struct RootScope {
    key: String,
    low: String,
    high: String,
}

fn root_scope(root: Option<&str>) -> rusqlite::Result<Option<RootScope>> {
    let Some(root) = root else {
        return Ok(None);
    };
    if root.trim().is_empty() || root.len() > 1024 || !is_absolute(root) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let key = locator_key(root);
    let (low, high) = subtree_bounds(&key);
    Ok(Some(RootScope { key, low, high }))
}

const SCOPE_CLAUSE: &str =
    "(e.locator_key = ?{k} OR (e.locator_key >= ?{l} AND e.locator_key < ?{h}))";

/// SQL predicate on alias `e` with the three scope parameters at the given indices.
fn scope_clause(k: usize, l: usize, h: usize) -> String {
    SCOPE_CLAUSE
        .replace("{k}", &k.to_string())
        .replace("{l}", &l.to_string())
        .replace("{h}", &h.to_string())
}

fn index_name(tx: &Connection, id: &str, kind: &str, name: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM activity_search_grams WHERE entity_id = ?1",
        [id],
    )?;
    let mut statement =
        tx.prepare("INSERT INTO activity_search_grams(kind, gram, entity_id) VALUES (?1, ?2, ?3)")?;
    for gram in search_grams(name) {
        statement.execute(params![kind, gram, id])?;
    }
    for token in search_tokens(name) {
        statement.execute(params![kind, format!("{TOKEN_MARK}{token}"), id])?;
    }
    for initials in search_initials(name) {
        statement.execute(params![kind, format!("{INITIALS_MARK}{initials}"), id])?;
    }
    Ok(())
}

fn entity_kind_name(kind: &ActivityKind) -> &'static str {
    match kind {
        ActivityKind::FolderVisit => "Folder",
        ActivityKind::FileOpenRequested => "File",
        _ => unreachable!(),
    }
}

fn kind_name(kind: &ActivityKind) -> &'static str {
    match kind {
        ActivityKind::FolderVisit => "folderVisit",
        ActivityKind::FileOpenRequested => "fileOpenRequested",
        ActivityKind::FilePreview => "filePreview",
        ActivityKind::OperationCompleted => "operationCompleted",
    }
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'\\' | b'/'))
}

fn within_root(path: &str, root: &str) -> bool {
    let (path, root) = if root.starts_with("\\\\") || root.as_bytes().get(1) == Some(&b':') {
        (
            path.replace('\\', "/").to_lowercase(),
            root.replace('\\', "/").to_lowercase(),
        )
    } else {
        (path.to_owned(), root.to_owned())
    };
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return path.starts_with('/');
    }
    path.trim_end_matches('/') == root
        || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/'))
}

fn excluded(tx: &Connection, path: &str) -> rusqlite::Result<bool> {
    let mut statement = tx.prepare("SELECT root FROM activity_exclusions")?;
    let roots = statement.query_map([], |row| row.get::<_, String>(0))?;
    for root in roots {
        if within_root(path, &root?) {
            return Ok(true);
        }
    }
    Ok(false)
}
