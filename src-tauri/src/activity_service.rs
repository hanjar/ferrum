//! Activity service: owns the SQLite store behind a mutex and maps committed
//! filesystem events to contract receipts. The renderer never touches SQL;
//! privacy failures degrade to "not recorded", never to broken navigation.

use crate::activity_chat_tool::{
    ActivityReader, ChatRequest, ChatResponse, ChatSnapshot, run_chat_tool,
};
use crate::activity_matcher::MatchClass;
use crate::activity_matcher::{MatchResult, compare_matches, match_candidate};
use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, MatchKind, Provenance,
    ReasonCode, ReceiptSource,
};
#[cfg(test)]
use ferrum_activity_store::RecordOutcome;
use ferrum_activity_store::{
    ActivityStore, FrequentRow, FsChange, FsChangeKind, Popularity, RecentRow, ValidationOutcome,
    ValidationProbe,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Retention default from the spec: 90 days of raw episodes (7/30/90 allowed).
const DEFAULT_RETENTION_DAYS: u32 = 90;
const RETENTION_CHOICES: [u32; 3] = [7, 30, 90];
const DAY_MS: u64 = 86_400_000;
/// Overview scan ceiling per kind: 10 pages x 100 rows; beyond this the UI is
/// told the view is truncated instead of claiming completeness.
const SCAN_PAGES: usize = 10;
const SCAN_PAGE_SIZE: u16 = 100;
const SETTINGS_FILE: &str = "activity-settings.json";
/// Candidate ceiling per gram seek inside the store; scoring re-ranks these.
const CANDIDATE_POOL: u16 = 100;
/// A result counts as "recent" for the UI reason when last used this recently.
const RECENT_REASON_MS: u64 = 7 * DAY_MS;
/// ... and as "frequent" with at least this many episodes on 2+ UTC days.
const FREQUENT_REASON_EPISODES: u64 = 3;
const FREQUENT_REASON_DAYS: u64 = 2;
/// Lazy validation: an entity checked more recently than this is not stat'ed
/// again. Never per keystroke or per query.
const VALIDATION_TTL_MS: u64 = 5 * 60_000;
/// A stat on a dead share can hang; give up and report Indeterminate.
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);
/// Bounded queue of identity jobs; when full the receipt is dropped (counted),
/// file operations never wait on the database.
const JOB_QUEUE: usize = 256;
/// Most visit receipts applied in one transaction (one fsync) by the worker.
const VISIT_BATCH: usize = 64;
/// FER-432: retention is re-enforced by the identity worker at this cadence
/// (besides startup and `set_retention`), so a long-running session never
/// keeps episodes older than the window on disk for more than this long.
const PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// FER-432: most hidden ids kept in `activity-settings.json`. Hiding one more
/// drops the OLDEST hidden id (first in the list, FIFO); that item simply
/// becomes suggestible again. Ids of entities that no longer exist are pruned
/// on purge/exclude/forget/clear, so the cap is only hit by real items.
const MAX_HIDDEN: usize = 5000;

static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// What the renderer observed. Only two receipt shapes are accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitKind {
    Folder,
    FileOpen,
}

/// Query scope: folders only, files only, or both merged by match class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Folder,
    File,
    All,
}

/// Owned by Tauri state; `None` means the store could not be opened this run.
pub struct ActivityHandle(pub Option<ActivityService>);

