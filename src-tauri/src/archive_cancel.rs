//! Request-id registry for cancellable archive reads (FER-438).
//!
//! The frontend tags `read_archive_member_preview` / `archive_read` with a
//! request id and calls `archive_cancel_request(id)` when the selection moves
//! on. `begin` hands the blocking thread a [`CancelToken`]; `cancel` flips it.
//! A cancel that arrives BEFORE its request starts (the blocking pool was
//! busy) is remembered in a small bounded tombstone list, so the request
//! starts already cancelled instead of running to completion. Tombstones
//! expire after `TOMBSTONE_TTL`, so a stale cancel (the request never came)
//! cannot hit a later request that happens to reuse the id.

use crate::archive_contract::CancelToken;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_ID_LEN: usize = 128;
const MAX_TOMBSTONES: usize = 256;
/// How long a cancel that found no running request is remembered.
const TOMBSTONE_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    active: HashMap<String, CancelToken>,
    early: VecDeque<(String, Instant)>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN
}

/// Registration of one running request; unregisters on drop.
pub struct Registration {
    id: Option<String>,
    token: CancelToken,
}

impl Registration {
    pub fn token(&self) -> &CancelToken {
        &self.token
    }

    /// Run `f` with this request's token installed on the current thread.
    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        crate::archive_contract::with_cancel(&self.token, f)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(id) = self.id.take()
            && let Ok(mut st) = state().lock()
            // Only remove the entry if it is still THIS job's token: a later job that reused the
            // id has replaced it, and must stay cancellable after this one finishes.
            && st.active.get(&id).is_some_and(|t| t.same_job(&self.token))
        {
            st.active.remove(&id);
        }
    }
}

/// Drop tombstones older than `ttl` (the deque is in insertion order).
fn prune(early: &mut VecDeque<(String, Instant)>, now: Instant, ttl: Duration) {
    while early
        .front()
        .is_some_and(|(_, at)| now.saturating_duration_since(*at) > ttl)
    {
        early.pop_front();
    }
}

/// Register a request. `None` (or an unusable id) yields a token nobody can
/// cancel, which is the pre-FER-438 behavior.
pub fn begin(request_id: Option<&str>) -> Registration {
    let token = CancelToken::new();
    let id = request_id.filter(|i| valid(i)).map(str::to_owned);
    if let Some(id) = &id
        && let Ok(mut st) = state().lock()
    {
        prune(&mut st.early, Instant::now(), TOMBSTONE_TTL);
        if let Some(pos) = st.early.iter().position(|(e, _)| e == id) {
            st.early.remove(pos);
            token.cancel();
        }
        st.active.insert(id.clone(), token.clone());
    }
    Registration { id, token }
}

/// Like [`begin`], but refuses an id that a running job still holds, so one job can never
/// cancel (or hide from cancel) another. `None` or an unusable id never conflicts.
pub fn begin_unique(request_id: Option<&str>) -> Result<Registration, String> {
    if let Some(id) = request_id.filter(|i| valid(i))
        && let Ok(st) = state().lock()
        && st.active.contains_key(id)
    {
        return Err("Request id is already in use by a running archive job".to_owned());
    }
    Ok(begin(request_id))
}

/// Cancel a request by id (running now, or about to start).
pub fn cancel(request_id: &str) {
    if !valid(request_id) {
        return;
    }
    let Ok(mut st) = state().lock() else { return };
    if let Some(token) = st.active.get(request_id) {
        token.cancel();
    } else {
        prune(&mut st.early, Instant::now(), TOMBSTONE_TTL);
        if st.early.len() >= MAX_TOMBSTONES {
            st.early.pop_front();
        }
        st.early.push_back((request_id.to_owned(), Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_contract::{check_cancelled, is_cancelled};

    #[test]
    fn cancel_flips_the_registered_token_and_unregisters_on_drop() {
        let reg = begin(Some("t-running"));
        assert!(!reg.token().is_cancelled());
        reg.run(|| assert!(!is_cancelled()));
        cancel("t-running");
        assert!(reg.token().is_cancelled());
        reg.run(|| {
            assert!(is_cancelled());
            assert!(check_cancelled().is_err());
        });
        // Outside `run` no token is installed on this thread.
        assert!(!is_cancelled());
        drop(reg);
        assert!(!state().lock().unwrap().active.contains_key("t-running"));
    }

    #[test]
    fn a_finished_job_does_not_unregister_a_later_job_with_the_same_id() {
        let first = begin(Some("t-dup"));
        let second = begin(Some("t-dup")); // reuses the id, replaces the entry
        let first_token = first.token().clone();
        drop(first);
        cancel("t-dup");
        assert!(
            second.token().is_cancelled(),
            "second job is still cancellable"
        );
        assert!(!first_token.is_cancelled());
        drop(second);
        assert!(!state().lock().unwrap().active.contains_key("t-dup"));
    }

    #[test]
    fn begin_unique_refuses_an_id_a_running_job_holds() {
        let running = begin_unique(Some("t-uniq")).unwrap();
        assert!(begin_unique(Some("t-uniq")).is_err());
        // The running job is untouched and still cancellable.
        cancel("t-uniq");
        assert!(running.token().is_cancelled());
        drop(running);
        assert!(
            begin_unique(Some("t-uniq")).is_ok(),
            "free again after it finished"
        );
        // No id: never a conflict.
        assert!(begin_unique(None).is_ok());
    }

    #[test]
    fn a_cancel_that_arrives_before_the_request_starts_is_honoured() {
        cancel("t-early");
        let reg = begin(Some("t-early"));
        assert!(reg.token().is_cancelled());
        // The tombstone is consumed: the next request with that id is fresh.
        drop(reg);
        assert!(!begin(Some("t-early")).token().is_cancelled());
    }

    #[test]
    fn requests_without_a_usable_id_cannot_be_cancelled() {
        let reg = begin(None);
        cancel("");
        assert!(!reg.token().is_cancelled());
        let long = "x".repeat(MAX_ID_LEN + 1);
        let reg2 = begin(Some(&long));
        cancel(&long);
        assert!(!reg2.token().is_cancelled());
    }

    #[test]
    fn an_expired_tombstone_does_not_cancel_a_new_request() {
        let now = Instant::now();
        let mut early: VecDeque<(String, Instant)> = VecDeque::new();
        early.push_back(("old".into(), now - Duration::from_secs(120)));
        early.push_back(("fresh".into(), now - Duration::from_secs(5)));
        prune(&mut early, now, TOMBSTONE_TTL);
        let ids: Vec<&str> = early.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(ids, ["fresh"]);

        // End to end through the registry: plant an already-expired tombstone at the FRONT (the
        // prune order), so other tests' fresh tombstones cannot shadow it.
        state().lock().unwrap().early.push_front((
            "t-expired".into(),
            now - TOMBSTONE_TTL - Duration::from_secs(1),
        ));
        let reg = begin(Some("t-expired"));
        assert!(!reg.token().is_cancelled());
    }
}
