//! SCM glue for `FerrumIndexer` (FER-474): the service entry point and the
//! install / uninstall / stop operations the installers run (elevated).

use crate::engine::IndexShared;
use crate::pipe::Endpoint;
use crate::proto::Mode;
use crate::server::{self, ServerState};
use crate::store::{self, DataDir, Trust};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceSidType, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

/// SCM state, re-exported so Ferrum needs no direct windows-service dependency.
pub use windows_service::service::ServiceState as ScmState;

pub use crate::SERVICE_NAME;
pub const DISPLAY_NAME: &str = "Ferrum Indexer";
pub const DESCRIPTION: &str = "Read-only NTFS index for Ferrum (search, disk usage, sync). \
     Answers Ferrum over a local, authenticated named pipe. Safe to stop; Ferrum falls back to \
     its own scanning.";
/// The only privilege the process keeps (`RequiredPrivileges`); every other
/// LocalSystem privilege is removed from the service token by the SCM.
/// Identification-level impersonation of pipe clients needs no privilege.
pub const REQUIRED_PRIVILEGES: &[&str] = &["SeChangeNotifyPrivilege"];
/// FER-476: `Restricted` is the default (required before S3 added queries
/// over IPC). The service token is then write-restricted: a write is
/// granted only when one of its restricting SIDs (the service SID,
/// Everyone, WRITE RESTRICTED, logon SID) is granted it too. Everything the
/// service writes therefore grants `NT SERVICE\FerrumIndexer`
/// ([`crate::store::SERVICE_SID`]): the data directory and its files, the
/// `index` directory, and the pipe DACL (adding pipe instances is a write).
/// `install` seals existing files ([`crate::store::DataDir::reseal`]) and
/// then PROBES the restricted token (see [`probe_restricted`]); only when
/// that probe fails does it register `Unrestricted` instead, and says so.
pub const SERVICE_SID_TYPE: ServiceSidType = ServiceSidType::Restricted;
/// Fallback when the restricted probe fails on this machine.
pub const SERVICE_SID_TYPE_FALLBACK: ServiceSidType = ServiceSidType::Unrestricted;
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: i32 = 1063;

static VERSION: OnceLock<String> = OnceLock::new();

// ── service entry ────────────────────────────────────────────────────────

define_windows_service!(ffi_service_main, service_main);

/// Hand the process to the SCM. `Ok(false)` = not started by the SCM (the
/// exe was run from a console without a command).
pub fn run_dispatcher(version: &str) -> Result<bool, String> {
    let _ = VERSION.set(version.to_owned());
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => Ok(true),
        Err(windows_service::Error::Winapi(e))
            if e.raw_os_error() == Some(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        store::log(&format!("service failed: {e}"));
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, exit: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: state,
        controls_accepted: accept,
        exit_code: if exit == 0 {
            ServiceExitCode::NO_ERROR
        } else {
            ServiceExitCode::ServiceSpecific(exit)
        },
        checkpoint: 0,
        wait_hint: if matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        ) {
            Duration::from_secs(10)
        } else {
            Duration::default()
        },
        process_id: None,
    }
}

