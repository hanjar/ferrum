//! Named-pipe plumbing for the `FerrumIndexer` IPC (FER-474): names, DACLs,
//! server instances, the client open + server verification, and overlapped
//! I/O with stop/timeout/deadline polling (pattern from FER-460
//! `fast_scan::Pipe`).
//!
//! Security model (see docs/indexer-service.md §3):
//! - Production pipe name `\\.\pipe\FerrumIndexer` (FER-480). It used to
//!   live under `ProtectedPrefix\Administrators\`, but NPFS refuses to create
//!   a pipe there for a write-restricted token (a `SERVICE_SID_TYPE_RESTRICTED`
//!   service), whatever the pipe's DACL — measured with
//!   `scripts/indexer-pipe-probe.ps1`. Squatting is therefore stopped on the
//!   CLIENT side: before sending a single byte, a client checks that the pipe
//!   is served by the running FerrumIndexer service process
//!   ([`verify_server`]: server PID == SCM PID, same image file as the
//!   registered service binary in Program Files, the service SID in its
//!   token when readable, and the pipe owner SYSTEM/Administrators).
//! - The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`; the
//!   accept loop always creates the next instance BEFORE handing the
//!   connected one to a worker, so the name never has zero instances while
//!   the service runs.
//! - DACL: SYSTEM and the service SID full; INTERACTIVE users only
//!   read/write data + read attributes/control — NOT
//!   `FILE_CREATE_PIPE_INSTANCE` (which is part of `FILE_GENERIC_WRITE`), so
//!   no client can add an instance of its own next to the service's.
//!   NETWORK is denied, `PIPE_REJECT_REMOTE_CLIENTS` is set, and a
//!   medium no-read-up/no-write-up label keeps low-IL processes out.
//! - The client opens with `SECURITY_IDENTIFICATION` (the service can only
//!   identify, never act as, the user).

use crate::win::{
    OwnedHandle, SID_ADMINISTRATORS, SID_LOCAL_SYSTEM, SecurityDescriptor, current_user_sid,
    sid_bytes, sid_string, wide, win_err,
};
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_SUCCESS, GENERIC_ALL, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_APPEND_DATA, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, READ_CONTROL, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    SYNCHRONIZE, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Production pipe (service, LocalSystem). FER-480: NPFS root, not
/// `ProtectedPrefix\Administrators\` (see the module docs).
pub const SERVICE_PIPE: &str = r"\\.\pipe\FerrumIndexer";
/// Prefix of the install probe's throw-away pipes (same namespace as
/// [`SERVICE_PIPE`]).
pub const PROBE_PIPE_PREFIX: &str = r"\\.\pipe\FerrumIndexer-probe-";
/// Prefix of console/dev pipes (`--console`), one per suffix.
pub const DEV_PIPE_PREFIX: &str = r"\\.\pipe\FerrumIndexer-dev-";

/// Rights a client gets on the pipe: read/write data, read/write attributes
/// (`SetNamedPipeHandleState`), read control, synchronize. Deliberately NOT
/// `FILE_APPEND_DATA` (= `FILE_CREATE_PIPE_INSTANCE` on a pipe).
pub const CLIENT_ACCESS: u32 = FILE_READ_DATA
    | FILE_WRITE_DATA
    | FILE_READ_ATTRIBUTES
    | FILE_WRITE_ATTRIBUTES
    | READ_CONTROL
    | SYNCHRONIZE;

/// Max simultaneous pipe instances (connections + the listening one + slack).
pub const MAX_INSTANCES: u32 = crate::policy::limits::MAX_CONNECTIONS as u32 + 2;
const IO_BUFFER: u32 = 4096;
const POLL_MS: u32 = 100;

/// Where the service listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The installed service (LocalSystem).
    Service,
    /// A console instance for development/tests (`--console --pipe <suffix>`).
    Dev(String),
    /// FER-487: the service's fallback name (`FerrumIndexer-<32 hex>`), used
    /// when the fixed name is taken. Same DACL and owner rules as
    /// [`Endpoint::Service`]. Only built through [`Endpoint::announced`].
    Announced(String),
}

impl Endpoint {
    /// Validated dev endpoint: suffix of 1–40 `[A-Za-z0-9-]`.
    pub fn dev(suffix: &str) -> Option<Endpoint> {
        crate::pipe_suffix_ok(suffix).then(|| Endpoint::Dev(suffix.to_owned()))
    }

    /// Validated fallback endpoint (FER-487): the name must be exactly
    /// `FerrumIndexer-[0-9a-f]{32}`.
    pub fn announced(name: &str) -> Option<Endpoint> {
        crate::announce::name_is_valid(name).then(|| Endpoint::Announced(name.to_owned()))
    }

    pub fn path(&self) -> String {
        match self {
            Endpoint::Service => SERVICE_PIPE.to_owned(),
            Endpoint::Dev(s) => format!("{DEV_PIPE_PREFIX}{s}"),
            Endpoint::Announced(name) => format!(r"\\.\pipe\{name}"),
        }
    }
}

/// Pipe DACL (+ owner + label) in SDDL.
///
/// - Service: owner SYSTEM (`O:SY`, FER-480 review F2: explicit, not the
///   token's default owner); SYSTEM full; INTERACTIVE [`CLIENT_ACCESS`];
///   NETWORK and ANONYMOUS denied; medium NR/NW label. The install probe
///   (an elevated administrator, who cannot assign `O:SY`) uses the same
///   DACL without the owner ([`service_dacl_sddl`], [`probe_sddl`]).
/// - Dev: only the current user (full), same label. A dev server never
///   serves other accounts.
///
/// FER-476: both also grant the service SID (`NT SERVICE\FerrumIndexer`)
/// full access. With `SERVICE_SID_TYPE_RESTRICTED` the service token is
/// write-restricted: adding an instance (`FILE_CREATE_PIPE_INSTANCE`, a
/// write right) must then ALSO be granted to one of its restricting SIDs,
/// and only the service SID qualifies (no client token carries it).
/// Verified by `restricted_token_can_add_instances_only_with_service_ace`.
pub fn pipe_sddl(endpoint: &Endpoint, user_sid: &str) -> String {
    const LABEL: &str = "S:(ML;;NWNR;;;ME)";
    let svc = crate::store::SERVICE_SID;
    match endpoint {
        Endpoint::Service | Endpoint::Announced(_) => format!("O:SY{}", service_dacl_sddl()),
        Endpoint::Dev(_) => {
            format!("D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;{user_sid})(A;;GA;;;{svc}){LABEL}")
        }
    }
}

