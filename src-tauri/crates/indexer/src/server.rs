//! The IPC server (FER-474): accept loop, per-connection authentication,
//! rate limiting and request handling. Used by the SCM service and by the
//! `--console` mode (tests/dev); the only difference is the endpoint and
//! where the config lives.

use crate::access::{SharedCache, TokenAccess, TokenKey};
use crate::announce;
use crate::config::Config;
use crate::engine::IndexShared;
use crate::pipe::{self, Endpoint, PipeIo, STOPPED};
use crate::policy::{self, ClientIdentity, RateLimiter, limits};
use crate::proto::{
    self, ErrorCode, HelloInfo, IndexState, IndexStatusInfo, Mode, PROTO_VERSION, ProtocolError,
    Request, Response, ServiceState, StatusInfo, SubtreeChunk,
};
use crate::query::{self, DirAccess, JournalMark, QueryError, SearchQuery, SubtreeInfo};
use crate::store::{self, DataDir};
use crate::win::{OwnedHandle, sid_string};
use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::{RevertToSelf, TOKEN_QUERY};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

/// Shared state of a running server.
pub struct ServerState {
    pub version: String,
    pub console: bool,
    started: Instant,
    mode: Mutex<Mode>,
    paused: AtomicBool,
    /// Where the config is persisted; `None` = in-memory only (untrusted or
    /// unavailable data directory).
    config_dir: Option<Arc<DataDir>>,
    active: AtomicUsize,
    /// The index worker's shared state (FER-475); `None` = no indexing
    /// (tests of the IPC skeleton).
    index: Option<Arc<IndexShared>>,
    /// Search/subtree queries running now (FER-476): at most
    /// [`MAX_HEAVY_QUERIES`] in total and [`MAX_HEAVY_PER_USER`] per user
    /// SID (review L4).
    heavy: Mutex<HeavyCount>,
    /// FER-476 review H1: search/subtree are served only when the
    /// service's own token is restricted by the service SID (checked at
    /// start). Otherwise they answer `degraded` and status says so.
    queries_enabled: bool,
    /// AccessCheck decisions shared per token identity (review L3/L5/N1).
    access_cache: Arc<SharedCache>,
    /// FER-503 (P3b): `changes` requests currently waiting at the barrier,
    /// per user SID.
    changes_waiting: Mutex<WaitingCount>,
}

#[derive(Default)]
struct HeavyCount {
    total: usize,
    per_user: std::collections::HashMap<Vec<u8>, usize>,
}

/// Heavy queries one user (SID) may run at once (review L4): one user
/// cannot take every slot from the others.
pub const MAX_HEAVY_PER_USER: usize = 1;

/// Search/subtree queries served at once; more are answered `busy`. Bounds
/// the CPU the service spends on queries whatever clients do.
pub const MAX_HEAVY_QUERIES: usize = 2;

/// Review I3: in shared mode, non-admin clients get coarse index figures
/// (entry counts in thousands, journal position in 16 MiB steps, memory
/// in MiB), so `indexStatus` is no fine-grained activity side channel.
pub fn coarsen(mut s: IndexStatusInfo) -> IndexStatusInfo {
    const MIB: u64 = 1 << 20;
    s.memory_bytes -= s.memory_bytes % MIB;
    for v in &mut s.volumes {
        v.entries -= v.entries % 1000;
        v.memory_bytes -= v.memory_bytes % MIB;
        v.next_usn = coarse_usn(v.next_usn);
    }
    s
}

/// RAII slot of [`MAX_HEAVY_QUERIES`] / [`MAX_HEAVY_PER_USER`].
struct Heavy<'a> {
    count: &'a Mutex<HeavyCount>,
    user: Vec<u8>,
}

impl<'a> Heavy<'a> {
    fn take(count: &'a Mutex<HeavyCount>, user: &[u8]) -> Option<Heavy<'a>> {
        let mut c = count.lock().unwrap_or_else(|e| e.into_inner());
        let mine = c.per_user.get(user).copied().unwrap_or(0);
        if c.total >= MAX_HEAVY_QUERIES || mine >= MAX_HEAVY_PER_USER {
            return None;
        }
        c.total += 1;
        c.per_user.insert(user.to_vec(), mine + 1);
        Some(Heavy {
            count,
            user: user.to_vec(),
        })
    }
}

impl Drop for Heavy<'_> {
    fn drop(&mut self) {
        let mut c = self.count.lock().unwrap_or_else(|e| e.into_inner());
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_user.get_mut(&self.user) {
            *n -= 1;
            if *n == 0 {
                c.per_user.remove(&self.user);
            }
        }
    }
}

/// FER-503 (P3b): `changes` requests of ONE identity allowed to wait at the
/// freshness barrier at once. The wait holds the connection up to
/// [`CHANGES_BARRIER_TIME`]; without this cap one client's parallel sync
/// could park every connection in the same wait. The extra requests are
/// answered `busy` at once (an existing wire answer — the protocol is
/// unchanged) and that run compares by content instead.
pub const MAX_CHANGES_WAITING_PER_USER: usize = 1;