fn run_service() -> Result<(), String> {
    let version = VERSION.get().cloned().unwrap_or_default();
    let path = store::data_dir().map_err(|e| e.to_string())?;
    // Persist only into a trusted data directory; otherwise run in memory.
    let dir = match DataDir::open_for_service(&path, Trust::Service) {
        Ok(Some(d)) => Some(Arc::new(d)),
        Ok(None) => None,
        Err(e) => {
            store::log_to_stderr();
            store::log(&format!("data directory unusable: {e}"));
            None
        }
    };
    let mode = match &dir {
        Some(d) => {
            store::log_to_dir(d.clone());
            match d.read_config() {
                Ok(Some((c, None))) => c.mode,
                Ok(Some((c, Some(e)))) => {
                    store::log(&format!("config invalid ({e}); using {}", c.mode.as_str()));
                    c.mode
                }
                Ok(None) => Mode::Shared,
                Err(e) => {
                    store::log(&format!("config unreadable ({e}); using shared"));
                    Mode::Shared
                }
            }
        }
        None => {
            store::log("data directory not trusted; running with shared mode, no persistence");
            Mode::Shared
        }
    };
    // FER-475: the index worker (read-only volume access, background
    // priority). Pause/mode/status reach it through the server state.
    let index = IndexShared::new(mode);
    let worker = match crate::volume::start_worker(dir.as_deref(), index.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            store::log(&format!("index worker not started: {e}"));
            None
        }
    };
    // FER-476 review H1: the query surface (search/subtree) needs the
    // write-restricted token. Checked on the REAL token at every start: the
    // SID type can be changed with `sc sidtype` at any time.
    let restricted = crate::win::sid_from_string(store::SERVICE_SID)
        .map(|sid| crate::win::process_restricted_by(&sid))
        .unwrap_or(false);
    if !restricted {
        store::log(
            "service token is not restricted by the service SID: search/subtree disabled (degraded)",
        );
    }
    let state = Arc::new(
        ServerState::new(&version, false, mode, dir)
            .with_index(index.clone())
            .with_queries(restricted),
    );
    let stop = Arc::new(AtomicBool::new(false));
    static HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();
    let running_accept = ServiceControlAccept::STOP
        | ServiceControlAccept::SHUTDOWN
        | ServiceControlAccept::PAUSE_CONTINUE;
    let (h_stop, h_state) = (stop.clone(), state.clone());
    let handler = move |ctl: ServiceControl| -> ServiceControlHandlerResult {
        match ctl {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                h_stop.store(true, Ordering::SeqCst);
                if let Some(h) = HANDLE.get() {
                    let _ = h.set_service_status(status(
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                        0,
                    ));
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Pause | ServiceControl::Continue => {
                let pause = matches!(ctl, ServiceControl::Pause);
                h_state.set_paused(pause);
                if let Some(h) = HANDLE.get() {
                    let st = if pause {
                        ServiceState::Paused
                    } else {
                        ServiceState::Running
                    };
                    let _ = h.set_service_status(status(st, running_accept, 0));
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let handle =
        service_control_handler::register(SERVICE_NAME, handler).map_err(|e| e.to_string())?;
    let _ = HANDLE.set(handle);
    let _ = handle.set_service_status(status(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        0,
    ));
    store::log(&format!(
        "starting v{version}, mode {}",
        state.mode().as_str()
    ));

    // The server thread owns the pipe; `Running` is reported once it runs.
    let (srv_state, srv_stop) = (state.clone(), stop.clone());
    let server = std::thread::spawn(move || server::run(&Endpoint::Service, srv_state, srv_stop));
    let _ = handle.set_service_status(status(ServiceState::Running, running_accept, 0));
    let result = server
        .join()
        .map_err(|_| "server thread panicked".to_string())?;
    // Stop the index worker; it saves changed indexes before returning.
    index.stop.store(true, Ordering::SeqCst);
    if let Some(w) = worker {
        let _ = w.join();
    }
    let exit = match &result {
        Ok(()) => 0,
        Err(e) => {
            store::log(&format!("pipe server failed: {e}"));
            1
        }
    };
    let _ = handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit,
    ));
    result.map_err(|e| e.to_string())
}

// ── install / uninstall (elevated; run by the MSI/NSIS installers) ───────

/// What `install` should do with the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeChoice {
    /// Write this mode (fresh choice in the installer).
    Set(Mode),
    /// Write this mode only when no valid config exists (upgrade/repair).
    IfNew(Mode),
    /// Keep an existing valid config; default `shared` otherwise.
    Keep,
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|e| format!("SCM: {e}"))
}

fn is_missing(e: &windows_service::Error) -> bool {
    matches!(e, windows_service::Error::Winapi(io) if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST))
}

/// The service image must be in Program Files in release builds: a binary
/// in a user-writable folder would let any process of that user replace
/// what runs as SYSTEM.
pub fn check_image_location(exe: &Path) -> Result<(), String> {
    let dir = exe.parent().ok_or("service binary has no directory")?;
    if !cfg!(debug_assertions) && !crate::win::in_protected_dir(dir, &crate::win::protected_roots())
    {
        return Err(format!(
            "refusing to register {}: the service binary must be installed under Program Files",
            exe.display()
        ));
    }
    Ok(())
}

