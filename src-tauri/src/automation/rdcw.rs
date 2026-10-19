//! The Windows change-notification backend (Automation design §9.2, §9.8; FER-565 / P2.1b):
//! `ReadDirectoryChangesW`, recursive, one worker thread per watched root, overlapped I/O.
//!
//! Why not `notify`: on Windows `notify` 7 ignores the number of bytes a completion returns, so a
//! buffer overflow (0 bytes, or `ERROR_NOTIFY_ENUM_DIR`) reaches nobody, and a watch whose
//! re-arm failed (root deleted) dies without a word. This backend reports both:
//!
//! - **Overflow**: the kernel lost events. The host marks the automation "Needs recheck".
//! - **Failed**: the watch is gone (root deleted or renamed away, access lost, any other error).
//!   The host looks at the root and puts a new watch in place.
//!
//! No timer, no polling: the worker sleeps in `WaitForMultipleObjects` on the I/O event and a stop
//! event. It never lists a folder; an event is a path string that the sink turns into a count.
//!
//! - The directory handle has `FILE_LIST_DIRECTORY` only and the full share mode (read, write,
//!   delete): the user can still use, rename and delete the folder while it is watched.
//! - [`parse`] is a pure function over bytes with no `unsafe`: it checks every offset and length
//!   before it reads and answers "corrupt" instead of reading outside the buffer.
//! - The buffer and the `OVERLAPPED` are heap boxes owned by one struct whose `Drop` cancels the
//!   pending read and WAITS for the completion before anything is freed.
//!
//! Tauri-free (the runner links this file).

use std::fmt;

// ── Parsing FILE_NOTIFY_INFORMATION ──

/// `FILE_NOTIFY_INFORMATION` header: `NextEntryOffset`, `Action`, `FileNameLength` (3 x u32).
const HEADER: usize = 12;
/// The actions a record may carry (FILE_ACTION_ADDED .. FILE_ACTION_RENAMED_NEW_NAME).
const ACTIONS: std::ops::RangeInclusive<u32> = 1..=5;

/// What one completion of `ReadDirectoryChangesW` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// 0 bytes: the kernel's buffer overflowed and the changes are lost.
    Overflow,
    /// The bytes do not parse (an offset or length is not believable). Nothing can be trusted.
    Corrupt,
    /// The paths that changed, each `root` + `\` + the name the record carries (UNTRUSTED text).
    Changes(Vec<String>),
}

/// Why a record was refused.
#[derive(Debug)]
struct Bad;

