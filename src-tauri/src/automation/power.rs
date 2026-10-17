//! Power, clock and time zone events for the schedule (Automation design §9.3; FER-525 / P1.4):
//! a hidden window on its own thread that tells the host "look at the schedules again" when
//!
//! - the PC resumes from sleep (`WM_POWERBROADCAST` / `PBT_APMRESUMEAUTOMATIC`),
//! - the system clock is set (`WM_TIMECHANGE`),
//! - the system time zone changes (`WM_SETTINGCHANGE` with the setting name `TimeZone`),
//!
//! and, for the watch triggers (FER-528 / P2.1), when a local **volume** arrives or is removed
//! (`WM_DEVICECHANGE`: `DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE` of a `DBT_DEVTYP_VOLUME`
//! that is not a network volume). A volume event names no folder and no automation; it only tells
//! the host to look at its watched roots again (WA-5.3). There is no polling for local drives.
//!
//! The window is a **hidden top-level window**, never shown: a message-only window would not
//! receive the broadcasts of the clock and the time zone. It owns no menu, no tray icon and no
//! input; it only turns three messages into a callback. The callback must be cheap and must not
//! block (the host's callback sets a flag and wakes its loop).
//!
//! The events are only a hint to look again. They carry no time and no authority: the host asks
//! the pure scheduler with its own clock, every occurrence is handled at most once, and every
//! run still goes through the gate. A forged or repeated message therefore cannot start a run
//! or make one fire twice.
//!
//! Tauri-free (the runner links this file). Windows only; elsewhere [`PowerWatcher::start`]
//! returns `None`.

use std::sync::Arc;

use crate::automation::host::Recompute;

/// Called on the window's thread for each event.
pub type OnEvent = Arc<dyn Fn(Recompute) + Send + Sync>;

/// A local volume came or went (WA-5.3). Carries nothing else: not a letter, not a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeChange {
    Arrived,
    Removed,
}

/// Called on the window's thread for each volume event. Must be cheap and must not block.
pub type OnVolume = Arc<dyn Fn(VolumeChange) + Send + Sync>;

const WM_TIMECHANGE: u32 = 0x001E;
const WM_SETTINGCHANGE: u32 = 0x001A;
const WM_POWERBROADCAST: u32 = 0x0218;
const WM_DEVICECHANGE: u32 = 0x0219;
const DBT_DEVICEARRIVAL: usize = 0x8000;
const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;
const DBT_DEVTYP_VOLUME: u32 = 2;
/// `DBTF_NET`: the volume is a network volume (not a local drive).
const DBTF_NET: u16 = 0x0002;
/// `sizeof(DEV_BROADCAST_VOLUME)`: size, type, reserved, unit mask (4 x u32) and flags (u16, padded).
const VOLUME_STRUCT_BYTES: usize = 20;
const PBT_APMRESUMESUSPEND: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
/// The setting name Windows sends in `lParam` of `WM_SETTINGCHANGE` for a time zone change.
const TIME_ZONE_SETTING: &str = "TimeZone";

/// What a window message means for the schedule. Pure: `setting` is the text of `lParam` for
/// `WM_SETTINGCHANGE`, already read by the caller.
pub(crate) fn classify(msg: u32, wparam: usize, setting: Option<&str>) -> Option<Recompute> {
    match msg {
        WM_TIMECHANGE => Some(Recompute::TimeChanged),
        WM_SETTINGCHANGE if setting == Some(TIME_ZONE_SETTING) => Some(Recompute::ZoneChanged),
        WM_POWERBROADCAST if wparam == PBT_APMRESUMEAUTOMATIC || wparam == PBT_APMRESUMESUSPEND => {
            Some(Recompute::Resumed)
        }
        _ => None,
    }
}

/// The part of `DEV_BROADCAST_VOLUME` the classification needs, already read by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VolumeHeader {
    pub size: u32,
    pub device_type: u32,
    pub flags: u16,
}

/// Does this `WM_DEVICECHANGE` carry a `DEV_BROADCAST_HDR` in `lParam`? Only the two events the
/// host uses do. For any other `wParam` (`DBT_DEVNODES_CHANGED`, ...) `lParam` is not a pointer the
/// system vouches for - another process on the desktop can send anything - so it is never read.
pub(crate) fn carries_header(wparam: usize) -> bool {
    wparam == DBT_DEVICEARRIVAL || wparam == DBT_DEVICEREMOVECOMPLETE
}

