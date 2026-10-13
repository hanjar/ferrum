//! FER-474: end-to-end IPC against an in-process console server on a dev
//! pipe (no SCM, no admin). Covers hello/auth, every request, the mode
//! policy, persistence, malformed frames, rate limiting, the connection cap
//! and FIRST_PIPE_INSTANCE.
#![cfg(windows)]

use ferrum_indexer::client::{Client, ClientError};
use ferrum_indexer::pipe::{self, Endpoint, PipeIo};
use ferrum_indexer::proto::{self, ErrorCode, Mode, Request, Response, ServiceState};
use ferrum_indexer::server::{self, ServerState};
use ferrum_indexer::store::{DataDir, Trust};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

struct Running {
    ep: Endpoint,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<std::io::Result<()>>>,
    dir: PathBuf,
    data: Option<Arc<DataDir>>,
    state: Option<Arc<ServerState>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // Release the held directory before removing it.
        self.state = None;
        self.data = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct AclDirGuard {
    tmp: PathBuf,
    closed: PathBuf,
}

impl Drop for AclDirGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("icacls")
            .arg(&self.closed)
            .args(["/reset"])
            .output();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn start(tag: &str, mode: Mode) -> Running {
    let suffix = format!("t{}-{tag}", std::process::id());
    let ep = Endpoint::dev(&suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("ferrum-indexer-it-{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    let (data, _) = DataDir::prepare(&dir, Trust::dev_current_user().unwrap()).unwrap();
    let data = Arc::new(data);
    let state = Arc::new(ServerState::new(
        "9.9.9-test",
        true,
        mode,
        Some(data.clone()),
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2, st2) = (ep.clone(), stop.clone(), state.clone());
    let thread = std::thread::spawn(move || server::run(&e2, st2, s2));
    // Wait for the pipe to exist.
    for _ in 0..100 {
        if std::fs::metadata(ep.path()).is_ok()
            || Client::connect(&ep, Duration::from_millis(50)).is_ok()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Running {
        ep,
        stop,
        thread: Some(thread),
        dir,
        data: Some(data),
        state: Some(state),
    }
}

fn connect(r: &Running) -> Client {
    Client::connect(&r.ep, Duration::from_secs(5)).expect("connect")
}

/// Raw connection that has completed the hello.
fn raw(r: &Running) -> PipeIo {
    let h = pipe::open_client(&r.ep, Duration::from_secs(5)).unwrap();
    let mut io = PipeIo::new(h, Arc::new(AtomicBool::new(false))).unwrap();
    io.write_all(&proto::encode_request(&Request::Hello))
        .unwrap();
    assert!(matches!(
        proto::read_response(&mut io).unwrap(),
        Response::HelloOk(_)
    ));
    io
}

#[test]
fn all_requests_work() {
    let r = start("all", Mode::Shared);
    let mut c = connect(&r);
    assert_eq!(c.hello.proto, proto::PROTO_VERSION);
    c.ping(42).unwrap();
    let (v, p) = c.version().unwrap();
    assert_eq!((v.as_str(), p), ("9.9.9-test", proto::PROTO_VERSION));
    let s = c.status().unwrap();
    assert_eq!(s.state, ServiceState::Running);
    assert_eq!(s.mode, Mode::Shared);
    assert!(s.console);
    assert_eq!(c.get_mode().unwrap(), Mode::Shared);
    r.state.as_ref().unwrap().set_paused(true);
    assert_eq!(c.status().unwrap().state, ServiceState::Paused);
}

#[test]
fn mode_policy_and_persistence() {
    let r = start("mode", Mode::HighPerformance);
    let mut c = connect(&r);
    // Toward more privacy: allowed for a normal interactive user, persisted.
    assert!(c.set_mode(Mode::Shared).unwrap());
    assert!(
        !c.set_mode(Mode::Shared).unwrap(),
        "no-op reports unchanged"
    );
    let (cfg, err) = r.data.as_ref().unwrap().read_config().unwrap().unwrap();
    assert_eq!((cfg.mode, err), (Mode::Shared, None));
    // Toward less privacy: needs an elevated administrator.
    let res = c.set_mode(Mode::HighPerformance);
    if c.hello.can_lower_privacy {
        assert!(res.unwrap());
    } else {
        match res {
            Err(ClientError::Refused {
                code: ErrorCode::NeedsElevation,
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(c.get_mode().unwrap(), Mode::Shared, "unchanged");
        let (cfg, _) = r.data.as_ref().unwrap().read_config().unwrap().unwrap();
        assert_eq!(cfg.mode, Mode::Shared, "config unchanged");
    }
}

#[test]
fn requests_before_hello_and_bad_frames_drop_the_client() {
    let r = start("bad", Mode::Shared);
    // A request other than Hello first.
    let h = pipe::open_client(&r.ep, Duration::from_secs(5)).unwrap();
    let mut io = PipeIo::new(h, Arc::new(AtomicBool::new(false))).unwrap();
    io.write_all(&proto::encode_request(&Request::Status))
        .unwrap();
    match proto::read_response(&mut io).unwrap() {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::UnsupportedVersion),
        other => panic!("{other:?}"),
    }
    let mut b = [0u8; 1];
    assert_eq!(io.read(&mut b).unwrap(), 0, "closed after refusal");

    // Oversized frame after hello.
    let mut io = raw(&r);
    io.write_all(&(proto::MAX_REQUEST_FRAME + 1).to_le_bytes())
        .unwrap();
    match proto::read_response(&mut io).unwrap() {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::BadRequest),
        other => panic!("{other:?}"),
    }
    assert_eq!(io.read(&mut b).unwrap(), 0);

    // Unknown kind / trailing bytes.
    let mut io = raw(&r);
    io.write_all(&[2, 0, 0, 0, 0x03, 0xAA]).unwrap();
    assert!(matches!(
        proto::read_response(&mut io).unwrap(),
        Response::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));

    // Duplicate hello.
    let mut io = raw(&r);
    io.write_all(&proto::encode_request(&Request::Hello))
        .unwrap();
    assert!(matches!(
        proto::read_response(&mut io).unwrap(),
        Response::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));

    // The server still serves well-formed clients.
    connect(&r).ping(1).unwrap();
}

#[test]
fn rate_limit_then_disconnect() {
    let r = start("rate", Mode::Shared);
    let mut io = raw(&r);
    let ping = proto::encode_request(&Request::Ping { token: 5 });
    let mut limited = 0;
    let mut closed = false;
    for _ in 0..200 {
        if io.write_all(&ping).is_err() {
            closed = true;
            break;
        }
        match proto::read_response(&mut io) {
            Ok(Response::Pong { .. }) => {}
            Ok(Response::Error {
                code: ErrorCode::RateLimited,
                ..
            }) => limited += 1,
            Ok(other) => panic!("{other:?}"),
            Err(_) => {
                closed = true;
                break;
            }
        }
    }
    assert!(limited > 0, "rate limit answered");
    assert!(closed, "abusive client dropped");
    connect(&r).ping(2).unwrap();
}

#[test]
fn connection_cap_answers_busy() {
    let r = start("cap", Mode::Shared);
    let held: Vec<PipeIo> = (0..ferrum_indexer::policy::limits::MAX_CONNECTIONS)
        .map(|_| raw(&r))
        .collect();
    match Client::connect(&r.ep, Duration::from_secs(5)) {
        Err(ClientError::Refused {
            code: ErrorCode::Busy,
            ..
        }) => {}
        Err(ClientError::Protocol(_)) => {} // closed right after Busy
        other => panic!("{:?}", other.map(|_| ())),
    }
    drop(held);
    // Slots free again once the held connections close.
    let mut ok = false;
    for _ in 0..50 {
        if Client::connect(&r.ep, Duration::from_secs(1)).is_ok() {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ok);
}

#[test]
fn second_server_on_the_same_name_fails() {
    let r = start("first", Mode::Shared);
    let err = server::run(
        &r.ep,
        Arc::new(ServerState::new("x", true, Mode::Shared, None)),
        Arc::new(AtomicBool::new(true)),
    );
    assert!(
        err.is_err(),
        "FIRST_PIPE_INSTANCE must refuse a second server"
    );
    connect(&r).ping(3).unwrap();
}

#[test]
fn missing_server_is_not_running() {
    let ep = Endpoint::dev(&format!("absent-{}", std::process::id())).unwrap();
    assert!(matches!(
        Client::connect(&ep, Duration::from_millis(200)),
        Err(ClientError::NotRunning)
    ));
}

#[test]
fn stop_ends_the_server_promptly() {
    let mut r = start("stop", Mode::Shared);
    let _c = connect(&r);
    r.stop.store(true, Ordering::SeqCst);
    let t = r.thread.take().unwrap();
    let started = std::time::Instant::now();
    t.join().unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// FER-475: `indexStatus` end to end with the real Windows platform. As a
/// standard user every fixed NTFS volume reports `access_denied` (nothing
/// is read); pause and mode changes reach the worker.
#[test]
fn index_status_over_the_pipe() {
    use ferrum_indexer::engine::IndexShared;
    use ferrum_indexer::proto::{IndexState, VolumeErrorCode, VolumeState};
    if ferrum_indexer::win::process_elevated() {
        return; // elevated, the worker would really index the disks
    }
    let suffix = format!("t{}-index", std::process::id());
    let ep = Endpoint::dev(&suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("ferrum-indexer-it-{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    let (data, _) = DataDir::prepare(&dir, Trust::dev_current_user().unwrap()).unwrap();
    let data = Arc::new(data);
    // high_performance: as a standard user the shared-mode profile list
    // fails closed (systemprofile is not resolvable), see volume.rs tests.
    let shared = IndexShared::new(Mode::HighPerformance);
    let worker = ferrum_indexer::volume::start_worker(Some(&data), shared.clone()).unwrap();
    let state = Arc::new(
        ServerState::new(
            "9.9.9-test",
            true,
            Mode::HighPerformance,
            Some(data.clone()),
        )
        .with_index(shared.clone()),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2, st2) = (ep.clone(), stop.clone(), state.clone());
    let thread = std::thread::spawn(move || server::run(&e2, st2, s2));
    let mut c = None;
    for _ in 0..100 {
        if let Ok(cl) = Client::connect(&ep, Duration::from_millis(100)) {
            c = Some(cl);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut c = c.expect("connect");
    let mut st = c.index_status().unwrap();
    for _ in 0..200 {
        if !st.volumes.is_empty() && st.volumes.iter().all(|v| v.state == VolumeState::Error) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
        st = c.index_status().unwrap();
    }
    assert!(!st.volumes.is_empty(), "the system drive is fixed NTFS");
    for v in &st.volumes {
        assert_eq!(v.state, VolumeState::Error);
        assert_eq!(v.error, VolumeErrorCode::AccessDenied);
        assert_eq!(v.entries, 0);
    }
    assert!(st.memory_limit >= 256 << 20);
    assert_eq!(c.status().unwrap().index, IndexState::Error);
    state.set_paused(true);
    assert!(c.index_status().unwrap().paused);
    state.set_paused(false);
    assert!(!c.index_status().unwrap().paused);
    // Nothing was saved (nothing could be read).
    assert!(
        std::fs::read_dir(dir.join("index"))
            .map(|d| d.count() == 0)
            .unwrap_or(true)
    );
    drop(c);
    stop.store(true, Ordering::SeqCst);
    shared.stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
    worker.join().unwrap();
    drop(state);
    drop(data);
    let _ = std::fs::remove_dir_all(&dir);
}

// ── FER-476: search + subtree over the pipe, with the REAL AccessCheck ──

fn frn_of(path: &std::path::Path) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
        OPEN_EXISTING,
    };
    let w = ferrum_indexer::win::wide(path);
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    assert!(!h.is_null() && h as isize != -1, "{}", path.display());
    let h = ferrum_indexer::win::OwnedHandle(h);
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    assert_ne!(unsafe { GetFileInformationByHandle(h.raw(), &mut info) }, 0);
    ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64
}

/// Replace the DACL of `path` with exactly `sddl`'s (protected: no
/// inherited ACEs), whatever ACEs the folder was created with.
fn set_dacl(path: &std::path::Path, sddl: &str) {
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
    };
    let sd = ferrum_indexer::win::SecurityDescriptor::from_sddl(sddl).unwrap();
    let w = ferrum_indexer::win::wide(path);
    let ok = unsafe {
        SetFileSecurityW(
            w.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd.0,
        )
    };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
}

/// `X:\` opened as the `OpenFileById` volume hint (attributes only).
fn open_volume(letter: u8) -> ferrum_indexer::win::OwnedHandle {
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let w = ferrum_indexer::win::wide(format!("{}:\\", letter as char));
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    assert!(
        !h.is_null() && h as isize != -1,
        "volume {}",
        letter as char
    );
    ferrum_indexer::win::OwnedHandle(h)
}

/// A synthetic index that mirrors REAL directories (real file references
/// from the volume root down to a temp tree), so the shared-mode filter
/// runs the real `OpenFileById` + `AccessCheck` with the client's token:
/// `<tmp>\open\visible.txt` is listable, `<tmp>\closed\hidden.txt` is not
/// (the user may traverse `closed` but not list it).
#[test]
fn search_and_subtree_over_the_pipe_with_real_access_checks() {
    use ferrum_indexer::engine::{IndexShared, LiveIndex, ServedVolume};
    use ferrum_indexer::index::{Kind, Meta, VolumeIndex};
    use ferrum_indexer::query::{KindFilter, MatchKind, QueryCursor, SearchQuery};
    use ferrum_indexer::scope::Scope;
    use std::sync::RwLock;

    let tmp = std::env::temp_dir().join(format!("ferrum-q-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let closed = tmp.join("closed");
    let _guard = AclDirGuard {
        tmp: tmp.clone(),
        closed: closed.clone(),
    };
    std::fs::create_dir_all(tmp.join("open")).unwrap();
    std::fs::write(tmp.join("open").join("visible.txt"), b"v").unwrap();
    std::fs::create_dir(&closed).unwrap();
    std::fs::write(closed.join("hidden.txt"), b"h").unwrap();
    // Read before the ACL change (afterwards the file is unreachable).
    let hidden_frn = frn_of(&closed.join("hidden.txt"));
    let user =
        ferrum_indexer::win::sid_string(&ferrum_indexer::win::current_user_sid().unwrap()).unwrap();
    // Traverse + read attributes + read control + delete/write-DAC only
    // (no list). The WHOLE DACL is replaced by an explicit, protected one —
    // not edited with `icacls /inheritance:r /grant:r`, which only drops
    // inherited ACEs and replaces this user's: any other explicit ACE (on an
    // elevated CI runner a new folder can carry the creator's default DACL,
    // `BUILTIN\Administrators` full, as explicit ACEs) survived and let the
    // elevated token list `closed`. Nothing here grants Administrators.
    set_dacl(
        &closed,
        &format!("D:P(A;;0x1200a0;;;{user})(A;;SDWD;;;{user})(A;;FA;;;SY)"),
    );
    // The fixture really is closed for this client — elevated or not —
    // before the service is asked (tells a fixture problem from a filter
    // problem).
    {
        let letter = closed.to_string_lossy().as_bytes()[0].to_ascii_uppercase();
        let vol = open_volume(letter);
        let tok = ferrum_indexer::access::own_identification_token().unwrap();
        assert_eq!(
            ferrum_indexer::access::token_check(&tok, &vol, frn_of(&closed)),
            ferrum_indexer::query::Access::No,
            "fixture: this token must not be able to list `closed`"
        );
    }

    let tmp_s = tmp.to_string_lossy().into_owned();
    let letter = tmp_s.as_bytes()[0].to_ascii_uppercase();
    let root_path = PathBuf::from(format!("{}:\\", letter as char));
    let mut ix = VolumeIndex::new(frn_of(&root_path));
    let mut parent = ix.root_frn();
    let mut cur = root_path.clone();
    for comp in tmp.components().skip(2) {
        cur.push(comp);
        let frn = frn_of(&cur);
        ix.add_name(
            frn,
            parent,
            Kind::Dir,
            Meta::default(),
            &comp.as_os_str().to_string_lossy(),
        )
        .unwrap();
        parent = frn;
    }
    let add = |ix: &mut VolumeIndex, p: &std::path::Path, parent: u64, kind: Kind| {
        let frn = frn_of(p);
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        ix.add_name(frn, parent, kind, Meta::default(), &name)
            .unwrap();
        frn
    };
    let open_frn = add(&mut ix, &tmp.join("open"), parent, Kind::Dir);
    add(
        &mut ix,
        &tmp.join("open").join("visible.txt"),
        open_frn,
        Kind::File,
    );
    let closed_frn = add(&mut ix, &closed, parent, Kind::Dir);
    ix.add_name(
        hidden_frn,
        closed_frn,
        Kind::File,
        Meta::default(),
        "hidden.txt",
    )
    .unwrap();
    let live = LiveIndex {
        index: ix,
        excluded: Default::default(),
        scope: Scope::everything(),
        journal_id: Some(1),
        next_usn: 0,
        dirty: false,
        stats: Default::default(),
        failed_rounds: 0,
        security_changes: 0,
        // FER-477: `open` and `closed` changed at USN 5; journal at 10.
        changes: {
            let mut l = ferrum_indexer::changelog::ChangeLog::new(0);
            l.record(open_frn, false, 5);
            l.record(closed_frn, false, 5);
            l
        },
        caught_pass: 0,
    };
    let live = LiveIndex {
        next_usn: 10,
        ..live
    };
    let served = vec![ServedVolume {
        letter,
        index: Some(Arc::new(RwLock::new(live))),
        stale: false,
    }];

    for mode in [Mode::HighPerformance, Mode::Shared] {
        let suffix = format!("t{}-q{}", std::process::id(), mode.as_str().len());
        let ep = Endpoint::dev(&suffix).unwrap();
        let shared = IndexShared::new(mode);
        shared.serve_for_test(served.clone());
        let state = Arc::new(
            ServerState::new("t", true, mode, None)
                .with_index(shared.clone())
                .with_queries(true),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let (e2, s2, st2) = (ep.clone(), stop.clone(), state.clone());
        let thread = std::thread::spawn(move || server::run(&e2, st2, s2));
        let mut c = None;
        for _ in 0..100 {
            if let Ok(cl) = Client::connect(&ep, Duration::from_millis(100)) {
                c = Some(cl);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut c = c.expect("connect");
        let q = SearchQuery {
            pattern: "*.txt".into(),
            match_kind: MatchKind::Glob,
            scope: tmp_s.clone(),
            extensions: vec![],
            kinds: KindFilter::Both,
            limit: 100,
            cursor: QueryCursor::default(),
        };
        let page = c.search(&q).unwrap();
        let mut names: Vec<String> = page
            .hits
            .iter()
            .map(|h| h.path.rsplit('\\').next().unwrap().to_owned())
            .collect();
        names.sort();
        let mut seen = Vec::new();
        c.subtree(&tmp_s, 1000, &mut |_, e| {
            seen.extend(e.into_iter().map(|e| e.name));
            true
        })
        .unwrap();
        seen.sort();
        let closed_scope = c.search(&SearchQuery {
            scope: closed.to_string_lossy().into_owned(),
            ..q.clone()
        });
        match mode {
            Mode::HighPerformance => {
                assert_eq!(names, vec!["hidden.txt", "visible.txt"]);
                assert_eq!(seen, vec!["closed", "hidden.txt", "open", "visible.txt"]);
                assert_eq!(closed_scope.unwrap().hits.len(), 1);
            }
            Mode::Shared => {
                assert_eq!(names, vec!["visible.txt"], "AccessCheck hides hidden.txt");
                assert_eq!(seen, vec!["closed", "open", "visible.txt"]);
                assert!(matches!(
                    closed_scope,
                    Err(ClientError::Refused {
                        code: ErrorCode::NotIndexed,
                        ..
                    })
                ));
            }
        }
        // FER-477a: folder sizes in high_performance only — the SERVICE
        // refuses them in shared mode (an aggregate would reveal the size
        // of `closed`, which this user cannot list).
        let sizes = c.folder_sizes(
            &tmp_s,
            &["open".to_string(), "closed".to_string(), "nope".to_string()],
        );
        match mode {
            Mode::HighPerformance => {
                use ferrum_indexer::query::FolderSize;
                let s = sizes.unwrap();
                assert_eq!(
                    s.sizes,
                    vec![
                        FolderSize::Ok { bytes: 0, files: 1 },
                        FolderSize::Ok { bytes: 0, files: 1 },
                        FolderSize::NotIndexed
                    ]
                );
            }
            Mode::Shared => assert!(
                matches!(
                    sizes,
                    Err(ClientError::Refused {
                        code: ErrorCode::Forbidden,
                        ..
                    })
                ),
                "{sizes:?}"
            ),
        }
        // FER-477b: no worker pass after the request → the freshness
        // barrier is not met → timeout (never a possibly stale answer).
        if mode == Mode::HighPerformance {
            let mut slow = Client::connect(&ep, Duration::from_secs(10)).unwrap();
            assert!(matches!(
                slow.changes(&tmp_s, None),
                Err(ClientError::Refused {
                    code: ErrorCode::Timeout,
                    ..
                })
            ));
        }
        // With passes running (stand-in for the worker), the answer lists
        // the changed directories this client may see.
        let passes_stop = Arc::new(AtomicBool::new(false));
        let passes = {
            let (sh, st) = (shared.clone(), passes_stop.clone());
            let ix = served[0].index.clone().unwrap();
            std::thread::spawn(move || {
                while !st.load(Ordering::SeqCst) {
                    sh.complete_pass_for_test(std::slice::from_ref(&ix));
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        let mut cc = Client::connect(&ep, Duration::from_secs(10)).unwrap();
        let mark = cc.changes(&tmp_s, None).unwrap();
        assert_eq!(mark.status, ferrum_indexer::query::ChangeStatus::MarkOnly);
        let coarse = mode == Mode::Shared && !cc.hello.can_lower_privacy;
        assert_eq!(mark.mark.usn, if coarse { 0 } else { 10 });
        let since = ferrum_indexer::query::JournalMark {
            journal_id: 1,
            usn: 0,
        };
        let ch = cc.changes(&tmp_s, Some(since)).unwrap();
        assert_eq!(ch.status, ferrum_indexer::query::ChangeStatus::Complete);
        let rels: Vec<&str> = ch.dirs.iter().map(|d| d.rel.as_str()).collect();
        match mode {
            Mode::HighPerformance => assert_eq!(rels, vec!["closed", "open"]),
            // `closed` cannot be listed by this user: not listed.
            Mode::Shared => assert_eq!(rels, vec!["open"]),
        }
        drop(cc);
        passes_stop.store(true, Ordering::SeqCst);
        let _ = passes.join();
        // A missing scope is NotIndexed in both modes (client falls back).
        assert!(matches!(
            c.search(&SearchQuery {
                scope: format!("{tmp_s}\\nope"),
                ..q.clone()
            }),
            Err(ClientError::Refused {
                code: ErrorCode::NotIndexed,
                ..
            })
        ));
        drop(c);
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }
}

/// FER-476 review H1: a server whose token is not restricted (the default
/// of `ServerState`; the service enables queries only after checking its
/// own token) answers search/subtree with `degraded`, and status says so —
/// even with an index served.
#[test]
fn queries_are_degraded_without_a_restricted_token() {
    use ferrum_indexer::engine::{IndexShared, LiveIndex, ServedVolume};
    use ferrum_indexer::index::VolumeIndex;
    use ferrum_indexer::query::{KindFilter, MatchKind, QueryCursor, SearchQuery};
    use std::sync::RwLock;
    let shared = IndexShared::new(Mode::HighPerformance);
    shared.serve_for_test(vec![ServedVolume {
        letter: b'C',
        index: Some(Arc::new(RwLock::new(LiveIndex {
            index: VolumeIndex::new(5 | (5 << 48)),
            excluded: Default::default(),
            scope: ferrum_indexer::scope::Scope::everything(),
            journal_id: Some(1),
            next_usn: 0,
            dirty: false,
            stats: Default::default(),
            failed_rounds: 0,
            security_changes: 0,
            changes: ferrum_indexer::changelog::ChangeLog::new(0),
            caught_pass: 0,
        }))),
        stale: false,
    }]);
    let ep = Endpoint::dev(&format!("t{}-degraded", std::process::id())).unwrap();
    let state = Arc::new(
        ServerState::new("t", false, Mode::HighPerformance, None).with_index(shared.clone()),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2, st2) = (ep.clone(), stop.clone(), state.clone());
    let thread = std::thread::spawn(move || server::run(&e2, st2, s2));
    let mut c = None;
    for _ in 0..100 {
        if let Ok(cl) = Client::connect(&ep, Duration::from_millis(100)) {
            c = Some(cl);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut c = c.expect("connect");
    assert!(c.status().unwrap().degraded);
    assert!(c.index_status().unwrap().degraded);
    let q = SearchQuery {
        pattern: "*".into(),
        match_kind: MatchKind::Glob,
        scope: String::new(),
        extensions: vec![],
        kinds: KindFilter::Both,
        limit: 10,
        cursor: QueryCursor::default(),
    };
    assert!(matches!(
        c.search(&q),
        Err(ClientError::Refused {
            code: ErrorCode::Degraded,
            ..
        })
    ));
    assert!(matches!(
        c.subtree(r"C:\", 10, &mut |_, _| true),
        Err(ClientError::Refused {
            code: ErrorCode::Degraded,
            ..
        })
    ));
    // FER-477: the v4 queries are off too.
    assert!(matches!(
        c.folder_sizes(r"C:\", &["x".to_string()]),
        Err(ClientError::Refused {
            code: ErrorCode::Degraded,
            ..
        })
    ));
    assert!(matches!(
        c.changes(r"C:\", None),
        Err(ClientError::Refused {
            code: ErrorCode::Degraded,
            ..
        })
    ));
    // The connection stays usable for status after a degraded answer.
    assert!(c.status().is_ok());
    drop(c);
    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
}

/// FER-480: the client verifies the server before its first byte. Against
/// the real console server: the right identity completes the hello, a
/// wrong PID or image is `Untrusted` and the server never sees a request.
#[test]
fn client_verifies_the_server_identity() {
    use ferrum_indexer::pipe::ExpectedServer;
    let r = start("verify", Mode::Shared);
    let me = ExpectedServer {
        pid: std::process::id(),
        image: std::env::current_exe().unwrap(),
        token_group: None,
    };
    let mut c = Client::connect_expect(&r.ep, Duration::from_secs(5), &me).expect("verified");
    c.ping(7).unwrap();
    drop(c);
    let wrong_pid = ExpectedServer {
        pid: std::process::id() ^ 0x10_0000,
        ..me.clone()
    };
    let other = std::env::temp_dir().join(format!("ferrum-it-verify-{}.exe", std::process::id()));
    let _other_guard = TempFileGuard(other.clone());
    std::fs::write(&other, b"x").unwrap();
    let wrong_image = ExpectedServer {
        image: other.clone(),
        ..me.clone()
    };
    for bad in [&wrong_pid, &wrong_image] {
        match Client::connect_expect(&r.ep, Duration::from_secs(5), bad) {
            // Wrong PID = another process holds the name (review F1);
            // wrong image = untrusted.
            Err(ClientError::Squatted { pid, why }) => {
                assert!(std::ptr::eq(bad, &wrong_pid), "{why}");
                assert_eq!(pid, Some(std::process::id()));
            }
            Err(ClientError::Untrusted(why)) => {
                assert!(std::ptr::eq(bad, &wrong_image), "{why}");
            }
            Err(e) => panic!("expected Squatted/Untrusted, got {e}"),
            Ok(_) => panic!("unverified server accepted"),
        }
    }
    // The server is unharmed and still serves verified clients.
    Client::connect_expect(&r.ep, Duration::from_secs(5), &me)
        .unwrap()
        .ping(8)
        .unwrap();
}