#[derive(Default)]
struct WaitingCount {
    per_user: std::collections::HashMap<Vec<u8>, usize>,
}

/// One request's place in the barrier wait; released on drop, so a
/// panicking connection handler frees it (like `Slot`).
struct Waiting<'a> {
    count: &'a Mutex<WaitingCount>,
    user: Vec<u8>,
}

impl<'a> Waiting<'a> {
    fn take(count: &'a Mutex<WaitingCount>, user: &[u8]) -> Option<Waiting<'a>> {
        let mut c = count.lock().unwrap_or_else(|e| e.into_inner());
        let mine = c.per_user.get(user).copied().unwrap_or(0);
        if mine >= MAX_CHANGES_WAITING_PER_USER {
            return None;
        }
        c.per_user.insert(user.to_vec(), mine + 1);
        Some(Waiting {
            count,
            user: user.to_vec(),
        })
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let mut c = self.count.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = c.per_user.get_mut(&self.user) {
            *n -= 1;
            if *n == 0 {
                c.per_user.remove(&self.user);
            }
        }
    }
}

fn degraded() -> Response {
    error(
        ErrorCode::Degraded,
        "queries disabled: service token is not restricted",
    )
}

fn query_error(e: QueryError) -> Response {
    match e {
        QueryError::BadQuery(why) => error(ErrorCode::BadRequest, why),
        QueryError::NotIndexed | QueryError::Aborted => {
            error(ErrorCode::NotIndexed, "not available from the index")
        }
        QueryError::Timeout => error(ErrorCode::Timeout, "query took too long"),
        QueryError::TooLarge => error(ErrorCode::TooLarge, "result too large"),
    }
}

impl ServerState {
    pub fn new(version: &str, console: bool, mode: Mode, config_dir: Option<Arc<DataDir>>) -> Self {
        ServerState {
            version: version.to_owned(),
            console,
            started: Instant::now(),
            mode: Mutex::new(mode),
            paused: AtomicBool::new(false),
            config_dir,
            active: AtomicUsize::new(0),
            index: None,
            heavy: Mutex::new(HeavyCount::default()),
            queries_enabled: false,
            access_cache: Arc::new(SharedCache::default()),
            changes_waiting: Mutex::new(WaitingCount::default()),
        }
    }

    /// Enable search/subtree (review H1). The service passes the result of
    /// its own token check; the console (dev pipe, current user only) and
    /// tests pass true explicitly. Default: off.
    pub fn with_queries(mut self, enabled: bool) -> Self {
        self.queries_enabled = enabled;
        self
    }

    pub fn queries_enabled(&self) -> bool {
        self.queries_enabled
    }

    /// Attach the index worker: pause, mode changes and status reach it.
    pub fn with_index(mut self, index: Arc<IndexShared>) -> Self {
        index
            .paused
            .store(self.paused.load(Ordering::SeqCst), Ordering::SeqCst);
        index.set_mode(self.mode());
        self.index = Some(index);
        self
    }

    pub fn mode(&self) -> Mode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::SeqCst);
        if let Some(ix) = &self.index {
            ix.paused.store(p, Ordering::SeqCst);
            if !p {
                // Review L5: the journal was not read while paused, so
                // SECURITY_CHANGE records may have been missed.
                ix.bump_security();
            }
        }
    }

    /// `indexStatus` as `id` may see it (review I3).
    fn index_status_for(&self, id: &ClientIdentity) -> IndexStatusInfo {
        let s = self.index_status();
        if self.mode() == Mode::Shared && !policy::can_lower_privacy(id) {
            coarsen(s)
        } else {
            s
        }
    }

    fn index_status(&self) -> IndexStatusInfo {
        match &self.index {
            Some(ix) => {
                let (memory_bytes, memory_limit) = ix.memory();
                IndexStatusInfo {
                    paused: ix.paused.load(Ordering::SeqCst),
                    degraded: !self.queries_enabled,
                    memory_bytes,
                    memory_limit,
                    volumes: ix.status(),
                }
            }
            None => IndexStatusInfo {
                paused: self.paused.load(Ordering::SeqCst),
                degraded: !self.queries_enabled,
                memory_bytes: 0,
                memory_limit: 0,
                volumes: Vec::new(),
            },
        }
    }

    fn status(&self) -> StatusInfo {
        StatusInfo {
            state: if self.paused.load(Ordering::SeqCst) {
                ServiceState::Paused
            } else {
                ServiceState::Running
            },
            mode: self.mode(),
            index: self
                .index
                .as_ref()
                .map(|ix| IndexState::aggregate(&ix.status()))
                .unwrap_or(IndexState::None),
            version: self.version.clone(),
            uptime_secs: self.started.elapsed().as_secs(),
            console: self.console,
            degraded: !self.queries_enabled,
        }
    }

    /// Apply a mode change requested by `id`. Persist first; the in-memory
    /// mode only changes when the write succeeded.
    fn set_mode(&self, id: &ClientIdentity, to: Mode) -> Response {
        let mut cur = self.mode.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(code) = policy::may_set_mode(id, *cur, to) {
            return error(code, "mode change not allowed for this client");
        }
        if *cur == to {
            return Response::ModeSet {
                mode: to,
                changed: false,
            };
        }
        let Some(dir) = &self.config_dir else {
            return error(ErrorCode::Internal, "configuration is not writable");
        };
        if let Err(e) = dir.write_config(&Config { mode: to }) {
            store::log(&format!("setMode: write failed: {e}"));
            return error(ErrorCode::Internal, "configuration write failed");
        }
        store::log(&format!(
            "mode {} -> {} by {}{}",
            cur.as_str(),
            to.as_str(),
            sid_string(&id.user_sid).unwrap_or_else(|_| "?".into()),
            if id.elevated { " (elevated)" } else { "" }
        ));
        *cur = to;
        // The worker drops every index (memory and disk) and rebuilds in
        // the new scope.
        if let Some(ix) = &self.index {
            ix.set_mode(to);
        }
        Response::ModeSet {
            mode: to,
            changed: true,
        }
    }
}

