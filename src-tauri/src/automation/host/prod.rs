//! Production dependencies of the host: the real clock and process probe, the DPAPI key, the real
//! engine environment and baseline store, threads, and a callback for events. Tauri-free: the
//! callback is supplied by `commands.rs` (the app) or the runner (P3.1).

use std::sync::Arc;

use super::engine::run_gated;
use super::{EngineCall, GateOk, Host, HostDeps, HostEvent};
use crate::automation::dpapi::ApprovalKeys;
use crate::automation::lease::LeaseEnv;
use crate::automation::model::Conditions;
use crate::automation::profile_baseline::ProfileBaselineStore;
use crate::automation::store::Store;
use crate::automation::store::queue::HostClass;
use crate::automation::watcher::{ProductionFactory, WatchFactory};
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::execute::EngineReport;

pub type Emit = Arc<dyn Fn(&HostEvent) + Send + Sync>;
pub type Flag = Arc<dyn Fn() -> bool + Send + Sync>;

pub struct ProdDeps {
    keys: Arc<dyn ApprovalKeys>,
    baselines: ProfileBaselineStore,
    lease_env: LeaseEnv,
    emit: Emit,
    incremental: Flag,
}

/// The host of the app process.
pub type AppHost = Host<ProdDeps>;

impl ProdDeps {
    pub fn new(
        keys: Arc<dyn ApprovalKeys>,
        baselines: ProfileBaselineStore,
        lease_env: LeaseEnv,
        emit: Emit,
        incremental: Flag,
    ) -> Self {
        Self {
            keys,
            baselines,
            lease_env,
            emit,
            incremental,
        }
    }
}

/// Build the in-app host (class `App`) over the opened store.
pub fn app_host(
    store: Arc<Store>,
    keys: Arc<dyn ApprovalKeys>,
    baselines: ProfileBaselineStore,
    emit: Emit,
    incremental: Flag,
) -> AppHost {
    Host::new(
        store,
        ProdDeps::new(keys, baselines, LeaseEnv::system(), emit, incremental),
        HostClass::App,
        env!("CARGO_PKG_VERSION"),
    )
}

impl HostDeps for ProdDeps {
    fn now_ms(&self) -> i64 {
        self.lease_env.now()
    }

    fn keys(&self) -> &dyn ApprovalKeys {
        self.keys.as_ref()
    }

    fn lease_env(&self) -> &LeaseEnv {
        &self.lease_env
    }

    fn engine_env(&self) -> EngineEnv {
        EngineEnv::production()
    }

    fn run_engine(&self, proof: &GateOk, call: &EngineCall<'_>) -> EngineReport {
        let env = self.engine_env();
        run_gated(proof, call, &self.baselines, &env)
    }

    /// OP-1. A battery is read from the OS; a metered connection cannot be told yet, so a run
    /// that asks to skip metered connections waits (fail closed) until OP-1 detects them.
    fn unmet_condition(&self, conditions: &Conditions) -> Option<&'static str> {
        if conditions.skip_on_battery && on_battery() != Some(false) {
            return Some("Waits for AC power");
        }
        if conditions.skip_on_metered {
            return Some("Waits for an unmetered connection");
        }
        None
    }

    fn spawn(&self, name: &'static str, job: Box<dyn FnOnce() + Send>) -> bool {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(job)
            .is_ok()
    }

    fn emit(&self, event: &HostEvent) {
        (self.emit)(event);
    }

    fn incremental(&self) -> bool {
        (self.incremental)()
    }

    /// Operating-system change notifications: the only source of change events (no polling).
    fn watch_factory(&self) -> &dyn WatchFactory {
        &ProductionFactory {}
    }
}

/// `Some(true)` on battery, `Some(false)` on AC power, `None` when Windows does not know.
#[cfg(windows)]
fn on_battery() -> Option<bool> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: the out parameter is a valid, writable plain struct.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return None;
    }
    match status.ACLineStatus {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

#[cfg(not(windows))]
fn on_battery() -> Option<bool> {
    None
}