/// What `WM_DEVICECHANGE` means for the watch triggers. Pure. Anything that is not the arrival or
/// the complete removal of a LOCAL volume is ignored.
pub(crate) fn classify_volume(wparam: usize, header: Option<VolumeHeader>) -> Option<VolumeChange> {
    let header = header?;
    if header.device_type != DBT_DEVTYP_VOLUME
        || (header.size as usize) < VOLUME_STRUCT_BYTES
        || header.flags & DBTF_NET != 0
    {
        return None;
    }
    match wparam {
        DBT_DEVICEARRIVAL => Some(VolumeChange::Arrived),
        DBT_DEVICEREMOVECOMPLETE => Some(VolumeChange::Removed),
        _ => None,
    }
}

#[cfg(windows)]
pub use imp::PowerWatcher;

#[cfg(not(windows))]
pub struct PowerWatcher;

#[cfg(not(windows))]
impl PowerWatcher {
    pub fn start(_on_event: OnEvent) -> Option<PowerWatcher> {
        None
    }

    pub fn start_with_volume(_on_event: OnEvent, _on_volume: OnVolume) -> Option<PowerWatcher> {
        None
    }

    pub fn stop(&self) {}
}

#[cfg(windows)]
mod imp {
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, MSG, PostMessageW, PostQuitMessage,
        RegisterClassW, SetWindowLongPtrW, TranslateMessage, WM_CLOSE, WM_DESTROY, WM_NCCREATE,
        WM_NCDESTROY, WNDCLASSW, WS_OVERLAPPED,
    };

    use super::{OnEvent, OnVolume, VolumeHeader, classify, classify_volume};

    /// What the window procedure calls. `GWLP_USERDATA` holds a `Box<Callbacks>`.
    struct Callbacks {
        power: OnEvent,
        volume: Option<OnVolume>,
    }

    /// Read the header of a `WM_DEVICECHANGE` broadcast.
    ///
    /// # Safety
    /// Call only for `DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE` (see `carries_header`).
    /// `lparam` is either 0 or a pointer to a `DEV_BROADCAST_HDR` that stays valid for the
    /// duration of the message (the documented contract of those events). Only the 12 bytes
    /// of the header are read first; the volume flags (offset 16) are read only after the header
    /// itself said it is a volume structure of at least 20 bytes.
    unsafe fn volume_header(lparam: LPARAM) -> Option<VolumeHeader> {
        if lparam == 0 {
            return None;
        }
        let base = lparam as *const u8;
        // SAFETY: see the function's contract; the header is 12 bytes.
        let size = unsafe { std::ptr::read_unaligned(base as *const u32) };
        let device_type = unsafe { std::ptr::read_unaligned(base.add(4) as *const u32) };
        let flags = if device_type == super::DBT_DEVTYP_VOLUME
            && size as usize >= super::VOLUME_STRUCT_BYTES
        {
            // SAFETY: the structure says it is a DEV_BROADCAST_VOLUME of at least 20 bytes.
            unsafe { std::ptr::read_unaligned(base.add(16) as *const u16) }
        } else {
            0
        };
        Some(VolumeHeader {
            size,
            device_type,
            flags,
        })
    }

    /// Class name, UTF-16, NUL-terminated: "FerrumAutomationPower".
    fn class_name() -> Vec<u16> {
        "FerrumAutomationPower\0".encode_utf16().collect()
    }

    /// The hidden window and its thread. Dropping it stops them.
    pub struct PowerWatcher {
        hwnd: isize,
        thread: Mutex<Option<JoinHandle<()>>>,
    }

    /// Read the setting name a `WM_SETTINGCHANGE` carries, at most `max` UTF-16 units.
    ///
    /// # Safety
    /// `lparam` is either 0 or a pointer to a NUL-terminated UTF-16 string that stays valid for
    /// the duration of the message (Windows marshals it for the receiver; the documented
    /// contract of this message). At most `max + 1` units are read and reading stops at the NUL,
    /// so even an unterminated string cannot make this read far.
    unsafe fn setting_name(lparam: LPARAM, max: usize) -> Option<String> {
        if lparam == 0 {
            return None;
        }
        let ptr = lparam as *const u16;
        let mut units = Vec::with_capacity(max);
        for i in 0..=max {
            // SAFETY: see the function's contract; `i <= max`.
            let unit = unsafe { *ptr.add(i) };
            if unit == 0 {
                return String::from_utf16(&units).ok();
            }
            if i == max {
                // Longer than any name we look for: not ours.
                return None;
            }
            units.push(unit);
        }
        None
    }

    /// The window procedure. `GWLP_USERDATA` holds a `Box<Callbacks>` created in `WM_NCCREATE`
    /// and freed in `WM_NCDESTROY`.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_NCCREATE {
            // SAFETY: for WM_NCCREATE `lparam` points to a valid CREATESTRUCTW whose
            // `lpCreateParams` is the `*mut Callbacks` we passed to CreateWindowExW.
            let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
            // SAFETY: the window is ours and not yet visible to other code.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
        } else if msg == WM_NCDESTROY {
            // SAFETY: the pointer is the one stored above (or 0); it is cleared before it is
            // freed, so no later message can read it.
            let raw = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut Callbacks;
            if !raw.is_null() {
                drop(unsafe { Box::from_raw(raw) });
            }
        } else if msg == WM_DESTROY {
            // SAFETY: plain call; ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            return 0;
        } else {
            let setting = if msg == super::WM_SETTINGCHANGE {
                // SAFETY: the contract of `setting_name` is the contract of this message.
                unsafe { setting_name(lparam, super::TIME_ZONE_SETTING.len()) }
            } else {
                None
            };
            let power = classify(msg, wparam, setting.as_deref());
            let volume = if msg == super::WM_DEVICECHANGE && super::carries_header(wparam) {
                // SAFETY: for these two `wParam` values the contract of `volume_header` is the
                // contract of this message; for every other one `lParam` is not touched.
                classify_volume(wparam, unsafe { volume_header(lparam) })
            } else {
                None
            };
            if power.is_some() || volume.is_some() {
                // SAFETY: the pointer is the live `Box<Callbacks>` of this window (see above);
                // messages reach this procedure on the thread that created the window, one at a
                // time, so no one frees it while we use it.
                let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Callbacks;
                if !raw.is_null() {
                    let callbacks = unsafe { &*raw };
                    // A panic must not unwind through the OS: swallow it, the host looks again
                    // at its next pass anyway.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if let Some(why) = power {
                            (callbacks.power)(why);
                        }
                        if let (Some(change), Some(cb)) = (volume, &callbacks.volume) {
                            cb(change);
                        }
                    }));
                }
            }
        }
        // SAFETY: forwarding the same arguments to the default procedure.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    impl PowerWatcher {
        /// Create the hidden window on a new thread. `None` if the window or the thread could not
        /// be created (the schedules still work from the host's own timer; they only learn about
        /// a clock or sleep change at the next pass).
        pub fn start(on_event: OnEvent) -> Option<PowerWatcher> {
            Self::start_with(on_event, None)
        }

        /// [`PowerWatcher::start`] that also reports local volume arrival and removal (WA-5.3).
        pub fn start_with_volume(on_event: OnEvent, on_volume: OnVolume) -> Option<PowerWatcher> {
            Self::start_with(on_event, Some(on_volume))
        }

        fn start_with(on_event: OnEvent, on_volume: Option<OnVolume>) -> Option<PowerWatcher> {
            let callbacks = Callbacks {
                power: on_event,
                volume: on_volume,
            };
            let (tx, rx) = mpsc::channel::<Option<isize>>();
            let thread = std::thread::Builder::new()
                .name("ferrum-automation-power".into())
                .spawn(move || run_window(callbacks, tx))
                .ok()?;
            match rx.recv() {
                Ok(Some(hwnd)) => Some(PowerWatcher {
                    hwnd,
                    thread: Mutex::new(Some(thread)),
                }),
                _ => {
                    let _ = thread.join();
                    None
                }
            }
        }

        /// The window handle, for the tests that send it messages.
        #[cfg(test)]
        pub(crate) fn hwnd(&self) -> HWND {
            self.hwnd as HWND
        }

        /// Close the window and join the thread. Safe to call twice.
        pub fn stop(&self) {
            let handle = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(handle) = handle {
                // SAFETY: posting WM_CLOSE to our own window handle; if the window is already
                // gone the call just fails.
                unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
                let _ = handle.join();
            }
        }
    }

    impl Drop for PowerWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn run_window(callbacks: Callbacks, ready: mpsc::Sender<Option<isize>>) {
        let class = class_name();
        // SAFETY: a null module name asks for this process's own module handle.
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        // SAFETY: `wc` is fully initialised and `class` outlives the call. A second watcher in
        // the same process finds the class registered already; that is fine.
        unsafe { RegisterClassW(&wc) };
        let boxed: *mut Callbacks = Box::into_raw(Box::new(callbacks));
        // SAFETY: the class is registered; the title and class are NUL-terminated UTF-16; a
        // top-level window without WS_VISIBLE is never shown. `boxed` is handed to the window
        // procedure, which owns it from WM_NCCREATE and frees it at WM_NCDESTROY.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                boxed as *const std::ffi::c_void,
            )
        };
        if hwnd.is_null() {
            // SAFETY: the window procedure never saw the pointer, so it is still ours.
            drop(unsafe { Box::from_raw(boxed) });
            let _ = ready.send(None);
            return;
        }
        let _ = ready.send(Some(hwnd as isize));
        // SAFETY: MSG is a plain struct; GetMessageW fills it. The loop ends at WM_QUIT
        // (PostQuitMessage in WM_DESTROY) or on error (-1).
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // The window is destroyed by now (WM_CLOSE -> DestroyWindow); make sure if the loop
        // ended another way. The class stays registered for the life of the process (another
        // watcher may be creating its window from it right now).
        // SAFETY: the call tolerates a handle that is already gone.
        unsafe {
            DestroyWindow(hwnd);
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

        use super::super::*;
        use super::*;

        fn watcher() -> (PowerWatcher, Arc<Mutex<Vec<Recompute>>>) {
            let seen: Arc<Mutex<Vec<Recompute>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = seen.clone();
            let cb: OnEvent = Arc::new(move |why| sink.lock().unwrap().push(why));
            (PowerWatcher::start(cb).expect("window"), seen)
        }

        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        }

        #[test]
        fn the_hidden_window_turns_the_three_messages_into_events() {
            let (w, seen) = watcher();
            // SendMessage is synchronous: when it returns the window procedure has run.
            unsafe {
                SendMessageW(w.hwnd(), WM_TIMECHANGE, 0, 0);
                SendMessageW(w.hwnd(), WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC, 0);
                let zone = wide("TimeZone");
                SendMessageW(w.hwnd(), WM_SETTINGCHANGE, 0, zone.as_ptr() as isize);
            }
            assert_eq!(
                *seen.lock().unwrap(),
                vec![
                    Recompute::TimeChanged,
                    Recompute::Resumed,
                    Recompute::ZoneChanged
                ]
            );
            w.stop();
        }

        #[test]
        fn other_messages_and_settings_are_ignored() {
            let (w, seen) = watcher();
            unsafe {
                // Suspend, battery, an unrelated setting, no setting, a name that is too long.
                SendMessageW(w.hwnd(), WM_POWERBROADCAST, 0x0004, 0);
                SendMessageW(w.hwnd(), WM_POWERBROADCAST, 0x000A, 0);
                let other = wide("intl");
                SendMessageW(w.hwnd(), WM_SETTINGCHANGE, 0, other.as_ptr() as isize);
                SendMessageW(w.hwnd(), WM_SETTINGCHANGE, 0, 0);
                let long = wide("TimeZoneTimeZoneTimeZone");
                SendMessageW(w.hwnd(), WM_SETTINGCHANGE, 0, long.as_ptr() as isize);
                SendMessageW(w.hwnd(), 0x0400, 0, 0);
            }
            assert!(seen.lock().unwrap().is_empty());
            w.stop();
        }

        /// A `DEV_BROADCAST_VOLUME`: size, type, reserved, unit mask, flags (padded to 20 bytes).
        fn volume_struct(flags: u16) -> [u32; 5] {
            [
                20,
                super::super::DBT_DEVTYP_VOLUME,
                0,
                1 << 4,
                u32::from(flags),
            ]
        }

        #[test]
        fn the_hidden_window_reports_local_volume_arrival_and_removal() {
            let seen: Arc<Mutex<Vec<VolumeChange>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = seen.clone();
            let on_volume: OnVolume = Arc::new(move |c| sink.lock().unwrap().push(c));
            let power: OnEvent = Arc::new(|_| {});
            let w = PowerWatcher::start_with_volume(power, on_volume).expect("window");
            let local = volume_struct(0);
            let network = volume_struct(super::super::DBTF_NET);
            let mut other = volume_struct(0);
            other[1] = 5; // DBT_DEVTYP_PORT
            unsafe {
                SendMessageW(
                    w.hwnd(),
                    WM_DEVICECHANGE,
                    DBT_DEVICEARRIVAL,
                    local.as_ptr() as isize,
                );
                SendMessageW(
                    w.hwnd(),
                    WM_DEVICECHANGE,
                    DBT_DEVICEREMOVECOMPLETE,
                    local.as_ptr() as isize,
                );
                // wParams that carry no header: lParam is garbage and must never be read.
                for wparam in [0x0007usize, 0x0018, 0x8001, 0x8003, 0x8005, 0] {
                    SendMessageW(w.hwnd(), WM_DEVICECHANGE, wparam, 0x1234);
                }
                // A network volume, another device type, an unrelated wParam and a null lParam.
                SendMessageW(
                    w.hwnd(),
                    WM_DEVICECHANGE,
                    DBT_DEVICEARRIVAL,
                    network.as_ptr() as isize,
                );
                SendMessageW(
                    w.hwnd(),
                    WM_DEVICECHANGE,
                    DBT_DEVICEARRIVAL,
                    other.as_ptr() as isize,
                );
                SendMessageW(w.hwnd(), WM_DEVICECHANGE, 0x0007, local.as_ptr() as isize);
                SendMessageW(w.hwnd(), WM_DEVICECHANGE, DBT_DEVICEARRIVAL, 0);
            }
            assert_eq!(
                *seen.lock().unwrap(),
                vec![VolumeChange::Arrived, VolumeChange::Removed]
            );
            w.stop();
        }

        #[test]
        fn stop_is_idempotent_and_a_second_watcher_can_start() {
            let (w, _) = watcher();
            w.stop();
            w.stop();
            let (w2, seen) = watcher();
            unsafe { SendMessageW(w2.hwnd(), WM_TIMECHANGE, 0, 0) };
            assert_eq!(seen.lock().unwrap().len(), 1);
            drop(w2);
        }

        #[test]
        fn a_panicking_callback_does_not_unwind_into_the_os() {
            let count = Arc::new(AtomicUsize::new(0));
            let c = count.clone();
            let cb: OnEvent = Arc::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
                panic!("callback failure");
            });
            let w = PowerWatcher::start(cb).expect("window");
            unsafe {
                SendMessageW(w.hwnd(), WM_TIMECHANGE, 0, 0);
                SendMessageW(w.hwnd(), WM_TIMECHANGE, 0, 0);
            }
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_arrival_and_removal_carry_a_header() {
        assert!(carries_header(DBT_DEVICEARRIVAL));
        assert!(carries_header(DBT_DEVICEREMOVECOMPLETE));
        for wparam in [
            0usize,
            0x0007,
            0x0018,
            0x8001,
            0x8002,
            0x8003,
            0x8005,
            usize::MAX,
        ] {
            assert!(!carries_header(wparam), "{wparam:#x}");
        }
    }

    #[test]
    fn volume_classification_is_pure_and_closed() {
        let vol = |flags| {
            Some(VolumeHeader {
                size: 20,
                device_type: DBT_DEVTYP_VOLUME,
                flags,
            })
        };
        assert_eq!(
            classify_volume(DBT_DEVICEARRIVAL, vol(0)),
            Some(VolumeChange::Arrived)
        );
        assert_eq!(
            classify_volume(DBT_DEVICEREMOVECOMPLETE, vol(0)),
            Some(VolumeChange::Removed)
        );
        // Media in a drive (flags 1) is still a local volume event.
        assert_eq!(
            classify_volume(DBT_DEVICEARRIVAL, vol(1)),
            Some(VolumeChange::Arrived)
        );
        assert_eq!(classify_volume(DBT_DEVICEARRIVAL, vol(DBTF_NET)), None);
        assert_eq!(classify_volume(0x8001, vol(0)), None);
        assert_eq!(classify_volume(DBT_DEVICEARRIVAL, None), None);
        let short = Some(VolumeHeader {
            size: 12,
            device_type: DBT_DEVTYP_VOLUME,
            flags: 0,
        });
        assert_eq!(classify_volume(DBT_DEVICEARRIVAL, short), None);
        let port = Some(VolumeHeader {
            size: 20,
            device_type: 3,
            flags: 0,
        });
        assert_eq!(classify_volume(DBT_DEVICEARRIVAL, port), None);
    }

    #[test]
    fn classification_is_pure_and_closed() {
        assert_eq!(
            classify(WM_TIMECHANGE, 0, None),
            Some(Recompute::TimeChanged)
        );
        assert_eq!(
            classify(WM_SETTINGCHANGE, 0, Some("TimeZone")),
            Some(Recompute::ZoneChanged)
        );
        assert_eq!(classify(WM_SETTINGCHANGE, 0, Some("Environment")), None);
        assert_eq!(classify(WM_SETTINGCHANGE, 0, None), None);
        assert_eq!(
            classify(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC, None),
            Some(Recompute::Resumed)
        );
        assert_eq!(
            classify(WM_POWERBROADCAST, PBT_APMRESUMESUSPEND, None),
            Some(Recompute::Resumed)
        );
        assert_eq!(classify(WM_POWERBROADCAST, 0x0004, None), None);
        assert_eq!(classify(0x0001, 0, None), None);
    }
}
