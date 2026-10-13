//! Starting an agent process safely and reading its output.
//!
//! - `std::process::Command` with an absolute `.exe` path: `CreateProcessW` directly, no shell,
//!   no `cmd.exe` (callers never pass `.cmd`/`.bat`, see `detect`).
//! - `env_clear()` + the caller's allowlist.
//! - Started SUSPENDED, put into a job object with `KILL_ON_JOB_CLOSE`, then resumed, so every
//!   process the agent starts (node, helpers, sandboxed commands) is in the job before it can
//!   run. Cancel, timeout, output limit and the end of the run all terminate the whole job.
//! - STDIN carries the prompt and is closed afterwards; STDOUT is read line by line up to
//!   `max_output` bytes; STDERR is drained and only its last few KB are kept for an error line.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Everything needed to start one agent process.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub max_output: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    Exited(Option<i32>),
    Cancelled,
    TimedOut,
    OutputLimit,
}

#[derive(Debug)]
pub struct RunResult {
    pub end: RunEnd,
    /// Last bytes of STDERR (lossy UTF-8), for one error line.
    pub stderr_tail: String,
    pub output_bytes: usize,
}

const STDERR_KEEP: usize = 4096;
const POLL: Duration = Duration::from_millis(50);
/// After the main process exited, how long its leftover children may keep STDOUT open.
const EXIT_GRACE: Duration = Duration::from_secs(2);

enum Msg {
    Line(String),
    Limit,
    Eof,
}

/// Split STDOUT into lines; stop (and report `Limit`) once `max` bytes were read.
fn read_lines(mut out: impl Read, max: usize, tx: mpsc::Sender<Msg>, total: &mut usize) {
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if *total + n > max {
            let _ = tx.send(Msg::Limit);
            return;
        }
        *total += n;
        pending.extend_from_slice(&buf[..n]);
        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line).trim_end().to_owned();
            if tx.send(Msg::Line(text)).is_err() {
                return;
            }
        }
    }
    if !pending.is_empty() {
        let _ = tx.send(Msg::Line(String::from_utf8_lossy(&pending).into_owned()));
    }
    let _ = tx.send(Msg::Eof);
}

/// Run the agent. `cancelled` is polled; `on_line` receives every complete STDOUT line.
#[cfg(windows)]
pub fn run(
    spec: &RunSpec,
    cancelled: &dyn Fn() -> bool,
    on_line: &mut dyn FnMut(&str),
) -> io::Result<RunResult> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = cmd.spawn()?;
    let job = match job::Job::new().and_then(|job| {
        job.assign(&child)?;
        Ok(job)
    }) {
        Ok(job) => job,
        Err(e) => {
            // Fail closed: a process outside a job is never resumed.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    if let Err(e) = job::resume_process(child.id()) {
        job.terminate();
        let _ = child.wait();
        return Err(e);
    }

    let mut stdin = child.stdin.take().expect("piped stdin");
    let prompt = spec.stdin.clone();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&prompt);
        // Dropping closes the pipe: the agent sees the end of the prompt.
    });
    let stdout = child.stdout.take().expect("piped stdout");
    let max = spec.max_output;
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut total = 0usize;
        read_lines(stdout, max, tx, &mut total);
        total
    });
    let mut stderr = child.stderr.take().expect("piped stderr");
    let err_reader = std::thread::spawn(move || {
        let mut keep: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            keep.extend_from_slice(&buf[..n]);
            if keep.len() > STDERR_KEEP {
                let cut = keep.len() - STDERR_KEEP;
                keep.drain(..cut);
            }
        }
        String::from_utf8_lossy(&keep).into_owned()
    });

    let deadline = Instant::now() + spec.timeout;
    let mut exited_at: Option<Instant> = None;
    let mut end: Option<RunEnd> = None;
    loop {
        if cancelled() {
            end = Some(RunEnd::Cancelled);
            break;
        }
        if Instant::now() >= deadline {
            end = Some(RunEnd::TimedOut);
            break;
        }
        match rx.recv_timeout(POLL) {
            Ok(Msg::Line(line)) => on_line(&line),
            Ok(Msg::Limit) => {
                end = Some(RunEnd::OutputLimit);
                break;
            }
            Ok(Msg::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // The agent exited but a leftover child still holds STDOUT open: stop waiting soon.
        if exited_at.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            exited_at = Some(Instant::now());
        }
        if exited_at.is_some_and(|t| t.elapsed() >= EXIT_GRACE) {
            break;
        }
    }
    // Always: no process of this run survives it.
    job.terminate();
    let status = child.wait().ok();
    // Lines already read before the end still count (the answer may be complete).
    if end.is_none() {
        while let Ok(Msg::Line(line)) = rx.try_recv() {
            on_line(&line);
        }
    }
    let output_bytes = reader.join().unwrap_or(0);
    let _ = writer.join();
    let stderr_tail = err_reader.join().unwrap_or_default();
    let end = end.unwrap_or(RunEnd::Exited(status.and_then(|s| s.code())));
    Ok(RunResult {
        end,
        stderr_tail,
        output_bytes,
    })
}

#[cfg(not(windows))]
pub fn run(
    _spec: &RunSpec,
    _cancelled: &dyn Fn() -> bool,
    _on_line: &mut dyn FnMut(&str),
) -> io::Result<RunResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "agent CLI is Windows-only",
    ))
}

#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    /// Kill-on-close job. Dropping it (or `terminate`) ends every process in it.
    pub struct Job(HANDLE);

    // The handle is only used through the kernel's thread-safe job APIs.
    unsafe impl Send for Job {}

    impl Job {
        pub fn new() -> io::Result<Self> {
            // SAFETY: plain Win32 calls with valid (null = default) arguments.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            // SAFETY: `info` is a valid, correctly sized struct for this class.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        pub fn assign(&self, child: &Child) -> io::Result<()> {
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn terminate(&self) {
            // SAFETY: valid job handle; failure (already empty) is harmless.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle; closing it kills what is left (KILL_ON_JOB_CLOSE).
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Resume the (single, suspended) main thread of a process started with CREATE_SUSPENDED.
    pub fn resume_process(pid: u32) -> io::Result<()> {
        // SAFETY: snapshot handle checked and closed; THREADENTRY32 sized as required.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut resumed = 0;
            let mut more = Thread32First(snap, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        if ResumeThread(thread) != u32::MAX {
                            resumed += 1;
                        }
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snap, &mut entry) != 0;
            }
            CloseHandle(snap);
            if resumed == 0 {
                return Err(io::Error::other("agent process could not be resumed"));
            }
        }
        Ok(())
    }
}
