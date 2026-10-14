//! Shared native Windows confirmation (Automation design §10.3), extracted unchanged from the
//! hotlist command dialog. A Windows message box owned by the Ferrum window, with text built by
//! the backend; the webview can neither draw it nor answer it. Cancel is the default button, so a
//! held Enter key cannot approve anything. Callers run it on `spawn_blocking`; the production
//! command wrappers take the confirmation as an injected closure (see `hotlist::run_command_in`)
//! so tests drive both answers without a window.

/// `true` only when the user pressed OK. Same flags as the hotlist confirmation:
/// `MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND`.
#[cfg(windows)]
pub fn confirm(owner: isize, caption: &str, text: &str) -> bool {
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
pub fn confirm(_owner: isize, _caption: &str, _text: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn off_windows_the_answer_is_always_no() {
        #[cfg(not(windows))]
        assert!(!super::confirm(0, "caption", "text"));
        // On Windows the real dialog cannot be answered by an unattended test; the behaviour is
        // pinned by the injected-confirmation tests of the hotlist and automation commands.
        #[cfg(windows)]
        let _ = super::confirm;
    }
}