impl fmt::Display for Bad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("malformed FILE_NOTIFY_INFORMATION")
    }
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32, Bad> {
    let end = at.checked_add(4).ok_or(Bad)?;
    let bytes: [u8; 4] = buf.get(at..end).ok_or(Bad)?.try_into().map_err(|_| Bad)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Parse the `buf` a completion filled (`buf` is exactly the bytes received).
///
/// Every record must start on a DWORD boundary, fit its 12-byte header and its name inside `buf`,
/// carry an even name length and a known action, and `NextEntryOffset` must move forward past the
/// record (so a hostile chain cannot loop or overlap). Anything else is [`Parsed::Corrupt`].
pub fn parse(buf: &[u8], root: &str) -> Parsed {
    if buf.is_empty() {
        return Parsed::Overflow;
    }
    match parse_records(buf, root) {
        Ok(paths) => Parsed::Changes(paths),
        Err(Bad) => Parsed::Corrupt,
    }
}

fn parse_records(buf: &[u8], root: &str) -> Result<Vec<String>, Bad> {
    let base = root.trim_end_matches('\\');
    let mut paths = Vec::new();
    let mut at = 0usize;
    loop {
        if !at.is_multiple_of(4) {
            return Err(Bad);
        }
        let next = u32_at(buf, at)? as usize;
        let action = u32_at(buf, at + 4)?;
        let name_len = u32_at(buf, at + 8)? as usize;
        if !ACTIONS.contains(&action) || !name_len.is_multiple_of(2) {
            return Err(Bad);
        }
        let name_start = at + HEADER;
        let name_end = name_start.checked_add(name_len).ok_or(Bad)?;
        let name = buf.get(name_start..name_end).ok_or(Bad)?;
        let (pairs, _) = name.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
        paths.push(format!("{base}\\{}", String::from_utf16_lossy(&units)));
        if next == 0 {
            return Ok(paths);
        }
        // The next record starts after this one (its name included) and inside the buffer.
        if next < HEADER + name_len || !next.is_multiple_of(4) {
            return Err(Bad);
        }
        at = at.checked_add(next).ok_or(Bad)?;
        if at >= buf.len() {
            return Err(Bad);
        }
    }
}

// ── The Windows backend ──

#[cfg(windows)]
pub use imp::{RdcwFactory, RdcwHandle};

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_INCOMPLETE, ERROR_NOTIFY_ENUM_DIR, FALSE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
        FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFinalPathNameByHandleW,
        OPEN_EXISTING, ReadDirectoryChangesW,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    };

    use super::{Parsed, parse};
    use crate::automation::watcher::{RawEvent, Sink, WatchError, WatchFactory, WatchHandle};

    /// The kernel buffer of a production watch. Local volumes only (a share is Unsupported), so
    /// the 64 KiB ceiling of network paths does not apply.
    pub const BUFFER_BYTES: usize = 64 * 1024;

    /// How long `Drop` waits for a cancelled read to complete before it gives up and LEAKS the
    /// buffer (never frees memory the kernel may still write to).
    const CANCEL_WAIT_MS: u32 = 10_000;

    const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_CREATION;

    /// The production watcher: `ReadDirectoryChangesW`, recursive.
    pub struct RdcwFactory;

    impl WatchFactory for RdcwFactory {
        fn watch(&self, root: &str, sink: Sink) -> Result<Box<dyn WatchHandle>, WatchError> {
            RdcwHandle::start(root, sink, BUFFER_BYTES).map(|h| Box::new(h) as Box<dyn WatchHandle>)
        }
    }

    /// A manual-reset event that closes itself.
    struct Event(HANDLE);

    // SAFETY: an event handle is a process-wide kernel object; setting, waiting on and closing it
    // are thread-safe operations.
    unsafe impl Send for Event {}
    unsafe impl Sync for Event {}

    impl Event {
        fn new() -> Option<Event> {
            // SAFETY: no pointers are passed (default security, unnamed); the result is checked.
            let h = unsafe { CreateEventW(null(), 1, 0, null()) };
            if h.is_null() { None } else { Some(Event(h)) }
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            // SAFETY: the handle was created by `CreateEventW`, is owned here and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// One armed directory watch: the handle, the I/O event, the `OVERLAPPED` and the buffer the
    /// kernel writes into. The last two are raw pointers from `Box::into_raw`: while a read is
    /// pending the kernel owns that memory, and no Rust reference or `Box` to it exists. They are
    /// freed in `Drop`, after the completion.
    struct Watch {
        dir: HANDLE,
        io: Event,
        ov: *mut OVERLAPPED,
        /// `u32` elements: the buffer is DWORD aligned, as `FILE_NOTIFY_INFORMATION` requires.
        buf: *mut u32,
        words: usize,
        /// A read is in flight: `ov` and `buf` belong to the kernel until it completes.
        pending: bool,
        /// Where the handle pointed when the watch began (lower case, no `\\?\`), to see a root
        /// that was renamed away: the handle follows the folder, the path does not.
        base_path: Option<String>,
    }

    // SAFETY: the raw handle and the allocations are owned by this struct alone and are used from
    // one thread at a time (it is created on the caller's thread, then moved to the worker).
    unsafe impl Send for Watch {}

    /// What a finished read told the worker.
    enum Done {
        /// The event was signalled early; the read is still in flight.
        Still,
        /// `n` bytes of changes are in the buffer.
        Bytes(usize),
        /// The kernel lost events.
        Overflow,
        /// The watch is gone.
        Died,
    }

    fn wide(path: &str) -> Vec<u16> {
        // `\\?\` turns off Win32 path normalization for a drive path (the root is canonical).
        let full = if path.starts_with(r"\\") {
            path.to_string()
        } else {
            format!(r"\\?\{path}")
        };
        std::ffi::OsStr::new(&full)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// The path the handle points at now, lower case without the `\\?\` prefix.
    fn final_path(dir: HANDLE) -> Option<String> {
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: `dir` is an open handle; `buf` is a live writable buffer of `len` units.
            let n = unsafe { GetFinalPathNameByHandleW(dir, buf.as_mut_ptr(), buf.len() as u32, 0) }
                as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                let text = String::from_utf16_lossy(&buf[..n]).to_lowercase();
                return Some(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string());
            }
            // Too small: `n` is the size needed (with the NUL).
            buf.resize(n + 1, 0);
        }
    }

    impl Watch {
        fn open(root: &str, buf_bytes: usize) -> Option<Watch> {
            let path = wide(root);
            // SAFETY: `path` is a NUL-terminated UTF-16 string that outlives the call; no security
            // attributes or template are passed. `FILE_LIST_DIRECTORY` is the one right asked for,
            // and the full share mode keeps the folder usable (rename, delete, write) by others.
            let dir = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    null_mut(),
                )
            };
            if dir == INVALID_HANDLE_VALUE {
                return None;
            }
            let Some(io) = Event::new() else {
                // SAFETY: `dir` was just opened and is closed once.
                unsafe { CloseHandle(dir) };
                return None;
            };
            let words = buf_bytes.div_ceil(4).max(64);
            let ov = Box::into_raw(Box::new(unsafe_zero_overlapped()));
            let buf = Box::into_raw(vec![0u32; words].into_boxed_slice()).cast::<u32>();
            Some(Watch {
                dir,
                io,
                ov,
                buf,
                words,
                pending: false,
                base_path: final_path(dir),
            })
        }

        /// Start a read. `false` = the watch cannot go on.
        fn arm(&mut self) -> bool {
            if self.pending {
                return true;
            }
            // SAFETY: `ov` is the live allocation made in `open`; no read is in flight (`pending`
            // is false), so the kernel is not using it.
            unsafe {
                self.ov.write(unsafe_zero_overlapped());
                (*self.ov).hEvent = self.io.0;
            }
            let bytes = (self.words * 4) as u32;
            // SAFETY: `dir` is an open overlapped directory handle. `buf` is a live, DWORD-aligned
            // allocation of `bytes` bytes and `ov` a live OVERLAPPED; both are owned by `self` and
            // freed only in `Drop` after the completion, so the kernel never writes into freed
            // memory. `bytes_returned` may be null for overlapped use.
            let ok = unsafe {
                ReadDirectoryChangesW(
                    self.dir,
                    self.buf.cast::<c_void>(),
                    bytes,
                    1, // bWatchSubtree
                    FILTER,
                    null_mut(),
                    self.ov,
                    None,
                )
            };
            if ok == 0 {
                return false;
            }
            self.pending = true;
            true
        }

        /// The I/O event was signalled: what came back?
        fn finish(&mut self) -> Done {
            let mut n: u32 = 0;
            // SAFETY: `dir` and `ov` are the pair the read was started with; `n` is a live u32.
            // `bWait` is FALSE: the event said the read completed, this only collects the result.
            let mut ok = unsafe { GetOverlappedResult(self.dir, self.ov, &mut n, FALSE) };
            // SAFETY: reads the calling thread's last error, set by the call above.
            let mut err = if ok == 0 {
                unsafe { GetLastError() }
            } else {
                0
            };
            if ok == 0 && err == ERROR_IO_INCOMPLETE {
                // Signalled but not complete: put the event down so the wait does not spin, and
                // look once more (a completion in between would have signalled it again).
                // SAFETY: `io` is a live event owned by `self`.
                unsafe { ResetEvent(self.io.0) };
                // SAFETY: as above.
                ok = unsafe { GetOverlappedResult(self.dir, self.ov, &mut n, FALSE) };
                // SAFETY: as above.
                err = if ok == 0 {
                    unsafe { GetLastError() }
                } else {
                    0
                };
                if ok == 0 && err == ERROR_IO_INCOMPLETE {
                    return Done::Still;
                }
            }
            self.pending = false;
            if ok == 0 {
                return if err == ERROR_NOTIFY_ENUM_DIR {
                    Done::Overflow
                } else {
                    // ERROR_ACCESS_DENIED (root deleted), ERROR_OPERATION_ABORTED, a lost volume...
                    Done::Died
                };
            }
            // The folder was renamed or moved away from the path that was approved: the handle
            // follows it, so what it reports is not about the watched path any more.
            if let (Some(base), Some(now)) = (&self.base_path, final_path(self.dir))
                && *base != now
            {
                return Done::Died;
            }
            if n == 0 {
                Done::Overflow
            } else {
                Done::Bytes((n as usize).min(self.words * 4))
            }
        }

        /// The first `n` bytes of the buffer, copied out so the next read can start at once.
        fn take(&self, n: usize) -> Vec<u8> {
            debug_assert!(!self.pending);
            // SAFETY: `buf` holds `words * 4` initialised bytes and `n` is at most that (clamped in
            // `finish`); no read is in flight (`pending` is false), so the kernel is not writing.
            let bytes =
                unsafe { std::slice::from_raw_parts(self.buf.cast::<u8>(), self.words * 4) };
            bytes[..n].to_vec()
        }
    }

    fn unsafe_zero_overlapped() -> OVERLAPPED {
        // SAFETY: `OVERLAPPED` is plain data; all-zero is its documented initial state.
        unsafe { std::mem::zeroed() }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            let mut leak = false;
            if self.pending {
                // SAFETY: `dir`/`ov` are the pair the pending read uses. Cancelling a read that
                // already completed returns ERROR_NOT_FOUND, which is harmless.
                unsafe { CancelIoEx(self.dir, self.ov) };
                // The completion (cancelled or not) signals the I/O event. Only after it is the
                // buffer free to go; if it never comes, the memory is leaked on purpose.
                // SAFETY: waits on an event handle owned by `self.io`.
                let waited = unsafe { WaitForSingleObject(self.io.0, CANCEL_WAIT_MS) };
                leak = waited != WAIT_OBJECT_0;
            }
            // SAFETY: `dir` is owned here and closed once; the I/O event closes in its own Drop.
            // Closing it also makes the kernel drop a request that is still pending.
            unsafe { CloseHandle(self.dir) };
            if !leak {
                // SAFETY: both pointers come from `Box::into_raw` in `open` with these exact
                // types and lengths, are freed only here, and no read is pending any more.
                unsafe {
                    drop(Box::from_raw(self.ov));
                    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        self.buf, self.words,
                    )));
                }
            }
        }
    }

    /// Test seam: called after every successful wake-up of the worker; `true` makes the worker
    /// behave as if the wait had failed (it may also panic to test the unwind path).
    #[cfg(test)]
    pub(crate) type Fault = Arc<dyn Fn() -> bool + Send + Sync>;

    /// What the worker is handed in place of the seam: the hook in tests, nothing in production
    /// (the production worker has no fault code at all).
    #[cfg(test)]
    type FaultArg = Option<Fault>;
    #[cfg(not(test))]
    type FaultArg = ();

    /// A running recursive watch of one root. Dropping it stops the worker, cancels the read,
    /// waits for the completion and frees every handle.
    pub struct RdcwHandle {
        stop: Arc<Event>,
        dead: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl WatchHandle for RdcwHandle {
        fn alive(&self) -> bool {
            !self.dead.load(Ordering::SeqCst)
        }
    }

    impl RdcwHandle {
        /// Open `root` and start the first read before returning, so a root that cannot be
        /// watched is an error here and not a silent worker. The read is armed ON the worker
        /// thread (a read belongs to the thread that issued it when the handle has no completion
        /// port, and is cancelled when that thread ends), and the result is awaited here.
        /// `buf_bytes` is the kernel buffer (tests make it small to provoke an overflow).
        pub(crate) fn start(root: &str, sink: Sink, buf_bytes: usize) -> Result<Self, WatchError> {
            #[cfg(test)]
            let fault: FaultArg = None;
            #[cfg(not(test))]
            let fault: FaultArg = ();
            Self::start_inner(root, sink, buf_bytes, fault)
        }

        /// `start` with a fault hook (tests only).
        #[cfg(test)]
        pub(crate) fn start_with(
            root: &str,
            sink: Sink,
            buf_bytes: usize,
            fault: Option<Fault>,
        ) -> Result<Self, WatchError> {
            Self::start_inner(root, sink, buf_bytes, fault)
        }

        fn start_inner(
            root: &str,
            sink: Sink,
            buf_bytes: usize,
            fault: FaultArg,
        ) -> Result<Self, WatchError> {
            let watch = Watch::open(root, buf_bytes).ok_or(WatchError::Failed)?;
            let stop = Arc::new(Event::new().ok_or(WatchError::Failed)?);
            let dead = Arc::new(AtomicBool::new(false));
            let root = root.to_string();
            let (stop2, dead2) = (stop.clone(), dead.clone());
            let (armed_tx, armed_rx) = std::sync::mpsc::channel::<bool>();
            let thread = std::thread::Builder::new()
                .name("ferrum-rdcw".into())
                // The worker only waits and parses: a small stack is plenty.
                .stack_size(256 * 1024)
                .spawn(move || work(watch, &root, &sink, &stop2, &dead2, &armed_tx, fault))
                // The closure (and with it the Watch) is dropped on failure: Drop cancels the read.
                .map_err(|_| WatchError::Failed)?;
            let handle = RdcwHandle {
                stop,
                dead,
                thread: Some(thread),
            };
            match armed_rx.recv() {
                Ok(true) => Ok(handle),
                // Dropping the handle ends the worker (it has already left).
                _ => Err(WatchError::Failed),
            }
        }
    }

    impl Drop for RdcwHandle {
        fn drop(&mut self) {
            // SAFETY: `stop` is a live event owned by the Arc.
            unsafe { SetEvent(self.stop.0) };
            if let Some(thread) = self.thread.take() {
                // The worker may itself be the last owner (a sink that drops the host): joining
                // itself would deadlock, so it is left to finish on its own.
                if thread.thread().id() != std::thread::current().id() {
                    let _ = thread.join();
                }
            }
        }
    }

    fn deliver(sink: &Sink, event: RawEvent) {
        // A panic must not unwind into the worker (and leave a read pending).
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(event)));
    }

    /// Says the watch is lost on every way out of the worker except a real stop: an error, a
    /// failed wait or a panic. The flag goes first, so whoever sees `Failed` sees `!alive()`.
    struct Exit<'a> {
        sink: &'a Sink,
        dead: &'a AtomicBool,
        stopped: bool,
    }

    impl Drop for Exit<'_> {
        fn drop(&mut self) {
            if !self.stopped {
                self.dead.store(true, Ordering::SeqCst);
                deliver(self.sink, RawEvent::Failed);
            }
        }
    }

    /// The worker: wait for stop or for the read; report; read again. Leaves (and thereby closes
    /// everything) on stop or when the watch is gone.
    fn work(
        mut watch: Watch,
        root: &str,
        sink: &Sink,
        stop: &Event,
        dead: &AtomicBool,
        armed: &std::sync::mpsc::Sender<bool>,
        fault: FaultArg,
    ) {
        let first = watch.arm();
        let _ = armed.send(first);
        if !first {
            return;
        }
        let mut exit = Exit {
            sink,
            dead,
            stopped: false,
        };
        // `stop` comes first: when both are signalled, the lowest index wins, so a flood of
        // changes can never hide the stop.
        let handles = [stop.0, watch.io.0];
        loop {
            // SAFETY: `handles` holds two live event handles that outlive the call.
            let signalled = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, INFINITE) };
            if signalled == WAIT_OBJECT_0 {
                // The real stop. Dropping `watch` cancels the read and waits for it.
                exit.stopped = true;
                return;
            }
            let failed_wait = signalled != WAIT_OBJECT_0 + 1;
            #[cfg(test)]
            let injected = fault.as_ref().is_some_and(|f| f());
            #[cfg(not(test))]
            let injected = {
                let () = fault;
                false
            };
            if failed_wait || injected {
                return; // `exit` reports the loss.
            }
            match watch.finish() {
                Done::Still => {}
                Done::Overflow => {
                    if !watch.arm() {
                        return;
                    }
                    deliver(sink, RawEvent::Overflow);
                }
                Done::Bytes(n) => {
                    let bytes = watch.take(n);
                    // Re-arm before the (possibly slow) sink: events that happen meanwhile wait in
                    // the kernel's buffer.
                    let armed_again = watch.arm();
                    // The batch in hand counts either way.
                    report(sink, &bytes, root);
                    if !armed_again {
                        return;
                    }
                }
                Done::Died => return,
            }
        }
    }

    fn report(sink: &Sink, bytes: &[u8], root: &str) {
        match parse(bytes, root) {
            Parsed::Changes(paths) => deliver(sink, RawEvent::Change(paths)),
            // Corrupt data cannot be trusted any more than lost events can.
            Parsed::Overflow | Parsed::Corrupt => deliver(sink, RawEvent::Overflow),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record: next offset, action, name (UTF-16LE), padded to a DWORD.
    fn record(next: u32, action: u32, name: &str) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&action.to_le_bytes());
        out.extend_from_slice(&(units.len() as u32).to_le_bytes());
        out.extend_from_slice(&units);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out
    }

    fn chain(names: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let probe = record(0, 1, n);
            let next = if i + 1 == names.len() {
                0
            } else {
                probe.len() as u32
            };
            out.extend(record(next, 1, n));
        }
        out
    }

    #[test]
    fn zero_bytes_is_an_overflow() {
        assert_eq!(parse(&[], r"C:\R"), Parsed::Overflow);
    }

    #[test]
    fn a_single_entry_becomes_a_path_under_the_root() {
        let buf = chain(&["a\\b.txt"]);
        assert_eq!(
            parse(&buf, r"C:\R"),
            Parsed::Changes(vec![r"C:\R\a\b.txt".to_string()])
        );
        // A trailing separator on the root does not double up.
        assert_eq!(
            parse(&buf, "C:\\R\\"),
            Parsed::Changes(vec![r"C:\R\a\b.txt".to_string()])
        );
    }

    #[test]
    fn several_entries_are_all_read_in_order() {
        let buf = chain(&["one", "two.txt", "x", "ünï"]);
        assert_eq!(
            parse(&buf, r"C:\R"),
            Parsed::Changes(vec![
                r"C:\R\one".to_string(),
                r"C:\R\two.txt".to_string(),
                r"C:\R\x".to_string(),
                "C:\\R\\ünï".to_string(),
            ])
        );
    }

    #[test]
    fn a_next_offset_past_the_buffer_is_corrupt() {
        let mut buf = record(4096, 1, "a");
        assert_eq!(parse(&buf, r"C:\R"), Parsed::Corrupt);
        // Exactly at the end is also past the last record.
        let len = buf.len() as u32;
        buf[0..4].copy_from_slice(&len.to_le_bytes());
        assert_eq!(parse(&buf, r"C:\R"), Parsed::Corrupt);
    }

    #[test]
    fn a_next_offset_that_does_not_advance_or_is_misaligned_is_corrupt() {
        // Misaligned, then overlapping its own name, then not moving at all.
        let mut overlap = chain(&["aaaa", "bbbb"]);
        overlap[0..4].copy_from_slice(&13u32.to_le_bytes());
        assert_eq!(parse(&overlap, r"C:\R"), Parsed::Corrupt);
        let mut small = chain(&["aaaa", "bbbb"]);
        small[0..4].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(parse(&small, r"C:\R"), Parsed::Corrupt);
        let mut stuck = chain(&["aaaa", "bbbb"]);
        stuck[0..4].copy_from_slice(&12u32.to_le_bytes());
        assert_eq!(parse(&stuck, r"C:\R"), Parsed::Corrupt);
    }

    #[test]
    fn an_odd_or_oversized_name_length_is_corrupt() {
        let mut odd = record(0, 1, "abc");
        odd[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(parse(&odd, r"C:\R"), Parsed::Corrupt);
        let mut big = record(0, 1, "abc");
        big[8..12].copy_from_slice(&0x1000u32.to_le_bytes());
        assert_eq!(parse(&big, r"C:\R"), Parsed::Corrupt);
        let mut huge = record(0, 1, "abc");
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&huge, r"C:\R"), Parsed::Corrupt);
    }

    #[test]
    fn a_truncated_header_or_an_unknown_action_is_corrupt() {
        assert_eq!(parse(&[1, 0, 0], r"C:\R"), Parsed::Corrupt);
        assert_eq!(parse(&[0u8; 8], r"C:\R"), Parsed::Corrupt);
        assert_eq!(parse(&[0u8; 12], r"C:\R"), Parsed::Corrupt, "action 0");
        assert_eq!(parse(&record(0, 9, "a"), r"C:\R"), Parsed::Corrupt);
    }

    #[test]
    fn no_slice_of_a_valid_buffer_panics_or_reads_outside_it() {
        // Every prefix and every single-byte corruption of a good buffer answers without a panic.
        let good = chain(&["alpha\\beta", "gamma", "delta.txt"]);
        for cut in 0..=good.len() {
            let _ = parse(&good[..cut], r"C:\R");
        }
        for at in 0..good.len() {
            for v in [0u8, 1, 3, 0x7f, 0xff] {
                let mut bad = good.clone();
                bad[at] = v;
                let _ = parse(&bad, r"C:\R");
            }
        }
    }

    #[test]
    fn a_name_that_is_not_valid_utf16_is_replaced_not_rejected() {
        // A lone surrogate: the path text carries U+FFFD and the filter decides what it means.
        let mut buf = vec![0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0x00, 0xD8, 0, 0];
        buf.truncate(16);
        match parse(&buf, r"C:\R") {
            Parsed::Changes(p) => assert_eq!(p, vec!["C:\\R\\\u{FFFD}".to_string()]),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[cfg(all(test, windows))]
mod real_tests {
    use super::imp::RdcwHandle;
    use crate::automation::watcher::{RawEvent, Sink};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(30);

    fn channel_sink() -> (Sink, mpsc::Receiver<RawEvent>) {
        let (tx, rx) = mpsc::channel::<RawEvent>();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |event| {
            let _ = tx.lock().unwrap_or_else(|p| p.into_inner()).send(event);
        });
        (sink, rx)
    }

    fn canonical_root(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let c = std::fs::canonicalize(tmp.path()).expect("canonical root");
        // `canonicalize` gives the `\\?\` form; the host uses the plain drive form.
        let s = c.to_string_lossy().into_owned();
        std::path::PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
    }

    /// A nested change is delivered; the temp path is compared in canonical form (a runner's temp
    /// dir may be an 8.3 name such as `RUNNER~1`), and the wait is a channel with a timeout.
    #[test]
    fn a_nested_change_is_delivered() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp);
        let (sink, rx) = channel_sink();
        let handle = RdcwHandle::start(&root.to_string_lossy(), sink, 64 * 1024).expect("starts");
        assert!(crate::automation::watcher::WatchHandle::alive(&handle));
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("nested dirs");
        let file = nested.join("c.txt");
        std::fs::write(&file, "v1").expect("write");
        let want = file.to_string_lossy().to_lowercase();
        let deadline = Instant::now() + WAIT;
        let mut seen = false;
        while !seen && Instant::now() < deadline {
            if let Ok(RawEvent::Change(paths)) = rx.recv_timeout(Duration::from_millis(500)) {
                seen = paths.iter().any(|p| {
                    std::fs::canonicalize(p).is_ok_and(|c| {
                        let c = c.to_string_lossy().to_lowercase();
                        c.trim_start_matches(r"\\?\") == want
                    })
                });
            }
        }
        assert!(seen, "no event for the nested file arrived");
        drop(handle);
    }

    /// Deleting the watched root ends the watch and says so.
    #[test]
    fn deleting_the_root_reports_the_watch_as_lost() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp).join("watched");
        std::fs::create_dir(&root).expect("root");
        let (sink, rx) = channel_sink();
        let handle = RdcwHandle::start(&root.to_string_lossy(), sink, 64 * 1024).expect("starts");
        std::fs::remove_dir_all(&root).expect("the folder can be deleted while watched");
        let deadline = Instant::now() + WAIT;
        let mut failed = false;
        while !failed && Instant::now() < deadline {
            if let Ok(RawEvent::Failed) = rx.recv_timeout(Duration::from_millis(500)) {
                failed = true;
            }
        }
        assert!(failed, "the lost watch was not reported");
        assert!(!crate::automation::watcher::WatchHandle::alive(&handle));
        // The folder is really gone (our handle does not keep it as delete-pending).
        let gone = Instant::now() + WAIT;
        while root.exists() && Instant::now() < gone {
            // Waits on the channel (nothing more is sent) instead of spinning.
            let _ = rx.recv_timeout(Duration::from_millis(20));
        }
        assert!(!root.exists());
        drop(handle);
    }

    /// With a tiny kernel buffer and a sink that is stuck on the first batch, a burst of files
    /// overflows the buffer, and the overflow is reported once the sink is free again.
    #[test]
    fn a_buffer_overflow_is_reported() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp);
        let (tx, rx) = mpsc::channel::<RawEvent>();
        let tx = Mutex::new(tx);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let first = std::sync::atomic::AtomicBool::new(true);
        let sink: Sink = Arc::new(move |event| {
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                // The worker is held here while the files are made.
                let _ = gate_rx
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .recv_timeout(WAIT);
            }
            let _ = tx.lock().unwrap_or_else(|p| p.into_inner()).send(event);
        });
        let handle = RdcwHandle::start(&root.to_string_lossy(), sink, 512).expect("starts");
        std::fs::write(root.join("first.txt"), "1").expect("first");
        // The worker takes the first batch and sticks in the sink (the gate); the files below
        // land in the kernel's 512-byte buffer, which they overflow. If the worker has not taken
        // the first batch yet, the overflow is that batch: the result is the same.
        for i in 0..400 {
            std::fs::write(root.join(format!("file-number-{i:04}.txt")), "x").expect("write");
        }
        let _ = gate_tx.send(());
        let deadline = Instant::now() + WAIT;
        let mut overflow = false;
        while !overflow && Instant::now() < deadline {
            if let Ok(RawEvent::Overflow) = rx.recv_timeout(Duration::from_millis(500)) {
                overflow = true;
            }
        }
        assert!(overflow, "the overflow was not reported");
        drop(handle);
    }

    /// S1: a flood of changes cannot hide the stop. Another thread writes files without a pause
    /// while the handle is dropped; the drop must come back (checked through a channel with a
    /// generous timeout, so a hung drop fails the test instead of hanging it).
    #[test]
    fn dropping_the_handle_during_a_flood_of_changes_returns() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp);
        let (sink, _rx) = channel_sink();
        let handle = RdcwHandle::start(&root.to_string_lossy(), sink, 64 * 1024).expect("starts");
        let writing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let w2 = writing.clone();
        let dir = root.clone();
        let writer = std::thread::spawn(move || {
            let mut i = 0u64;
            while w2.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = std::fs::write(dir.join(format!("f{}.txt", i % 50)), i.to_string());
                i += 1;
            }
        });
        // Let the flood run until the first events have gone through.
        std::thread::sleep(Duration::from_millis(200));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let dropper = std::thread::spawn(move || {
            drop(handle);
            let _ = done_tx.send(());
        });
        let returned = done_rx.recv_timeout(Duration::from_secs(20)).is_ok();
        writing.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = writer.join();
        assert!(
            returned,
            "the drop did not return during a flood of changes"
        );
        let _ = dropper.join();
    }

    /// S2: a worker that leaves for any reason but a real stop says the watch is lost and is not
    /// alive: a failed wait and a panic outside the sink (the test seam injects both).
    #[test]
    fn a_worker_that_leaves_without_a_stop_reports_the_loss() {
        for panic_it in [false, true] {
            let tmp = tempfile::tempdir().expect("temp dir");
            let root = canonical_root(&tmp);
            let (sink, rx) = channel_sink();
            let fault: super::imp::Fault = Arc::new(move || {
                if panic_it {
                    panic!("injected fault in the worker");
                }
                true
            });
            let handle =
                RdcwHandle::start_with(&root.to_string_lossy(), sink, 64 * 1024, Some(fault))
                    .expect("starts");
            std::fs::write(root.join("x.txt"), "1").expect("write");
            let deadline = Instant::now() + WAIT;
            let mut failed = false;
            while !failed && Instant::now() < deadline {
                if let Ok(RawEvent::Failed) = rx.recv_timeout(Duration::from_millis(500)) {
                    failed = true;
                }
            }
            assert!(failed, "no Failed (panic = {panic_it})");
            assert!(
                !crate::automation::watcher::WatchHandle::alive(&handle),
                "still alive (panic = {panic_it})"
            );
            drop(handle);
        }
    }

    /// A real stop is silent: no Failed, and the handle was alive until dropped.
    #[test]
    fn a_real_stop_reports_nothing() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp);
        let (sink, rx) = channel_sink();
        let handle = RdcwHandle::start(&root.to_string_lossy(), sink, 64 * 1024).expect("starts");
        drop(handle);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    /// The watched folder is renamed away: the handle follows it, so the next completion is
    /// reported as a lost watch (the path that was approved is not the folder any more).
    #[test]
    fn renaming_the_root_away_reports_the_watch_as_lost() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let base = canonical_root(&tmp);
        let root = base.join("watched");
        std::fs::create_dir(&root).expect("root");
        let (sink, rx) = channel_sink();
        let handle = RdcwHandle::start(&root.to_string_lossy(), sink, 64 * 1024).expect("starts");
        let moved = base.join("moved");
        // The watch opens the folder with FILE_SHARE_DELETE, so the OS must allow the rename. If
        // it does not, the test has nothing to test: fail loudly instead of passing silently.
        std::fs::rename(&root, &moved)
            .expect("renaming a watched folder must be allowed (the watch shares delete access)");
        std::fs::write(moved.join("x.txt"), "1").expect("write in the moved folder");
        let deadline = Instant::now() + WAIT;
        let mut failed = false;
        while !failed && Instant::now() < deadline {
            if let Ok(RawEvent::Failed) = rx.recv_timeout(Duration::from_millis(500)) {
                failed = true;
            }
        }
        assert!(failed, "the renamed root was not reported");
        drop(handle);
    }

    /// Dropping the handle ends the worker and frees the folder (no leaked thread or handle:
    /// the folder can be deleted at once and the call returns).
    #[test]
    fn dropping_the_handle_releases_the_folder() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp).join("w");
        std::fs::create_dir(&root).expect("root");
        let (sink, _rx) = channel_sink();
        for _ in 0..20 {
            let h = RdcwHandle::start(&root.to_string_lossy(), sink.clone(), 64 * 1024)
                .expect("starts");
            drop(h);
        }
        std::fs::remove_dir(&root).expect("nothing holds the folder");
    }

    #[test]
    fn a_missing_root_does_not_start() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = canonical_root(&tmp).join("nope");
        let (sink, _rx) = channel_sink();
        assert!(RdcwHandle::start(&root.to_string_lossy(), sink, 64 * 1024).is_err());
    }
}