/// The service pipe's DACL + label WITHOUT an owner: what [`pipe_sddl`]
/// puts after `O:SY`, and the base of [`probe_sddl`].
pub fn service_dacl_sddl() -> String {
    let svc = crate::store::SERVICE_SID;
    format!(
        "D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;SY)(A;;GA;;;{svc})(A;;{CLIENT_ACCESS:#x};;;IU)S:(ML;;NWNR;;;ME)"
    )
}

/// Security descriptor for the server side of `endpoint`.
pub fn server_security(endpoint: &Endpoint) -> io::Result<SecurityDescriptor> {
    let sid = sid_string(&current_user_sid()?)?;
    SecurityDescriptor::from_sddl(&pipe_sddl(endpoint, &sid))
}

/// Create one server instance (overlapped, byte mode, local clients only).
pub fn create_instance(
    endpoint: &Endpoint,
    sd: &SecurityDescriptor,
    first: bool,
) -> io::Result<OwnedHandle> {
    create_named_instance(&endpoint.path(), sd, first)
}

/// [`create_instance`] for a full pipe path (`install`'s restricted-SID
/// probe uses a throw-away name next to the production pipe).
pub fn create_named_instance(
    path: &str,
    sd: &SecurityDescriptor,
    first: bool,
) -> io::Result<OwnedHandle> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let name = wide(path);
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            MAX_INSTANCES,
            IO_BUFFER,
            IO_BUFFER,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(h))
}

/// A fresh throw-away probe pipe name (`PROBE_PIPE_PREFIX` + pid + clock).
pub fn probe_pipe_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{PROBE_PIPE_PREFIX}{}-{nanos}", std::process::id())
}

/// The production service DACL with one extra full-access ACE for
/// `creator_sid`, standing in for SYSTEM: an administrator (install) or a
/// test user is not SYSTEM, and adding an instance needs
/// `FILE_CREATE_PIPE_INSTANCE` from BOTH the normal access check (SYSTEM's
/// ACE for the real service, this one for the probe) and the restricted
/// check (the service-SID ACE, unchanged).
pub fn probe_sddl(creator_sid: &str) -> String {
    service_dacl_sddl().replacen(
        "(A;;GA;;;SY)",
        &format!("(A;;GA;;;SY)(A;;GA;;;{creator_sid})"),
        1,
    )
}

/// FER-480: the pipe half of the restricted-SID probe, run under the
/// CURRENT thread token (the caller impersonates the write-restricted
/// token). Exercises the service's own code path and namespace: the first
/// instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`, a second instance (what
/// the accept loop does after every connection), and the owner a client
/// will accept (`owners`).
pub fn probe_instances(path: &str, creator_sid: &str, owners: &[Vec<u8>]) -> Result<(), String> {
    let sd = SecurityDescriptor::from_sddl(&probe_sddl(creator_sid))
        .map_err(|e| format!("probe sd: {e}"))?;
    let first = create_named_instance(path, &sd, true).map_err(|e| format!("pipe {path}: {e}"))?;
    let second =
        create_named_instance(path, &sd, false).map_err(|e| format!("pipe instance: {e}"))?;
    let owner = handle_owner(&first).map_err(|e| format!("pipe owner: {e}"))?;
    if !owners.contains(&owner) {
        return Err(format!(
            "pipe owner {} would not be trusted by clients",
            sid_string(&owner).unwrap_or_else(|_| "<invalid SID>".into())
        ));
    }
    drop((first, second));
    Ok(())
}

// Sanity: the client mask must never include instance creation, and
// PIPE_UNLIMITED_INSTANCES must not be what we pass.
const _: () = assert!(CLIENT_ACCESS & FILE_APPEND_DATA == 0);
const _: () = assert!(MAX_INSTANCES < PIPE_UNLIMITED_INSTANCES);
const _: () = assert!(CLIENT_ACCESS & GENERIC_ALL == 0);

/// Owner SIDs a client accepts for `endpoint`'s pipe.
pub fn trusted_owners(endpoint: &Endpoint) -> io::Result<Vec<Vec<u8>>> {
    let mut v = vec![SID_LOCAL_SYSTEM.to_vec(), SID_ADMINISTRATORS.to_vec()];
    if matches!(endpoint, Endpoint::Dev(_)) {
        v.push(current_user_sid()?);
    }
    Ok(v)
}

/// Raw owner SID of a kernel object handle (needs READ_CONTROL).
pub fn handle_owner(h: &OwnedHandle) -> io::Result<Vec<u8>> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let rc = unsafe {
        GetSecurityInfo(
            h.raw(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win_err(rc));
    }
    let out = unsafe { sid_bytes(owner) };
    unsafe { LocalFree(sd) };
    out
}

