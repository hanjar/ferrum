//! ACT-08 chat capability: a pure, read-only adapter between a structured AI
//! request and the local activity store. It is NOT wired to any chat runtime
//! (phase-3 chat does not exist yet); a future runtime calls `run_chat_tool`.
//!
//! Guarantees (see docs/activity/chat-integration.md):
//! - reads go through the `ActivityReader` trait, which exposes no write calls;
//! - output carries minimum metadata only: stable id, display name, truncated
//!   parent hint, kind, availability, ranking reasons. Never a locator, counts,
//!   timestamps, database or file content;
//! - hidden entities, excluded subtrees, private sessions and stale privacy
//!   epochs never produce entries;
//! - duplicate names return candidates to disambiguate, never a silent pick;
//! - follow-up operations are references only and need a phase-3 prepared plan.

use crate::activity_matcher::{MatchClass, MatchResult, compare_matches, match_candidate};
use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, Availability, EntityKind, ReasonCode,
};
use ferrum_activity_store::{ActivityStore, FrequentRow, RecentRow};
use serde::Serialize;
use std::collections::HashSet;

pub const DEFAULT_RESULTS: u16 = 10;
pub const MAX_RESULTS: u16 = 20;
pub const PARENT_HINT_MAX_CHARS: usize = 48;
pub const MAX_QUERY_BYTES: usize = 255;
pub const FOLLOW_UP_POLICY: &str =
    "reference-only: zip/sync require a phase-3 prepared plan and user approval";
const DAY_MS: u64 = 86_400_000;
const STORE_PAGE_MAX: u16 = 100;
const CANDIDATE_POOL: u16 = 100;
const HINT_BUDGETS: [usize; 4] = [PARENT_HINT_MAX_CHARS, 96, 160, 255];

