//! `ferrum-scan-helper.exe` main logic (Windows, runs ELEVATED). FER-460.
//!
//! Lifetime, in order — any failure exits immediately:
//! 1. validate argv ([`crate::args`]) before touching any handle;
//! 2. start a watchdog that kills the process after [`GLOBAL_TIMEOUT`];
//! 3. connect to `\\.\pipe\ferrum-scan-<hex>` with SECURITY_IDENTIFICATION
//!    (the non-elevated server can identify us but never impersonate our
//!    elevated token);
//! 4. check the pipe SERVER process lives in this helper's own directory
//!    (the protected install dir) and runs as the SAME user account as the
//!    helper's token (over-the-shoulder UAC with another admin is refused);
//!    send the data-free `ClientHello` (Ferrum then checks OUR token), read
//!    Ferrum's hello and compare the nonce from argv in constant time — a
//!    squatting server gets nothing. A server that never answers ends the
//!    helper after [`HELLO_TIMEOUT`] (FER-463 I2);
//! 5. check the volume is a local fixed/removable NTFS volume;
//! 6. open `\\.\X:` READ-ONLY, read the MFT, stream entries, send `Done`;
//! 7. exit. A closed pipe (Ferrum cancelled) makes the next write fail,
//!    which ends the scan.
//!
//! The helper performs no writes to any file or volume, opens no network
//! connection and starts no process.

use crate::args::{self, HelperArgs};
use crate::mft::{self, MftError, ScanEvent, VolumeGeometry, VolumeReader};
use crate::protocol::{
    self, DoneInfo, MAX_BATCH_ENTRIES, Message, TARGET_BATCH_BYTES, VolumeInfo, WireEntry,
    error_code,
};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FlushFileBuffers,
    GetDriveTypeW, GetVolumeInformationW, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, OVERLAPPED};
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

/// Hard ceiling on the helper's whole life (a 4.5M-file C: takes seconds;
/// this only catches a hung pipe or a stalled disk).
pub const GLOBAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long the helper waits for Ferrum's pipe to accept it.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// FER-463 I2: how long the helper waits for Ferrum's `ServerHello` after
/// connecting. A server that stays silent no longer keeps the elevated
/// helper idle until [`GLOBAL_TIMEOUT`].
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

pub mod exit_code {
    pub const OK: i32 = 0;
    pub const BAD_ARGS: i32 = 2;
    pub const PIPE: i32 = 3;
    pub const SERVER_REJECTED: i32 = 4;
    pub const SCAN_FAILED: i32 = 5;
    pub const TIMEOUT: i32 = 70;
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Entry point used by `src/bin/ferrum-scan-helper.rs`.
pub fn run() -> i32 {
    let argv: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(v) => v,
        Err(_) => return exit_code::BAD_ARGS,
    };
    let args = match args::parse(&argv) {
        Ok(a) => a,
        Err(_) => return exit_code::BAD_ARGS,
    };
    std::thread::spawn(|| {
        std::thread::sleep(GLOBAL_TIMEOUT);
        std::process::exit(exit_code::TIMEOUT);
    });
    let mut pipe = match connect_pipe(&args) {
        Ok(p) => p,
        Err(_) => return exit_code::PIPE,
    };
    let hello_done = Arc::new(AtomicBool::new(false));
    spawn_deadline(HELLO_TIMEOUT, hello_done.clone(), || {
        std::process::exit(exit_code::TIMEOUT)
    });
    if protocol::write_message(&mut pipe, &Message::ClientHello).is_err() {
        return exit_code::PIPE;
    }
    match protocol::read_message(&mut pipe) {
        Ok(Message::ServerHello { nonce }) if protocol::nonce_eq(&nonce, &args.nonce) => {}
        _ => return exit_code::SERVER_REJECTED,
    }
    hello_done.store(true, Ordering::SeqCst);
    let code = match scan_volume(&args, &mut pipe) {
        Ok(()) => exit_code::OK,
        Err((code, msg)) => {
            let _ = protocol::write_message(&mut pipe, &Message::Error { code, message: msg });
            exit_code::SCAN_FAILED
        }
    };
    let _ = pipe.flush();
    code
}