#[derive(Debug)]
pub enum OpenError {
    /// No such pipe: the service is not running (or not installed).
    NotRunning,
    /// All instances busy for the whole wait.
    Busy,
    /// FER-480 review F1: the pipe NAME is held by another process (its
    /// owner is not SYSTEM/Administrators, or its server PID is not the
    /// service's). `pid` = that server process, when readable. Nothing
    /// was sent.
    Squatted {
        pid: Option<u32>,
        why: String,
    },
    /// The pipe is served by the service's PID but another check failed
    /// (image, token, SCM). Nothing was sent.
    Untrusted(String),
    Io(io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotRunning => write!(f, "service pipe not found"),
            OpenError::Busy => write!(f, "service pipe busy"),
            OpenError::Squatted { pid: Some(p), why } => {
                write!(f, "pipe name taken by another process (pid {p}): {why}")
            }
            OpenError::Squatted { pid: None, why } => {
                write!(f, "pipe name taken by another process: {why}")
            }
            OpenError::Untrusted(why) => write!(f, "untrusted service pipe: {why}"),
            OpenError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// FER-480: the process a client expects behind a pipe.
#[derive(Debug, Clone)]
pub struct ExpectedServer {
    pub pid: u32,
    /// The image file the server process must run (compared by file
    /// identity, not by string).
    pub image: std::path::PathBuf,
    /// A group the server's token must hold — checked when the client may
    /// open the server's token (an elevated client, or a same-user server);
    /// a standard user cannot open a SYSTEM service's process at all.
    pub token_group: Option<Vec<u8>>,
}

impl ExpectedServer {
    /// The running FerrumIndexer service, from the SCM. `Ok(None)`: the
    /// service is not installed or not serving (a pipe that exists anyway
    /// is a squatter). `Err`: the SCM could not be asked.
    pub fn service() -> Result<Option<ExpectedServer>, String> {
        let svc = crate::win::sid_from_string(crate::store::SERVICE_SID)
            .map_err(|e| format!("service SID: {e}"))?;
        match crate::service::service_process() {
            Ok(Some(p)) => Ok(Some(ExpectedServer {
                pid: p.pid,
                image: p.image,
                token_group: Some(svc),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("SCM: {e}")),
        }
    }
}

/// FER-480: is the server end of the connected pipe `h` the expected
/// process? Called before the client sends anything. Checks, in order:
/// 1. `GetNamedPipeServerProcessId` == `expect.pid` (NPFS records the PID
///    of the process that created the instance; a squatter's instance
///    carries the squatter's PID);
/// 2. that process's image (`SystemProcessIdInformation`, readable without
///    opening the process) is the same FILE as `expect.image` — guards a
///    PID recycled between the SCM query and this check;
/// 3. when the client may open the server's token: it holds
///    `expect.token_group` (enabled). `Ok(None)` (access denied, the usual
///    case for a standard user and a SYSTEM service) skips this check;
///    any other failure fails closed.
///
/// A PID mismatch is [`OpenError::Squatted`] (the name is held by another
/// process); every other failure is [`OpenError::Untrusted`].
pub fn verify_server(h: &OwnedHandle, expect: &ExpectedServer) -> Result<(), OpenError> {
    let pid = crate::win::pipe_server_pid(h)
        .map_err(|e| OpenError::Untrusted(format!("server PID: {e}")))?;
    if pid != expect.pid {
        return Err(OpenError::Squatted {
            pid: Some(pid),
            why: format!(
                "pipe served by process {pid}, not by the service (process {})",
                expect.pid
            ),
        });
    }
    verify_image_and_token(pid, expect).map_err(OpenError::Untrusted)
}

fn verify_image_and_token(pid: u32, expect: &ExpectedServer) -> Result<(), String> {
    use crate::win::{file_identity, nt_file_identity, process_has_group, process_image_nt};
    let image = process_image_nt(pid).map_err(|e| format!("server image: {e}"))?;
    let got = nt_file_identity(&image).map_err(|e| format!("server image {image}: {e}"))?;
    let want = file_identity(expect.image.as_os_str())
        .map_err(|e| format!("service image {}: {e}", expect.image.display()))?;
    if got != want {
        return Err(format!(
            "server process {pid} runs {image}, not {}",
            expect.image.display()
        ));
    }
    if let Some(group) = &expect.token_group {
        match process_has_group(pid, group) {
            Ok(None) | Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return Err(format!(
                    "server process {pid} does not hold {}",
                    sid_string(group).unwrap_or_else(|_| "<invalid SID>".into())
                ));
            }
            Err(e) => return Err(format!("server token: {e}")),
        }
    }
    Ok(())
}

/// Client: open `endpoint` (identification-level SQOS) and verify the
/// server before returning the handle (nothing is sent here):
/// - every endpoint: the pipe owner ([`trusted_owners`]);
/// - [`Endpoint::Service`] (FER-480): [`verify_server`] against the running
///   service from the SCM ([`ExpectedServer::service`]).
///
/// Dev endpoints are per-user test pipes (DACL: that user only); the owner
/// check is all they get unless the caller uses [`open_client_expect`].
pub fn open_client(endpoint: &Endpoint, wait: Duration) -> Result<OwnedHandle, OpenError> {
    if !matches!(endpoint, Endpoint::Service) {
        return open_owner_checked(endpoint, wait);
    }
    let expect = ExpectedServer::service().map_err(OpenError::Untrusted)?;
    open_service(endpoint, wait, expect.as_ref(), announced_endpoint)
}

/// FER-487: the service's announced fallback endpoint, when the registry
/// announcement passes every check (see `announce`).
fn announced_endpoint() -> Option<Endpoint> {
    let sid = crate::store::SERVICE_SID;
    crate::announce::read_announced(&crate::announce::Hklm, sid)
        .and_then(|name| Endpoint::announced(&name))
}

/// FER-487: open the service pipe `primary`, verifying the server against
/// the SCM (`expect`). When the primary is squatted, untrusted or missing,
/// and `fallback` names an endpoint, that endpoint is opened and verified
/// the same way. Nothing is sent before verification. Without `expect` (the
/// SCM says the service is not running) a pipe that exists is a squatter.
pub fn open_service(
    primary: &Endpoint,
    wait: Duration,
    expect: Option<&ExpectedServer>,
    fallback: impl FnOnce() -> Option<Endpoint>,
) -> Result<OwnedHandle, OpenError> {
    let Some(expect) = expect else {
        let h = open_owner_checked(primary, wait)?;
        return Err(OpenError::Squatted {
            pid: crate::win::pipe_server_pid(&h).ok(),
            why: "FerrumIndexer is not running, but the pipe exists".into(),
        });
    };
    let first = open_owner_checked(primary, wait).and_then(|h| {
        verify_server(&h, expect)?;
        Ok(h)
    });
    match first {
        Err(OpenError::Squatted { .. } | OpenError::Untrusted(_) | OpenError::NotRunning) => {
            match fallback() {
                Some(alt) => open_owner_checked(&alt, wait).and_then(|h| {
                    verify_server(&h, expect)?;
                    Ok(h)
                }),
                None => first,
            }
        }
        other => other,
    }
}

/// [`open_client`] that verifies the server against `expect` (any
/// endpoint). Used by tests and by callers that know their server.
pub fn open_client_expect(
    endpoint: &Endpoint,
    wait: Duration,
    expect: &ExpectedServer,
) -> Result<OwnedHandle, OpenError> {
    let h = open_owner_checked(endpoint, wait)?;
    verify_server(&h, expect)?;
    Ok(h)
}

/// FER-480 review F1: who holds pipe `path`? Opens it as a client
/// (identification-level SQOS, client rights only) and closes it again
/// WITHOUT sending anything; reads the server PID, the pipe owner and the
/// server's image. For the service's log when the name is taken.
pub fn describe_pipe_holder(path: &str) -> String {
    let name = wide(path);
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            CLIENT_ACCESS,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return format!("holder not readable ({})", io::Error::last_os_error());
    }
    let h = OwnedHandle(h);
    let pid = crate::win::pipe_server_pid(&h);
    let owner = handle_owner(&h)
        .ok()
        .and_then(|o| sid_string(&o).ok())
        .unwrap_or_else(|| "?".into());
    drop(h);
    match pid {
        Ok(pid) => {
            let image = crate::win::process_image_nt(pid).unwrap_or_else(|e| format!("? ({e})"));
            format!("held by pid {pid}, owner {owner}, image {image}")
        }
        Err(e) => format!("held by pid ? ({e}), owner {owner}"),
    }
}

/// Open `endpoint` and check the pipe's owner.
fn open_owner_checked(endpoint: &Endpoint, wait: Duration) -> Result<OwnedHandle, OpenError> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
    let name = wide(endpoint.path());
    let start = Instant::now();
    let h = loop {
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                CLIENT_ACCESS,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            break OwnedHandle(h);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error().map(|c| c as u32) {
            Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PATH_NOT_FOUND) => {
                return Err(OpenError::NotRunning);
            }
            Some(ERROR_PIPE_BUSY) => {
                let left = wait.saturating_sub(start.elapsed());
                if left.is_zero() {
                    return Err(OpenError::Busy);
                }
                unsafe { WaitNamedPipeW(name.as_ptr(), left.as_millis().clamp(1, 5000) as u32) };
            }
            _ => return Err(OpenError::Io(err)),
        }
    };
    let owner = handle_owner(&h).map_err(OpenError::Io)?;
    if !trusted_owners(endpoint)
        .map_err(OpenError::Io)?
        .contains(&owner)
    {
        return Err(OpenError::Squatted {
            pid: crate::win::pipe_server_pid(&h).ok(),
            why: format!(
                "pipe owner {} is not trusted",
                sid_string(&owner).unwrap_or_else(|_| "<invalid SID>".into())
            ),
        });
    }
    Ok(h)
}

