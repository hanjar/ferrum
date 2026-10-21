//! FER-583: native close guard for the main window.
//!
//! A JS-only `onCloseRequested` listener makes Tauri always prevent the close, so a frozen or crashed
//! webview could never be closed. The guard therefore lives here: a close request is held back and
//! forwarded to the frontend, which must answer with `close_ack` quickly. No ack within
//! [`ACK_TIMEOUT`], or a second close request while the first is still unanswered, lets the close through.

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const CLOSE_EVENT: &str = "ferrum-close-requested";
pub const ACK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct GuardState {
    next_seq: u64,
    pending: Option<(u64, Instant)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CloseDecision {
    /// Close proceeds natively.
    Allow,
    /// Close is held; ask the frontend (seq identifies the request for the timeout).
    Hold { seq: u64 },
}

impl GuardState {
    pub const fn new() -> Self {
        Self {
            next_seq: 0,
            pending: None,
        }
    }

    /// A native close request arrived at `now`.
    pub fn on_close_requested(&mut self, now: Instant) -> CloseDecision {
        if self.pending.take().is_some() {
            // Unanswered request: the user pressed close again. Never trap them.
            return CloseDecision::Allow;
        }
        self.next_seq += 1;
        let seq = self.next_seq;
        self.pending = Some((seq, now));
        CloseDecision::Hold { seq }
    }

    /// The frontend answered: the request is handled by the UI.
    pub fn on_ack(&mut self) {
        self.pending = None;
    }

    /// The ack timer of request `seq` fired. True when the window must be closed now.
    pub fn on_timeout(&mut self, seq: u64) -> bool {
        match self.pending {
            Some((s, _)) if s == seq => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

static STATE: Mutex<GuardState> = Mutex::new(GuardState::new());

fn with_state<T>(f: impl FnOnce(&mut GuardState) -> T) -> T {
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut g)
}

/// `WindowEvent::CloseRequested` hook for the main window.
pub fn handle_close_requested<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    api: &tauri::CloseRequestApi,
) {
    use tauri::Emitter;
    if window.label() != "main" {
        return;
    }
    match with_state(|s| s.on_close_requested(Instant::now())) {
        CloseDecision::Allow => {}
        CloseDecision::Hold { seq } => {
            api.prevent_close();
            if window.emit_to("main", CLOSE_EVENT, ()).is_err() {
                // Cannot even notify the frontend: do not trap the user.
                with_state(|s| s.on_ack());
                let _ = window.destroy();
                return;
            }
            let w = window.clone();
            std::thread::spawn(move || {
                std::thread::sleep(ACK_TIMEOUT);
                if with_state(|s| s.on_timeout(seq)) {
                    let _ = w.destroy();
                }
            });
        }
    }
}

/// The frontend received the close request and takes over (dialog or destroy).
#[tauri::command]
pub fn close_ack() {
    with_state(|s| s.on_ack());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_is_held() {
        let mut s = GuardState::new();
        assert_eq!(
            s.on_close_requested(Instant::now()),
            CloseDecision::Hold { seq: 1 }
        );
    }

    #[test]
    fn ack_clears_pending_and_timeout_is_noop() {
        let mut s = GuardState::new();
        let CloseDecision::Hold { seq } = s.on_close_requested(Instant::now()) else {
            panic!()
        };
        s.on_ack();
        assert!(!s.on_timeout(seq));
    }

    #[test]
    fn missing_ack_times_out_into_close() {
        let mut s = GuardState::new();
        let CloseDecision::Hold { seq } = s.on_close_requested(Instant::now()) else {
            panic!()
        };
        assert!(s.on_timeout(seq));
        assert!(!s.on_timeout(seq)); // only once
    }

    #[test]
    fn second_close_without_ack_is_allowed() {
        let mut s = GuardState::new();
        let t = Instant::now();
        let CloseDecision::Hold { seq } = s.on_close_requested(t) else {
            panic!()
        };
        assert_eq!(s.on_close_requested(t), CloseDecision::Allow);
        assert!(!s.on_timeout(seq)); // stale timer must not fire afterwards
    }

    #[test]
    fn close_after_ack_is_held_again_with_new_seq() {
        let mut s = GuardState::new();
        let t = Instant::now();
        let CloseDecision::Hold { seq: a } = s.on_close_requested(t) else {
            panic!()
        };
        s.on_ack();
        let CloseDecision::Hold { seq: b } = s.on_close_requested(t) else {
            panic!()
        };
        assert_ne!(a, b);
        assert!(!s.on_timeout(a)); // old timer is stale
        assert!(s.on_timeout(b));
    }
}