/// Runs `on_expire` once `timeout` has passed, unless `done` was set by then
/// (FER-463 I2: the hello watchdog; `on_expire` exits the process).
fn spawn_deadline(
    timeout: Duration,
    done: Arc<AtomicBool>,
    on_expire: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        if !done.load(Ordering::SeqCst) {
            on_expire();
        }
    });
}

/// Connect as a client with identification-only impersonation level, then
/// check who the server is before reading anything from it.
fn connect_pipe(args: &HelperArgs) -> io::Result<File> {
    let path = wide(&args.pipe_path());
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let handle = loop {
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            break h;
        }
        let err = io::Error::last_os_error();
        if Instant::now() >= deadline {
            return Err(err);
        }
        match err.raw_os_error().map(|c| c as u32) {
            Some(ERROR_PIPE_BUSY) => unsafe {
                WaitNamedPipeW(path.as_ptr(), 1000);
            },
            Some(ERROR_FILE_NOT_FOUND) => std::thread::sleep(Duration::from_millis(100)),
            _ => return Err(err),
        }
    };
    // SAFETY: `handle` is a fresh, owned, valid pipe handle.
    let file = unsafe { File::from_raw_handle(handle as _) };
    verify_server(handle)?;
    Ok(file)
}

/// The server must be a process whose image lives in the helper's own
/// directory (the protected install dir, next to ferrum.exe). Together with
/// the nonce this keeps the elevated helper from talking to anyone else.
fn verify_server(pipe: HANDLE) -> io::Result<()> {
    let mut pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let server = image_path_of(pid)?;
    let own = std::env::current_exe()?;
    if !same_dir(&server, &own) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe server is not a Ferrum binary from the helper's directory",
        ));
    }
    // Same account only (review W1): the elevated helper can open the
    // non-elevated server's token; a different user SID means the UAC
    // prompt was answered with another account's credentials.
    let server_sid = crate::token::user_sid(crate::token::process_token(pid)?.0)?;
    let own_sid = crate::token::user_sid(crate::token::current_process_token()?.0)?;
    if server_sid != own_sid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe server runs as a different user account",
        ));
    }
    Ok(())
}

fn image_path_of(pid: u32) -> io::Result<PathBuf> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let _guard = OwnedHandle(h);
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
}

/// Case-insensitive parent-directory comparison (NTFS paths).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.parent(), b.parent()) {
        (Some(pa), Some(pb)) => {
            let norm = |p: &Path| p.to_string_lossy().to_lowercase();
            !pa.as_os_str().is_empty() && norm(pa) == norm(pb)
        }
        _ => false,
    }
}

/// Fixed or removable drive formatted NTFS. UNC, network, CD, RAM disks,
/// FAT/exFAT/ReFS are refused (Ferrum falls back to the regular scan).
fn check_local_ntfs(volume: char) -> Result<(), String> {
    let root = wide(&format!("{volume}:\\"));
    let ty = unsafe { GetDriveTypeW(root.as_ptr()) };
    if ty != DRIVE_FIXED && ty != DRIVE_REMOVABLE {
        return Err(format!("drive {volume}: is not a local disk (type {ty})"));
    }
    let mut fs = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return Err(format!(
            "volume information unavailable: {}",
            io::Error::last_os_error()
        ));
    }
    let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    let name = String::from_utf16_lossy(&fs[..end]);
    if name != "NTFS" {
        return Err(format!("file system is {name}, not NTFS"));
    }
    Ok(())
}

/// Raw read-only volume handle. Public for the FerrumIndexer service
/// (FER-475), which reads the same MFT and the volume's USN journal through
/// it; the open itself stays exactly the helper's (GENERIC_READ, full share).
pub struct Volume {
    handle: OwnedHandle,
}