pub fn install(choice: ModeChoice) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("own path: {e}"))?;
    check_image_location(&exe)?;

    // 0. A running instance holds the data directory; stop it first.
    stop()?;

    // 1. Data directory + config (before the service can start).
    let path = store::data_dir().map_err(|e| e.to_string())?;
    let (dir, aside) =
        DataDir::prepare(&path, Trust::Service).map_err(|e| format!("data directory: {e}"))?;
    if let Some(aside) = aside {
        println!("untrusted {} moved to {}", path.display(), aside.display());
    }
    let existing = dir
        .read_config()
        .ok()
        .flatten()
        .filter(|(_, err)| err.is_none())
        .map(|(c, _)| c.mode);
    let mode = match (choice, existing) {
        (ModeChoice::Set(m), _) => Some(m),
        (ModeChoice::IfNew(m), None) => Some(m),
        (ModeChoice::Keep, None) => Some(Mode::Shared),
        (_, Some(_)) => None,
    };
    if let Some(m) = mode {
        dir.write_config(&crate::config::Config { mode: m })
            .map_err(|e| format!("config: {e}"))?;
        println!("mode: {}", m.as_str());
    } else if let Some(m) = existing {
        println!("mode kept: {}", m.as_str());
    }
    drop(dir);

    // 2. Service registration (create or update).
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let mgr = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::QUERY_CONFIG;
    // FER-476: emulate the restricted service token (same machine, same
    // directory, the production pipe namespace, FER-480) before relying on it.
    let sid_type = match probe_restricted(&path) {
        Ok(()) => {
            println!("service SID type: restricted (probe ok)");
            SERVICE_SID_TYPE
        }
        Err(e) => {
            let msg =
                format!("restricted service SID probe failed ({e}); registering unrestricted");
            println!("WARNING: {msg}");
            if let Ok(Some(d)) = DataDir::open_existing(&path, Trust::Service) {
                let _ = d.append_log(&format!("install: {msg}\n"));
            }
            SERVICE_SID_TYPE_FALLBACK
        }
    };
    let mut created = false;
    let svc = match mgr.open_service(SERVICE_NAME, access) {
        Ok(svc) => {
            stop_and_wait(&svc, Duration::from_secs(30))?;
            svc.change_config(&info)
                .map_err(|e| format!("update service: {e}"))?;
            svc
        }
        Err(e) if is_missing(&e) => {
            created = true;
            mgr.create_service(&info, access)
                .map_err(|e| format!("create service: {e}"))?
        }
        Err(e) => return Err(format!("open service: {e}")),
    };
    svc.set_description(DESCRIPTION)
        .map_err(|e| format!("description: {e}"))?;
    svc.set_delayed_auto_start(true)
        .map_err(|e| format!("delayed start: {e}"))?;
    // Service SID type: see SERVICE_SID_TYPE (FER-476: restricted unless
    // the probe above failed).
    svc.set_config_service_sid_info(sid_type)
        .map_err(|e| format!("service SID: {e}"))?;
    set_required_privileges(&svc, REQUIRED_PRIVILEGES)?;
    svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(5),
            },
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(30),
            },
            ServiceAction {
                action_type: ServiceActionType::None,
                delay: Duration::default(),
            },
        ]),
    })
    .map_err(|e| format!("recovery: {e}"))?;
    svc.set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| format!("recovery flag: {e}"))?;

    // FER-487: where the service announces its fallback pipe name. The key
    // gets the ACL the service's restricted token needs (SYSTEM and the
    // service SID write, Users read); the probe proves that token can write
    // it. A failure stops the install: the SID type is not relaxed.
    if let Err(e) = register_pipe_name_key() {
        // The service is registered already. Nothing is started, and the
        // registration does not stay behind half-configured (see
        // `undo_registration`).
        let undone = undo_registration(&mgr, svc, created, &info);
        return Err(format!("install stopped: {e}; {undone}"));
    }

    // 3. Start now (auto-start covers later boots).
    match svc.start::<&str>(&[]) {
        Ok(()) => {}
        Err(windows_service::Error::Winapi(e))
            if e.raw_os_error() == Some(ERROR_SERVICE_ALREADY_RUNNING) => {}
        Err(e) => return Err(format!("start: {e}")),
    }
    println!("{SERVICE_NAME} installed from {}", exe.display());
    Ok(())
}

