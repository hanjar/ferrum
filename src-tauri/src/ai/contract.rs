use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, fmt};

pub const MAX_INPUT_BYTES: usize = 32 * 1024;
pub const MAX_JSON_DEPTH: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    InputTooLarge,
    TooDeep,
    InvalidEnvelope(String),
    UnsupportedVersion,
    InvalidRequestId,
    RequestMismatch,
    InvalidOutputName,
    InvalidPanePair,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ContractError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntentEnvelope {
    pub schema_version: u8,
    pub request_id: String,
    pub intent: Intent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capability", deny_unknown_fields)]
pub enum Intent {
    #[serde(rename = "archive.create")]
    ArchiveCreate(ArchiveCreate),
    #[serde(rename = "directories.sync")]
    DirectoriesSync(DirectoriesSync),
    /// ACT-08 read intent: the model only chooses a local history query. It never produces a
    /// plan, never mutates, and its results are never sent back to the provider. Accepted only
    /// by `parse_intent_offered` when the app offered it.
    #[serde(rename = "activity.query")]
    ActivityQuery(ActivityQuery),
    /// FER-471 control intent: change threads / auto mode / priority of the copy job that is
    /// running now. Never a plan, never a path, never an operation id (the app resolves the job
    /// locally) and it only runs after the user presses the confirm button on the chat card.
    #[serde(rename = "copy.adjust")]
    CopyAdjust(CopyAdjust),
    /// FER-476 read intent: the app reads the FerrumIndexer status and answers on a card. Carries
    /// nothing; never mutates; the result is never sent back to the provider.
    #[serde(rename = "indexer.status")]
    IndexerStatus(IndexerStatusRequest),
    /// FER-476 control intent: switch the FerrumIndexer privacy mode. The model only picks the
    /// target mode; the app always asks for confirmation on a card first (and the OS asks for UAC
    /// when raising), so nothing is executed from this intent alone.
    #[serde(rename = "indexer.setMode")]
    IndexerSetMode(IndexerSetMode),
    /// FER-466 read intent: the model only picks the kind of question about the Disk Usage result
    /// that is on screen. The app runs it locally on that snapshot and answers on a card; no scan
    /// data (path, name, size, count) is ever sent to the provider and nothing is mutated.
    #[serde(rename = "diskUsage.query")]
    DiskUsageQuery(DiskUsageQuery),
}

impl Intent {
    pub fn capability_id(&self) -> &'static str {
        match self {
            Intent::ArchiveCreate(_) => "archive.create",
            Intent::DirectoriesSync(_) => "directories.sync",
            Intent::ActivityQuery(_) => ACTIVITY_QUERY_CAPABILITY,
            Intent::CopyAdjust(_) => COPY_ADJUST_CAPABILITY,
            Intent::IndexerStatus(_) => INDEXER_STATUS_CAPABILITY,
            Intent::IndexerSetMode(_) => INDEXER_SET_MODE_CAPABILITY,
            Intent::DiskUsageQuery(_) => DISK_USAGE_QUERY_CAPABILITY,
        }
    }
}

pub const ACTIVITY_QUERY_CAPABILITY: &str = "activity.query";
pub const COPY_ADJUST_CAPABILITY: &str = "copy.adjust";
pub const INDEXER_STATUS_CAPABILITY: &str = "indexer.status";
pub const INDEXER_SET_MODE_CAPABILITY: &str = "indexer.setMode";
pub const DISK_USAGE_QUERY_CAPABILITY: &str = "diskUsage.query";
/// Largest list a `diskUsage.query` may ask for (same as the Disk Usage "Top 20").
pub const MAX_DISK_USAGE_LIMIT: u8 = 20;
/// Largest +/- step one request may ask for.
pub const MAX_COPY_ADJUST_STEP: u8 = 8;
/// Same bounds as `copy_pool::MAX_THREADS`.
pub const MAX_COPY_THREADS: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CopyAdjustAction {
    Increase,
    Decrease,
    Set,
    Auto,
    Priority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CopyPriorityChoice {
    Normal,
    Background,
}

/// Wire form of a copy tuning request (the intent minus `capability`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopyAdjust {
    pub action: CopyAdjustAction,
    /// `increase`/`decrease` only: 1..=8, default 1. `null` is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_u8"
    )]
    pub by: Option<u8>,
    /// `set` only: 1..=16. `null` is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_u8"
    )]
    pub threads: Option<u8>,
    /// `priority` only. `null` is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_priority"
    )]
    pub priority: Option<CopyPriorityChoice>,
}