fn error(code: ErrorCode, message: &str) -> Response {
    Response::Error {
        code,
        message: message.to_owned(),
    }
}

/// One counted connection slot (review L1): RAII, so a panicking worker
/// cannot leak it.
struct Slot(Arc<ServerState>);

impl Slot {
    fn take(state: &Arc<ServerState>) -> Option<Slot> {
        if state.active.fetch_add(1, Ordering::SeqCst) >= limits::MAX_CONNECTIONS {
            state.active.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(state.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Serve `endpoint` until `stop` is set. Returns an error only when the
/// pipe cannot be created at all (a dev endpoint whose name is taken; the
/// service endpoint keeps retrying, see [`create_first`]).
pub fn run(endpoint: &Endpoint, state: Arc<ServerState>, stop: Arc<AtomicBool>) -> io::Result<()> {
    if matches!(endpoint, Endpoint::Service) {
        // FER-487: an announcement from an earlier run is stale; this run
        // announces its own name, if it needs one, after its pipe exists.
        if let Err(e) = announce::clear(&announce::Hklm) {
            store::log(&format!(
                "PipeName: cannot clear the old announcement ({e})"
            ));
        }
    }
    let Some((active, sd, mut next)) = create_first(endpoint, &stop)? else {
        store::log("stopped before the pipe could be created");
        return Ok(());
    };
    let endpoint = &active;
    let owner = pipe::handle_owner(&next)
        .ok()
        .and_then(|o| crate::win::sid_string(&o).ok())
        .unwrap_or_else(|| "?".into());
    store::log(&format!("listening on {} (owner {owner})", endpoint.path()));
    // FER-487: announce only now that the first instance exists.
    if let Endpoint::Announced(name) = endpoint {
        match announce::announce(&announce::Hklm, name) {
            Ok(()) => store::log(&format!("fallback name {name} announced in PipeName")),
            Err(e) => store::log(&format!(
                "cannot announce {name} ({e}); clients can reach the service only through the fixed name"
            )),
        }
    }
    loop {
        let io = PipeIo::new(next, stop.clone())?;
        match io.accept() {
            Ok(()) => {}
            Err(e) if e.kind() == STOPPED => break,
            Err(e) => {
                store::log(&format!("accept failed: {e}"));
                // Keep the name alive: replace the broken instance first.
                next = recreate(endpoint, &sd, &stop)?;
                drop(io);
                continue;
            }
        }
        // Create the next instance BEFORE releasing this one, so the name
        // never has zero instances (no squatting window).
        next = match recreate(endpoint, &sd, &stop) {
            Ok(h) => h,
            Err(_) => break,
        };
        let Some(slot) = Slot::take(&state) else {
            // Over the cap: a short write so a client that does not read
            // cannot stall the accept loop, then close.
            let mut io = io;
            io.write_timeout = Duration::from_millis(200);
            let _ = io.write_all(&proto::encode_response(&error(
                ErrorCode::Busy,
                "too many connections",
            )));
            continue;
        };
        let st = state.clone();
        // The slot moves into the worker; it is released when the worker ends
        // — also on panic (unwinding drops it) and when spawning fails (the
        // closure, and with it the slot, is dropped).
        let _ = std::thread::Builder::new()
            .name("ferrum-indexer-conn".into())
            .spawn(move || {
                let _slot = slot;
                serve_connection(io, &st);
            });
    }
    // Wait briefly for workers (they observe `stop` within ~100 ms).
    let until = Instant::now() + Duration::from_secs(3);
    while state.active.load(Ordering::SeqCst) > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    store::log("stopped");
    Ok(())
}

/// Create the FIRST instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`). FER-480:
/// the service pipe lives in the open NPFS namespace, so another process
/// may hold the name (squatting; clients refuse such a server, see
/// `pipe::verify_server`). The service then keeps retrying every 2 s until
/// the name is free or it is stopped, instead of exiting and exhausting
/// the SCM recovery actions. Dev endpoints fail at once. `Ok(None)` =
/// stopped while waiting.
///
/// Review F1: each logged failure also names the holder of the name (PID,
/// owner, image — `pipe::describe_pipe_holder`, which sends nothing), so
/// an administrator can act. Review F2: should the explicit `O:SY` owner
/// ever be refused (`ERROR_INVALID_OWNER`), the service falls back once to
/// the same DACL without an owner and says so.
///
/// FER-487: when the fixed name is held by another process
/// (`ERROR_ACCESS_DENIED` on the first instance), the service creates the
/// fallback `FerrumIndexer-<32 hex>` instead, with the same DACL, and
/// returns that endpoint. The caller announces it AFTER this returns.
/// Returns the endpoint actually served, its descriptor and the instance.
fn create_first(
    endpoint: &Endpoint,
    stop: &AtomicBool,
) -> io::Result<Option<(Endpoint, crate::win::SecurityDescriptor, OwnedHandle)>> {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_OWNER};
    const RETRY: Duration = Duration::from_secs(2);
    const LOG_EVERY: u32 = 150; // ~5 minutes
    let is_service = |e: &Endpoint| matches!(e, Endpoint::Service | Endpoint::Announced(_));
    let mut endpoint = endpoint.clone();
    let mut sd = pipe::server_security(&endpoint)?;
    let mut tries = 0u32;
    let mut owner_fallback = false;
    let mut name_fallback = false;
    loop {
        match pipe::create_instance(&endpoint, &sd, true) {
            Ok(h) => {
                if tries > 0 {
                    store::log(&format!(
                        "pipe {} created after {tries} retries",
                        endpoint.path()
                    ));
                }
                return Ok(Some((endpoint, sd, h)));
            }
            Err(e)
                if is_service(&endpoint)
                    && !owner_fallback
                    && e.raw_os_error() == Some(ERROR_INVALID_OWNER as i32) =>
            {
                store::log(&format!(
                    "pipe owner O:SY refused ({e}); using the token's default owner"
                ));
                sd = crate::win::SecurityDescriptor::from_sddl(&pipe::service_dacl_sddl())?;
                owner_fallback = true;
                continue;
            }
            Err(e)
                if matches!(endpoint, Endpoint::Service)
                    && !name_fallback
                    && e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
            {
                // FER-487: the fixed name is taken. Serve the same way under
                // a random name; the fixed name stays free for a later run.
                name_fallback = true;
                store::log(&format!(
                    "pipe {} is held by another process ({e}); {}",
                    endpoint.path(),
                    pipe::describe_pipe_holder(&endpoint.path())
                ));
                match announce::random_name()
                    .ok()
                    .and_then(|name| Endpoint::announced(&name))
                {
                    Some(alt) => {
                        sd = pipe::server_security(&alt)?;
                        endpoint = alt;
                        tries = 0;
                    }
                    None => {
                        store::log("no fallback name (OS random source); retrying the fixed name")
                    }
                }
                continue;
            }
            Err(e) if is_service(&endpoint) => {
                if tries.is_multiple_of(LOG_EVERY) {
                    store::log(&format!(
                        "cannot create pipe {} ({e}); {}; clients refuse it, retrying",
                        endpoint.path(),
                        pipe::describe_pipe_holder(&endpoint.path())
                    ));
                }
                tries = tries.saturating_add(1);
                let until = Instant::now() + RETRY;
                while Instant::now() < until {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Create a follow-up instance, retrying briefly on transient failure.
fn recreate(
    endpoint: &Endpoint,
    sd: &crate::win::SecurityDescriptor,
    stop: &AtomicBool,
) -> io::Result<OwnedHandle> {
    let mut tries = 0;
    loop {
        match pipe::create_instance(endpoint, sd, false) {
            Ok(h) => return Ok(h),
            Err(e) => {
                tries += 1;
                if stop.load(Ordering::Relaxed) || tries >= 50 {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Read the client's token at identification level: impersonate, open the
/// thread token, revert IMMEDIATELY (abort if reverting fails — a worker
/// thread must never keep another security context).
fn identify_client(pipe: HANDLE) -> io::Result<(ClientIdentity, OwnedHandle, TokenKey)> {
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut tok: HANDLE = std::ptr::null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut tok) };
    let open_err = io::Error::last_os_error();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    if opened == 0 {
        return Err(open_err);
    }
    let tok = OwnedHandle(tok);
    // FER-476: the identification-level token is kept for the connection;
    // shared-mode results are AccessChecked against it (never impersonated:
    // identification level cannot act as the user).
    // Review N1/N2: the AccessCheck cache key is the token's full
    // AccessCheck identity; if it cannot be read, the client is refused
    // (fail closed) — never a shared fallback key.
    let key: TokenKey = Arc::from(crate::win::token_fingerprint(tok.raw())?);
    Ok((crate::win::client_identity(tok.raw())?, tok, key))
}

/// Per-connection query context: the client's token and its AccessCheck
/// cache (shared mode only).
struct QueryCtx {
    token: Arc<OwnedHandle>,
    /// AccessCheck identity of the client token (cache key, review N1).
    key: TokenKey,
    user: Vec<u8>,
    cache: Arc<SharedCache>,
    access: Option<TokenAccess>,
}

impl QueryCtx {
    /// The filter a query in `mode` must use: none in high_performance,
    /// the client's AccessCheck in shared.
    fn filter(&mut self, mode: Mode, ix: &IndexShared) -> Option<&mut dyn DirAccess> {
        if mode == Mode::HighPerformance {
            return None;
        }
        let epoch = ix.security_epoch();
        let (token, key, cache) = (self.token.clone(), self.key.clone(), self.cache.clone());
        let a = self
            .access
            .get_or_insert_with(|| TokenAccess::new(token, key, cache));
        a.refresh(epoch);
        Some(a)
    }

    fn done(&mut self) {
        if let Some(a) = &mut self.access {
            a.release_volumes();
        }
    }
}

fn run_search(state: &ServerState, ctx: &mut QueryCtx, q: &SearchQuery) -> Response {
    let Some(ix) = &state.index else {
        return error(ErrorCode::NotIndexed, "no index");
    };
    if !state.queries_enabled {
        return degraded();
    }
    let Some(_slot) = Heavy::take(&state.heavy, &ctx.user) else {
        return error(ErrorCode::Busy, "too many queries");
    };
    // Review M1: list, epoch and mode in one consistent snapshot.
    let (vols, epoch, mode) = ix.served();
    let abort = || ix.epoch() != epoch;
    let r = query::search(
        &vols,
        q,
        ctx.filter(mode, ix),
        Instant::now() + query::SEARCH_TIME,
        &abort,
    );
    ctx.done();
    match r {
        Ok(page) => Response::SearchPage(page),
        Err(e) => query_error(e),
    }
}

/// A journal position rounded down to 16 MiB (review I3 granularity).
pub fn coarse_usn(usn: i64) -> i64 {
    usn & !((1i64 << 24) - 1)
}

/// Longest wait of a `changes` query for its freshness barrier (FER-477).
pub const CHANGES_BARRIER_TIME: Duration = Duration::from_secs(5);

/// FER-477a: folder sizes. Only in `high_performance` — in shared mode an
/// aggregate would reveal the size of content the client may not see, and
/// there is no per-entry AccessCheck for aggregates (design decision),
/// so the SERVICE refuses (`forbidden`), not only the UI.
fn run_folder_sizes(
    state: &ServerState,
    ctx: &QueryCtx,
    parent: &str,
    names: &[String],
) -> Response {
    let Some(ix) = &state.index else {
        return error(ErrorCode::NotIndexed, "no index");
    };
    if !state.queries_enabled {
        return degraded();
    }
    // List, epoch and mode in one snapshot (review M1): the mode checked
    // here is the mode of the indexes that answer.
    let (vols, epoch, mode) = ix.served();
    if mode != Mode::HighPerformance {
        return error(
            ErrorCode::Forbidden,
            "folder sizes are available in high_performance mode only",
        );
    }
    let Some(_slot) = Heavy::take(&state.heavy, &ctx.user) else {
        return error(ErrorCode::Busy, "too many queries");
    };
    let abort = || ix.epoch() != epoch;
    match query::folder_sizes(
        &vols,
        parent,
        names,
        Instant::now() + query::FOLDER_SIZES_TIME,
        &abort,
    ) {
        Ok(r) => Response::FolderSizes(r),
        Err(e) => query_error(e),
    }
}

/// FER-477b: changed directories below `path` since `since`.
///
/// Freshness barrier first: the worker is woken and the answer waits until
/// the volume's index read its journal to the end in a pass that STARTED
/// after this request — so a change the user made before asking is always
/// in the answer (never a wrong "nothing changed"). No barrier within
/// [`CHANGES_BARRIER_TIME`] → `timeout`; paused → `paused`.
fn run_changes(
    state: &ServerState,
    ctx: &mut QueryCtx,
    id: &ClientIdentity,
    path: &str,
    since: Option<JournalMark>,
) -> Response {
    let Some(ix) = &state.index else {
        return error(ErrorCode::NotIndexed, "no index");
    };
    if !state.queries_enabled {
        return degraded();
    }
    let Some((letter, _)) = query::parse_abs_dir(path) else {
        return error(ErrorCode::BadRequest, "path");
    };
    // FER-503 (P3b): at most one barrier wait per identity; the others get
    // `busy` at once instead of parking their connections in the same wait
    // (that run then compares by content). The barrier and the answer below
    // are unchanged.
    let Some(_waiting) = Waiting::take(&state.changes_waiting, &ctx.user) else {
        return error(ErrorCode::Busy, "changes already waiting");
    };
    let g0 = ix.poll_gen();
    ix.request_poll();
    let deadline = Instant::now() + CHANGES_BARRIER_TIME;
    loop {
        if ix.paused.load(Ordering::SeqCst) {
            return error(ErrorCode::Paused, "index paused");
        }
        let (vols, ..) = ix.served();
        match vols.iter().find(|v| v.letter == letter) {
            Some(v) if v.stale => break, // answered below: rescan
            Some(crate::engine::ServedVolume {
                index: Some(live), ..
            }) if crate::engine::read_index(live).caught_pass > g0 => break,
            Some(crate::engine::ServedVolume { index: Some(_), .. }) => {}
            _ => return error(ErrorCode::NotIndexed, "not available from the index"),
        }
        if Instant::now() >= deadline {
            return error(ErrorCode::Timeout, "index not current");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let Some(_slot) = Heavy::take(&state.heavy, &ctx.user) else {
        return error(ErrorCode::Busy, "too many queries");
    };
    // Review M1: list, epoch and mode in one consistent snapshot.
    let (vols, epoch, mode) = ix.served();
    // Review I3: no fine-grained journal position for non-admins in shared
    // mode — neither in the mark nor through the window (a client probing
    // `since` USN by USN would learn when other users' changes happened).
    // Rounding DOWN is safe: the window starts earlier and lists more,
    // never less.
    let coarse = mode == Mode::Shared && !policy::can_lower_privacy(id);
    let since = since.map(|m| JournalMark {
        usn: if coarse { coarse_usn(m.usn) } else { m.usn },
        ..m
    });
    let abort = || ix.epoch() != epoch;
    let r = query::changes(&vols, path, since, ctx.filter(mode, ix), &abort);
    ctx.done();
    match r {
        Ok(mut c) => {
            if coarse {
                c.mark.usn = coarse_usn(c.mark.usn);
            }
            Response::Changes(c)
        }
        Err(e) => query_error(e),
    }
}

/// Stream a subtree. Returns false when the client must be dropped (a
/// write failed or timed out — a slow client only ever blocks its own
/// connection thread; no index lock is held while writing).
fn run_subtree(state: &ServerState, ctx: &mut QueryCtx, io: &mut PipeIo, path: &str) -> bool {
    let Some(ix) = &state.index else {
        return io
            .write_all(&proto::encode_response(&error(
                ErrorCode::NotIndexed,
                "no index",
            )))
            .is_ok();
    };
    if !state.queries_enabled {
        return io.write_all(&proto::encode_response(&degraded())).is_ok();
    }
    let Some(_slot) = Heavy::take(&state.heavy, &ctx.user) else {
        return io
            .write_all(&proto::encode_response(&error(
                ErrorCode::Busy,
                "too many queries",
            )))
            .is_ok();
    };
    // Review M1: list, epoch and mode in one consistent snapshot.
    let (vols, epoch, mode) = ix.served();
    let abort = || ix.epoch() != epoch;
    let info: Cell<Option<SubtreeInfo>> = Cell::new(None);
    let write_failed = Cell::new(false);
    let send = |done: bool, entries: Vec<query::SubtreeEntry>| -> bool {
        let Some(i) = info.get() else { return false };
        let frame = proto::encode_response(&Response::SubtreeChunk(SubtreeChunk {
            info: i,
            done,
            entries,
        }));
        let ok = io.write_all(&frame).is_ok();
        write_failed.set(!ok);
        ok
    };
    let r = query::subtree(
        &vols,
        path,
        ctx.filter(mode, ix),
        Instant::now() + query::SUBTREE_TIME,
        &abort,
        &mut |i| {
            info.set(Some(*i));
            true
        },
        &mut |entries| send(false, entries),
    );
    ctx.done();
    match r {
        Ok(_) => send(true, Vec::new()),
        Err(_) if write_failed.get() => false,
        Err(e) => io
            .write_all(&proto::encode_response(&query_error(e)))
            .is_ok(),
    }
}

fn serve_connection(mut io: PipeIo, state: &ServerState) {
    let now = Instant::now();
    io.set_deadline(now + limits::MAX_CONNECTION_LIFETIME);
    io.read_timeout = limits::HELLO_TIMEOUT;
    io.write_timeout = limits::WRITE_TIMEOUT;

    // 1. Hello first: one read is needed before a client can be impersonated,
    //    and it carries no data.
    match proto::read_request(&mut io) {
        Ok(Request::Hello) => {}
        Ok(_) | Err(ProtocolError::Malformed("unsupported version")) => {
            let _ = io.write_all(&proto::encode_response(&error(
                ErrorCode::UnsupportedVersion,
                "expected hello with a supported protocol version",
            )));
            return;
        }
        Err(_) => return,
    }
    // 2. Authenticate by token BEFORE anything else is answered.
    let (id, token, key) = match identify_client(io.raw()) {
        Ok(v) => v,
        Err(e) => {
            store::log(&format!("client identification failed: {e}"));
            let _ = io.write_all(&proto::encode_response(&error(
                ErrorCode::Unauthenticated,
                "client identity unavailable",
            )));
            return;
        }
    };
    if let Err(code) = policy::accept_client(&id) {
        store::log(&format!(
            "refused client {} (interactive={}, il={:#x}, restricted={}, appcontainer={})",
            sid_string(&id.user_sid).unwrap_or_else(|_| "?".into()),
            id.interactive,
            id.integrity_rid,
            id.restricted,
            id.app_container
        ));
        let _ = io.write_all(&proto::encode_response(&error(code, "client not allowed")));
        return;
    }
    let hello = Response::HelloOk(HelloInfo {
        proto: PROTO_VERSION,
        can_lower_privacy: policy::can_lower_privacy(&id),
    });
    if io.write_all(&proto::encode_response(&hello)).is_err() {
        return;
    }

    // 3. Requests, one at a time, rate limited.
    io.read_timeout = limits::IDLE_TIMEOUT;
    let mut ctx = QueryCtx {
        token: Arc::new(token),
        key,
        user: id.user_sid.clone(),
        cache: state.access_cache.clone(),
        access: None,
    };
    let mut rate = RateLimiter::new(limits::RATE_BURST, limits::RATE_PER_SEC, Instant::now());
    loop {
        let req = match proto::read_request(&mut io) {
            Ok(r) => r,
            Err(ProtocolError::Eof) => return,
            Err(ProtocolError::Io(_)) => return,
            Err(_) => {
                // Malformed/oversized: answer once, then drop the client.
                let _ = io.write_all(&proto::encode_response(&error(
                    ErrorCode::BadRequest,
                    "malformed request",
                )));
                return;
            }
        };
        if !rate.allow(Instant::now()) {
            if rate.refused > limits::MAX_REFUSALS {
                return;
            }
            if io
                .write_all(&proto::encode_response(&error(
                    ErrorCode::RateLimited,
                    "too many requests",
                )))
                .is_err()
            {
                return;
            }
            continue;
        }
        let res = match req {
            Request::Hello => {
                let _ = io.write_all(&proto::encode_response(&error(
                    ErrorCode::BadRequest,
                    "duplicate hello",
                )));
                return;
            }
            Request::Ping { token } => Response::Pong { token },
            Request::Status => Response::Status(state.status()),
            Request::Version => Response::Version {
                version: state.version.clone(),
                proto: PROTO_VERSION,
            },
            Request::GetMode => Response::Mode(state.mode()),
            Request::SetMode(to) => state.set_mode(&id, to),
            Request::IndexStatus => Response::IndexStatus(state.index_status_for(&id)),
            Request::Search(q) => run_search(state, &mut ctx, &q),
            Request::Subtree { path } => {
                if !run_subtree(state, &mut ctx, &mut io, &path) {
                    return;
                }
                continue;
            }
            Request::FolderSizes { parent, names } => {
                run_folder_sizes(state, &ctx, &parent, &names)
            }
            Request::Changes { path, since } => run_changes(state, &mut ctx, &id, &path, since),
        };
        if io.write_all(&proto::encode_response(&res)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FER-487: a standard user holds the fixed production name. The service
    /// serves under a random `FerrumIndexer-<hex>` name instead, and that
    /// name is what `run` would announce. Skipped when the production name is
    /// in use (an installed service holds it).
    #[test]
    fn service_falls_back_when_the_fixed_name_is_squatted() {
        let sid = sid_string(&crate::win::current_user_sid().unwrap()).unwrap();
        let sd = crate::win::SecurityDescriptor::from_sddl(&format!("D:P(A;;GA;;;{sid})")).unwrap();
        let Ok(squat) = pipe::create_named_instance(pipe::SERVICE_PIPE, &sd, true) else {
            return;
        };
        let stop = AtomicBool::new(false);
        let (ep, _sd, _first) = create_first(&Endpoint::Service, &stop)
            .unwrap()
            .expect("not stopped");
        match &ep {
            Endpoint::Announced(name) => assert!(crate::announce::name_is_valid(name), "{name}"),
            other => panic!("expected the fallback endpoint, got {other:?}"),
        }
        assert_ne!(ep.path(), pipe::SERVICE_PIPE);
        drop(squat);
    }

    #[test]
    fn coarse_usn_rounds_down_to_16_mib() {
        assert_eq!(coarse_usn(0), 0);
        assert_eq!(coarse_usn((1 << 24) - 1), 0);
        assert_eq!(coarse_usn((5 << 24) + 7), 5 << 24);
        assert!(coarse_usn(123_456_789) <= 123_456_789);
    }

    #[test]
    fn coarse_index_status_for_non_admins() {
        use crate::proto::{VolumeErrorCode, VolumeState, VolumeStatus};
        let s = IndexStatusInfo {
            paused: false,
            degraded: false,
            memory_bytes: (5 << 20) + 123,
            memory_limit: 1 << 30,
            volumes: vec![VolumeStatus {
                letter: b'C',
                state: VolumeState::Ready,
                error: VolumeErrorCode::None,
                entries: 1_234_567,
                memory_bytes: (3 << 20) + 9,
                journal_id: 42,
                next_usn: (7 << 24) + 12_345,
                live: true,
                progress_permille: 1000,
            }],
        };
        let c = coarsen(s);
        assert_eq!(c.memory_bytes, 5 << 20);
        assert_eq!(c.volumes[0].entries, 1_234_000);
        assert_eq!(c.volumes[0].memory_bytes, 3 << 20);
        assert_eq!(c.volumes[0].next_usn, 7 << 24);
        assert_eq!(c.volumes[0].journal_id, 42);
    }

    #[test]
    fn heavy_queries_are_capped_globally_and_per_user() {
        let n = Mutex::new(HeavyCount::default());
        let (alice, bob, carol) = (vec![1u8], vec![2u8], vec![3u8]);
        let a = Heavy::take(&n, &alice).unwrap();
        assert!(Heavy::take(&n, &alice).is_none(), "one per user");
        let b = Heavy::take(&n, &bob).unwrap();
        assert!(Heavy::take(&n, &carol).is_none(), "two in total");
        drop(a);
        let c = Heavy::take(&n, &carol).unwrap();
        drop((b, c));
        assert_eq!(n.lock().unwrap().total, 0);
        assert!(n.lock().unwrap().per_user.is_empty());
    }

    /// FER-503 (P3b): one `changes` barrier wait per identity; the place is
    /// released by drop — also by a panicking handler (unwind), like `Slot`.
    #[test]
    fn changes_barrier_waits_are_capped_per_user_and_released() {
        let n = Mutex::new(WaitingCount::default());
        let (alice, bob) = (vec![1u8], vec![2u8]);
        let a = Waiting::take(&n, &alice).unwrap();
        assert!(Waiting::take(&n, &alice).is_none(), "one wait per identity");
        let b = Waiting::take(&n, &bob).unwrap();
        drop(a);
        let a2 = Waiting::take(&n, &alice).unwrap();
        drop((b, a2));
        assert!(
            n.lock().unwrap().per_user.is_empty(),
            "every place released"
        );
        // A panic while holding the place frees it by unwinding.
        let r = std::thread::scope(|s| {
            let holder = s.spawn(|| {
                let _w = Waiting::take(&n, &alice).unwrap();
                panic!("handler panic");
            });
            holder.join().is_err()
        });
        assert!(r);
        assert!(Waiting::take(&n, &alice).is_some(), "released by unwinding");
        assert!(n.lock().unwrap().per_user.is_empty());
    }

    #[test]
    fn queries_are_off_unless_enabled() {
        let st = ServerState::new("t", false, Mode::Shared, None);
        assert!(!st.queries_enabled());
        assert!(st.status().degraded);
        assert!(st.index_status().degraded);
        let st = st.with_queries(true);
        assert!(!st.status().degraded);
        assert_eq!(
            degraded(),
            Response::Error {
                code: ErrorCode::Degraded,
                message: "queries disabled: service token is not restricted".into()
            }
        );
    }

    /// Review L1: a panicking worker releases its slot.
    #[test]
    fn slots_are_capped_and_released_on_panic() {
        let st = Arc::new(ServerState::new("t", true, Mode::Shared, None));
        let held: Vec<Slot> = (0..limits::MAX_CONNECTIONS)
            .map(|_| Slot::take(&st).expect("slot"))
            .collect();
        assert!(Slot::take(&st).is_none(), "cap reached");
        drop(held);
        assert_eq!(st.active.load(Ordering::SeqCst), 0);
        let slot = Slot::take(&st).unwrap();
        let r = std::thread::spawn(move || {
            let _slot = slot;
            panic!("worker panic");
        })
        .join();
        assert!(r.is_err());
        assert_eq!(
            st.active.load(Ordering::SeqCst),
            0,
            "slot released by unwinding"
        );
    }
}