/// FER-487: create the `PipeName` key with its ACL, then prove that the
/// restricted service token can write it.
fn register_pipe_name_key() -> Result<(), String> {
    crate::announce::create_parameters_key(store::SERVICE_SID)
        .map_err(|e| format!("PipeName key: {e}"))?;
    probe_pipe_name_write()
}

/// FER-487: an install that fails after the service was registered leaves
/// nothing half-configured. A registration this call created is deleted
/// again (a fresh install has nothing to keep). An existing service is set
/// to start type `disabled`, so the failed configuration never starts at
/// boot; the next successful install enables it again. Returns what was done.
fn undo_registration(
    mgr: &ServiceManager,
    svc: windows_service::service::Service,
    created: bool,
    info: &ServiceInfo,
) -> String {
    if created {
        drop(svc);
        return match mgr
            .open_service(SERVICE_NAME, ServiceAccess::DELETE)
            .and_then(|s| s.delete())
        {
            Ok(()) => "the new service registration was removed again".into(),
            Err(e) => format!(
                "the new service registration could not be removed ({e}); run `ferrum-indexer uninstall`"
            ),
        };
    }
    let disabled = ServiceInfo {
        start_type: ServiceStartType::Disabled,
        ..info.clone()
    };
    match svc.change_config(&disabled) {
        Ok(()) => {
            "the existing service is disabled (start type disabled) until an install succeeds"
                .into()
        }
        Err(e) => format!(
            "the existing service could not be disabled ({e}); run `ferrum-indexer uninstall`"
        ),
    }
}

/// FER-487: the restricted service token must write the `PipeName` key.
/// Probed at install with a write and a delete of a probe value.
fn probe_pipe_name_write() -> Result<(), String> {
    let token =
        crate::win::write_restricted_token(&RESTRICTING_SIDS).map_err(|e| format!("token: {e}"))?;
    let _imp = crate::win::Impersonation::begin(&token).map_err(|e| format!("impersonate: {e}"))?;
    crate::announce::probe_write(&crate::announce::Hklm).map_err(|e| e.to_string())
}

/// Restricting SIDs the SCM puts into a `SERVICE_SID_TYPE_RESTRICTED` token
/// (the logon SID is added too; it is granted nothing of ours).
pub const RESTRICTING_SIDS: [&str; 3] = [
    crate::store::SERVICE_SID,
    crate::win::SID_EVERYONE_STR,
    crate::win::SID_WRITE_RESTRICTED_STR,
];

/// FER-476/FER-480: run what the service does at start with a
/// write-restricted copy of the CALLER's token (install runs as an elevated
/// administrator, so Administrators is enabled, like SYSTEM's token): hold
/// the data directory, append to the log, open the `index` directory, and
/// run [`crate::pipe::probe_instances`] — the first and a second instance
/// of a throw-away pipe in the SAME namespace as the production pipe (the
/// NPFS root, FER-480) with the production DACL, plus the owner check a
/// client applies. The pipe half is unit-tested without admin rights with
/// exactly this function (`pipe::tests::restricted_probe_*`).
pub fn probe_restricted(data: &std::path::Path) -> Result<(), String> {
    let token =
        crate::win::write_restricted_token(&RESTRICTING_SIDS).map_err(|e| format!("token: {e}"))?;
    let me = crate::win::current_user_sid()
        .and_then(|s| crate::win::sid_string(&s))
        .map_err(|e| format!("own SID: {e}"))?;
    let owners = crate::pipe::trusted_owners(&Endpoint::Service).map_err(|e| e.to_string())?;
    let name = crate::pipe::probe_pipe_name();
    let _imp = crate::win::Impersonation::begin(&token).map_err(|e| format!("impersonate: {e}"))?;
    let dir = DataDir::open_for_service(data, Trust::Service)
        .map_err(|e| format!("data directory: {e}"))?
        .ok_or("data directory not trusted")?;
    dir.append_log("install: restricted service SID probe\n")
        .map_err(|e| format!("log: {e}"))?;
    dir.index_dir()
        .map_err(|e| format!("index directory: {e}"))?
        .ok_or("index directory not trusted")?;
    crate::pipe::probe_instances(&name, &me, &owners)
}

