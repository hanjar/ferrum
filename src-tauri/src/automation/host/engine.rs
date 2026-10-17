//! The ONE place of the host that calls the sync engine (FER-524, review P1.1 #1).
//!
//! [`run_gated`] needs the proof ([`GateOk`]) the gate returned, so no code path - run now,
//! scheduled or watch trigger, retry, recheck, catch-up, pending rerun - can reach the engine
//! without having passed `policy::verify` and the rest of design §10.4. A source test
//! (`only_engine_rs_calls_the_engine`) keeps every other file of the host from importing the
//! engine's `run`.
//!
//! It also refuses to run inside a store closure: the engine loads and saves baselines through
//! `KeyedAuth`, which opens the store, and the store mutex is not re-entrant.

use super::EngineCall;
use super::gate::{GateOk, RunMode};
use crate::automation::profile_baseline::ProfileBaselineStore;
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::execute::{
    EngineError, EngineOutcome, EngineReport, EngineRequest, SettleState, run,
};

fn refused(code: &'static str, text: &str) -> EngineReport {
    EngineReport {
        outcome: EngineOutcome::Failed {
            error: EngineError {
                code,
                text: text.to_string(),
            },
        },
        ops: Vec::new(),
        pending_rerun: false,
        settle: SettleState::NotAttempted,
        fingerprint: None,
        incremental: None,
        blockers: Vec::new(),
    }
}

/// Run the engine for a run the gate cleared. `call.dry_run` must match the proof: only a
/// recheck or watch-comparison proof is read-only, and a run proof can never be turned into a dry run or vice versa.
pub fn run_gated(
    proof: &GateOk,
    call: &EngineCall<'_>,
    baselines: &ProfileBaselineStore,
    env: &EngineEnv,
) -> EngineReport {
    if crate::automation::store::inside_closure() {
        return refused(
            "internal",
            "The run could not start from inside a store transaction. Nothing was changed.",
        );
    }
    let read_only = matches!(proof.mode(), RunMode::Recheck { .. } | RunMode::Compare);
    if read_only != call.dry_run {
        return refused(
            "internal",
            "The run does not match what the gate cleared. Nothing was changed.",
        );
    }
    let mut req = EngineRequest::new(proof.profile(), baselines.engine_store(), call.sink, env);
    req.run_id = call.run_id;
    req.retries = call.retries;
    req.dry_run = call.dry_run;
    req.incremental = call.incremental;
    req.cancel = call.cancel.clone();
    req.lease = proof.lease();
    run(&req)
}
