use std::collections::{BTreeMap, HashMap, HashSet};

pub const RANKING_VERSION: u32 = 1;
const DAY_MS: i64 = 86_400_000;
const HALF_LIFE_DAYS: f64 = 14.0;
const DAILY_CAP: u32 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AccessKind {
    FolderVisit,
    FileOpenRequested,
}

#[derive(Clone, Copy, Debug)]
pub struct Episode<'a> {
    pub entity_id: &'a str,
    pub kind: AccessKind,
    pub last_utc_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RankedEntity {
    pub entity_id: String,
    pub kind: AccessKind,
    pub last_utc_ms: i64,
    pub episode_count: u64,
    pub active_days: usize,
    pub score: f64,
    pub ranking_version: u32,
}

struct Counts {
    last_utc_ms: i64,
    episode_count: u64,
    daily: BTreeMap<i64, u32>,
}

impl Counts {
    fn new(last_utc_ms: i64) -> Self {
        Self {
            last_utc_ms,
            episode_count: 0,
            daily: BTreeMap::new(),
        }
    }
}

fn collect<'a>(
    episodes: &[Episode<'a>],
    from_utc_ms: i64,
    until_utc_ms: i64,
    kind: AccessKind,
) -> HashMap<&'a str, Counts> {
    let mut entities: HashMap<&'a str, Counts> = HashMap::new();
    if from_utc_ms >= until_utc_ms {
        return entities;
    }
    for episode in episodes.iter().filter(|episode| {
        episode.kind == kind
            && episode.last_utc_ms >= from_utc_ms
            && episode.last_utc_ms < until_utc_ms
    }) {
        let counts = entities
            .entry(episode.entity_id)
            .or_insert_with(|| Counts::new(episode.last_utc_ms));
        counts.last_utc_ms = counts.last_utc_ms.max(episode.last_utc_ms);
        counts.episode_count += 1;
        *counts
            .daily
            .entry(episode.last_utc_ms.div_euclid(DAY_MS))
            .or_default() += 1;
    }
    entities
}

fn row(entity_id: &str, kind: AccessKind, counts: Counts, score: f64) -> RankedEntity {
    RankedEntity {
        entity_id: entity_id.to_owned(),
        kind,
        last_utc_ms: counts.last_utc_ms,
        episode_count: counts.episode_count,
        active_days: counts.daily.len(),
        score,
        ranking_version: RANKING_VERSION,
    }
}

/// Bounds are UTC milliseconds: inclusive start, exclusive end. Input contains eligible episodes only.
pub fn recent(
    episodes: &[Episode<'_>],
    from_utc_ms: i64,
    until_utc_ms: i64,
    kind: AccessKind,
) -> Vec<RankedEntity> {
    let mut rows: Vec<_> = collect(episodes, from_utc_ms, until_utc_ms, kind)
        .into_iter()
        .map(|(id, counts)| row(id, kind, counts, 0.0))
        .collect();
    rows.sort_by(|a, b| {
        b.last_utc_ms
            .cmp(&a.last_utc_ms)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    rows
}

/// Score uses UTC calendar buckets and one fixed query clock; episode_count remains uncapped.
pub fn frequent(
    episodes: &[Episode<'_>],
    from_utc_ms: i64,
    now_utc_ms: i64,
    kind: AccessKind,
) -> Vec<RankedEntity> {
    let Some(until_utc_ms) = now_utc_ms.checked_add(1) else {
        return Vec::new();
    };
    let today = now_utc_ms.div_euclid(DAY_MS);
    let mut rows: Vec<_> = collect(episodes, from_utc_ms, until_utc_ms, kind)
        .into_iter()
        .filter(|(_, counts)| counts.daily.len() >= 2)
        .map(|(id, counts)| {
            let score = counts
                .daily
                .iter()
                .map(|(&day, &count)| {
                    let age_days = today.saturating_sub(day).max(0) as f64;
                    f64::from(count.min(DAILY_CAP)) * 2.0_f64.powf(-age_days / HALF_LIFE_DAYS)
                })
                .sum();
            row(id, kind, counts, score)
        })
        .collect();
    rows.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.last_utc_ms.cmp(&a.last_utc_ms))
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    rows
}

/// Page a fixed result snapshot; callers must invalidate snapshots when history or privacy epoch changes.
pub fn page<'a>(
    rows: &'a [RankedEntity],
    after_id: Option<&str>,
    limit: usize,
) -> &'a [RankedEntity] {
    if limit == 0 || rows.is_empty() {
        return &rows[0..0];
    }
    let start = match after_id {
        None => 0,
        Some(id) => match rows.iter().position(|row| row.entity_id == id) {
            Some(index) => index + 1,
            None => return &rows[0..0],
        },
    };
    &rows[start..rows.len().min(start.saturating_add(limit))]
}

/// Reject duplicate entity IDs when constructing a snapshot from mixed result kinds.
pub fn distinct_entities(rows: &[RankedEntity]) -> bool {
    let mut seen = HashSet::new();
    rows.iter()
        .all(|row| seen.insert((&row.entity_id, row.kind)))
}