/// Read-only view of the store. Deliberately the only way the tool reads.
pub trait ActivityReader {
    fn privacy_epoch(&self) -> Result<u64, String>;
    fn recent(
        &mut self,
        from: u64,
        until: u64,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<RecentRow>>, String>;
    fn frequent(
        &mut self,
        from: u64,
        now: u64,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<FrequentRow>>, String>;
    fn name_candidates(
        &mut self,
        query: &str,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<ActivityEntity>>, String>;
}

impl ActivityReader for ActivityStore {
    fn privacy_epoch(&self) -> Result<u64, String> {
        ActivityStore::privacy_epoch(self).map_err(|e| e.to_string())
    }
    fn recent(
        &mut self,
        from: u64,
        until: u64,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<RecentRow>>, String> {
        ActivityStore::recent(self, from, until, kind, epoch, limit).map_err(|e| e.to_string())
    }
    fn frequent(
        &mut self,
        from: u64,
        now: u64,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<FrequentRow>>, String> {
        ActivityStore::frequent(self, from, now, kind, epoch, limit).map_err(|e| e.to_string())
    }
    fn name_candidates(
        &mut self,
        query: &str,
        kind: ActivityKind,
        epoch: u64,
        limit: u16,
    ) -> Result<Option<Vec<ActivityEntity>>, String> {
        ActivityStore::search_candidates(self, query, kind, epoch, limit).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRequestKind {
    /// `days` must be 7, 30 or 90 (same windows as the UI retention choices).
    Recent {
        days: u16,
        file: bool,
    },
    Frequent {
        days: u16,
        file: bool,
    },
    /// Name only; path-like queries are rejected so the tool is not a path probe.
    FindByName {
        query: String,
        file: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    pub kind: ChatRequestKind,
    pub limit: Option<u16>,
}

/// Facts the caller owns: clock, privacy state and local suppression lists.
pub struct ChatContext<'a> {
    pub now_utc_ms: u64,
    pub private_session: bool,
    /// Privacy epoch observed when the caller took its snapshot. When set, the
    /// tool queries with it, so any privacy change since then yields `Stale`
    /// instead of silently re-reading under a newer epoch. `None` reads the
    /// reader's current epoch (single-shot callers).
    pub expected_epoch: Option<u64>,
    pub hidden_entity_ids: &'a HashSet<String>,
    pub excluded_roots: &'a [String],
}

/// Owned, point-in-time copy of the caller-side facts (see
/// `ActivityService::chat_context`). `context()` borrows it for one request.
#[derive(Debug, Clone)]
pub struct ChatSnapshot {
    pub now_utc_ms: u64,
    /// True while recording is paused (the only private mode Ferrum has); a
    /// runtime with its own private-session flag may OR it in here.
    pub private_session: bool,
    pub privacy_epoch: u64,
    pub hidden_entity_ids: HashSet<String>,
    /// Defence in depth only: the store already deletes data under an excluded
    /// root and refuses to record it, and exposes no read API for the list.
    pub excluded_roots: Vec<String>,
}

impl ChatSnapshot {
    pub fn context(&self) -> ChatContext<'_> {
        ChatContext {
            now_utc_ms: self.now_utc_ms,
            private_session: self.private_session,
            expected_epoch: Some(self.privacy_epoch),
            hidden_entity_ids: &self.hidden_entity_ids,
            excluded_roots: &self.excluded_roots,
        }
    }
}

/// Minimum disclosure per entity. No locator, counts, scores or timestamps.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEntityRef {
    pub id: String,
    pub display_name: String,
    pub parent_hint: String,
    pub kind: EntityKind,
    pub availability: Availability,
    pub reasons: Vec<ReasonCode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ChatResponse {
    Entities {
        entries: Vec<ChatEntityRef>,
        truncated: bool,
    },
    Resolved {
        entry: ChatEntityRef,
    },
    /// Several entities share the best match; the user must choose.
    Ambiguous {
        candidates: Vec<ChatEntityRef>,
    },
    NotFound,
    /// Private session or recording state: history is not offered at all.
    Withheld,
    /// Privacy epoch changed mid-request; retry rather than use stale data.
    Stale,
    Rejected {
        reason: String,
    },
    Unavailable,
}

/// Follow-up handle for operations such as ZIP or sync. It names the entity
/// only; executing anything needs a phase-3 prepared plan plus approval.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityReference {
    pub entity_id: String,
    pub requires_prepared_plan: bool,
}

pub fn follow_up_reference(entry: &ChatEntityRef) -> EntityReference {
    EntityReference {
        entity_id: entry.id.clone(),
        requires_prepared_plan: true,
    }
}

fn reject(reason: &str) -> ChatResponse {
    ChatResponse::Rejected {
        reason: reason.to_owned(),
    }
}

fn kind_of(file: bool) -> ActivityKind {
    if file {
        ActivityKind::FileOpenRequested
    } else {
        ActivityKind::FolderVisit
    }
}

fn effective_limit(limit: Option<u16>) -> Option<u16> {
    match limit {
        None => Some(DEFAULT_RESULTS),
        Some(0) => None,
        Some(value) => Some(value.min(MAX_RESULTS)),
    }
}

fn is_windows_like(path: &str) -> bool {
    path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\")
}

fn norm(path: &str) -> String {
    let unified = path.replace('/', "\\");
    let trimmed = unified.trim_end_matches('\\');
    if is_windows_like(trimmed) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Subtree boundary check: `C:\Priv` excludes `C:\Priv\x` but not `C:\PrivOther`.
pub fn is_excluded(locator: &str, roots: &[String]) -> bool {
    let target = norm(locator);
    roots.iter().any(|root| {
        let root = norm(root);
        !root.is_empty()
            && (target == root
                || target
                    .strip_prefix(&root)
                    .is_some_and(|rest| rest.starts_with('\\')))
    })
}

fn suppressed(entity: &ActivityEntity, context: &ChatContext<'_>) -> bool {
    context.hidden_entity_ids.contains(&entity.id)
        || is_excluded(&entity.locator, context.excluded_roots)
}

fn parent_hint(parent: &str, budget: usize) -> String {
    let trimmed = parent.trim_end_matches(['/', '\\']);
    let count = trimmed.chars().count();
    if count <= budget {
        return trimmed.to_owned();
    }
    let tail: String = trimmed.chars().skip(count - budget).collect();
    format!("\u{2026}{tail}")
}

fn entity_ref(entity: &ActivityEntity, reasons: Vec<ReasonCode>, budget: usize) -> ChatEntityRef {
    ChatEntityRef {
        id: entity.id.clone(),
        display_name: entity.display_name.clone(),
        parent_hint: parent_hint(&entity.parent_path, budget),
        kind: entity.kind.clone(),
        availability: entity.availability.clone(),
        reasons,
    }
}

/// Widens parent hints until candidates are pairwise distinct; if two share a
/// parent entirely, the stable id is appended so the user can still tell them apart.
fn disambiguated(items: &[(&ActivityEntity, Vec<ReasonCode>)]) -> Vec<ChatEntityRef> {
    for budget in HINT_BUDGETS {
        let refs: Vec<_> = items
            .iter()
            .map(|(entity, reasons)| entity_ref(entity, reasons.clone(), budget))
            .collect();
        let distinct: HashSet<_> = refs.iter().map(|r| r.parent_hint.as_str()).collect();
        if distinct.len() == refs.len() {
            return refs;
        }
    }
    items
        .iter()
        .map(|(entity, reasons)| {
            let mut entry = entity_ref(entity, reasons.clone(), HINT_BUDGETS[3]);
            entry.parent_hint = format!("{} (id {})", entry.parent_hint, entity.id);
            entry
        })
        .collect()
}

fn reasons_for(class: MatchClass) -> Vec<ReasonCode> {
    vec![match class {
        MatchClass::ExactPath | MatchClass::Exact => ReasonCode::ExactName,
        MatchClass::Initialism | MatchClass::Subsequence => ReasonCode::Abbreviation,
        MatchClass::Prefix | MatchClass::Token | MatchClass::Path | MatchClass::Fuzzy => {
            ReasonCode::SimilarName
        }
    }]
}

fn path_like(query: &str) -> bool {
    query.contains('/') || query.contains('\\') || query.as_bytes().get(1) == Some(&b':')
}

fn period_ms(days: u16) -> Option<u64> {
    matches!(days, 7 | 30 | 90).then(|| u64::from(days) * DAY_MS)
}

/// Same local queries the UI uses (store recent/frequent/candidates + matcher),
/// bounded and filtered. Never mutates the store.
pub fn run_chat_tool(
    reader: &mut dyn ActivityReader,
    request: &ChatRequest,
    context: &ChatContext<'_>,
) -> ChatResponse {
    let Some(limit) = effective_limit(request.limit) else {
        return reject("limit must be at least 1");
    };
    if context.private_session {
        return ChatResponse::Withheld;
    }
    let epoch = match context.expected_epoch {
        Some(epoch) => epoch,
        None => match reader.privacy_epoch() {
            Ok(epoch) => epoch,
            Err(_) => return ChatResponse::Unavailable,
        },
    };
    let want = usize::from(limit);
    let fetch = u16::try_from(
        (want + 1 + context.hidden_entity_ids.len()).min(usize::from(STORE_PAGE_MAX)),
    )
    .unwrap_or(STORE_PAGE_MAX);
    match &request.kind {
        ChatRequestKind::Recent { days, file } => {
            let Some(span) = period_ms(*days) else {
                return reject("days must be 7, 30 or 90");
            };
            let from = context.now_utc_ms.saturating_sub(span);
            let until = context.now_utc_ms.saturating_add(DAY_MS);
            let rows = match reader.recent(from, until, kind_of(*file), epoch, fetch) {
                Ok(Some(rows)) => rows,
                Ok(None) => return ChatResponse::Stale,
                Err(_) => return ChatResponse::Unavailable,
            };
            listed(
                rows.into_iter().map(|row| row.entity),
                ReasonCode::Recent,
                want,
                context,
            )
        }
        ChatRequestKind::Frequent { days, file } => {
            let Some(span) = period_ms(*days) else {
                return reject("days must be 7, 30 or 90");
            };
            let from = context.now_utc_ms.saturating_sub(span);
            let rows = match reader.frequent(from, context.now_utc_ms, kind_of(*file), epoch, fetch)
            {
                Ok(Some(rows)) => rows,
                Ok(None) => return ChatResponse::Stale,
                Err(_) => return ChatResponse::Unavailable,
            };
            listed(
                rows.into_iter().map(|row| row.entity),
                ReasonCode::Frequent,
                want,
                context,
            )
        }
        ChatRequestKind::FindByName { query, file } => {
            let query = query.trim();
            if query.is_empty() || query.len() > MAX_QUERY_BYTES {
                return reject("query must be a non-empty name of at most 255 bytes");
            }
            if path_like(query) {
                return reject("query must be a name, not a path");
            }
            let found = match reader.name_candidates(query, kind_of(*file), epoch, CANDIDATE_POOL) {
                Ok(Some(found)) => found,
                Ok(None) => return ChatResponse::Stale,
                Err(_) => return ChatResponse::Unavailable,
            };
            resolve(query, &found, want, context)
        }
    }
}

fn listed(
    entities: impl Iterator<Item = ActivityEntity>,
    reason: ReasonCode,
    want: usize,
    context: &ChatContext<'_>,
) -> ChatResponse {
    let mut kept: Vec<ActivityEntity> = entities.filter(|e| !suppressed(e, context)).collect();
    let truncated = kept.len() > want;
    kept.truncate(want);
    let entries = kept
        .iter()
        .map(|entity| entity_ref(entity, vec![reason.clone()], PARENT_HINT_MAX_CHARS))
        .collect();
    ChatResponse::Entities { entries, truncated }
}

fn resolve(
    query: &str,
    found: &[ActivityEntity],
    want: usize,
    context: &ChatContext<'_>,
) -> ChatResponse {
    let mut scored: Vec<(MatchResult, &ActivityEntity)> = found
        .iter()
        .filter(|entity| !suppressed(entity, context))
        .filter_map(|entity| {
            match_candidate(query, &entity.display_name, &entity.locator, 0)
                .map(|result| (result, entity))
        })
        .collect();
    scored.sort_by(|a, b| compare_matches(&a.0, &a.1.locator, &b.0, &b.1.locator));
    let Some((best, _)) = scored.first().map(|(r, e)| (*r, *e)) else {
        return ChatResponse::NotFound;
    };
    let tied: Vec<(&ActivityEntity, Vec<ReasonCode>)> = scored
        .iter()
        .take_while(|(result, _)| result.class == best.class && result.score == best.score)
        .take(want)
        .map(|(result, entity)| (*entity, reasons_for(result.class)))
        .collect();
    if let [(entity, reasons)] = tied.as_slice() {
        return ChatResponse::Resolved {
            entry: entity_ref(entity, reasons.clone(), PARENT_HINT_MAX_CHARS),
        };
    }
    ChatResponse::Ambiguous {
        candidates: disambiguated(&tied),
    }
}

#[cfg(test)]
#[path = "chat_tool_tests.rs"]
mod tests;