/// `SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO` (not wrapped by windows-service).
fn set_required_privileges(
    svc: &windows_service::service::Service,
    privs: &[&str],
) -> Result<(), String> {
    use windows_sys::Win32::System::Services::{
        ChangeServiceConfig2W, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
        SERVICE_REQUIRED_PRIVILEGES_INFOW,
    };
    let mut multi: Vec<u16> = Vec::new();
    for p in privs {
        multi.extend(p.encode_utf16());
        multi.push(0);
    }
    multi.push(0);
    let mut info = SERVICE_REQUIRED_PRIVILEGES_INFOW {
        pmszRequiredPrivileges: multi.as_mut_ptr(),
    };
    let ok = unsafe {
        ChangeServiceConfig2W(
            svc.raw_handle(),
            SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
            (&mut info as *mut SERVICE_REQUIRED_PRIVILEGES_INFOW).cast(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "required privileges: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn stop_and_wait(svc: &windows_service::service::Service, max: Duration) -> Result<(), String> {
    match svc.stop() {
        Ok(_) => {}
        Err(windows_service::Error::Winapi(e))
            if e.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE) => {}
        Err(e) => return Err(format!("stop: {e}")),
    }
    let start = Instant::now();
    loop {
        let st = svc
            .query_status()
            .map_err(|e| format!("query status: {e}"))?;
        if st.current_state == ServiceState::Stopped {
            return Ok(());
        }
        if start.elapsed() >= max {
            return Err("service did not stop in time".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Stop the service if installed (used before files are replaced).
pub fn stop() -> Result<(), String> {
    let mgr = manager(ServiceManagerAccess::CONNECT)?;
    match mgr.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(svc) => stop_and_wait(&svc, Duration::from_secs(30)),
        Err(e) if is_missing(&e) => Ok(()),
        Err(e) => Err(format!("open service: {e}")),
    }
}

/// Stop and delete the service; optionally remove its ProgramData files.
pub fn uninstall(purge_data: bool) -> Result<(), String> {
    let mgr = manager(ServiceManagerAccess::CONNECT)?;
    match mgr.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS | ServiceAccess::DELETE,
    ) {
        Ok(svc) => {
            // Delete even if the stop timed out: the SCM removes the entry
            // once the process exits.
            let stopped = stop_and_wait(&svc, Duration::from_secs(30));
            svc.delete().map_err(|e| format!("delete: {e}"))?;
            if let Err(e) = stopped {
                println!("warning: {e}; service marked for deletion");
            }
            println!("{SERVICE_NAME} removed");
        }
        Err(e) if is_missing(&e) => println!("{SERVICE_NAME} not installed"),
        Err(e) => return Err(format!("open service: {e}")),
    }
    if purge_data {
        let dir = store::data_dir().map_err(|e| e.to_string())?;
        match DataDir::open_existing(&dir, Trust::Service).map_err(|e| format!("purge: {e}"))? {
            Some(d) => d.purge().map_err(|e| format!("purge: {e}"))?,
            None if std::fs::symlink_metadata(&dir).is_err() => {}
            None => return Err("data directory not trusted; not purging".into()),
        }
        println!("data removed from {}", dir.display());
    }
    Ok(())
}

/// SCM view of the service for a non-admin caller (Ferrum):
/// `None` = not installed.
pub fn query_state() -> Result<Option<ServiceState>, String> {
    let mgr = manager(ServiceManagerAccess::CONNECT)?;
    match mgr.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(svc) => svc
            .query_status()
            .map(|s| Some(s.current_state))
            .map_err(|e| format!("query status: {e}")),
        Err(e) if is_missing(&e) => Ok(None),
        Err(e) => Err(format!("open service: {e}")),
    }
}

/// FER-480: the installed service process as the SCM knows it — what a
/// client compares the pipe's server against before sending anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceProcess {
    pub pid: u32,
    /// Registered image (`lpBinaryPathName`, unquoted, no arguments).
    pub image: PathBuf,
}

/// SCM states in which the service process serves its pipe. Start/stop
/// pending are excluded: the pipe is not up yet / going away.
pub fn serving_state(s: ServiceState) -> bool {
    matches!(
        s,
        ServiceState::Running
            | ServiceState::Paused
            | ServiceState::PausePending
            | ServiceState::ContinuePending
    )
}

/// `lpBinaryPathName` → image path. We register a quoted path without
/// arguments; anything else (arguments, unquoted spaces, relative or UNC
/// paths, `..`) is refused rather than guessed at.
pub fn parse_image_path(raw: &std::ffi::OsStr) -> Option<PathBuf> {
    let s = raw.to_str()?.trim();
    let path = match s.strip_prefix('"') {
        Some(rest) => {
            let (p, tail) = rest.split_once('"')?;
            if !tail.trim().is_empty() {
                return None;
            }
            p
        }
        None if s.contains(' ') => return None,
        None => s,
    };
    let p = PathBuf::from(path);
    let ok = p.is_absolute()
        && !path.starts_with(r"\\")
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    ok.then_some(p)
}

/// The running service process (PID from `QueryServiceStatusEx`, image
/// from `QueryServiceConfigW`; both readable by standard users with the
/// default service DACL). `Ok(None)`: not installed or not serving.
pub fn service_process() -> Result<Option<ServiceProcess>, String> {
    let mgr = manager(ServiceManagerAccess::CONNECT)?;
    let svc = match mgr.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
    ) {
        Ok(svc) => svc,
        Err(e) if is_missing(&e) => return Ok(None),
        Err(e) => return Err(format!("open service: {e}")),
    };
    let st = svc
        .query_status()
        .map_err(|e| format!("query status: {e}"))?;
    let pid = match (serving_state(st.current_state), st.process_id) {
        (true, Some(pid)) if pid != 0 => pid,
        _ => return Ok(None),
    };
    let cfg = svc
        .query_config()
        .map_err(|e| format!("query config: {e}"))?;
    let image = parse_image_path(cfg.executable_path.as_os_str()).ok_or_else(|| {
        format!(
            "unexpected service image path {}",
            cfg.executable_path.display()
        )
    })?;
    // Same location rule as `install` (Program Files in release builds).
    check_image_location(&image)?;
    Ok(Some(ServiceProcess { pid, image }))
}

/// Does a valid config exist? (Installer: ask for the mode only when not.)
pub fn has_config() -> bool {
    store::data_dir()
        .ok()
        .and_then(|p| DataDir::open_existing(&p, Trust::Service).ok().flatten())
        .and_then(|d| d.read_config().ok().flatten())
        .is_some_and(|(_, err)| err.is_none())
}

/// Path of the service binary next to `app_exe` (Ferrum's own exe).
pub fn binary_next_to(app_exe: &Path) -> Option<PathBuf> {
    Some(app_exe.parent()?.join(crate::BINARY_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn image_path_parsing_is_strict() {
        let ok = |s: &str| parse_image_path(OsStr::new(s));
        assert_eq!(
            ok(r#""C:\Program Files\Ferrum\ferrum-indexer.exe""#),
            Some(PathBuf::from(r"C:\Program Files\Ferrum\ferrum-indexer.exe"))
        );
        assert_eq!(
            ok(r"C:\Ferrum\ferrum-indexer.exe"),
            Some(PathBuf::from(r"C:\Ferrum\ferrum-indexer.exe"))
        );
        for bad in [
            r#""C:\Program Files\Ferrum\ferrum-indexer.exe" --console"#,
            r"C:\Program Files\Ferrum\ferrum-indexer.exe",
            r#""C:\Program Files\Ferrum\ferrum-indexer.exe"#,
            r#""ferrum-indexer.exe""#,
            r#""\host\share\ferrum-indexer.exe""#,
            r#""C:\Program Files\..\Users\x\ferrum-indexer.exe""#,
            "",
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
    }

    #[test]
    fn serving_states() {
        assert!(serving_state(ServiceState::Running));
        assert!(serving_state(ServiceState::Paused));
        assert!(!serving_state(ServiceState::Stopped));
        assert!(!serving_state(ServiceState::StartPending));
        assert!(!serving_state(ServiceState::StopPending));
    }
}
