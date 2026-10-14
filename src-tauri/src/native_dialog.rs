//! Shared native Windows confirmation (Automation design §10.3), extracted unchanged from the
//! hotlist command dialog. A Windows message box owned by the Ferrum window, with text built by
//! the backend; the webview can neither draw it nor answer it. Cancel is the default button, so a
//! held Enter key cannot approve anything. Callers run it on `spawn_blocking`; the production
//! command wrappers take the confirmation as an injected closure (see `hotlist::run_command_in`)
//! so tests drive both answers without a window.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::domain::{ErrorCode, FsError};

static DIALOG_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The one app-wide guard for native confirmation dialogs: only one is open at a time, whoever
/// asks (hotlist, RAR path, AI endpoint, indexer mode, shell open, automation). A second request
/// while one is open is answered `Busy` and shows nothing, so a flood of requests from the
/// webview can never stack dialogs and trick a click into the wrong one.
#[derive(Debug)]
pub struct DialogGuard;

impl DialogGuard {
    pub fn try_acquire() -> Result<Self, FsError> {
        if DIALOG_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            Ok(DialogGuard)
        } else {
            Err(FsError::new(
                ErrorCode::Busy,
                "Konfirmasi lain masih terbuka",
            ))
        }
    }
}

impl Drop for DialogGuard {
    fn drop(&mut self) {
        DIALOG_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// The raw message box, without the guard (callers that already hold a [`DialogGuard`]).
/// `true` only when the user pressed OK. Same flags as the hotlist confirmation:
/// `MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND`.
#[cfg(windows)]
pub fn message_box(owner: isize, caption: &str, text: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDOK, MB_DEFBUTTON2, MB_ICONWARNING, MB_OKCANCEL, MB_SETFOREGROUND, MessageBoxW,
    };
    let text = crate::shell_open::to_wide(text);
    let caption = crate::shell_open::to_wide(caption);
    // SAFETY: both strings are NUL-terminated UTF-16; `owner` is the window handle or 0.
    unsafe {
        MessageBoxW(
            owner as _,
            text.as_ptr(),
            caption.as_ptr(),
            MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
        ) == IDOK
    }
}

/// Fail closed off-Windows: no dialog can be answered, so nothing is ever confirmed.
#[cfg(not(windows))]
pub fn message_box(_owner: isize, _caption: &str, _text: &str) -> bool {
    false
}

/// Guarded confirmation with an injectable box (tests drive the busy path without a window).
pub fn confirm_guarded_with(
    owner: isize,
    caption: &str,
    text: &str,
    show: impl FnOnce(isize, &str, &str) -> bool,
) -> Result<bool, FsError> {
    let _guard = DialogGuard::try_acquire()?;
    Ok(show(owner, caption, text))
}

/// Guarded native confirmation: `Err(Busy)` (nothing shown) while another dialog is open.
pub fn confirm_or_busy(owner: isize, caption: &str, text: &str) -> Result<bool, FsError> {
    confirm_guarded_with(owner, caption, text, message_box)
}

/// Guarded native confirmation as a plain answer: `false` (no) when busy, so callers with a
/// `bool` seam store nothing and change nothing while another dialog is open.
pub fn confirm(owner: isize, caption: &str, text: &str) -> bool {
    confirm_or_busy(owner, caption, text).unwrap_or(false)
}

/// The guard is one process-wide flag, so every test that takes it runs under this lock.
#[cfg(test)]
pub(crate) static GUARD_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_windows_the_answer_is_always_no() {
        #[cfg(not(windows))]
        assert!(!message_box(0, "caption", "text"));
        // On Windows the real dialog cannot be answered by an unattended test; the behaviour is
        // pinned by the injected-confirmation tests of the hotlist and automation commands.
        #[cfg(windows)]
        let _ = message_box;
    }

    /// P2-4: a second request while a dialog is open is Busy, shows nothing, and the first
    /// dialog's slot is released afterwards.
    #[test]
    fn a_second_confirmation_while_one_is_open_is_busy_and_shows_nothing() {
        let _serial = GUARD_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        let mut inner_error = None;
        let outer = confirm_guarded_with(0, "c", "t", |_, _, _| {
            let mut shown = false;
            inner_error = confirm_guarded_with(0, "c2", "t2", |_, _, _| {
                shown = true;
                true
            })
            .err();
            assert!(!shown, "the second dialog must not be shown");
            true
        });
        assert!(outer.unwrap());
        assert_eq!(inner_error.expect("busy").code, ErrorCode::Busy);
        // Released: a later request works.
        assert!(confirm_guarded_with(0, "c", "t", |_, _, _| true).unwrap());
    }

    #[test]
    fn the_plain_bool_wrapper_says_no_while_busy() {
        let _serial = GUARD_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        let _held = DialogGuard::try_acquire().unwrap();
        assert!(!confirm(0, "caption", "text"));
        assert_eq!(
            confirm_or_busy(0, "caption", "text").unwrap_err().code,
            ErrorCode::Busy
        );
    }

    /// P2-4: the shell-open confirmation and every other caller share this one guard: while the
    /// shell-open dialog is open a native confirmation (automation, hotlist, ...) is Busy, and
    /// the other way round.
    #[test]
    fn shell_open_and_the_shared_dialog_use_one_guard() {
        let _serial = GUARD_TESTS.lock().unwrap_or_else(|p| p.into_inner());
        let mut inner = None;
        let outer = crate::shell_open::native_confirm_with_guard(0, "text", |_, _| {
            inner = Some(confirm_or_busy(0, "caption", "text"));
            true
        });
        assert!(outer.unwrap());
        assert_eq!(inner.unwrap().unwrap_err().code, ErrorCode::Busy);
        // And the other direction.
        let mut other = None;
        confirm_guarded_with(0, "c", "t", |_, _, _| {
            other = Some(crate::shell_open::native_confirm_with_guard(
                0,
                "x",
                |_, _| true,
            ));
            true
        })
        .unwrap();
        assert_eq!(other.unwrap().unwrap_err().code, ErrorCode::Busy);
    }
}