/// FER-432 #5: `activity.query` may be offered to a provider only when the
/// service is open AND the first-run recording disclosure was acknowledged.
pub fn chat_offer_ready(service: Option<&ActivityService>) -> bool {
    service.is_some_and(|service| service.settings_snapshot().disclosure_acked)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    pub entity: ActivityEntity,
    pub last_utc_ms: u64,
    pub episode_count: u64,
    pub active_days: u64,
    pub score: Option<f64>,
    /// Why the item is listed (contract `ReasonCode`, camelCase on the wire).
    pub reason_codes: Vec<ReasonCode>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredMatch {
    pub entity: ActivityEntity,
    pub match_class: String,
    /// Contract `MatchKind`; subsequence matches report `fuzzy` (the contract
    /// has no subsequence kind). `match_class` keeps the matcher's own name.
    pub match_kind: MatchKind,
    pub reason_codes: Vec<ReasonCode>,
    #[serde(skip)]
    result: MatchResult,
}

fn match_kind_of(class: MatchClass) -> MatchKind {
    match class {
        MatchClass::ExactPath => MatchKind::ExactPath,
        MatchClass::Exact => MatchKind::ExactName,
        MatchClass::Prefix => MatchKind::Prefix,
        MatchClass::Token => MatchKind::Token,
        MatchClass::Path => MatchKind::Path,
        MatchClass::Initialism => MatchKind::Initialism,
        MatchClass::Fuzzy | MatchClass::Subsequence => MatchKind::Fuzzy,
    }
}

/// Primary reason from the match class, then usage reasons, then workspace.
fn match_reasons(
    class: MatchClass,
    popularity: Option<&Popularity>,
    now: u64,
    scoped: bool,
) -> Vec<ReasonCode> {
    let mut reasons = vec![match class {
        MatchClass::ExactPath | MatchClass::Exact => ReasonCode::ExactName,
        MatchClass::Initialism => ReasonCode::Abbreviation,
        MatchClass::Fuzzy | MatchClass::Subsequence => ReasonCode::SimilarName,
        MatchClass::Prefix | MatchClass::Token | MatchClass::Path => ReasonCode::VisitedInFerrum,
    }];
    if let Some(p) = popularity {
        if p.episodes >= FREQUENT_REASON_EPISODES && p.active_days >= FREQUENT_REASON_DAYS {
            reasons.push(ReasonCode::Frequent);
        }
        if now.saturating_sub(p.last_utc_ms) <= RECENT_REASON_MS {
            reasons.push(ReasonCode::Recent);
        }
    }
    if scoped {
        reasons.push(ReasonCode::SameWorkspace);
    }
    reasons
}

fn list_reasons(primary: ReasonCode, scoped: bool) -> Vec<ReasonCode> {
    let mut reasons = vec![primary];
    if scoped {
        reasons.push(ReasonCode::SameWorkspace);
    }
    reasons
}

/// ACT-08 R7 v1: where one chat-listed entity lives, for an explicit "Open in
/// panel" click. Stays in the renderer; never part of a provider payload.
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatLocation {
    pub path: String,
    pub parent_path: String,
    pub kind: EntityKind,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStateDto {
    pub recording_enabled: bool,
    pub privacy_epoch: u64,
    pub retention_days: u32,
    pub hidden_count: u64,
    pub disclosure_acked: bool,
    /// True when this run replaced a corrupt database (the old file was moved
    /// to a quarantine name). Lets the UI tell the user history was reset.
    pub recovered: bool,
}

/// Persistent user choices that are not activity statistics. Kept beside the
/// SQLite file (app-data), never in the repository.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
struct Settings {
    retention_days: Option<u32>,
    hidden: Vec<String>,
    disclosure_acked: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewItem {
    #[serde(flatten)]
    pub item: ActivityItem,
    /// Hidden items stay in statistics but are dropped from suggestions.
    pub hidden: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyBucket {
    pub day_utc_ms: u64,
    /// Distinct items whose latest access falls on this UTC day (not clicks).
    pub items: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityOverview {
    pub days: u32,
    pub sort: String,
    pub items: Vec<OverviewItem>,
    pub daily: Vec<DailyBucket>,
    pub total_items: u64,
    pub total_episodes: u64,
    /// Oldest last-access among scanned items; the real retained interval.
    pub oldest_utc_ms: Option<u64>,
    pub truncated: bool,
    pub retention_days: u32,
    pub privacy_epoch: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopePreview {
    pub scope: String,
    pub items: u64,
    pub episodes: u64,
    pub sample: Vec<String>,
    pub truncated: bool,
    /// Pins are user data and are never touched by any history scope.
    pub favorites_kept: u64,
    pub hidden_kept: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteDto {
    pub path: String,
    pub label: String,
    pub kind: String,
}

/// Hard cap so pinning stays a deliberate list, not a second history.
const FAVORITES_LIMIT: usize = 200;

enum Job {
    Changes(Vec<FsChange>),
    /// A visit/open receipt plus the privacy generation observed when it was
    /// queued; a changed generation means a clear/forget/exclude/pause ran in
    /// between and the receipt is discarded instead of resurrecting data.
    Visit(Box<ActivityReceipt>, u64),
    SyncVolumes,
    /// Barrier: acknowledged once earlier jobs are applied.
    Flush(SyncSender<()>),
}

pub struct ActivityService {
    store: Arc<Mutex<ActivityStore>>,
    /// Shared with the identity worker (periodic purge prunes hidden ids).
    settings: Arc<Mutex<Settings>>,
    dir: PathBuf,
    jobs: SyncSender<Job>,
    dropped_jobs: AtomicU64,
    dropped_visits: AtomicU64,
    /// Bumped (before the store lock is taken) by every privacy-epoch-changing
    /// operation; compared by the worker against each queued visit.
    generation: Arc<AtomicU64>,
    recovered: bool,
}

// chat-reader:begin
/// Adapter for the chat tool. Same availability/workspace/retention filters as
/// the UI queries; hidden ids are applied by the tool from `ChatSnapshot`.
/// Strictly read-only: it takes no receipt and never touches episodes/ranking.
pub struct ServiceReader<'a> {
    service: &'a ActivityService,
    workspace_root: Option<String>,
}

impl ServiceReader<'_> {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ActivityStore>, String> {
        self.service
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())
    }

    /// Over-fetch so unverified rows dropped below do not starve the limit.
    fn ask(limit: u16) -> u16 {
        limit.saturating_add(25).min(100)
    }
}

impl ActivityReader for ServiceReader<'_> {
    fn privacy_epoch(&self) -> Result<u64, String> {
        self.lock()?.privacy_epoch().map_err(|e| e.to_string())
    }

    fn recent(
        &mut self,
        from: u64,
        until: u64,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<RecentRow>>, String> {
        if limit == 0 {
            return Ok(Some(Vec::new()));
        }
        // `until` is now + 1 day; never reach past the retention window.
        let floor = until
            .saturating_sub(DAY_MS)
            .saturating_sub(self.service.retention_ms());
        let mut store = self.lock()?;
        let rows = store
            .recent_in(
                from.max(floor),
                until,
                kind,
                epoch,
                Self::ask(limit),
                self.workspace_root.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        Ok(rows.map(|rows| {
            rows.into_iter()
                .filter(|row| suggestible(&row.entity.availability))
                .take(usize::from(limit))
                .collect()
        }))
    }

    fn frequent(
        &mut self,
        from: u64,
        now: u64,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<FrequentRow>>, String> {
        if limit == 0 {
            return Ok(Some(Vec::new()));
        }
        let floor = now.saturating_sub(self.service.retention_ms());
        let mut store = self.lock()?;
        let rows = store
            .frequent_in(
                from.max(floor),
                now,
                kind,
                epoch,
                Self::ask(limit),
                self.workspace_root.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        Ok(rows.map(|rows| {
            rows.into_iter()
                .filter(|row| suggestible(&row.entity.availability))
                .take(usize::from(limit))
                .collect()
        }))
    }

    fn name_candidates(
        &mut self,
        query: &str,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<ActivityEntity>>, String> {
        let mut store = self.lock()?;
        let found = store
            .search_candidates_in(
                query,
                kind.clone(),
                epoch,
                limit.clamp(1, 100),
                self.workspace_root.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        let Some(found) = found else {
            return Ok(None);
        };
        // FER-432: same retention floor as recent/frequent, even before the
        // next purge removed the rows.
        let retained = retained_ids(
            &mut store,
            &found,
            kind,
            now_ms(),
            self.service.retention_ms(),
        )?;
        Ok(Some(
            found
                .into_iter()
                .filter(|entity| suggestible(&entity.availability) && retained.contains(&entity.id))
                .collect(),
        ))
    }
}
// chat-reader:end

/// Ids (of `entities`) with at least one episode inside the retention window
/// `[now - retention, now + 1 day]` (the extra day tolerates clock skew, like
/// `recent_at`). Rows older than that are invisible even before a purge.
fn retained_ids(
    store: &mut ActivityStore,
    entities: &[ActivityEntity],
    kind: ActivityKind,
    now: u64,
    retention_ms: u64,
) -> Result<HashSet<String>, String> {
    if entities.is_empty() {
        return Ok(HashSet::new());
    }
    let ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();
    let usage = store
        .entity_popularity(
            &ids,
            kind,
            now.saturating_sub(retention_ms),
            now.saturating_add(DAY_MS),
        )
        .map_err(|e| e.to_string())?;
    Ok(usage.into_keys().collect())
}

fn retention_days_of(settings: &Settings) -> u32 {
    settings
        .retention_days
        .filter(|d| RETENTION_CHOICES.contains(d))
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Mutate + persist settings; a failed write leaves the in-memory value
/// untouched so UI state never claims a setting that was not saved.
fn persist_settings(
    dir: &Path,
    settings: &Mutex<Settings>,
    change: impl FnOnce(&mut Settings),
) -> Result<(), String> {
    let mut guard = settings
        .lock()
        .map_err(|_| "activity settings unavailable".to_owned())?;
    let mut next = guard.clone();
    change(&mut next);
    let bytes = serde_json::to_vec(&next).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(SETTINGS_FILE), bytes).map_err(|e| e.to_string())?;
    *guard = next;
    Ok(())
}

/// FER-432 #6: drop hidden ids whose entity no longer exists (purged,
/// excluded, forgotten). Called with the store lock held (lock order: store,
/// then settings). Best-effort: a lookup error keeps the id.
fn prune_hidden(store: &ActivityStore, settings: &Mutex<Settings>, dir: &Path) {
    let hidden = settings
        .lock()
        .map(|s| s.hidden.clone())
        .unwrap_or_default();
    let dead: HashSet<String> = hidden
        .into_iter()
        .filter(|id| matches!(store.identity_state(id), Ok(None)))
        .collect();
    if dead.is_empty() {
        return;
    }
    if let Err(error) =
        persist_settings(dir, settings, |s| s.hidden.retain(|id| !dead.contains(id)))
    {
        log::warn!("activity: hidden ids not pruned: {error}");
    }
}

/// Retention enforcement shared by startup, `set_retention` and the worker's
/// periodic run: purge at `now - retention`, then prune stale hidden ids.
fn purge_expired(
    store: &Mutex<ActivityStore>,
    settings: &Mutex<Settings>,
    dir: &Path,
    now: u64,
) -> Result<usize, String> {
    let days = settings
        .lock()
        .map(|s| retention_days_of(&s))
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    let mut guard = store
        .lock()
        .map_err(|_| "activity store unavailable".to_owned())?;
    let removed = guard
        .purge_before(now.saturating_sub(u64::from(days) * DAY_MS))
        .map_err(|e| e.to_string())?;
    prune_hidden(&guard, settings, dir);
    Ok(removed)
}

/// Keep the newest `MAX_HIDDEN` ids (the list is ordered oldest first).
fn cap_hidden(hidden: &mut Vec<String>) {
    if hidden.len() > MAX_HIDDEN {
        let excess = hidden.len() - MAX_HIDDEN;
        hidden.drain(..excess);
    }
}

/// What the worker needs for the periodic retention run.
struct PurgeSchedule {
    settings: Arc<Mutex<Settings>>,
    dir: PathBuf,
    interval: Duration,
}

/// Identity jobs run off the caller's thread so a slow or failing database
/// can never fail or stall a file operation. Messages never carry raw paths.
/// Also re-enforces retention every `purge.interval` (checked after each job
/// too, so a steady job stream cannot starve it). Exits when the service (the
/// only sender) is dropped.
fn run_worker(
    store: Arc<Mutex<ActivityStore>>,
    generation: Arc<AtomicU64>,
    jobs: Receiver<Job>,
    purge: PurgeSchedule,
) {
    let mut carry: Option<Job> = None;
    let mut last_purge = Instant::now();
    loop {
        let next = match carry.take() {
            Some(job) => Some(job),
            None => match jobs.recv_timeout(purge.interval.saturating_sub(last_purge.elapsed())) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };
        if let Some(job) = next {
            run_job(&store, &generation, &jobs, &mut carry, job);
        }
        if last_purge.elapsed() >= purge.interval {
            if let Err(error) = purge_expired(&store, &purge.settings, &purge.dir, now_ms()) {
                log::warn!("activity: periodic retention purge failed: {error}");
            }
            last_purge = Instant::now();
        }
    }
}

fn run_job(
    store: &Mutex<ActivityStore>,
    generation: &AtomicU64,
    jobs: &Receiver<Job>,
    carry: &mut Option<Job>,
    job: Job,
) {
    match job {
        Job::Visit(first, seen) => {
            // Opportunistic batching: take whatever visits are already
            // waiting (up to VISIT_BATCH) and apply them in one transaction.
            let mut batch = vec![(*first, seen)];
            while batch.len() < VISIT_BATCH {
                match jobs.try_recv() {
                    Ok(Job::Visit(receipt, seen)) => batch.push((*receipt, seen)),
                    Ok(other) => {
                        *carry = Some(other);
                        break;
                    }
                    Err(_) => break,
                }
            }
            apply_visits(store, generation, batch);
        }
        Job::Changes(changes) => {
            let Ok(mut guard) = store.lock() else {
                log::warn!("activity: store unavailable, identity receipts dropped");
                return;
            };
            for change in &changes {
                if let Err(error) = guard.apply_change(change) {
                    log::warn!("activity: {:?} receipt not applied: {error}", change.kind);
                }
            }
        }
        Job::SyncVolumes => sync_volumes(store),
        Job::Flush(ack) => {
            let _ = ack.send(());
        }
    }
}

/// Applies queued visits under one store lock. A receipt queued before a
/// privacy-epoch change is dropped; the rest carry the current epoch. A database
/// failure is logged (no paths) and never propagates.
fn apply_visits(
    store: &Mutex<ActivityStore>,
    generation: &AtomicU64,
    batch: Vec<(ActivityReceipt, u64)>,
) {
    let Ok(mut guard) = store.lock() else {
        log::warn!("activity: store unavailable, visit receipts dropped");
        return;
    };
    // Read under the store lock: operations bump the generation before they
    // take this lock, so anything they invalidate is seen as stale here.
    let current = generation.load(Ordering::SeqCst);
    let Ok(epoch) = guard.privacy_epoch() else {
        log::warn!("activity: privacy epoch unreadable, visit receipts dropped");
        return;
    };
    let receipts: Vec<ActivityReceipt> = batch
        .into_iter()
        .filter(|(_, seen)| *seen == current)
        .map(|(mut receipt, _)| {
            receipt.privacy_epoch = epoch;
            receipt
        })
        .collect();
    if receipts.is_empty() {
        return;
    }
    match guard.record_batch(&receipts) {
        Ok(results) => {
            let failed = results.iter().filter(|r| r.is_err()).count();
            if failed > 0 {
                log::warn!("activity: {failed} visit receipt(s) not applied");
            }
        }
        Err(error) => log::warn!("activity: visit batch failed: {error}"),
    }
}

/// Registers every present local drive by serial and re-tags rows recorded
/// when the drive letter was the volume id. Best-effort per drive.
fn sync_volumes(store: &Mutex<ActivityStore>) {
    for (letter, id) in volume::local_drives() {
        let mount = format!("{}:", letter as char);
        let Ok(mut guard) = store.lock() else {
            return;
        };
        if let Err(error) = guard.mount_volume(&id, &mount, now_ms()) {
            log::warn!("activity: mount_volume failed: {error}");
            continue;
        }
        if let Err(error) = guard.adopt_legacy_volume(&mount, &id) {
            log::warn!("activity: adopt_legacy_volume failed: {error}");
        }
    }
}

/// Entities in these states are unverified: they stay on the Activity page
/// (with an availability label) but are not offered as suggestions.
fn suggestible(availability: &Availability) -> bool {
    !matches!(
        availability,
        Availability::Missing | Availability::Offline | Availability::Unknown
    )
}

/// Caller-side stat with a deadline. The probe runs on a throwaway thread so
/// a hung share cannot block the caller past `PROBE_TIMEOUT`.
fn probe_path(path: &str) -> ValidationProbe {
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = path.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(stat_probe(&owned));
    });
    rx.recv_timeout(PROBE_TIMEOUT)
        .unwrap_or(ValidationProbe::Indeterminate)
}

fn stat_probe(path: &str) -> ValidationProbe {
    match std::fs::symlink_metadata(path) {
        Ok(_) => ValidationProbe::Present { platform_id: None },
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ValidationProbe::AccessDenied
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Missing only if the volume root is reachable; otherwise Offline.
            let root = Path::new(path).ancestors().last().map(Path::to_path_buf);
            match root {
                Some(root) if std::fs::symlink_metadata(&root).is_ok() => ValidationProbe::NotFound,
                _ => ValidationProbe::VolumeUnavailable,
            }
        }
        Err(_) => ValidationProbe::Indeterminate,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn fnv1a(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Windows locators fold case for a stable entity id; POSIX keeps case.
/// Trailing separators never change identity.
fn normalized(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let bytes = trimmed.as_bytes();
    if bytes.get(1) == Some(&b':') || trimmed.starts_with("\\\\") {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Stable volume identity for a locator: the volume serial on Windows
/// (`v<serial hex>`), so a recycled drive letter cannot retarget old history.
/// Falls back to the upper-cased drive letter when the serial is unreadable
/// (and on non-Windows), UNC to a shared bucket, anything else to `fs`.
fn volume_of(path: &str) -> String {
    let p = path.replace('/', "\\");
    if p.starts_with("\\\\") {
        "\\\\unc".to_owned()
    } else if p.as_bytes().get(1) == Some(&b':') {
        volume::id_for_letter(p.as_bytes()[0].to_ascii_uppercase())
            .unwrap_or_else(|| p[..2].to_uppercase())
    } else {
        "fs".to_owned()
    }
}

/// Volume identity probing. Serial lookups are cached briefly so receipts do
/// not hit the OS per call, yet a re-lettered drive is noticed within seconds.
mod volume {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const TTL: Duration = Duration::from_secs(30);
    type Cache = HashMap<u8, (Instant, Option<String>)>;
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

    pub fn id_for_letter(letter: u8) -> Option<String> {
        if let Ok(mut guard) = CACHE.lock() {
            let cache = guard.get_or_insert_with(HashMap::new);
            if let Some((at, id)) = cache.get(&letter)
                && at.elapsed() < TTL
            {
                return id.clone();
            }
            let id = query(letter);
            cache.insert(letter, (Instant::now(), id.clone()));
            return id;
        }
        query(letter)
    }

    /// Local fixed/removable drives currently present with their identity.
    /// Network and optical drives are skipped so startup never blocks on them.
    pub fn local_drives() -> Vec<(u8, String)> {
        mounted_letters()
            .into_iter()
            .filter_map(|letter| id_for_letter(letter).map(|id| (letter, id)))
            .collect()
    }

    #[cfg(windows)]
    fn root_wide(letter: u8) -> Vec<u16> {
        format!("{}:\\", letter as char)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    #[cfg(windows)]
    fn query(letter: u8) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
        let root = root_wide(letter);
        let mut serial: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        (ok != 0 && serial != 0).then(|| format!("v{serial:08x}"))
    }

    #[cfg(not(windows))]
    fn query(_letter: u8) -> Option<String> {
        None
    }

    #[cfg(windows)]
    fn mounted_letters() -> Vec<u8> {
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
        let mask = unsafe { GetLogicalDrives() };
        (0..26u8)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| b'A' + i)
            .filter(|letter| {
                let kind = unsafe { GetDriveTypeW(root_wide(*letter).as_ptr()) };
                kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE
            })
            .collect()
    }

    #[cfg(not(windows))]
    fn mounted_letters() -> Vec<u8> {
        Vec::new()
    }
}

fn base_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if let Some(name) = trimmed.rsplit(['/', '\\']).next()
        && !name.is_empty()
    {
        return name.to_owned();
    }
    let p = path.replace('/', "\\");
    if p.len() >= 2 && p.as_bytes()[1] == b':' {
        p[..2].to_uppercase()
    } else {
        path.to_owned()
    }
}

fn parent_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if let Some(index) = trimmed.rfind(['/', '\\']) {
        let parent = &trimmed[..index + 1];
        if !parent.is_empty() {
            return parent.to_owned();
        }
    }
    let p = path.replace('/', "\\");
    if p.len() >= 3 && p.as_bytes()[1] == b':' {
        format!("{}\\", &p[..2])
    } else {
        path.to_owned()
    }
}

/// Contract-valid receipt for one deliberate access at `path`.
/// Contract-shape validation for any locator crossing IPC: trimmed, absolute,
/// no traversal, and bare drives rooted ("C:" → "C:\"). Returns the owned
/// normalized form.
pub fn validate_locator(path: &str) -> Result<String, String> {
    let trimmed = path.trim();
    let owned;
    let normalized: &str = if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        owned = format!("{trimmed}\\");
        &owned
    } else {
        trimmed
    };
    let bytes = normalized.as_bytes();
    let absolute = normalized.starts_with('/')
        || normalized.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'));
    if !absolute
        || normalized.len() > 4096
        || normalized.contains('\0')
        || normalized.split(['/', '\\']).any(|part| part == "..")
    {
        return Err("activity path is not an absolute locator".to_owned());
    }
    Ok(normalized.to_owned())
}

/// Entity id is a stable hash of the normalized locator; rename identity
/// transfer is future work (ACT-06) and not claimed here.
pub fn build_receipt(path: &str, visit: VisitKind) -> Result<ActivityReceipt, String> {
    let path = &validate_locator(path)?;
    let (kind, source, entity_kind) = match visit {
        VisitKind::Folder => (
            ActivityKind::FolderVisit,
            ReceiptSource::NavigationCommit,
            EntityKind::Folder,
        ),
        VisitKind::FileOpen => (
            ActivityKind::FileOpenRequested,
            ReceiptSource::OpenAccepted,
            EntityKind::File,
        ),
    };
    let stamp = now_ms();
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
    let event_id = format!("e{stamp:x}-{seq:x}");
    Ok(ActivityReceipt {
        version: 1,
        event_id: event_id.clone(),
        action_id: event_id,
        privacy_epoch: 0,
        source,
        kind,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: format!("l{:016x}", fnv1a(&normalized(path))),
            kind: entity_kind,
            provider: "local".to_owned(),
            volume_id: volume_of(path),
            locator: path.to_owned(),
            parent_path: parent_of(path),
            display_name: base_name(path),
            availability: Availability::Available,
        },
        occurred_utc_ms: stamp,
    })
}

/// Receipt for a successful rename/move/copy/delete. `None` when a locator is
/// not a valid absolute path (nothing to track). `action_id` is unique per item.
pub fn build_change(kind: FsChangeKind, from: &str, to: Option<&str>) -> Option<FsChange> {
    let from = validate_locator(from).ok()?;
    let to = match (kind, to) {
        (FsChangeKind::Delete, _) => None,
        (_, Some(to)) => Some(validate_locator(to).ok()?),
        (_, None) => return None,
    };
    let stamp = now_ms();
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
    Some(FsChange {
        action_id: format!("c{stamp:x}-{:x}-{seq:x}", std::process::id()),
        kind,
        from_volume_id: volume_of(&from),
        to_volume_id: to.as_deref().map(volume_of),
        from,
        to,
        occurred_utc_ms: stamp,
    })
}

fn map_kind(scope: SearchScope) -> &'static [ActivityKind] {
    match scope {
        SearchScope::Folder => &[ActivityKind::FolderVisit],
        SearchScope::File => &[ActivityKind::FileOpenRequested],
        SearchScope::All => &[ActivityKind::FolderVisit, ActivityKind::FileOpenRequested],
    }
}

impl ActivityService {
    /// `dir` must be the OS app-data location; never a repository or temp dir.
    pub fn open(dir: &Path) -> Result<Self, String> {
        Self::open_with_purge_interval(dir, PURGE_INTERVAL)
    }

    /// `open` with a configurable periodic-purge cadence (tests use ms).
    pub(crate) fn open_with_purge_interval(
        dir: &Path,
        purge_interval: Duration,
    ) -> Result<Self, String> {
        let mut settings = std::fs::read(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default();
        cap_hidden(&mut settings.hidden);
        let settings = Arc::new(Mutex::new(settings));
        // A corrupt database is quarantined and replaced; a newer-schema one is
        // refused untouched (the error propagates and the service stays off).
        let (opened, recovered) = ActivityStore::open_recovering(&dir.join("activity.sqlite3"))
            .map_err(|e| e.to_string())?;
        if recovered {
            log::warn!("activity: database was corrupt; quarantined and recreated");
        }
        let store = Arc::new(Mutex::new(opened));
        let generation = Arc::new(AtomicU64::new(0));
        let (jobs, inbox) = sync_channel(JOB_QUEUE);
        let worker_store = Arc::clone(&store);
        let worker_generation = Arc::clone(&generation);
        let schedule = PurgeSchedule {
            settings: Arc::clone(&settings),
            dir: dir.to_path_buf(),
            interval: purge_interval,
        };
        std::thread::Builder::new()
            .name("activity-identity".into())
            .spawn(move || run_worker(worker_store, worker_generation, inbox, schedule))
            .map_err(|e| e.to_string())?;
        let service = Self {
            store,
            settings,
            dir: dir.to_path_buf(),
            jobs,
            dropped_jobs: AtomicU64::new(0),
            dropped_visits: AtomicU64::new(0),
            generation,
            recovered,
        };
        // Volume identity is established off the startup path.
        let _ = service.jobs.try_send(Job::SyncVolumes);
        // Retention is enforced on startup, then periodically by the worker;
        // queries also floor at `now - retention` between purges.
        if let Err(error) = purge_expired(&service.store, &service.settings, dir, now_ms()) {
            log::warn!("activity: startup retention purge failed: {error}");
        }
        Ok(service)
    }

    /// Receipts of successful file operations. Best-effort: queued for the
    /// identity worker; a full queue or an invalid locator drops the receipt
    /// (counted, logged without paths) and never affects the file operation.
    /// `pairs` are (from, to); `to` is `None` only for deletes.
    pub fn note_fs_changes(&self, kind: FsChangeKind, pairs: &[(String, Option<String>)]) {
        let changes: Vec<FsChange> = pairs
            .iter()
            .filter_map(|(from, to)| build_change(kind, from, to.as_deref()))
            .collect();
        if changes.is_empty() {
            return;
        }
        match self.jobs.try_send(Job::Changes(changes)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                let dropped = self.dropped_jobs.fetch_add(1, Ordering::Relaxed) + 1;
                log::warn!("activity: identity queue full or closed ({dropped} dropped)");
            }
        }
    }

    /// Test seam: a synchronous receipt with a chosen timestamp.
    #[cfg(test)]
    pub(crate) fn record_at_for_test(
        &self,
        path: &str,
        visit: VisitKind,
        at: u64,
    ) -> Result<(), String> {
        let mut receipt = build_receipt(path, visit)?;
        receipt.occurred_utc_ms = at;
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        receipt.privacy_epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        store.record(&receipt).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn episode_count_for_test(&self) -> Result<u64, String> {
        self.store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?
            .episode_count()
            .map_err(|e| e.to_string())
    }

    /// Blocks until identity jobs queued so far are applied (tests, shutdown).
    pub fn flush(&self) {
        let (ack, wait) = sync_channel(1);
        if self.jobs.send(Job::Flush(ack)).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(10));
        }
    }

    /// Lazy validation for one locator (suggestion chosen / item opened).
    /// Stats only when the entity is stale; returns its availability, or
    /// `None` when the path is not tracked.
    pub fn validate_if_stale(&self, path: &str) -> Result<Option<Availability>, String> {
        self.validate_with(path, now_ms(), probe_path)
    }

    fn validate_with(
        &self,
        path: &str,
        now: u64,
        prober: impl FnOnce(&str) -> ValidationProbe,
    ) -> Result<Option<Availability>, String> {
        let locator = validate_locator(path)?;
        let volume = volume_of(&locator);
        let id = {
            let store = self
                .store
                .lock()
                .map_err(|_| "activity store unavailable".to_owned())?;
            let Some(entity) = store
                .entity_at_locator(&volume, &locator)
                .map_err(|e| e.to_string())?
            else {
                return Ok(None);
            };
            let state = store
                .identity_state(&entity.id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "entity vanished".to_owned())?;
            if !state.is_stale(now, VALIDATION_TTL_MS) {
                return Ok(Some(state.entity.availability));
            }
            entity.id
        };
        // The stat runs without the store lock held.
        let probe = prober(&locator);
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        Ok(
            match store
                .apply_validation(&id, &probe, now)
                .map_err(|e| e.to_string())?
            {
                ValidationOutcome::Updated(availability) => Some(availability),
                ValidationOutcome::IdentityMismatch => Some(Availability::Missing),
                ValidationOutcome::UnknownEntity => None,
            },
        )
    }

    fn settings_snapshot(&self) -> Settings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn retention_days(&self) -> u32 {
        retention_days_of(&self.settings_snapshot())
    }

    fn retention_ms(&self) -> u64 {
        u64::from(self.retention_days()) * DAY_MS
    }

    fn hidden_set(&self) -> HashSet<String> {
        self.settings_snapshot().hidden.into_iter().collect()
    }

    /// Mutate + persist settings; a failed write leaves the in-memory value
    /// untouched so UI state never claims a setting that was not saved.
    fn update_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
        persist_settings(&self.dir, &self.settings, change)
    }

    /// Queues a visit/open receipt for the single worker and returns at once:
    /// `"queued"`, or `"dropped"` when the bounded queue is full or closed
    /// (counted, logged without paths). The database is never touched on the
    /// caller's thread, so navigation cannot wait on or fail because of it.
    /// Only an invalid path is an error. The receipt keeps its own timestamp.
    pub fn record_visit(&self, path: &str, visit: VisitKind) -> Result<String, String> {
        let receipt = build_receipt(path, visit)?;
        let seen = self.generation.load(Ordering::SeqCst);
        Ok(
            match self.jobs.try_send(Job::Visit(Box::new(receipt), seen)) {
                Ok(()) => "queued",
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                    let dropped = self.dropped_visits.fetch_add(1, Ordering::Relaxed) + 1;
                    log::warn!("activity: visit queue full or closed ({dropped} dropped)");
                    "dropped"
                }
            }
            .to_owned(),
        )
    }

    /// Invalidate queued visits before a privacy-epoch-changing operation.
    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Synchronous variant (waits for the store, returns the store outcome).
    /// Kept for tests that assert outcomes; production uses the queue.
    #[cfg(test)]
    fn record_visit_now(&self, path: &str, visit: VisitKind) -> Result<String, String> {
        let mut receipt = build_receipt(path, visit)?;
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        receipt.privacy_epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        Ok(match store.record(&receipt).map_err(|e| e.to_string())? {
            RecordOutcome::NewEpisode | RecordOutcome::ContinuedEpisode => "recorded",
            RecordOutcome::Duplicate => "duplicate",
            RecordOutcome::Ignored => "ignored",
            RecordOutcome::StaleEpoch => "stale-epoch",
        }
        .to_owned())
    }

    pub fn recent(&self, kind: ActivityKind, limit: u16) -> Result<Vec<ActivityItem>, String> {
        self.recent_at(kind, limit, now_ms(), None)
    }

    /// `workspace_root` is an absolute path supplied by the caller (Ferrum has
    /// no persisted workspace entity); `None` means no workspace filter.
    pub fn recent_scoped(
        &self,
        kind: ActivityKind,
        limit: u16,
        workspace_root: Option<&str>,
    ) -> Result<Vec<ActivityItem>, String> {
        self.recent_at(kind, limit, now_ms(), workspace_root)
    }

    /// `now` is the one UTC query clock (injectable for tests).
    fn recent_at(
        &self,
        kind: ActivityKind,
        limit: u16,
        now: u64,
        workspace_root: Option<&str>,
    ) -> Result<Vec<ActivityItem>, String> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        let hidden = self.hidden_set();
        let ask = limit
            .saturating_add(u16::try_from(hidden.len()).unwrap_or(100))
            .saturating_add(25)
            .min(100);
        // FER-432: floor at `now - retention` so rows past the window stay
        // invisible between periodic purges.
        let rows = store
            .recent_in(
                now.saturating_sub(self.retention_ms()),
                now.saturating_add(DAY_MS),
                kind,
                epoch,
                ask,
                workspace_root,
            )
            .map_err(|e| e.to_string())?;
        let scoped = workspace_root.is_some();
        Ok(rows
            .unwrap_or_default()
            .into_iter()
            .filter(|row| !hidden.contains(&row.entity.id) && suggestible(&row.entity.availability))
            .take(usize::from(limit))
            .map(|row| ActivityItem {
                entity: row.entity,
                last_utc_ms: row.last_utc_ms,
                episode_count: row.episode_count,
                active_days: 0,
                score: None,
                reason_codes: list_reasons(ReasonCode::Recent, scoped),
            })
            .collect())
    }

    pub fn frequent(&self, kind: ActivityKind, limit: u16) -> Result<Vec<ActivityItem>, String> {
        self.frequent_at(kind, limit, now_ms(), None)
    }

    pub fn frequent_scoped(
        &self,
        kind: ActivityKind,
        limit: u16,
        workspace_root: Option<&str>,
    ) -> Result<Vec<ActivityItem>, String> {
        self.frequent_at(kind, limit, now_ms(), workspace_root)
    }

    fn frequent_at(
        &self,
        kind: ActivityKind,
        limit: u16,
        now: u64,
        workspace_root: Option<&str>,
    ) -> Result<Vec<ActivityItem>, String> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        let hidden = self.hidden_set();
        let ask = limit
            .saturating_add(u16::try_from(hidden.len()).unwrap_or(100))
            .saturating_add(25)
            .min(100);
        let rows = store
            .frequent_in(
                now.saturating_sub(self.retention_ms()),
                now,
                kind,
                epoch,
                ask,
                workspace_root,
            )
            .map_err(|e| e.to_string())?;
        let scoped = workspace_root.is_some();
        Ok(rows
            .unwrap_or_default()
            .into_iter()
            .filter(|row| !hidden.contains(&row.entity.id) && suggestible(&row.entity.availability))
            .take(usize::from(limit))
            .map(|row| ActivityItem {
                entity: row.entity,
                last_utc_ms: row.last_utc_ms,
                episode_count: row.episode_count,
                active_days: row.active_days,
                score: Some(row.score),
                reason_codes: list_reasons(ReasonCode::Frequent, scoped),
            })
            .collect())
    }

    /// `Ok(None)` means the caller's epoch went stale mid-request.
    pub fn search(
        &self,
        query: &str,
        scope: SearchScope,
        limit: u16,
    ) -> Result<Option<Vec<ScoredMatch>>, String> {
        self.search_at(query, scope, limit, now_ms(), None)
    }

    /// `search` restricted to entities at or below the caller's workspace root.
    pub fn search_scoped(
        &self,
        query: &str,
        scope: SearchScope,
        limit: u16,
        workspace_root: Option<&str>,
    ) -> Result<Option<Vec<ScoredMatch>>, String> {
        self.search_at(query, scope, limit, now_ms(), workspace_root)
    }

    fn search_at(
        &self,
        query: &str,
        scope: SearchScope,
        limit: u16,
        now: u64,
        workspace_root: Option<&str>,
    ) -> Result<Option<Vec<ScoredMatch>>, String> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        // Path-like queries carry no name grams; candidates come from the
        // locator index instead, and exact paths rank first (spec §5).
        let bytes = query.as_bytes();
        let locator_query =
            query.contains('/') || query.contains('\\') || (bytes.len() >= 2 && bytes[1] == b':');
        let escaped: String = query
            .to_lowercase()
            .chars()
            .flat_map(|c| {
                if c == '%' || c == '_' || c == '!' {
                    vec!['!', c]
                } else {
                    vec![c]
                }
            })
            .collect();
        let hidden = self.hidden_set();
        let popularity_from = now.saturating_sub(self.retention_ms());
        let scoped = workspace_root.is_some();
        let mut out: Vec<ScoredMatch> = Vec::new();
        for kind in map_kind(scope) {
            let candidates = if locator_query {
                store
                    .locator_candidates_in(
                        &escaped,
                        kind.clone(),
                        epoch,
                        CANDIDATE_POOL,
                        workspace_root,
                    )
                    .map_err(|e| e.to_string())?
            } else {
                store
                    .search_candidates_in(
                        query,
                        kind.clone(),
                        epoch,
                        CANDIDATE_POOL,
                        workspace_root,
                    )
                    .map_err(|e| e.to_string())?
            };
            let Some(candidates) = candidates else {
                return Ok(None);
            };
            let candidates: Vec<ActivityEntity> = candidates
                .into_iter()
                .filter(|entity| !hidden.contains(&entity.id) && suggestible(&entity.availability))
                .collect();
            // Real usage inside the retention window feeds the matcher's
            // popularity boost (at most 24 points, never enough to cross a
            // match class), so an exact name always outranks a popular fuzzy one.
            let ids: Vec<String> = candidates.iter().map(|e| e.id.clone()).collect();
            let usage = store
                .entity_popularity(&ids, kind.clone(), popularity_from, now)
                .map_err(|e| e.to_string())?;
            // FER-432: only entities with an episode inside the retention
            // window are offered, even before the next purge removes the rest.
            let retained = retained_ids(
                &mut store,
                &candidates,
                kind.clone(),
                now,
                self.retention_ms(),
            )?;
            for entity in candidates {
                if !retained.contains(&entity.id) {
                    continue;
                }
                let popularity = usage.get(&entity.id);
                let episodes =
                    u32::try_from(popularity.map_or(0, |p| p.episodes)).unwrap_or(u32::MAX);
                if let Some(result) =
                    match_candidate(query, &entity.display_name, &entity.locator, episodes)
                {
                    out.push(ScoredMatch {
                        match_class: format!("{:?}", result.class).to_lowercase(),
                        match_kind: match_kind_of(result.class),
                        reason_codes: match_reasons(result.class, popularity, now, scoped),
                        entity,
                        result,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            compare_matches(&a.result, &a.entity.locator, &b.result, &b.entity.locator)
        });
        out.truncate(usize::from(limit));
        Ok(Some(out))
    }

    /// Point-in-time facts for the read-only chat tool (ACT-08): hidden ids,
    /// pause flag, privacy epoch and clock, taken together under the store
    /// lock. Pass `snapshot.context()` with a `chat_reader`; a privacy change
    /// after this call makes the tool answer `Stale`.
    pub fn chat_context(&self) -> Result<ChatSnapshot, String> {
        let store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        Ok(ChatSnapshot {
            now_utc_ms: now_ms(),
            private_session: !store.recording_enabled().map_err(|e| e.to_string())?,
            privacy_epoch: store.privacy_epoch().map_err(|e| e.to_string())?,
            hidden_entity_ids: self.hidden_set(),
            excluded_roots: Vec::new(),
        })
    }

    /// ACT-08 R1: one read-only chat lookup. Takes a fresh `chat_context()` snapshot, runs the
    /// tool over `chat_reader(None)` and retries once with a new snapshot if a privacy change
    /// raced the query (`Stale`); a second `Stale` is returned as is (never re-read silently).
    /// Records nothing: no receipt, episode or ranking input.
    pub fn chat_query(&self, request: &ChatRequest) -> ChatResponse {
        for _ in 0..2 {
            let Ok(snapshot) = self.chat_context() else {
                return ChatResponse::Unavailable;
            };
            let mut reader = self.chat_reader(None);
            let response = run_chat_tool(&mut reader, request, &snapshot.context());
            if response != ChatResponse::Stale {
                return response;
            }
        }
        ChatResponse::Stale
    }

    /// ACT-08 R7 v1: locator of one entity a chat answer listed, read when the user clicks
    /// "Open in panel". Read-only (identity lookup only: no receipt, episode or ranking input).
    /// `None` while recording is paused, for hidden/unknown/detached entities and for the same
    /// availabilities the chat reader drops (Missing/Offline/Unknown).
    pub fn chat_locate(&self, entity_id: &str) -> Option<ChatLocation> {
        if entity_id.trim().is_empty() || entity_id.len() > 128 || entity_id.contains('\0') {
            return None;
        }
        if self.hidden_set().contains(entity_id) {
            return None;
        }
        let store = self.store.lock().ok()?;
        if !store.recording_enabled().ok()? {
            return None;
        }
        let state = store.identity_state(entity_id).ok()??;
        if !state.attached || !suggestible(&state.entity.availability) {
            return None;
        }
        Some(ChatLocation {
            path: state.entity.locator,
            parent_path: state.entity.parent_path,
            kind: state.entity.kind,
        })
    }

    /// Read-only `ActivityReader` over this service. `workspace_root` is the
    /// same optional absolute filter the UI passes to `*_scoped`.
    pub fn chat_reader(&self, workspace_root: Option<&str>) -> ServiceReader<'_> {
        ServiceReader {
            service: self,
            workspace_root: workspace_root.map(str::to_owned),
        }
    }

    fn state_of(&self, store: &ActivityStore) -> Result<ActivityStateDto, String> {
        let settings = self.settings_snapshot();
        Ok(ActivityStateDto {
            recording_enabled: store.recording_enabled().map_err(|e| e.to_string())?,
            privacy_epoch: store.privacy_epoch().map_err(|e| e.to_string())?,
            retention_days: self.retention_days(),
            hidden_count: settings.hidden.len() as u64,
            disclosure_acked: settings.disclosure_acked,
            recovered: self.recovered,
        })
    }

    pub fn state(&self) -> Result<ActivityStateDto, String> {
        let store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        self.state_of(&store)
    }

    pub fn set_recording(&self, enabled: bool) -> Result<ActivityStateDto, String> {
        self.bump_generation();
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        store
            .set_recording_enabled(enabled)
            .map_err(|e| e.to_string())?;
        self.state_of(&store)
    }

    /// 7/30/90 only. Persisted first, then purge at the new cutoff so a
    /// failed write never deletes data under a setting that did not stick.
    pub fn set_retention(&self, days: u32) -> Result<ActivityStateDto, String> {
        if !RETENTION_CHOICES.contains(&days) {
            return Err(format!(
                "retention must be one of {RETENTION_CHOICES:?} days"
            ));
        }
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        self.update_settings(|s| s.retention_days = Some(days))?;
        store
            .purge_before(now_ms().saturating_sub(u64::from(days) * DAY_MS))
            .map_err(|e| e.to_string())?;
        prune_hidden(&store, &self.settings, &self.dir);
        self.state_of(&store)
    }

    pub fn ack_disclosure(&self) -> Result<ActivityStateDto, String> {
        self.update_settings(|s| s.disclosure_acked = true)?;
        self.state()
    }

    /// Forget removes one item's history effects (and its hide flag).
    pub fn forget(&self, entity_id: &str) -> Result<(), String> {
        self.bump_generation();
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        store.forget_entity(entity_id).map_err(|e| e.to_string())?;
        let _ = self.update_settings(|s| s.hidden.retain(|id| id != entity_id));
        Ok(())
    }

    /// Hide drops one item from suggestions only; statistics are kept.
    /// At most `MAX_HIDDEN` ids are kept: hiding one more drops the oldest
    /// hidden id (it becomes suggestible again). Re-hiding moves it to newest.
    pub fn set_hidden(&self, entity_id: &str, hidden: bool) -> Result<ActivityStateDto, String> {
        if entity_id.is_empty() || entity_id.len() > 64 {
            return Err("invalid entity id".to_owned());
        }
        self.update_settings(|s| {
            s.hidden.retain(|id| id != entity_id);
            if hidden {
                s.hidden.push(entity_id.to_owned());
                cap_hidden(&mut s.hidden);
            }
        })?;
        self.state()
    }

    pub fn exclude(&self, root: &str) -> Result<u64, String> {
        self.bump_generation();
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let removed = store.exclude_root(root).map_err(|e| e.to_string())?;
        // Excluded entities are gone; their hide flags must not linger.
        prune_hidden(&store, &self.settings, &self.dir);
        Ok(removed)
    }

    pub fn clear(&self) -> Result<u64, String> {
        self.bump_generation();
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let removed = store.clear_all().map_err(|e| e.to_string())?;
        let _ = self.update_settings(|s| s.hidden.clear());
        Ok(removed)
    }

    /// Scan the given kinds newest-first via the paged store API.
    fn scan(
        &self,
        store: &mut ActivityStore,
        kinds: &[ActivityKind],
        from_utc_ms: u64,
        epoch: u64,
    ) -> Result<(Vec<RecentRow>, bool), String> {
        let until = now_ms().saturating_add(DAY_MS);
        let mut rows = Vec::new();
        let mut truncated = false;
        for kind in kinds {
            let mut cursor = None;
            for page_index in 0..SCAN_PAGES {
                let Some(page) = store
                    .recent_page(
                        from_utc_ms,
                        until,
                        kind.clone(),
                        epoch,
                        SCAN_PAGE_SIZE,
                        cursor.as_ref(),
                    )
                    .map_err(|e| e.to_string())?
                else {
                    break;
                };
                rows.extend(page.rows);
                match page.next_cursor {
                    Some(next) => {
                        if page_index + 1 == SCAN_PAGES {
                            truncated = true;
                        }
                        cursor = Some(next);
                    }
                    None => break,
                }
            }
        }
        Ok((rows, truncated))
    }

    /// Windowed history for the Activity page. Coverage is the real retained
    /// interval (`oldest_utc_ms`), never "all time".
    pub fn overview(
        &self,
        days: u32,
        kind: ActivityKind,
        by_frequency: bool,
        limit: u16,
    ) -> Result<ActivityOverview, String> {
        if !RETENTION_CHOICES.contains(&days) {
            return Err("window must be 7, 30 or 90 days".to_owned());
        }
        let limit = limit.clamp(1, 100);
        let now = now_ms();
        let from = now.saturating_sub(u64::from(days) * DAY_MS);
        let hidden = self.hidden_set();
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        let (rows, truncated) = self.scan(&mut store, std::slice::from_ref(&kind), from, epoch)?;
        let total_items = rows.len() as u64;
        let total_episodes = rows.iter().map(|r| r.episode_count).sum();
        let oldest_utc_ms = rows.iter().map(|r| r.last_utc_ms).min();
        let mut buckets: std::collections::BTreeMap<u64, u64> = Default::default();
        for row in &rows {
            *buckets
                .entry(row.last_utc_ms / DAY_MS * DAY_MS)
                .or_default() += 1;
        }
        let daily = buckets
            .into_iter()
            .map(|(day_utc_ms, items)| DailyBucket { day_utc_ms, items })
            .collect();
        let raw: Vec<ActivityItem> = if by_frequency {
            store
                .frequent(from, now, kind, epoch, limit)
                .map_err(|e| e.to_string())?
                .unwrap_or_default()
                .into_iter()
                .map(|row| ActivityItem {
                    entity: row.entity,
                    last_utc_ms: row.last_utc_ms,
                    episode_count: row.episode_count,
                    active_days: row.active_days,
                    score: Some(row.score),
                    reason_codes: vec![ReasonCode::Frequent],
                })
                .collect()
        } else {
            rows.into_iter()
                .take(usize::from(limit))
                .map(|row| ActivityItem {
                    entity: row.entity,
                    last_utc_ms: row.last_utc_ms,
                    episode_count: row.episode_count,
                    active_days: 0,
                    score: None,
                    reason_codes: vec![ReasonCode::Recent],
                })
                .collect()
        };
        let items = raw
            .into_iter()
            .map(|item| OverviewItem {
                hidden: hidden.contains(&item.entity.id),
                item,
            })
            .collect();
        Ok(ActivityOverview {
            days,
            sort: if by_frequency { "frequent" } else { "recent" }.to_owned(),
            items,
            daily,
            total_items,
            total_episodes,
            oldest_utc_ms,
            truncated,
            retention_days: self.retention_days(),
            privacy_epoch: epoch,
        })
    }

    /// Read-only preview of what a destructive scope would remove. `scope` is
    /// `all`, `forget` (target = entity id), `exclude` (target = root path)
    /// or `retention` (target = days as text). Never mutates anything.
    pub fn preview(&self, scope: &str, target: &str) -> Result<ScopePreview, String> {
        let settings = self.settings_snapshot();
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let epoch = store.privacy_epoch().map_err(|e| e.to_string())?;
        let (rows, truncated) = self.scan(
            &mut store,
            &[ActivityKind::FolderVisit, ActivityKind::FileOpenRequested],
            0,
            epoch,
        )?;
        let now = now_ms();
        let affected: Vec<&RecentRow> = match scope {
            "all" => rows.iter().collect(),
            "forget" => rows.iter().filter(|r| r.entity.id == target).collect(),
            "exclude" => {
                let root = normalized(&validate_locator(target)?);
                rows.iter()
                    .filter(|r| {
                        let loc = normalized(&r.entity.locator);
                        loc == root
                            || loc
                                .strip_prefix(&root)
                                .is_some_and(|rest| rest.starts_with(['/', '\\']))
                    })
                    .collect()
            }
            "retention" => {
                let days: u32 = target.parse().map_err(|_| "invalid days".to_owned())?;
                if !RETENTION_CHOICES.contains(&days) {
                    return Err("retention must be 7, 30 or 90 days".to_owned());
                }
                let cutoff = now.saturating_sub(u64::from(days) * DAY_MS);
                rows.iter().filter(|r| r.last_utc_ms < cutoff).collect()
            }
            other => return Err(format!("unknown preview scope {other:?}")),
        };
        let episodes = if scope == "all" && !truncated {
            store.episode_count().map_err(|e| e.to_string())?
        } else {
            affected.iter().map(|r| r.episode_count).sum()
        };
        let favorites_kept = store.favorites().map_err(|e| e.to_string())?.len() as u64;
        Ok(ScopePreview {
            scope: scope.to_owned(),
            items: affected.len() as u64,
            episodes,
            sample: affected
                .iter()
                .take(5)
                .map(|r| r.entity.locator.clone())
                .collect(),
            truncated,
            favorites_kept,
            hidden_kept: settings.hidden.len() as u64,
        })
    }

    /// Manual pins are user data, not activity statistics: list/add/remove
    /// only — history clearing never touches them. `kind` selects the click
    /// behavior in the UI: folders navigate, files open with the OS default.
    pub fn favorites(&self) -> Result<Vec<FavoriteDto>, String> {
        let store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        Ok(store
            .favorites()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(path, label, kind)| FavoriteDto { path, label, kind })
            .collect())
    }

    pub fn add_favorite(&self, path: &str, kind: &str) -> Result<bool, String> {
        let locator = validate_locator(path)?;
        let kind = match kind {
            "folder" | "file" => kind,
            other => return Err(format!("unknown favorite kind {other:?}")),
        };
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let favorites = store.favorites().map_err(|e| e.to_string())?;
        // Windows pins compare case/trailing-insensitively, like entity ids.
        if favorites
            .iter()
            .any(|(existing, _, _)| normalized(existing) == normalized(&locator))
        {
            return Ok(false);
        }
        if favorites.len() >= FAVORITES_LIMIT {
            return Err("favorites limit reached".to_owned());
        }
        store
            .add_favorite(&locator, &base_name(&locator), kind)
            .map_err(|e| e.to_string())
    }

    pub fn remove_favorite(&self, path: &str) -> Result<bool, String> {
        let locator = validate_locator(path)?;
        let mut store = self
            .store
            .lock()
            .map_err(|_| "activity store unavailable".to_owned())?;
        let target = store
            .favorites()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(stored, _, _)| stored)
            .find(|stored| normalized(stored) == normalized(&locator));
        match target {
            Some(stored) => store.remove_favorite(&stored).map_err(|e| e.to_string()),
            None => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_identity_is_stable_and_contract_valid() {
        let first = build_receipt("C:\\Work\\Reports", VisitKind::Folder).unwrap();
        let second = build_receipt("c:\\work\\reports\\", VisitKind::Folder).unwrap();
        assert_eq!(first.entity.id, second.entity.id);
        first.validate().expect("contract valid");
        assert_eq!(first.entity.display_name, "Reports");
        assert_eq!(first.entity.parent_path, "C:\\Work\\");
        // Serial-based on Windows, drive-letter fallback elsewhere: stable either way.
        assert_eq!(first.entity.volume_id, volume_of("C:\\"));
        assert_eq!(second.entity.volume_id, first.entity.volume_id);
        assert!(build_receipt("relative\\path", VisitKind::Folder).is_err());
        assert!(build_receipt("C:\\a\\..\\b", VisitKind::Folder).is_err());
        // Bare drive crumbs from the UI must normalize to the rooted form.
        let drive = build_receipt("C:", VisitKind::Folder).unwrap();
        drive.validate().expect("drive root contract valid");
        assert_eq!(drive.entity.locator, "C:\\");
        assert_eq!(drive.entity.display_name, "C:");
    }

    #[test]
    fn visits_record_search_and_privacy_flow() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        assert_eq!(
            service
                .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
                .unwrap(),
            "recorded"
        );
        // Episode dedup: the same folder within 30s continues, not doubles.
        assert_eq!(
            service
                .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
                .unwrap(),
            "recorded"
        );
        assert_eq!(service.episode_count().unwrap(), 1);

        let recent = service.recent(ActivityKind::FolderVisit, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].entity.display_name, "Invoices");

        let hits = service
            .search("invoce", SearchScope::Folder, 10)
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity.id, recent[0].entity.id);

        service.exclude("C:\\Work").unwrap();
        assert!(
            service
                .recent(ActivityKind::FolderVisit, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            service
                .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
                .unwrap(),
            "ignored"
        );

        service.set_recording(false).unwrap();
        let state = service.state().unwrap();
        assert!(!state.recording_enabled);
        assert_eq!(
            service
                .record_visit_now("C:\\Elsewhere", VisitKind::Folder)
                .unwrap(),
            "ignored"
        );
        service.clear().unwrap();
        assert_eq!(service.episode_count().unwrap(), 0);
    }

    #[test]
    fn locator_query_matches_visited_paths_without_correction() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
            .unwrap();
        let hits = service
            .search("c:\\work", SearchScope::All, 10)
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].match_class, "path");
        let bare_drive = service.search("C:", SearchScope::All, 10).unwrap().unwrap();
        assert_eq!(bare_drive.len(), 1);
        assert_eq!(bare_drive[0].entity.locator, "C:\\Work\\Invoices");
        // No silent correction: an unrelated path stays empty.
        assert!(
            service
                .search("D:\\elsewhere", SearchScope::All, 10)
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn favorites_pin_unpin_and_survive_clear() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        assert!(service.add_favorite(r"C:\Work\Invoices", "folder").unwrap());
        assert!(
            !service
                .add_favorite(r"c:\work\invoices\", "folder")
                .unwrap()
        );
        assert!(service.add_favorite("C:", "folder").unwrap()); // rooted to C:\
        assert!(service.add_favorite(r"C:\Work\book.xlsx", "file").unwrap());
        assert!(
            !service
                .add_favorite(r"C:\Work\book.xlsx", "folder")
                .unwrap()
        );
        assert!(service.add_favorite(r"C:\x", "disk").is_err());
        let favorites = service.favorites().unwrap();
        assert_eq!(favorites.len(), 3);
        assert_eq!(favorites[0].label, "Invoices");
        assert_eq!(favorites[0].kind, "folder");
        assert_eq!(favorites[2].kind, "file");
        assert_eq!(favorites[2].path, r"C:\Work\book.xlsx");
        service.clear().unwrap();
        assert_eq!(service.favorites().unwrap().len(), 3);
        assert!(service.remove_favorite(r"C:\Work\Invoices").unwrap());
        assert!(!service.remove_favorite(r"C:\Work\Invoices").unwrap());
        assert!(service.add_favorite(r"relative\path", "folder").is_err());
    }

    #[test]
    fn forget_removes_entity_from_recent_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
            .unwrap();
        let id = format!("l{:016x}", fnv1a(&normalized("c:\\work\\invoices")));
        service.forget(&id).unwrap();
        // The service always reads the current epoch, so a forgotten entity
        // is simply gone; the stale-epoch None path guards queued IPC calls.
        assert!(
            service
                .recent(ActivityKind::FolderVisit, 10)
                .unwrap()
                .is_empty()
        );
        let hits = service.search("invoice", SearchScope::All, 10).unwrap();
        assert!(matches!(hits, Some(ref found) if found.is_empty()));
    }

    #[test]
    fn retention_is_validated_persisted_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        assert_eq!(service.state().unwrap().retention_days, 90);
        assert!(service.set_retention(15).is_err());
        assert_eq!(service.set_retention(30).unwrap().retention_days, 30);
        drop(service);
        let reopened = ActivityService::open(dir.path()).unwrap();
        assert_eq!(reopened.state().unwrap().retention_days, 30);
    }

    #[test]
    fn hide_drops_suggestions_but_keeps_statistics() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
            .unwrap();
        let id = format!("l{:016x}", fnv1a(&normalized("c:\\work\\invoices")));
        assert_eq!(service.set_hidden(&id, true).unwrap().hidden_count, 1);
        assert!(
            service
                .recent(ActivityKind::FolderVisit, 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            service
                .search("invoice", SearchScope::All, 10)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        let overview = service
            .overview(7, ActivityKind::FolderVisit, false, 10)
            .unwrap();
        assert_eq!(overview.items.len(), 1);
        assert!(overview.items[0].hidden);
        assert_eq!(overview.total_episodes, 1);
        service.set_hidden(&id, false).unwrap();
        assert_eq!(
            service.recent(ActivityKind::FolderVisit, 10).unwrap().len(),
            1
        );
        // Forget clears the hide flag along with the history.
        service.set_hidden(&id, true).unwrap();
        service.forget(&id).unwrap();
        assert_eq!(service.state().unwrap().hidden_count, 0);
    }

    #[test]
    fn preview_never_mutates_and_matches_scope() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
            .unwrap();
        service
            .record_visit_now("C:\\Workshop\\Other", VisitKind::Folder)
            .unwrap();
        service.add_favorite("C:\\Work", "folder").unwrap();
        let all = service.preview("all", "").unwrap();
        assert_eq!((all.items, all.episodes, all.favorites_kept), (2, 2, 1));
        // Subtree boundary: C:\Work must not swallow C:\Workshop.
        let exclude = service.preview("exclude", "C:\\Work").unwrap();
        assert_eq!(exclude.items, 1);
        let id = format!("l{:016x}", fnv1a(&normalized("c:\\work\\invoices")));
        assert_eq!(service.preview("forget", &id).unwrap().items, 1);
        assert_eq!(service.preview("retention", "7").unwrap().items, 0);
        assert!(service.preview("bogus", "").is_err());
        assert_eq!(
            service.recent(ActivityKind::FolderVisit, 10).unwrap().len(),
            2
        );
    }

    #[test]
    fn overview_reports_window_daily_buckets_and_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now("C:\\Work\\Invoices", VisitKind::Folder)
            .unwrap();
        let overview = service
            .overview(30, ActivityKind::FolderVisit, true, 10)
            .unwrap();
        assert_eq!(overview.days, 30);
        assert_eq!(overview.sort, "frequent");
        assert_eq!(overview.total_items, 1);
        assert_eq!(overview.daily.len(), 1);
        assert!(overview.oldest_utc_ms.is_some());
        assert!(!overview.truncated);
        assert!(
            service
                .overview(10, ActivityKind::FolderVisit, false, 10)
                .is_err()
        );
    }

    #[test]
    fn clear_resets_hidden_and_disclosure_survives() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        assert!(!service.state().unwrap().disclosure_acked);
        assert!(service.ack_disclosure().unwrap().disclosure_acked);
        service.set_hidden("labc", true).unwrap();
        service.clear().unwrap();
        let state = service.state().unwrap();
        assert_eq!(state.hidden_count, 0);
        assert!(state.disclosure_acked);
    }

    fn folder_ids(service: &ActivityService) -> Vec<(String, String)> {
        service
            .recent(ActivityKind::FolderVisit, 10)
            .unwrap()
            .into_iter()
            .map(|item| (item.entity.id, item.entity.locator))
            .collect()
    }

    #[test]
    fn build_change_validates_and_gives_unique_action_ids() {
        let a = build_change(FsChangeKind::Rename, r"C:\Work\a", Some(r"C:\Work\b")).unwrap();
        let b = build_change(FsChangeKind::Rename, r"C:\Work\a", Some(r"C:\Work\b")).unwrap();
        assert_ne!(a.action_id, b.action_id);
        assert_eq!(a.to_volume_id.as_deref(), Some(a.from_volume_id.as_str()));
        let delete = build_change(FsChangeKind::Delete, r"C:\Work\a", Some(r"C:\ignored")).unwrap();
        assert!(delete.to.is_none() && delete.to_volume_id.is_none());
        assert!(build_change(FsChangeKind::Move, r"C:\Work\a", None).is_none());
        assert!(build_change(FsChangeKind::Move, r"relative\a", Some(r"C:\b")).is_none());
        assert!(build_change(FsChangeKind::Move, r"C:\a", Some(r"C:\..\b")).is_none());
    }

    #[test]
    fn rename_receipt_keeps_entity_id_and_moves_locator() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now(r"C:\Work\Old", VisitKind::Folder)
            .unwrap();
        let before = folder_ids(&service);
        service.note_fs_changes(
            FsChangeKind::Rename,
            &[(r"C:\Work\Old".into(), Some(r"C:\Work\New".into()))],
        );
        service.flush();
        let after = folder_ids(&service);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].0, before[0].0, "entity id must survive rename");
        assert_eq!(after[0].1, r"C:\Work\New");
    }

    #[test]
    fn move_and_copy_receipts_follow_contract() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now(r"C:\Work\Docs", VisitKind::Folder)
            .unwrap();
        let id = folder_ids(&service)[0].0.clone();
        // Copy inherits nothing and leaves the source in place.
        service.note_fs_changes(
            FsChangeKind::Copy,
            &[(r"C:\Work\Docs".into(), Some(r"C:\Backup\Docs".into()))],
        );
        service.flush();
        assert_eq!(
            folder_ids(&service),
            vec![(id.clone(), r"C:\Work\Docs".into())]
        );
        service.note_fs_changes(
            FsChangeKind::Move,
            &[(r"C:\Work\Docs".into(), Some(r"C:\Archive\Docs".into()))],
        );
        service.flush();
        assert_eq!(folder_ids(&service), vec![(id, r"C:\Archive\Docs".into())]);
    }

    #[test]
    fn delete_receipt_hides_suggestion_but_keeps_activity_page_item() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now(r"C:\Work\Gone", VisitKind::Folder)
            .unwrap();
        service.note_fs_changes(FsChangeKind::Delete, &[(r"C:\Work\Gone".into(), None)]);
        service.flush();
        assert!(folder_ids(&service).is_empty(), "Missing is not suggested");
        assert!(
            service
                .search("gone", SearchScope::All, 10)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        let overview = service
            .overview(7, ActivityKind::FolderVisit, false, 10)
            .unwrap();
        assert_eq!(overview.items.len(), 1);
        assert_eq!(
            overview.items[0].item.entity.availability,
            Availability::Missing
        );
    }

    #[test]
    fn invalid_receipts_never_fail_and_do_not_change_state() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now(r"C:\Work\Keep", VisitKind::Folder)
            .unwrap();
        service.note_fs_changes(FsChangeKind::Rename, &[("not absolute".into(), None)]);
        service.note_fs_changes(FsChangeKind::Delete, &[(r"C:\Work\Untracked".into(), None)]);
        service.flush();
        assert_eq!(folder_ids(&service).len(), 1);
    }

    #[test]
    fn lazy_validation_stats_only_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now(r"C:\Work\Check", VisitKind::Folder)
            .unwrap();
        // Fresh (the visit counts as a check): the prober must not run.
        let fresh = service
            .validate_with(r"C:\Work\Check", now_ms(), |_| {
                panic!("fresh entity must not be stat'ed")
            })
            .unwrap();
        assert_eq!(fresh, Some(Availability::Available));
        // Untracked path: nothing to validate, no stat.
        assert_eq!(
            service
                .validate_with(r"C:\Work\Other", now_ms(), |_| panic!("untracked"))
                .unwrap(),
            None
        );
        // Stale and gone: Missing, and suggestions drop it.
        let later = now_ms() + 2 * VALIDATION_TTL_MS;
        let stale = service
            .validate_with(r"C:\Work\Check", later, |_| ValidationProbe::NotFound)
            .unwrap();
        assert_eq!(stale, Some(Availability::Missing));
        assert!(folder_ids(&service).is_empty());
    }

    #[test]
    fn offline_and_unknown_are_hidden_from_suggestions_only() {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service
            .record_visit_now(r"C:\Work\Share", VisitKind::Folder)
            .unwrap();
        let later = now_ms() + 2 * VALIDATION_TTL_MS;
        service
            .validate_with(r"C:\Work\Share", later, |_| {
                ValidationProbe::VolumeUnavailable
            })
            .unwrap();
        assert!(folder_ids(&service).is_empty());
        assert!(
            service
                .frequent(ActivityKind::FolderVisit, 10)
                .unwrap()
                .is_empty()
        );
        let overview = service
            .overview(7, ActivityKind::FolderVisit, false, 10)
            .unwrap();
        assert_eq!(overview.items.len(), 1);
        // A later successful visit re-verifies it.
        service
            .record_visit_now(r"C:\Work\Share", VisitKind::Folder)
            .unwrap();
        assert_eq!(folder_ids(&service).len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_volume_id_is_serial_based() {
        let id = volume_of(r"C:\Windows");
        assert!(id.starts_with('v') && id.len() == 9, "got {id}");
        assert_eq!(id, volume_of("c:/other"));
    }

    // ── FER-254 / FER-255: fixed-clock ranking, workspace scope, reasons ────

    /// A UTC midnight a few weeks ago: inside retention, so delayed receipts
    /// recorded "in the past" are accepted, and a clock we control.
    fn base_day() -> u64 {
        (now_ms() / DAY_MS - 30) * DAY_MS
    }

    fn open_service() -> (tempfile::TempDir, ActivityService) {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service.flush();
        (dir, service)
    }

    /// Synchronous receipt with a chosen timestamp (the service keeps the
    /// caller-supplied `occurred_utc_ms`).
    fn record_at(service: &ActivityService, path: &str, visit: VisitKind, at: u64) {
        let mut receipt = build_receipt(path, visit).unwrap();
        receipt.occurred_utc_ms = at;
        let mut store = service.store.lock().unwrap();
        receipt.privacy_epoch = store.privacy_epoch().unwrap();
        let outcome = store.record(&receipt).unwrap();
        assert!(
            matches!(
                outcome,
                RecordOutcome::NewEpisode | RecordOutcome::ContinuedEpisode
            ),
            "{path} at {at}: {outcome:?}"
        );
    }

    /// `visits` distinct UTC days, `per_day` episodes a day (>30 s apart).
    fn visit_many(service: &ActivityService, path: &str, first_day: u64, days: u64, per_day: u64) {
        for day in 0..days {
            for n in 0..per_day {
                let at = (first_day + day) * DAY_MS + 3_600_000 + n * 120_000;
                record_at(service, path, VisitKind::Folder, at);
            }
        }
    }

    #[test]
    fn exact_name_beats_frequent_fuzzy_candidate_with_real_popularity() {
        let (_dir, service) = open_service();
        let day = base_day() / DAY_MS;
        let now = (day + 20) * DAY_MS + 43_200_000;
        // One visit to the exact name; many visits on many days to a transposition.
        visit_many(&service, "C:\\Work\\Exact\\Reports", day + 18, 1, 1);
        visit_many(&service, "C:\\Work\\Fuzzy\\Reprots", day, 19, 5);
        let hits = service
            .search_at("reports", SearchScope::Folder, 10, now, None)
            .unwrap()
            .unwrap();
        let names: Vec<_> = hits
            .iter()
            .map(|h| h.entity.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["Reports", "Reprots"], "exact must lead");
        assert_eq!(hits[0].match_class, "exact");
        assert_eq!(hits[1].match_class, "fuzzy");
        // Popularity is real, not a constant: the fuzzy hit carries the maximum
        // boost short of its class base (400 - 10 edit + up to 24), and still loses.
        let fuzzy = hits[1].result.score;
        assert!(
            fuzzy > 390,
            "fuzzy score {fuzzy} should include a popularity boost"
        );
        assert!(fuzzy < hits[0].result.score);
        assert!(hits[0].result.score >= 900);
    }

    #[test]
    fn popularity_orders_matches_within_one_class() {
        let (_dir, service) = open_service();
        let day = base_day() / DAY_MS;
        let now = (day + 20) * DAY_MS + 43_200_000;
        // Same class and edit distance; the alphabetically first one is unused.
        visit_many(&service, "C:\\Work\\Reprots A", day + 19, 1, 1);
        visit_many(&service, "C:\\Work\\Reprots Z", day + 5, 10, 4);
        let hits = service
            .search_at("reports", SearchScope::Folder, 10, now, None)
            .unwrap()
            .unwrap();
        let names: Vec<_> = hits
            .iter()
            .map(|h| h.entity.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["Reprots Z", "Reprots A"]);
        assert!(hits[0].result.score > hits[1].result.score);
    }

    #[test]
    fn search_reports_match_kind_and_reason_codes() {
        let (_dir, service) = open_service();
        let day = base_day() / DAY_MS;
        let now = (day + 20) * DAY_MS + 43_200_000;
        visit_many(&service, "C:\\Work\\Exact\\Reports", day + 20, 1, 1);
        visit_many(&service, "C:\\Work\\Fuzzy\\Reprots", day, 15, 3);
        visit_many(
            &service,
            "C:\\Work\\Quarterly Business Review",
            day + 3,
            1,
            1,
        );
        let find = |query: &str| {
            service
                .search_at(query, SearchScope::Folder, 10, now, None)
                .unwrap()
                .unwrap()
        };
        let hits = find("reports");
        assert_eq!(hits[0].match_kind, MatchKind::ExactName);
        assert_eq!(hits[0].reason_codes[0], ReasonCode::ExactName);
        assert!(hits[0].reason_codes.contains(&ReasonCode::Recent));
        assert!(!hits[0].reason_codes.contains(&ReasonCode::Frequent));
        assert_eq!(hits[1].match_kind, MatchKind::Fuzzy);
        assert_eq!(hits[1].reason_codes[0], ReasonCode::SimilarName);
        assert!(hits[1].reason_codes.contains(&ReasonCode::Frequent));
        // Last visit 5 days before `now`: recent window (7 d) still covers it.
        let initialism = find("qbr");
        assert_eq!(initialism[0].match_kind, MatchKind::Initialism);
        assert_eq!(initialism[0].reason_codes[0], ReasonCode::Abbreviation);
        let prefix = find("quarterly");
        assert_eq!(prefix[0].match_kind, MatchKind::Prefix);
        assert_eq!(prefix[0].reason_codes[0], ReasonCode::VisitedInFerrum);
        // Wire format is the contract's camelCase.
        let json = serde_json::to_value(&hits[1]).unwrap();
        assert_eq!(json["matchKind"], "fuzzy");
        assert_eq!(json["reasonCodes"][0], "similarName");
        assert_eq!(json["matchClass"], "fuzzy");
    }

    #[test]
    fn recent_and_frequent_items_carry_their_reason() {
        let (_dir, service) = open_service();
        let day = base_day() / DAY_MS;
        let now = (day + 20) * DAY_MS + 43_200_000;
        visit_many(&service, "C:\\Work\\Habit", day + 10, 5, 2);
        let recent = service
            .recent_at(ActivityKind::FolderVisit, 5, now, None)
            .unwrap();
        assert_eq!(recent[0].reason_codes, vec![ReasonCode::Recent]);
        let frequent = service
            .frequent_at(ActivityKind::FolderVisit, 5, now, None)
            .unwrap();
        assert_eq!(frequent[0].reason_codes, vec![ReasonCode::Frequent]);
        let scoped = service
            .frequent_at(ActivityKind::FolderVisit, 5, now, Some("C:\\Work"))
            .unwrap();
        assert_eq!(
            scoped[0].reason_codes,
            vec![ReasonCode::Frequent, ReasonCode::SameWorkspace]
        );
    }

    #[test]
    fn workspace_root_scopes_recent_frequent_and_search_through_the_service() {
        let (_dir, service) = open_service();
        let day = base_day() / DAY_MS;
        let now = (day + 20) * DAY_MS + 43_200_000;
        for path in [
            "C:\\Work\\A\\Invoices",
            "C:\\Work\\AB\\Invoices",
            "C:\\Other\\Invoices",
        ] {
            visit_many(&service, path, day + 15, 3, 2);
        }
        let locators = |items: Vec<ActivityItem>| -> Vec<String> {
            items.into_iter().map(|i| i.entity.locator).collect()
        };
        let kind = ActivityKind::FolderVisit;
        assert_eq!(
            locators(
                service
                    .recent_at(kind.clone(), 10, now, Some("c:\\work\\a"))
                    .unwrap()
            ),
            vec!["C:\\Work\\A\\Invoices"]
        );
        assert_eq!(
            locators(
                service
                    .frequent_at(kind.clone(), 10, now, Some("C:\\Work\\A\\"))
                    .unwrap()
            ),
            vec!["C:\\Work\\A\\Invoices"]
        );
        assert_eq!(
            service
                .recent_at(kind.clone(), 10, now, None)
                .unwrap()
                .len(),
            3
        );
        let hits = service
            .search_at(
                "invoices",
                SearchScope::Folder,
                10,
                now,
                Some("C:\\Work\\A"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entity.locator, "C:\\Work\\A\\Invoices");
        assert!(hits[0].reason_codes.contains(&ReasonCode::SameWorkspace));
        // Path-like queries are scoped as well.
        let by_path = service
            .search_at(
                "\\Invoices",
                SearchScope::Folder,
                10,
                now,
                Some("C:\\Work\\A"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(by_path.len(), 1);
        // Both kinds merge under the same root; an unrelated root is empty.
        assert!(
            service
                .search_at("invoices", SearchScope::All, 10, now, Some("D:\\Nowhere"))
                .unwrap()
                .unwrap()
                .is_empty()
        );
        // A relative root is a caller error, not "no results".
        assert!(service.recent_at(kind, 10, now, Some("relative")).is_err());
    }

    #[test]
    fn workspace_scope_applies_to_files_and_hidden_still_wins() {
        let (_dir, service) = open_service();
        let day = base_day() / DAY_MS;
        let now = (day + 20) * DAY_MS + 43_200_000;
        for path in ["C:\\Work\\A\\budget.xlsx", "C:\\Other\\budget.xlsx"] {
            record_at(&service, path, VisitKind::FileOpen, (day + 19) * DAY_MS);
        }
        let hits = service
            .search_at("budget", SearchScope::File, 10, now, Some("C:\\Work\\A"))
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        service.set_hidden(&hits[0].entity.id, true).unwrap();
        assert!(
            service
                .search_at("budget", SearchScope::File, 10, now, Some("C:\\Work\\A"))
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    /// FER-255: the UTC bound is exact around midnight and across days,
    /// independent of the machine's local timezone.
    #[test]
    fn frequent_utc_bounds_hold_near_midnight_and_across_days() {
        let (_dir, service) = open_service();
        let midnight = base_day() + 10 * DAY_MS;
        // 45 s apart (separate episodes) but on different UTC days.
        record_at(
            &service,
            "C:\\Work\\Edge",
            VisitKind::Folder,
            midnight - 40_000,
        );
        record_at(
            &service,
            "C:\\Work\\Edge",
            VisitKind::Folder,
            midnight + 5_000,
        );
        let kind = ActivityKind::FolderVisit;
        let top = |now: u64| {
            service
                .frequent_at(kind.clone(), 5, now, None)
                .unwrap()
                .into_iter()
                .next()
        };
        // 10 s past midnight UTC: ages 1 and 0.
        let item = top(midnight + 10_000).expect("two UTC days");
        assert_eq!(item.active_days, 2);
        let expected = 1.0 + 2.0_f64.powf(-1.0 / 14.0);
        assert!((item.score.unwrap() - expected).abs() < 1e-9);
        // One ms before midnight: the later visit is in the future, one day left.
        assert!(top(midnight - 1).is_none());
        // Next UTC day: ages 2 and 1.
        let item = top(midnight + DAY_MS + 1).unwrap();
        let expected = 2.0_f64.powf(-2.0 / 14.0) + 2.0_f64.powf(-1.0 / 14.0);
        assert!((item.score.unwrap() - expected).abs() < 1e-9);
        // Retention window (90 d) is a UTC millisecond bound: the oldest visit
        // falls out exactly when `now - 90 d` passes it.
        let edge = midnight - 40_000 + 90 * DAY_MS;
        assert!(top(edge).is_some(), "visit at the window start is inside");
        assert!(top(edge + 1).is_none(), "one ms later it is outside");
    }

    impl ActivityService {
        fn episode_count(&self) -> Result<u64, String> {
            self.store
                .lock()
                .map_err(|_| "activity store unavailable".to_owned())?
                .episode_count()
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
#[path = "activity_service_queue_tests.rs"]
mod queue_tests;

#[cfg(test)]
#[path = "activity_service_retention_tests.rs"]
mod retention_tests;