/// Wire form of a Disk Usage question (the intent minus `capability`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskUsageQuery {
    pub query: DiskUsageQueryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum DiskUsageQueryKind {
    /// Largest files, optionally of one category. `scope`: `view` (default; what the panel shows
    /// now) or `root` (the whole scan). `limit` 1..=20, default 10. `null` is rejected.
    LargestFiles {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null_optional_scope"
        )]
        scope: Option<DiskUsageScope>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null_optional_category"
        )]
        category: Option<DiskUsageCategory>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null_optional_u8"
        )]
        limit: Option<u8>,
    },
    /// Largest folders directly under the scope.
    LargestFolders {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null_optional_scope"
        )]
        scope: Option<DiskUsageScope>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "non_null_optional_u8"
        )]
        limit: Option<u8>,
    },
    /// Totals of the whole scan. Takes no field.
    Summary {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiskUsageScope {
    View,
    Root,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiskUsageCategory {
    Video,
    Image,
    Audio,
    Document,
    Archive,
    Other,
}

/// Semantic check serde cannot express: limit 1..=20.
pub fn validate_disk_usage_query(request: &DiskUsageQuery) -> Result<(), ContractError> {
    let limit = match &request.query {
        DiskUsageQueryKind::LargestFiles { limit, .. }
        | DiskUsageQueryKind::LargestFolders { limit, .. } => *limit,
        DiskUsageQueryKind::Summary {} => None,
    };
    if limit.is_some_and(|n| !(1..=MAX_DISK_USAGE_LIMIT).contains(&n)) {
        return Err(ContractError::InvalidEnvelope(
            "Disk usage limit must be 1..20".into(),
        ));
    }
    Ok(())
}

/// `indexer.status` takes no argument at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexerStatusRequest {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexerModeChoice {
    HighPerformance,
    Shared,
}

/// Wire form of a mode request (the intent minus `capability`): exactly one `mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexerSetMode {
    pub mode: IndexerModeChoice,
}

/// Each action carries exactly its own argument and nothing else.
pub fn validate_copy_adjust(request: &CopyAdjust) -> Result<(), ContractError> {
    let bad = |why: &str| Err(ContractError::InvalidEnvelope(why.into()));
    let CopyAdjust {
        action,
        by,
        threads,
        priority,
    } = request;
    match action {
        CopyAdjustAction::Increase | CopyAdjustAction::Decrease => {
            if threads.is_some() || priority.is_some() {
                return bad("copy.adjust increase/decrease takes only by");
            }
            if by.is_some_and(|n| !(1..=MAX_COPY_ADJUST_STEP).contains(&n)) {
                return bad("copy.adjust step must be 1..8");
            }
        }
        CopyAdjustAction::Set => {
            if by.is_some() || priority.is_some() {
                return bad("copy.adjust set takes only threads");
            }
            if !threads.is_some_and(|n| (1..=MAX_COPY_THREADS).contains(&n)) {
                return bad("copy.adjust threads must be 1..16");
            }
        }
        CopyAdjustAction::Auto => {
            if by.is_some() || threads.is_some() || priority.is_some() {
                return bad("copy.adjust auto takes no arguments");
            }
        }
        CopyAdjustAction::Priority => {
            if by.is_some() || threads.is_some() || priority.is_none() {
                return bad("copy.adjust priority takes only priority");
            }
        }
    }
    Ok(())
}
pub const MAX_HISTORY_LIMIT: u8 = 20;
pub const MAX_HISTORY_NAME_BYTES: usize = 255;
pub const HISTORY_PERIOD_DAYS: [u16; 3] = [7, 30, 90];

