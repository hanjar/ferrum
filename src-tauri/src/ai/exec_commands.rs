//! Tauri commands over the AI execution ledger (FER-196). The ledger records the intent to enqueue
//! BEFORE the write and survives a restart; an uncertain outcome is never replayed automatically.
//! Only opaque ids and digests cross this boundary: no paths, credentials or file contents.

use serde::Serialize;
use tauri::State;

use super::executor::{Entry, ExecState, ExecutionLedger, LedgerError, Reservation, Resolution};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntryDto {
    key: String,
    execution_id: String,
    plan_id: String,
    plan_digest: String,
    state: &'static str,
    job_ids: Vec<String>,
}

impl From<Entry> for LedgerEntryDto {
    fn from(entry: Entry) -> Self {
        let state = match entry.state {
            ExecState::Reserved => "reserved",
            ExecState::Enqueued => "enqueued",
            ExecState::UnknownOutcome => "unknownOutcome",
            ExecState::NotStarted => "notStarted",
        };
        Self {
            key: entry.key,
            execution_id: entry.execution_id,
            plan_id: entry.plan_id,
            plan_digest: entry.plan_digest,
            state,
            job_ids: entry.job_ids,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReservationDto {
    Fresh { key: String },
    Existing { entry: LedgerEntryDto },
}

/// Rejection data object `{ code }`; never a free-form message.
#[derive(Serialize)]
pub struct LedgerFailure {
    code: &'static str,
}

impl From<LedgerError> for LedgerFailure {
    fn from(error: LedgerError) -> Self {
        let code = match error {
            LedgerError::InvalidReference => "invalid_reference",
            LedgerError::UnknownKey => "unknown_key",
            LedgerError::BadState => "bad_state",
            LedgerError::Journal(_) => "journal",
        };
        Self { code }
    }
}

type Outcome<T> = Result<T, LedgerFailure>;

#[tauri::command]
pub fn ai_exec_reserve(
    ledger: State<'_, ExecutionLedger>,
    execution_id: String,
    plan_id: String,
    plan_digest: String,
) -> Outcome<ReservationDto> {
    Ok(
        match ledger.reserve(&execution_id, &plan_id, &plan_digest)? {
            Reservation::Fresh(key) => ReservationDto::Fresh { key },
            Reservation::Existing(entry) => ReservationDto::Existing {
                entry: entry.into(),
            },
        },
    )
}

#[tauri::command]
pub fn ai_exec_record_enqueued(
    ledger: State<'_, ExecutionLedger>,
    key: String,
    job_ids: Vec<String>,
) -> Outcome<()> {
    Ok(ledger.record_enqueued(&key, &job_ids)?)
}

#[tauri::command]
pub fn ai_exec_record_not_started(ledger: State<'_, ExecutionLedger>, key: String) -> Outcome<()> {
    Ok(ledger.record_not_started(&key)?)
}

#[tauri::command]
pub fn ai_exec_mark_unknown(ledger: State<'_, ExecutionLedger>, key: String) -> Outcome<()> {
    Ok(ledger.mark_unknown(&key)?)
}

/// Wire form of `Resolution`: `{ kind: 'found', jobIds } | { kind: 'notFound' }`.
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolutionDto {
    #[serde(rename_all = "camelCase")]
    Found {
        job_ids: Vec<String>,
    },
    NotFound,
}

#[tauri::command]
pub fn ai_exec_resolve_unknown(
    ledger: State<'_, ExecutionLedger>,
    key: String,
    resolution: ResolutionDto,
) -> Outcome<LedgerEntryDto> {
    let resolution = match resolution {
        ResolutionDto::Found { job_ids } => Resolution::Found(job_ids),
        ResolutionDto::NotFound => Resolution::NotFound,
    };
    Ok(ledger.resolve_unknown(&key, resolution)?.into())
}

#[tauri::command]
pub fn ai_exec_by_execution(
    ledger: State<'_, ExecutionLedger>,
    execution_id: String,
) -> Option<LedgerEntryDto> {
    ledger.by_execution(&execution_id).map(Into::into)
}

#[tauri::command]
pub fn ai_exec_unresolved(ledger: State<'_, ExecutionLedger>) -> Vec<LedgerEntryDto> {
    ledger.unresolved().into_iter().map(Into::into).collect()
}