impl Volume {
    pub fn open(volume: char) -> io::Result<Self> {
        let path = wide(&format!(r"\\.\{volume}:"));
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: OwnedHandle(h),
        })
    }

    /// The raw handle, for read-only FSCTLs (USN journal, file records).
    /// Valid while the `Volume` lives; never close it.
    pub fn raw_handle(&self) -> HANDLE {
        self.handle.0
    }

    pub fn geometry(&self) -> io::Result<VolumeGeometry> {
        let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle.0,
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                (&mut data as *mut NTFS_VOLUME_DATA_BUFFER).cast(),
                std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if data.MftStartLcn < 0 || data.MftValidDataLength < 0 || data.TotalClusters <= 0 {
            return Err(io::Error::other("negative MFT geometry"));
        }
        Ok(VolumeGeometry {
            bytes_per_sector: data.BytesPerSector,
            bytes_per_cluster: data.BytesPerCluster,
            bytes_per_record: data.BytesPerFileRecordSegment,
            mft_start_lcn: data.MftStartLcn as u64,
            mft_valid_data_length: data.MftValidDataLength as u64,
            total_clusters: data.TotalClusters as u64,
        })
    }
}

impl VolumeReader for Volume {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.Anonymous.Anonymous.Offset = pos as u32;
            ov.Anonymous.Anonymous.OffsetHigh = (pos >> 32) as u32;
            let want = (buf.len() - done).min(u32::MAX as usize & !511) as u32;
            let mut got = 0u32;
            let ok = unsafe {
                ReadFile(
                    self.handle.0,
                    buf[done..].as_mut_ptr(),
                    want,
                    &mut got,
                    &mut ov,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if got == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "volume read past end",
                ));
            }
            done += got as usize;
        }
        Ok(())
    }
}

/// Collects entries into bounded batch frames.
struct Batcher<'a, W: Write> {
    out: &'a mut W,
    entries: Vec<WireEntry>,
    bytes: usize,
    last_progress: Option<Instant>,
}

impl<W: Write> Batcher<'_, W> {
    fn flush_batch(&mut self) -> io::Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.entries);
        self.bytes = 0;
        protocol::write_message(self.out, &Message::Batch(batch))
    }

    fn on_event(&mut self, ev: ScanEvent) -> io::Result<()> {
        match ev {
            ScanEvent::Entry(e) => {
                self.bytes += protocol::entry_wire_len(&e);
                self.entries.push(e);
                if self.bytes >= TARGET_BATCH_BYTES
                    || self.entries.len() as u32 >= MAX_BATCH_ENTRIES
                {
                    self.flush_batch()?;
                }
            }
            ScanEvent::Progress { done, total } => {
                let due = self
                    .last_progress
                    .is_none_or(|t| t.elapsed() >= PROGRESS_EVERY)
                    || done == total;
                if due {
                    self.flush_batch()?;
                    self.last_progress = Some(Instant::now());
                    protocol::write_message(self.out, &Message::Progress { done, total })?;
                }
            }
        }
        Ok(())
    }
}

fn scan_volume(args: &HelperArgs, pipe: &mut File) -> Result<(), (u16, String)> {
    check_local_ntfs(args.volume).map_err(|m| (error_code::NOT_NTFS, m))?;
    let mut vol =
        Volume::open(args.volume).map_err(|e| (error_code::VOLUME_OPEN, e.to_string()))?;
    let geometry = vol
        .geometry()
        .map_err(|e| (error_code::NOT_NTFS, format!("NTFS volume data: {e}")))?;
    protocol::write_message(
        pipe,
        &Message::Volume(VolumeInfo {
            bytes_per_cluster: geometry.bytes_per_cluster,
            bytes_per_record: geometry.bytes_per_record,
            total_records: geometry.mft_valid_data_length / geometry.bytes_per_record.max(1) as u64,
        }),
    )
    .map_err(|e| (error_code::INTERNAL, e.to_string()))?;
    let mut batcher = Batcher {
        out: pipe,
        entries: Vec::new(),
        bytes: 0,
        last_progress: None,
    };
    let result = mft::scan(&mut vol, &geometry, &|| false, &mut |ev| {
        batcher.on_event(ev).map_err(MftError::Sink)
    });
    let summary = match result {
        Ok(s) => s,
        Err(MftError::Sink(e)) => return Err((error_code::INTERNAL, e.to_string())),
        Err(MftError::Layout(why)) => return Err((error_code::MFT_LAYOUT, why.to_string())),
        Err(e) => return Err((error_code::READ, e.to_string())),
    };
    batcher
        .flush_batch()
        .map_err(|e| (error_code::INTERNAL, e.to_string()))?;
    protocol::write_message(
        pipe,
        &Message::Done(DoneInfo {
            root_frn: summary.root_frn,
            records_scanned: summary.records_scanned,
            corrupt_records: summary.corrupt_records,
        }),
    )
    .map_err(|e| (error_code::INTERNAL, e.to_string()))?;
    unsafe { FlushFileBuffers(pipe_handle(pipe)) };
    Ok(())
}