/// Wire form of a history lookup; also the exact argument of the `activity_chat_query` IPC
/// command, so the model intent and the IPC share one strict schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityQuery {
    pub query: HistoryQuery,
    /// 1..=20; absent means the tool default (10). `null` is rejected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_u8"
    )]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum HistoryQuery {
    #[serde(rename_all = "camelCase")]
    Recent {
        period_days: u16,
        entity: HistoryEntity,
    },
    #[serde(rename_all = "camelCase")]
    Frequent {
        period_days: u16,
        entity: HistoryEntity,
    },
    /// `name` is a plain name the user typed, never a path.
    FindByName { name: String, entity: HistoryEntity },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryEntity {
    File,
    Folder,
}

/// Semantic checks serde cannot express: period 7/30/90, limit 1..=20, name non-empty,
/// at most 255 bytes, no `\` `/` `:` or control characters (not a path probe).
pub fn validate_activity_query(query: &ActivityQuery) -> Result<(), ContractError> {
    let bad = |why: &str| Err(ContractError::InvalidEnvelope(why.into()));
    if query
        .limit
        .is_some_and(|limit| !(1..=MAX_HISTORY_LIMIT).contains(&limit))
    {
        return bad("History limit must be 1..20");
    }
    match &query.query {
        HistoryQuery::Recent { period_days, .. } | HistoryQuery::Frequent { period_days, .. } => {
            if !HISTORY_PERIOD_DAYS.contains(period_days) {
                return bad("History period must be 7, 30 or 90 days");
            }
        }
        HistoryQuery::FindByName { name, .. } => {
            if name.trim().is_empty()
                || name.len() > MAX_HISTORY_NAME_BYTES
                || name
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\\' | '/' | ':'))
            {
                return bad("History name must be a plain name");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveCreate {
    pub source_pane: SourcePane,
    pub selector: Selector,
    pub format: ArchiveFormat,
    /// FER-576: optional compression level; absent = the engine default (normal).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_archive_level"
    )]
    pub level: Option<ArchiveLevel>,
    pub target: ArchiveTarget,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_string"
    )]
    pub output_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourcePane {
    Left,
    Right,
    ActiveAtSubmit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Pane {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Selector {
    Selected(SelectedSelector),
    AllVisible(AllVisibleSelector),
    DirectoryContents(DirectoryContentsSelector),
}

/// Maximum number of extensions in one selector filter (AI-09).
pub const MAX_EXTENSIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllVisibleSelector {
    pub entries: EntryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryContentsSelector {
    pub include_hidden: bool,
    pub entries: EntryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

impl Selector {
    fn extensions(&self) -> Option<&Vec<String>> {
        match self {
            Selector::Selected(s) => s.extensions.as_ref(),
            Selector::AllVisible(s) => s.extensions.as_ref(),
            Selector::DirectoryContents(s) => s.extensions.as_ref(),
        }
    }
}

/// Extension filter: 1..=8 unique entries, each 1..=16 chars of `[a-z0-9]` (no dot/wildcard/path).
fn safe_extensions(list: &[String]) -> bool {
    if list.is_empty() || list.len() > MAX_EXTENSIONS {
        return false;
    }
    let mut seen = HashSet::new();
    list.iter().all(|ext| {
        (1..=16).contains(&ext.len())
            && ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && seen.insert(ext.as_str())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryScope {
    All,
    FilesOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveFormat {
    Zip,
    /// FER-576: 7z (LZMA2) through the same Pack service as the manual 7Z dialog.
    #[serde(rename = "7z")]
    Sevenz,
}

/// FER-576: closed set of compression levels the model may name. `maximum` and `ultra` both map to
/// the engine's strongest level; the model never sees or sets engine flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveLevel {
    Store,
    Fast,
    Normal,
    Maximum,
    Ultra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ArchiveTarget {
    Pane(PaneTarget),
    SourceDirectory(SourceDirectoryTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneTarget {
    pub pane: SourcePane,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDirectoryTarget {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoriesSync {
    pub left_pane: Pane,
    pub right_pane: Pane,
    pub mode: SyncMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncMode {
    Bidirectional,
}

/// Legacy parser: mutating capabilities only. `activity.query` is rejected here exactly as it was
/// before the variant existed; use `parse_intent_offered` with the offered list instead.
pub fn parse_intent(
    value: Value,
    active_request_id: &str,
) -> Result<IntentEnvelope, ContractError> {
    let envelope = parse_envelope(value, active_request_id)?;
    if matches!(
        envelope.intent,
        Intent::ActivityQuery(_)
            | Intent::CopyAdjust(_)
            | Intent::IndexerStatus(_)
            | Intent::IndexerSetMode(_)
            | Intent::DiskUsageQuery(_)
    ) {
        return Err(ContractError::InvalidEnvelope(
            "Capability not offered".into(),
        ));
    }
    Ok(envelope)
}

/// Strict parser for a model reply: the intent's capability must be in `offered` (the list the
/// app sent for this request), then the same per-capability validation applies.
pub fn parse_intent_offered(
    value: Value,
    active_request_id: &str,
    offered: &[&str],
) -> Result<IntentEnvelope, ContractError> {
    let envelope = parse_envelope(value, active_request_id)?;
    if !offered.contains(&envelope.intent.capability_id()) {
        return Err(ContractError::InvalidEnvelope(
            "Capability not offered".into(),
        ));
    }
    Ok(envelope)
}

fn parse_envelope(value: Value, active_request_id: &str) -> Result<IntentEnvelope, ContractError> {
    let envelope: IntentEnvelope = decode(value)?;
    if envelope.schema_version != 1 {
        return Err(ContractError::UnsupportedVersion);
    }
    if !safe_request_id(&envelope.request_id) || !safe_request_id(active_request_id) {
        return Err(ContractError::InvalidRequestId);
    }
    if envelope.request_id != active_request_id {
        return Err(ContractError::RequestMismatch);
    }
    match &envelope.intent {
        Intent::ArchiveCreate(archive) => {
            if archive
                .selector
                .extensions()
                .is_some_and(|list| !safe_extensions(list))
            {
                return Err(ContractError::InvalidEnvelope(
                    "Invalid extension filter".into(),
                ));
            }
            if archive
                .output_name
                .as_deref()
                .is_some_and(|name| !safe_output_name(name))
            {
                return Err(ContractError::InvalidOutputName);
            }
        }
        Intent::DirectoriesSync(sync) => {
            if sync.left_pane != Pane::Left || sync.right_pane != Pane::Right {
                return Err(ContractError::InvalidPanePair);
            }
        }
        Intent::ActivityQuery(query) => validate_activity_query(query)?,
        Intent::CopyAdjust(request) => validate_copy_adjust(request)?,
        Intent::DiskUsageQuery(request) => validate_disk_usage_query(request)?,
        // serde already enforces the closed shapes (empty object / one `mode`).
        Intent::IndexerStatus(_) | Intent::IndexerSetMode(_) => {}
    }
    Ok(envelope)
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ContractError> {
    let input_bytes = serde_json::to_vec(&value)
        .map_err(|error| ContractError::InvalidEnvelope(error.to_string()))?;
    if input_bytes.len() > MAX_INPUT_BYTES {
        return Err(ContractError::InputTooLarge);
    }
    // Iterate instead of recursing over untrusted nested JSON.
    let mut pending = vec![(&value, 0usize)];
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_JSON_DEPTH {
            return Err(ContractError::TooDeep);
        }
        match node {
            Value::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(items) => pending.extend(items.values().map(|item| (item, depth + 1))),
            _ => {}
        }
    }
    serde_json::from_value(value).map_err(|error| ContractError::InvalidEnvelope(error.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaneSnapshot {
    pub schema_version: u8,
    pub request_id: String,
    pub active_pane_at_submit: Pane,
    pub panes: [PaneStateSnapshot; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaneStateSnapshot {
    pub pane_id: Pane,
    pub tab_id: String,
    pub location: PaneLocation,
    pub context_revision: u64,
    pub selection_revision: u64,
    pub selection_handle: String,
    pub filter: String,
    pub show_hidden: bool,
    pub sort: PaneSort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaneLocation {
    pub kind: LocationKind,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocationKind {
    Disk,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaneSort {
    pub key: PaneSortKey,
    pub dir: PaneSortDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaneSortKey {
    Name,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSortDirection {
    Ascending,
    Descending,
}

impl Serialize for PaneSortDirection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i8(match self {
            Self::Ascending => 1,
            Self::Descending => -1,
        })
    }
}

impl<'de> Deserialize<'de> for PaneSortDirection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match i8::deserialize(deserializer)? {
            1 => Ok(Self::Ascending),
            -1 => Ok(Self::Descending),
            _ => Err(serde::de::Error::custom("sort dir must be 1 or -1")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedPlan {
    pub schema_version: u8,
    pub plan_id: String,
    pub request_id: String,
    pub revision: u64,
    pub capability: Capability,
    pub source_snapshot: PaneSnapshot,
    pub operation: ResolvedOperation,
    pub preview: PlanPreview,
    pub steps: Vec<PlanStep>,
    pub preview_digest: String,
    pub risk: Risk,
    pub requires_approval: bool,
    pub step_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ResolvedOperation {
    #[serde(rename = "directories.sync")]
    DirectoriesSync {
        #[serde(rename = "leftRoot")]
        left_root: String,
        #[serde(rename = "rightRoot")]
        right_root: String,
        #[serde(rename = "leftManifest")]
        left_manifest: String,
        #[serde(rename = "rightManifest")]
        right_manifest: String,
    },
    #[serde(rename = "archive.create")]
    ArchiveCreate {
        #[serde(rename = "sourceManifest")]
        source_manifest: String,
        #[serde(rename = "targetDirectory")]
        target_directory: String,
        #[serde(rename = "outputName")]
        output_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanPreview {
    pub item_count: u64,
    pub conflict_count: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanStep {
    pub id: String,
    pub capability: Capability,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "archive.create")]
    ArchiveCreate,
    #[serde(rename = "directories.sync")]
    DirectoriesSync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Risk {
    Mutating,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedHandle {
    pub schema_version: u8,
    pub handle_id: String,
    pub plan_id: String,
    pub request_id: String,
    pub revision: u64,
    pub plan_digest: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionReceipt {
    pub schema_version: u8,
    pub execution_id: String,
    pub plan_id: String,
    pub job_ids: Vec<String>,
    pub state: ExecutionState,
    pub completed: u64,
    pub failed: u64,
    pub errors: Vec<ItemError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionState {
    Queued,
    Running,
    Completed,
    Partial,
    Failed,
    Cancelled,
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemError {
    pub code: ContractErrorCode,
    pub message: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_optional_string"
    )]
    pub item_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractErrorCode {
    InvalidPayload,
    UnsupportedVersion,
    RequestMismatch,
    InvalidReference,
    LimitExceeded,
}

fn check_version(schema_version: u8) -> Result<(), ContractError> {
    if schema_version != 1 {
        Err(ContractError::UnsupportedVersion)
    } else {
        Ok(())
    }
}

fn check_ref(reference: &str) -> Result<(), ContractError> {
    if safe_request_id(reference) {
        Ok(())
    } else {
        Err(ContractError::InvalidRequestId)
    }
}

/// Item counts scale with the panel (matches the TS contract and the provider context limit).
const MAX_ITEMS: u64 = 1_000_000;

fn check_count(value: u64, max: u64) -> Result<(), ContractError> {
    if value <= max {
        Ok(())
    } else {
        Err(ContractError::InputTooLarge)
    }
}

pub fn parse_pane_snapshot(value: Value) -> Result<PaneSnapshot, ContractError> {
    let snapshot: PaneSnapshot = decode(value)?;
    check_version(snapshot.schema_version)?;
    check_ref(&snapshot.request_id)?;
    if snapshot.panes[0].pane_id != Pane::Left
        || snapshot.panes[1].pane_id != Pane::Right
        || snapshot.panes[0].tab_id == snapshot.panes[1].tab_id
    {
        return Err(ContractError::InvalidPanePair);
    }
    for pane in &snapshot.panes {
        check_ref(&pane.tab_id)?;
        check_ref(&pane.selection_handle)?;
        check_count(pane.context_revision, 10_000)?;
        check_count(pane.selection_revision, 10_000)?;
        if pane.location.path.is_empty()
            || pane.location.path.len() > 4096
            || pane.filter.len() > 256
        {
            return Err(ContractError::InputTooLarge);
        }
    }
    Ok(snapshot)
}

pub fn parse_resolved_plan(value: Value) -> Result<ResolvedPlan, ContractError> {
    let plan: ResolvedPlan = decode(value)?;
    check_version(plan.schema_version)?;
    check_ref(&plan.plan_id)?;
    check_ref(&plan.request_id)?;
    check_ref(&plan.preview_digest)?;
    check_count(plan.revision, 10_000)?;
    check_count(plan.step_count, 10_000)?;
    if plan.revision == 0 || plan.step_count == 0 || !plan.requires_approval {
        return Err(ContractError::InvalidEnvelope(
            "Invalid plan authority or count".into(),
        ));
    }
    for count in [plan.preview.item_count, plan.preview.conflict_count] {
        check_count(count, MAX_ITEMS)?;
    }
    check_count(plan.preview.total_bytes, 9_007_199_254_740_991)?;
    if plan.preview.conflict_count > plan.preview.item_count
        || plan.step_count != plan.steps.len() as u64
    {
        return Err(ContractError::InvalidEnvelope("Invalid plan count".into()));
    }
    let mut seen = HashSet::new();
    for step in &plan.steps {
        check_ref(&step.id)?;
        if step.capability != plan.capability || !seen.insert(step.id.as_str()) {
            return Err(ContractError::InvalidEnvelope("Invalid plan step".into()));
        }
        let mut dependencies = HashSet::new();
        for dependency in &step.depends_on {
            check_ref(dependency)?;
            if !seen.contains(dependency.as_str()) || !dependencies.insert(dependency.as_str()) {
                return Err(ContractError::InvalidEnvelope(
                    "Invalid step dependency".into(),
                ));
            }
        }
    }
    parse_pane_snapshot(
        serde_json::to_value(&plan.source_snapshot)
            .map_err(|error| ContractError::InvalidEnvelope(error.to_string()))?,
    )?;
    if plan.request_id != plan.source_snapshot.request_id {
        return Err(ContractError::RequestMismatch);
    }
    match &plan.operation {
        ResolvedOperation::DirectoriesSync {
            left_root,
            right_root,
            left_manifest,
            right_manifest,
        } => {
            if plan.capability != Capability::DirectoriesSync
                || plan
                    .source_snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.location.kind != LocationKind::Disk)
                || !safe_windows_path(left_root)
                || !safe_windows_path(right_root)
                || left_root != &plan.source_snapshot.panes[0].location.path
                || right_root != &plan.source_snapshot.panes[1].location.path
            {
                return Err(ContractError::InvalidEnvelope("Invalid sync roots".into()));
            }
            check_ref(left_manifest)?;
            check_ref(right_manifest)?;
        }
        ResolvedOperation::ArchiveCreate {
            source_manifest,
            target_directory,
            output_name,
        } => {
            if plan.capability != Capability::ArchiveCreate
                || !safe_windows_path(target_directory)
                || !safe_output_name(output_name)
            {
                return Err(ContractError::InvalidEnvelope(
                    "Invalid archive operation".into(),
                ));
            }
            check_ref(source_manifest)?;
        }
    }
    Ok(plan)
}

fn safe_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() > 4096
        || bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || path.contains('/')
        || path.contains('\0')
        || path.starts_with("\\\\")
    {
        return false;
    }
    path[3..].split('\\').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && !segment.ends_with(['.', ' '])
            && !segment
                .chars()
                .any(|ch| ch.is_control() || "<>:\"|?*".contains(ch))
            && !is_reserved_windows_name(segment)
    })
}

fn is_reserved_windows_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

pub fn parse_prepared_handle(value: Value) -> Result<PreparedHandle, ContractError> {
    let handle: PreparedHandle = decode(value)?;
    check_version(handle.schema_version)?;
    for reference in [
        &handle.handle_id,
        &handle.plan_id,
        &handle.request_id,
        &handle.plan_digest,
    ] {
        check_ref(reference)?;
    }
    check_count(handle.revision, 10_000)?;
    if handle.revision == 0 || handle.expires_at_ms > 9_007_199_254_740_991 {
        return Err(ContractError::InvalidEnvelope(
            "Invalid revision or expiry".into(),
        ));
    }
    Ok(handle)
}

pub fn assert_prepared_for_plan(
    handle: &PreparedHandle,
    plan: &ResolvedPlan,
    now_ms: u64,
) -> Result<(), ContractError> {
    if handle.plan_id != plan.plan_id
        || handle.request_id != plan.request_id
        || handle.revision != plan.revision
        || handle.plan_digest != plan.preview_digest
    {
        return Err(ContractError::RequestMismatch);
    }
    if now_ms >= handle.expires_at_ms {
        return Err(ContractError::InvalidEnvelope(
            "Prepared handle expired".into(),
        ));
    }
    Ok(())
}

pub fn parse_execution_receipt(value: Value) -> Result<ExecutionReceipt, ContractError> {
    let receipt: ExecutionReceipt = decode(value)?;
    check_version(receipt.schema_version)?;
    check_ref(&receipt.execution_id)?;
    check_ref(&receipt.plan_id)?;
    if receipt.job_ids.len() > 256 || receipt.errors.len() > 256 {
        return Err(ContractError::InputTooLarge);
    }
    for id in &receipt.job_ids {
        check_ref(id)?;
    }
    check_count(receipt.completed, MAX_ITEMS)?;
    check_count(receipt.failed, MAX_ITEMS)?;
    for error in &receipt.errors {
        if error.message.len() > 512 {
            return Err(ContractError::InputTooLarge);
        }
        if let Some(reference) = &error.item_ref {
            check_ref(reference)?;
        }
    }
    if matches!(receipt.state, ExecutionState::Completed)
        && (receipt.failed != 0 || !receipt.errors.is_empty())
    {
        return Err(ContractError::InvalidEnvelope(
            "Completed receipt contains failures".into(),
        ));
    }
    if matches!(receipt.state, ExecutionState::Queued)
        && (receipt.completed != 0 || receipt.failed != 0 || !receipt.errors.is_empty())
    {
        return Err(ContractError::InvalidEnvelope(
            "Queued receipt contains results".into(),
        ));
    }
    Ok(receipt)
}

fn safe_request_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn non_null_optional_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn non_null_optional_priority<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CopyPriorityChoice>, D::Error> {
    CopyPriorityChoice::deserialize(deserializer).map(Some)
}

fn non_null_optional_archive_level<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ArchiveLevel>, D::Error> {
    ArchiveLevel::deserialize(deserializer).map(Some)
}

fn non_null_optional_scope<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DiskUsageScope>, D::Error> {
    DiskUsageScope::deserialize(deserializer).map(Some)
}

fn non_null_optional_category<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DiskUsageCategory>, D::Error> {
    DiskUsageCategory::deserialize(deserializer).map(Some)
}

fn non_null_optional_u8<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u8>, D::Error> {
    u8::deserialize(deserializer).map(Some)
}

fn safe_output_name(name: &str) -> bool {
    if !(1..=128).contains(&name.len())
        || !name.is_ascii()
        || !name.as_bytes()[0].is_ascii_alphanumeric()
    {
        return false;
    }
    if name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return false;
    }
    if name
        .bytes()
        .any(|byte| !byte.is_ascii_alphanumeric() && !b" _.()-".contains(&byte))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }
    if stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
        && stem.as_bytes()[3] != b'0'
    {
        return false;
    }
    true
}

#[cfg(test)]
#[path = "contract_activity_tests.rs"]
mod activity_query_tests;

#[cfg(test)]
#[path = "contract_copy_adjust_tests.rs"]
mod copy_adjust_tests;

#[cfg(test)]
#[path = "contract_disk_usage_tests.rs"]
mod disk_usage_tests;

#[cfg(test)]
#[path = "contract_indexer_tests.rs"]
mod indexer_tests;

#[cfg(test)]
#[path = "contract_rar_tests.rs"]
mod rar_boundary_tests;

#[cfg(test)]
#[path = "contract_ipc_gate_tests.rs"]
mod ipc_gate_tests;

#[cfg(test)]
#[path = "contract_archive_tests.rs"]
mod archive_tests;