// ── overlapped I/O ───────────────────────────────────────────────────────

/// Overlapped pipe I/O with a stop flag, a per-operation timeout and an
/// absolute deadline (bounds slow-loris clients).
pub struct PipeIo {
    handle: OwnedHandle,
    event: OwnedHandle,
    stop: Arc<AtomicBool>,
    /// Timeout of each read (`Read::read`).
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    deadline: Option<Instant>,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
}

enum Wait {
    Done(u32),
    Eof,
}

/// `io::ErrorKind` used when the stop flag aborted an operation.
pub const STOPPED: io::ErrorKind = io::ErrorKind::ConnectionAborted;

impl PipeIo {
    pub fn new(handle: OwnedHandle, stop: Arc<AtomicBool>) -> io::Result<Self> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(PipeIo {
            handle,
            event: OwnedHandle(event),
            stop,
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
            deadline: None,
            buf: vec![0u8; IO_BUFFER as usize],
            pos: 0,
            len: 0,
        })
    }

    pub fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle.raw()
    }

    pub fn set_deadline(&mut self, at: Instant) {
        self.deadline = Some(at);
    }

    fn overlapped(&self) -> OVERLAPPED {
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.event.raw();
        ov
    }

    /// Wait for a pending op; on stop/timeout/deadline cancel it and wait for
    /// the cancellation to land before `ov` goes out of scope.
    fn wait(&self, ov: &mut OVERLAPPED, timeout: Duration) -> io::Result<Wait> {
        let start = Instant::now();
        loop {
            if unsafe { WaitForSingleObject(self.event.raw(), POLL_MS) } == WAIT_OBJECT_0 {
                let mut n = 0u32;
                if unsafe { GetOverlappedResult(self.handle.raw(), ov, &mut n, 0) } == 0 {
                    let err = io::Error::last_os_error();
                    return match err.raw_os_error().map(|c| c as u32) {
                        Some(ERROR_BROKEN_PIPE) => Ok(Wait::Eof),
                        // Byte-mode pipes never report this, but be safe.
                        Some(ERROR_MORE_DATA) => Ok(Wait::Done(n)),
                        _ => Err(err),
                    };
                }
                return Ok(Wait::Done(n));
            }
            let reason = if self.stop.load(Ordering::Relaxed) {
                Some(io::Error::new(STOPPED, "stopped"))
            } else if start.elapsed() >= timeout {
                Some(io::Error::new(io::ErrorKind::TimedOut, "pipe timeout"))
            } else if self.deadline.is_some_and(|d| Instant::now() >= d) {
                Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connection lifetime",
                ))
            } else {
                None
            };
            if let Some(e) = reason {
                unsafe { CancelIoEx(self.handle.raw(), ov) };
                let mut n = 0u32;
                unsafe { GetOverlappedResult(self.handle.raw(), ov, &mut n, 1) };
                return Err(e);
            }
        }
    }

    /// Server: wait for a client, polling the stop flag (no timeout).
    pub fn accept(&self) -> io::Result<()> {
        let mut ov = self.overlapped();
        if unsafe { ConnectNamedPipe(self.handle.raw(), &mut ov) } != 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error().map(|c| c as u32) {
            Some(ERROR_PIPE_CONNECTED) => Ok(()),
            Some(ERROR_IO_PENDING) => loop {
                match self.wait(&mut ov, Duration::from_secs(3600)) {
                    Ok(_) => return Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        // Re-arm after the hour-long wait was cancelled.
                        ov = self.overlapped();
                        if unsafe { ConnectNamedPipe(self.handle.raw(), &mut ov) } != 0 {
                            return Ok(());
                        }
                        let e2 = io::Error::last_os_error();
                        match e2.raw_os_error().map(|c| c as u32) {
                            Some(ERROR_PIPE_CONNECTED) => return Ok(()),
                            Some(ERROR_IO_PENDING) => continue,
                            _ => return Err(e2),
                        }
                    }
                    Err(e) => return Err(e),
                }
            },
            _ => Err(err),
        }
    }

    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < data.len() {
            let mut ov = self.overlapped();
            let mut n = 0u32;
            let chunk = (data.len() - done).min(IO_BUFFER as usize) as u32;
            let ok = unsafe {
                WriteFile(
                    self.handle.raw(),
                    data[done..].as_ptr(),
                    chunk,
                    &mut n,
                    &mut ov,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(err);
                }
            }
            match self.wait(&mut ov, self.write_timeout)? {
                Wait::Done(0) => return Err(io::ErrorKind::WriteZero.into()),
                Wait::Done(n) => done += n as usize,
                Wait::Eof => return Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        Ok(())
    }
}