/// In-process MFT scan without the pipe — for the benchmark harness
/// (`examples/bench_fast_scan.rs`, run from an ELEVATED terminal by the
/// owner). Same volume checks and the same read-only access as the helper.
pub fn scan_local(volume: char) -> Result<(Vec<WireEntry>, mft::ScanSummary), String> {
    check_local_ntfs(volume)?;
    let mut vol = Volume::open(volume).map_err(|e| format!(r"open \\.\{volume}: {e}"))?;
    let geometry = vol.geometry().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let summary = mft::scan(&mut vol, &geometry, &|| false, &mut |ev| {
        if let ScanEvent::Entry(e) = ev {
            out.push(e);
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    Ok((out, summary))
}

fn pipe_handle(f: &File) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    f.as_raw_handle() as HANDLE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FER-463 I2: the hello watchdog fires only when the hello never came.
    #[test]
    fn hello_deadline_fires_only_without_a_hello() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let silent = Arc::new(AtomicBool::new(false));
        let tx2 = tx.clone();
        spawn_deadline(Duration::from_millis(30), silent, move || {
            let _ = tx2.send("expired");
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("expired"));
        let answered = Arc::new(AtomicBool::new(false));
        spawn_deadline(Duration::from_millis(30), answered.clone(), move || {
            let _ = tx.send("expired");
        });
        answered.store(true, Ordering::SeqCst);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(HELLO_TIMEOUT < GLOBAL_TIMEOUT && HELLO_TIMEOUT >= Duration::from_secs(5));
    }

    #[test]
    fn same_dir_is_case_insensitive_and_strict() {
        let a = Path::new(r"C:\Program Files\Ferrum\ferrum.exe");
        let b = Path::new(r"c:\program files\ferrum\ferrum-scan-helper.exe");
        let c = Path::new(r"C:\Users\x\Downloads\ferrum.exe");
        assert!(same_dir(a, b));
        assert!(!same_dir(a, c));
        assert!(!same_dir(Path::new("ferrum.exe"), Path::new("helper.exe")));
    }

    #[test]
    fn batcher_respects_caps() {
        let mut sink = Vec::new();
        let mut b = Batcher {
            out: &mut sink,
            entries: Vec::new(),
            bytes: 0,
            last_progress: None,
        };
        for i in 0..(MAX_BATCH_ENTRIES as u64 + 10) {
            b.on_event(ScanEvent::Entry(WireEntry {
                frn: 100 + i,
                parent: 5,
                kind: protocol::EntryKind::File,
                logical: 1,
                allocated: Some(1),
                modified: 0,
                name: "n".into(),
            }))
            .unwrap();
        }
        b.flush_batch().unwrap();
        let mut r = &sink[..];
        let mut total = 0;
        while let Ok(Message::Batch(v)) = protocol::read_message(&mut r) {
            assert!(v.len() as u32 <= MAX_BATCH_ENTRIES);
            total += v.len();
        }
        assert_eq!(total, MAX_BATCH_ENTRIES as usize + 10);
    }
}
