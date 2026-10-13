use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u8 = 1;
pub const RANKING_VERSION: u8 = 1;
pub const MAX_PAGE_LIMIT: u16 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum EntityKind {
    File,
    Folder,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Availability {
    Available,
    Offline,
    Missing,
    Inaccessible,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum MatchKind {
    ExactPath,
    ExactName,
    Prefix,
    Token,
    Path,
    Initialism,
    Fuzzy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ReasonCode {
    ExactName,
    SimilarName,
    Abbreviation,
    Recent,
    Frequent,
    Favorite,
    SameWorkspace,
    VisitedInFerrum,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ActivityKind {
    FolderVisit,
    FileOpenRequested,
    FilePreview,
    OperationCompleted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ReceiptSource {
    NavigationCommit,
    OpenAccepted,
    PreviewCompleted,
    JobCompleted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Provenance {
    Manual,
    Chat,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ActivityView {
    Recent,
    Frequent,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityEntity {
    pub id: String,
    pub kind: EntityKind,
    pub provider: String,
    pub volume_id: String,
    pub locator: String,
    pub display_name: String,
    pub parent_path: String,
    pub availability: Availability,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityBounds {
    pub from_utc_ms: u64,
    pub until_utc_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityQuery {
    pub version: u8,
    pub request_id: String,
    pub privacy_epoch: u64,
    pub view: ActivityView,
    pub query: String,
    pub kind: Option<EntityKind>,
    pub bounds: Option<ActivityBounds>,
    pub limit: u16,
    pub cursor: Option<String>,
    pub workspace_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityEntry {
    pub entity: ActivityEntity,
    pub last_access_utc_ms: u64,
    pub episode_count: u64,
    pub active_days: u64,
    pub frequent_score: f64,
    pub match_kind: MatchKind,
    pub reason_codes: Vec<ReasonCode>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityCoverage {
    pub from_utc_ms: u64,
    pub until_utc_ms: u64,
    pub oldest_retained_utc_ms: Option<u64>,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityResult {
    pub version: u8,
    pub request_id: String,
    pub privacy_epoch: u64,
    pub ranking_version: u8,
    pub complete: bool,
    pub next_cursor: Option<String>,
    pub coverage: ActivityCoverage,
    pub entries: Vec<ActivityEntry>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityReceipt {
    pub version: u8,
    pub event_id: String,
    pub action_id: String,
    pub privacy_epoch: u64,
    pub source: ReceiptSource,
    pub kind: ActivityKind,
    pub provenance: Provenance,
    pub entity: ActivityEntity,
    pub occurred_utc_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityPreferences {
    pub version: u8,
    pub privacy_epoch: u64,
    pub recording_enabled: bool,
    pub private_session: bool,
    pub retention_days: u16,
    pub excluded_roots: Vec<String>,
    pub hidden_entity_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyCount {
    pub utc_day: String,
    pub episode_count: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityStats {
    pub version: u8,
    pub privacy_epoch: u64,
    pub bounds: ActivityBounds,
    pub coverage: ActivityCoverage,
    pub episode_count: u64,
    pub unique_entities: u64,
    pub daily: Vec<DailyCount>,
}

fn valid_utc_day(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, b)| index == 4 || index == 7 || b.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = value[0..4].parse().unwrap();
    let month: usize = value[5..7].parse().unwrap();
    let day: u32 = value[8..10].parse().unwrap();
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=12).contains(&month) && day >= 1 && day <= days[month - 1]
}

impl ActivityPreferences {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != CONTRACT_VERSION || !matches!(self.retention_days, 7 | 30 | 90) {
            return Err("invalid preferences version or retention");
        }
        if self.excluded_roots.len() > 1000
            || self.hidden_entity_ids.len() > 1000
            || self.excluded_roots.iter().any(|root| !valid_path(root))
            || self
                .hidden_entity_ids
                .iter()
                .any(|entity_id| !valid_id(entity_id))
        {
            return Err("invalid privacy list");
        }
        Ok(())
    }
}

impl ActivityStats {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != CONTRACT_VERSION
            || self.bounds.from_utc_ms >= self.bounds.until_utc_ms
            || self.coverage.from_utc_ms != self.bounds.from_utc_ms
            || self.coverage.until_utc_ms != self.bounds.until_utc_ms
            || self.unique_entities > self.episode_count
            || self.daily.len() > 1000
        {
            return Err("invalid statistics bounds or count");
        }
        let mut total = 0_u64;
        let mut previous = "";
        for day in &self.daily {
            if !valid_utc_day(&day.utc_day) || day.utc_day.as_str() <= previous {
                return Err("invalid or unsorted UTC day");
            }
            total = total
                .checked_add(day.episode_count)
                .ok_or("statistics count overflow")?;
            previous = &day.utc_day;
        }
        if total != self.episode_count {
            return Err("inconsistent statistics count");
        }
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.contains('\0')
}
fn valid_path(value: &str) -> bool {
    let absolute = value.starts_with('/')
        || value.starts_with("\\\\")
        || (value.len() >= 3
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'\\' | b'/'));
    absolute
        && value.len() <= 4096
        && !value.contains('\0')
        && !value.split(['/', '\\']).any(|part| part == "..")
}
impl ActivityEntity {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_id(&self.id) || !valid_id(&self.volume_id) || self.provider != "local" {
            return Err("invalid entity identity");
        }
        if !valid_path(&self.locator)
            || !valid_path(&self.parent_path)
            || self.display_name.len() > 255
        {
            return Err("invalid local locator");
        }
        Ok(())
    }
}
impl ActivityQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != CONTRACT_VERSION || !valid_id(&self.request_id) {
            return Err("invalid contract version or request ID");
        }
        if self.limit == 0
            || self.limit > MAX_PAGE_LIMIT
            || self.query.len() > 256
            || self.query.contains('\0')
        {
            return Err("invalid query or limit");
        }
        if self
            .bounds
            .as_ref()
            .is_some_and(|b| b.from_utc_ms >= b.until_utc_ms)
        {
            return Err("invalid time bounds");
        }
        if self.cursor.as_deref().is_some_and(|v| !valid_id(v))
            || self.workspace_id.as_deref().is_some_and(|v| !valid_id(v))
        {
            return Err("invalid cursor or workspace ID");
        }
        Ok(())
    }
}
impl ActivityReceipt {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != CONTRACT_VERSION
            || !valid_id(&self.event_id)
            || !valid_id(&self.action_id)
        {
            return Err("invalid receipt ID or version");
        }
        self.entity.validate()?;
        if !matches!(
            (&self.source, &self.kind, &self.entity.kind),
            (
                ReceiptSource::NavigationCommit,
                ActivityKind::FolderVisit,
                EntityKind::Folder
            ) | (
                ReceiptSource::OpenAccepted,
                ActivityKind::FileOpenRequested,
                EntityKind::File
            ) | (
                ReceiptSource::PreviewCompleted,
                ActivityKind::FilePreview,
                EntityKind::File
            ) | (
                ReceiptSource::JobCompleted,
                ActivityKind::OperationCompleted,
                _
            )
        ) {
            return Err("receipt source and entity mismatch");
        }
        Ok(())
    }
}