impl Read for PipeIo {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.len {
            let mut ov = self.overlapped();
            let mut n = 0u32;
            let cap = self.buf.len() as u32;
            let ok = unsafe {
                ReadFile(
                    self.handle.raw(),
                    self.buf.as_mut_ptr(),
                    cap,
                    &mut n,
                    &mut ov,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                match err.raw_os_error().map(|c| c as u32) {
                    Some(ERROR_IO_PENDING) => {}
                    Some(ERROR_BROKEN_PIPE) => return Ok(0),
                    _ => return Err(err),
                }
            }
            match self.wait(&mut ov, self.read_timeout)? {
                Wait::Done(n) => {
                    self.pos = 0;
                    self.len = n as usize;
                }
                Wait::Eof => return Ok(0),
            }
            if self.len == 0 {
                return Ok(0);
            }
        }
        let n = out.len().min(self.len - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_access_excludes_instance_creation() {
        assert_eq!(CLIENT_ACCESS & FILE_APPEND_DATA, 0);
        assert_eq!(CLIENT_ACCESS, 0x0012_0183);
    }

    #[test]
    fn sddl_shapes() {
        let s = pipe_sddl(&Endpoint::Service, "S-1-5-21-1-2-3-1001");
        // FER-480 review F2: the service pipe sets its owner explicitly.
        assert!(s.starts_with("O:SYD:P("), "{s}");
        assert_eq!(s, format!("O:SY{}", service_dacl_sddl()));
        let probe = probe_sddl("S-1-5-21-1-2-3-1001");
        assert!(!probe.contains("O:"), "probe has no owner: {probe}");
        assert!(probe.starts_with("D:P("));
        // Clients trust SYSTEM and Administrators only.
        assert_eq!(
            trusted_owners(&Endpoint::Service).unwrap(),
            vec![SID_LOCAL_SYSTEM.to_vec(), SID_ADMINISTRATORS.to_vec()]
        );
        assert!(s.contains("(A;;GA;;;SY)"));
        assert!(s.contains("(A;;0x120183;;;IU)"));
        assert!(s.contains("(D;;GA;;;NU)"));
        assert!(!s.contains("S-1-5-21"), "service DACL names no user");
        assert!(s.contains(&format!("(A;;GA;;;{})", crate::store::SERVICE_SID)));
        assert!(!s.contains(";;;WD)") && !s.contains(";;;AU)") && !s.contains(";;;BU)"));
        assert!(s.ends_with("S:(ML;;NWNR;;;ME)"));
        // Deny ACEs come first (canonical order).
        assert!(s.find("(D;").unwrap() < s.find("(A;").unwrap());

        let d = pipe_sddl(&Endpoint::dev("t1").unwrap(), "S-1-5-21-1-2-3-1001");
        assert!(d.contains("(A;;GA;;;S-1-5-21-1-2-3-1001)"));
        assert!(!d.contains(";;;IU)") && !d.contains(";;;SY)"));
        assert!(!d.starts_with("O:"));
    }

    #[test]
    fn dev_suffix_is_validated() {
        assert!(Endpoint::dev("abc-123").is_some());
        for bad in ["", "a b", r"..\x", "a/b", &"x".repeat(41), "é"] {
            assert!(Endpoint::dev(bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            Endpoint::dev("t").unwrap().path(),
            r"\\.\pipe\FerrumIndexer-dev-t"
        );
        // FER-480: NPFS root, where a write-restricted token can create it.
        assert_eq!(Endpoint::Service.path(), r"\\.\pipe\FerrumIndexer");
        assert!(!SERVICE_PIPE.contains("ProtectedPrefix"));
        assert!(probe_pipe_name().starts_with(PROBE_PIPE_PREFIX));
        assert_ne!(probe_pipe_name(), Endpoint::Service.path());
    }

    #[test]
    fn both_sddls_parse() {
        let sid = sid_string(&current_user_sid().unwrap()).unwrap();
        SecurityDescriptor::from_sddl(&pipe_sddl(&Endpoint::Service, &sid)).unwrap();
        SecurityDescriptor::from_sddl(&pipe_sddl(&Endpoint::Dev("x".into()), &sid)).unwrap();
    }

    /// INTERACTIVE gets data access but cannot add an instance: create a pipe
    /// with the SERVICE DACL (FER-480: no explicit owner any more), then
    /// try to add a second instance as that same (interactive, non-SYSTEM)
    /// user — refused — while a client open with CLIENT_ACCESS succeeds.
    #[test]
    fn interactive_clients_cannot_add_instances() {
        if crate::win::process_elevated() {
            return;
        }
        let sid = sid_string(&current_user_sid().unwrap()).unwrap();
        let _ = &sid; // a user cannot assign O:SY: the service DACL alone
        let sddl = service_dacl_sddl();
        let sd = SecurityDescriptor::from_sddl(&sddl).unwrap();
        let ep = Endpoint::dev(&format!("acl-{}", std::process::id())).unwrap();
        let first = create_instance(&ep, &sd, true).unwrap();
        let second = create_instance(&ep, &sd, false);
        assert_eq!(
            second.err().map(|e| e.kind()),
            Some(io::ErrorKind::PermissionDenied)
        );
        // Opening as a client with exactly CLIENT_ACCESS works.
        let name = wide(ep.path());
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                CLIENT_ACCESS,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(h, INVALID_HANDLE_VALUE, "{}", io::Error::last_os_error());
        let h = OwnedHandle(h);
        // The owner is this user here, so a SERVICE client refuses it.
        let owner = handle_owner(&h).unwrap();
        assert!(!trusted_owners(&Endpoint::Service).unwrap().contains(&owner));
        drop(h);
        drop(first);
    }

    /// FER-476 (Restricted service SID): a write-restricted token shaped
    /// like the service's (restricting SIDs: service SID, Everyone, WRITE
    /// RESTRICTED) can create the first instance AND add instances when the
    /// DACL grants the service SID — and cannot add one without that ACE.
    #[test]
    fn restricted_token_can_add_instances_only_with_service_ace() {
        use crate::win::{
            Impersonation, SID_EVERYONE_STR, SID_WRITE_RESTRICTED_STR, write_restricted_token,
        };
        let tok = write_restricted_token(&[
            crate::store::SERVICE_SID,
            SID_EVERYONE_STR,
            SID_WRITE_RESTRICTED_STR,
        ])
        .unwrap();
        let sid = sid_string(&current_user_sid().unwrap()).unwrap();
        let with =
            SecurityDescriptor::from_sddl(&pipe_sddl(&Endpoint::Dev("x".into()), &sid)).unwrap();
        let without = SecurityDescriptor::from_sddl(&format!(
            "D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;{sid})S:(ML;;NWNR;;;ME)"
        ))
        .unwrap();
        let pid = std::process::id();
        let ok_ep = Endpoint::dev(&format!("rs-ok-{pid}")).unwrap();
        let bad_ep = Endpoint::dev(&format!("rs-bad-{pid}")).unwrap();
        let _imp = Impersonation::begin(&tok).unwrap();
        let first = create_instance(&ok_ep, &with, true).expect("first instance");
        let second = create_instance(&ok_ep, &with, false).expect("second instance");
        let bad_first = create_instance(&bad_ep, &without, true).expect("first instance");
        let bad_second = create_instance(&bad_ep, &without, false);
        drop(_imp);
        assert_eq!(
            bad_second.err().map(|e| e.kind()),
            Some(io::ErrorKind::PermissionDenied),
            "without the service-SID ACE a restricted token cannot add instances"
        );
        drop((first, second, bad_first));
    }

    /// FER-480 regression: the restricted-SID probe — the very function
    /// `install` runs — succeeds for a write-restricted token shaped like
    /// a Restricted service's, in the production namespace (NPFS root):
    /// first instance, second instance, trusted owner. Runs without admin
    /// rights (the creator ACE stands in for SYSTEM).
    #[test]
    fn restricted_probe_succeeds_in_the_service_namespace() {
        use crate::win::{Impersonation, write_restricted_token};
        let tok = write_restricted_token(&crate::service::RESTRICTING_SIDS).unwrap();
        let me = current_user_sid().unwrap();
        let me_str = sid_string(&me).unwrap();
        // Elevated runs get Administrators as default owner, others the user.
        let mut owners = trusted_owners(&Endpoint::Service).unwrap();
        owners.push(me.clone());
        let name = probe_pipe_name();
        assert!(!name.contains("ProtectedPrefix"));
        let imp = Impersonation::begin(&tok).unwrap();
        let r = probe_instances(&name, &me_str, &owners);
        // The probe's owner check is real: neither the user nor
        // Administrators is SYSTEM.
        let strict = probe_instances(&probe_pipe_name(), &me_str, &[SID_LOCAL_SYSTEM.to_vec()]);
        drop(imp);
        r.expect("restricted token must create both instances");
        assert!(strict.unwrap_err().contains("would not be trusted"));
    }

    /// The probe DACL is the service DACL plus exactly one creator ACE.
    #[test]
    fn probe_sddl_is_service_sddl_plus_creator() {
        let p = probe_sddl("S-1-5-21-1-2-3-1001");
        assert_eq!(
            p.replacen("(A;;GA;;;S-1-5-21-1-2-3-1001)", "", 1),
            service_dacl_sddl()
        );
        SecurityDescriptor::from_sddl(&p).unwrap();
    }

    /// A write-restricted token (even an elevated one) cannot create pipes
    /// under `ProtectedPrefix\Administrators\` — the FER-480 root cause; a
    /// non-elevated token cannot either way. Documents why the service
    /// pipe moved.
    #[test]
    fn restricted_token_cannot_use_protected_prefix() {
        use crate::win::{Impersonation, write_restricted_token};
        let tok = write_restricted_token(&crate::service::RESTRICTING_SIDS).unwrap();
        let me = sid_string(&current_user_sid().unwrap()).unwrap();
        let name = format!(
            r"\\.\pipe\ProtectedPrefix\Administrators\FerrumIndexer-t-{}",
            std::process::id()
        );
        let imp = Impersonation::begin(&tok).unwrap();
        let r = probe_instances(&name, &me, &[]);
        drop(imp);
        let e = r.unwrap_err();
        assert!(e.contains("os error 5"), "{e}");
    }

    // ── FER-480: server verification ────────────────────────────────────

    /// An in-process server with `n` listening instances (one per client
    /// connection the test makes); the first is returned separately.
    fn own_server(tag: &str, n: usize) -> (Endpoint, OwnedHandle, Vec<OwnedHandle>) {
        let sid = sid_string(&current_user_sid().unwrap()).unwrap();
        let ep = Endpoint::dev(&format!("v{}-{tag}", std::process::id())).unwrap();
        let sd = SecurityDescriptor::from_sddl(&pipe_sddl(&ep, &sid)).unwrap();
        let h = create_instance(&ep, &sd, true).unwrap();
        let more = (1..n)
            .map(|_| create_instance(&ep, &sd, false).unwrap())
            .collect();
        (ep, h, more)
    }

    fn me_expected() -> ExpectedServer {
        ExpectedServer {
            pid: std::process::id(),
            image: std::env::current_exe().unwrap(),
            token_group: None,
        }
    }

    #[test]
    fn own_process_identity_reads() {
        use crate::win::{file_identity, nt_file_identity, process_has_group, process_image_nt};
        let nt = process_image_nt(std::process::id()).unwrap();
        assert!(nt.starts_with(r"\Device\"), "{nt}");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            nt_file_identity(&nt).unwrap(),
            file_identity(exe.as_os_str()).unwrap()
        );
        assert_eq!(
            process_has_group(std::process::id(), &crate::win::SID_INTERACTIVE).unwrap(),
            Some(true)
        );
        assert!(nt_file_identity(r"C:\x").is_err(), "NT paths only");
    }

    #[test]
    fn verify_accepts_the_expected_server() {
        let (ep, _srv, _more) = own_server("ok", 2);
        open_client_expect(&ep, Duration::from_secs(2), &me_expected()).expect("verified");
        let with_group = ExpectedServer {
            token_group: Some(crate::win::SID_INTERACTIVE.to_vec()),
            ..me_expected()
        };
        open_client_expect(&ep, Duration::from_secs(2), &with_group).expect("group held");
    }

    #[test]
    fn verify_rejects_wrong_pid_image_or_token() {
        let (ep, _srv, _more) = own_server("bad", 3);
        let untrusted =
            |exp: &ExpectedServer| match open_client_expect(&ep, Duration::from_secs(2), exp) {
                Err(OpenError::Untrusted(why)) => why,
                other => panic!("expected Untrusted, got {:?}", other.map(|_| ())),
            };
        // Another PID (the SCM would report the service's PID): the name is
        // held by another process (review F1), with that process's PID.
        let bad_pid = ExpectedServer {
            pid: std::process::id() ^ 0x10_0000,
            ..me_expected()
        };
        match open_client_expect(&ep, Duration::from_secs(2), &bad_pid) {
            Err(OpenError::Squatted { pid, why }) => {
                assert_eq!(pid, Some(std::process::id()));
                assert!(why.contains("not by the service"), "{why}");
            }
            other => panic!("expected Squatted, got {:?}", other.map(|_| ())),
        }
        // Right PID, other image (a PID recycled by another program).
        let other = std::env::temp_dir().join(format!("ferrum-verify-{}.exe", std::process::id()));
        std::fs::write(&other, b"x").unwrap();
        let why = untrusted(&ExpectedServer {
            image: other.clone(),
            ..me_expected()
        });
        let _ = std::fs::remove_file(&other);
        assert!(why.contains(" runs "), "{why}");
        // Readable token without the service SID.
        let why = untrusted(&ExpectedServer {
            token_group: Some(crate::win::sid_from_string(crate::store::SERVICE_SID).unwrap()),
            ..me_expected()
        });
        assert!(why.contains("does not hold"), "{why}");
    }

    /// Nothing reaches a server that fails verification: the fake server
    /// accepts the connection and reads end-of-pipe, not a byte.
    #[test]
    fn rejected_server_receives_nothing() {
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        // Bytes the client left in the server's inbound buffer (0 also when
        // the closed pipe reports broken/no data).
        let pending = |h: &OwnedHandle| -> u32 {
            let mut avail = 0u32;
            let ok = unsafe {
                PeekNamedPipe(
                    h.raw(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 { 0 } else { avail }
        };
        let (ep, srv, more) = own_server("quiet", 2);
        let bad = ExpectedServer {
            pid: std::process::id() ^ 0x10_0000,
            ..me_expected()
        };
        assert!(matches!(
            open_client_expect(&ep, Duration::from_secs(2), &bad),
            Err(OpenError::Squatted { .. })
        ));
        assert_eq!(
            pending(&srv),
            0,
            "client sent nothing to the unverified server"
        );
        // Control: a verified client that writes is seen by the same check.
        let h = open_client_expect(&ep, Duration::from_secs(2), &me_expected()).unwrap();
        let io = PipeIo::new(h, Arc::new(AtomicBool::new(false))).unwrap();
        io.write_all(b"x").unwrap();
        drop(io); // closed like the rejected client: its byte is still seen
        assert_eq!(pending(&more[0]), 1);
    }

    // ── FER-487: fallback name ───────────────────────────────────────────

    fn absent(tag: &str) -> Endpoint {
        Endpoint::dev(&format!("absent-{tag}-{}", std::process::id())).unwrap()
    }

    #[test]
    fn announced_endpoints_are_validated() {
        let name = format!("FerrumIndexer-{}", "0f".repeat(16));
        let ep = Endpoint::announced(&name).expect("valid name");
        assert_eq!(ep.path(), format!(r"\\.\pipe\{name}"));
        for bad in [
            "FerrumIndexer-r-0123",
            r"..\..\FerrumIndexer-0123456789abcdef0123456789abcdef",
            "FerrumIndexer",
        ] {
            assert!(Endpoint::announced(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn first_instance_on_a_taken_name_is_access_denied() {
        // The condition the service uses to fall back (ERROR_ACCESS_DENIED).
        let (ep, _srv, _more) = own_server("taken", 1);
        let sid = sid_string(&current_user_sid().unwrap()).unwrap();
        let sd = SecurityDescriptor::from_sddl(&pipe_sddl(&ep, &sid)).unwrap();
        let err = match create_instance(&ep, &sd, true) {
            Ok(_) => panic!("second first-instance must fail"),
            Err(e) => e,
        };
        assert_eq!(err.raw_os_error(), Some(5), "{err}");
    }

    #[test]
    fn fallback_is_used_when_the_fixed_name_is_missing() {
        let (served, _srv, _more) = own_server("fb-ok", 2);
        let fixed = absent("fb-ok");
        let h = open_service(&fixed, Duration::from_secs(2), Some(&me_expected()), || {
            Some(served.clone())
        })
        .expect("fallback verified");
        drop(h);
    }

    #[test]
    fn fallback_is_verified_like_the_primary() {
        // A fallback served by a process other than the SCM's service is
        // refused: the announcement is a hint, not a trust decision.
        let (served, _srv, _more) = own_server("fb-bad", 2);
        let fixed = absent("fb-bad");
        let wrong_pid = ExpectedServer {
            pid: std::process::id() ^ 0x10_0000,
            ..me_expected()
        };
        match open_service(&fixed, Duration::from_secs(2), Some(&wrong_pid), || {
            Some(served.clone())
        }) {
            Err(OpenError::Squatted { pid, .. }) => assert_eq!(pid, Some(std::process::id())),
            other => panic!("expected Squatted, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn no_announcement_keeps_the_primary_error() {
        let fixed = absent("no-ann");
        match open_service(&fixed, Duration::from_secs(1), Some(&me_expected()), || {
            None
        }) {
            Err(OpenError::NotRunning) => {}
            other => panic!("expected NotRunning, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn no_fallback_is_tried_when_the_service_is_not_running() {
        // SCM says not running: a pipe that exists is a squatter, and no
        // announcement is consulted.
        let (served, _srv, _more) = own_server("no-scm", 2);
        match open_service(&served, Duration::from_secs(1), None, || {
            panic!("fallback must not be read without a running service")
        }) {
            Err(OpenError::Squatted { .. }) => {}
            other => panic!("expected Squatted, got {:?}", other.map(|_| ())),
        }
    }

    /// A squatter on the PRODUCTION name is refused by the service client
    /// (owner first; and by PID when the SCM knows a running service).
    /// Skipped when the name is in use (an installed service holds it).
    #[test]
    fn service_endpoint_refuses_a_squatter() {
        let sid = sid_string(&current_user_sid().unwrap()).unwrap();
        let sd = SecurityDescriptor::from_sddl(&format!("D:P(A;;GA;;;{sid})")).unwrap();
        let Ok(squat) = create_named_instance(SERVICE_PIPE, &sd, true) else {
            return;
        };
        // Owner check first: classified as a squatter, with its PID.
        match open_client(&Endpoint::Service, Duration::from_secs(2)) {
            Err(OpenError::Squatted { pid, .. }) => assert_eq!(pid, Some(std::process::id())),
            other => panic!("squatter accepted: {:?}", other.map(|_| ())),
        }
        // The PID check refuses it on its own as well.
        if let Ok(Some(exp)) = ExpectedServer::service() {
            let squat2 = create_named_instance(SERVICE_PIPE, &sd, false).unwrap();
            let name = wide(SERVICE_PIPE);
            let c = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    CLIENT_ACCESS,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(c, INVALID_HANDLE_VALUE, "{}", io::Error::last_os_error());
            let c = OwnedHandle(c);
            let why = verify_server(&c, &exp).unwrap_err().to_string();
            assert!(why.contains("not by the service"), "{why}");
            drop(c);
            drop(squat2);
        }
        drop(squat);
    }

    /// Review F1: the service's log line about a squatter names its PID,
    /// owner and image — and the holder receives nothing.
    #[test]
    fn describe_pipe_holder_names_the_squatter() {
        let (ep, srv, _more) = own_server("holder", 1);
        let line = describe_pipe_holder(&ep.path());
        assert!(
            line.contains(&format!("held by pid {}", std::process::id())),
            "{line}"
        );
        // The line names the pipe OBJECT's owner — the SID the client trust
        // check (`open_owner_checked`) compares — not the server's user.
        // The dev SDDL sets no owner, so it is the creating token's default
        // owner: this user normally, BUILTIN\Administrators when elevated.
        let owner = handle_owner(&srv).unwrap();
        let me = current_user_sid().unwrap();
        assert!(
            owner == me || owner == SID_ADMINISTRATORS.to_vec(),
            "unexpected default owner {}",
            sid_string(&owner).unwrap()
        );
        let owner = sid_string(&owner).unwrap();
        assert!(line.contains(&format!("owner {owner},")), "{line}");
        assert!(line.contains(r"\Device\"), "{line}");
        let mut avail = 0u32;
        let ok = unsafe {
            windows_sys::Win32::System::Pipes::PeekNamedPipe(
                srv.raw(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        assert!(ok == 0 || avail == 0, "nothing sent to the holder");
        let missing = describe_pipe_holder(&Endpoint::dev("nobody-here").unwrap().path());
        assert!(missing.contains("not readable"), "{missing}");
    }
}
