//! Pure security decisions of the service (FER-474), separated from the
//! Win32 plumbing so every rule is unit-tested.
//!
//! - [`accept_client`]: who may talk to the service at all.
//! - [`may_set_mode`]: who may change the privacy mode.
//! - [`RateLimiter`]: per-connection token bucket.

use crate::proto::{ErrorCode, Mode};
use std::time::{Duration, Instant};

/// `SECURITY_MANDATORY_MEDIUM_RID`.
pub const INTEGRITY_MEDIUM: u32 = 0x2000;

/// What the service learned from the client's token (read at
/// identification level right after the client's `Hello`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    /// Raw user SID bytes.
    pub user_sid: Vec<u8>,
    /// `TokenIntegrityLevel` RID.
    pub integrity_rid: u32,
    /// `TokenElevation.TokenIsElevated`.
    pub elevated: bool,
    /// BUILTIN\Administrators is an ENABLED group of the token
    /// (`CheckTokenMembership`), i.e. not the deny-only filtered half of a
    /// split admin token.
    pub admin: bool,
    /// NT AUTHORITY\INTERACTIVE (S-1-5-4) is an enabled group: the client
    /// runs in an interactive logon (console or RDP), not as a service,
    /// batch job or network logon.
    pub interactive: bool,
    /// The token is a restricted token (`IsTokenRestricted`).
    pub restricted: bool,
    /// The token belongs to an AppContainer (`TokenIsAppContainer`).
    pub app_container: bool,
}

/// May this client use the service at all? Interactive, at least medium
/// integrity, and not sandboxed. `Err` carries the code sent before the
/// connection is dropped.
pub fn accept_client(id: &ClientIdentity) -> Result<(), ErrorCode> {
    if !id.interactive
        || id.integrity_rid < INTEGRITY_MEDIUM
        || id.restricted
        || id.app_container
        || id.user_sid.is_empty()
    {
        return Err(ErrorCode::Unauthenticated);
    }
    Ok(())
}

/// May an accepted client switch the mode from `current` to `to`?
///
/// - Toward MORE privacy (`→ shared`) or no change: any accepted client.
/// - Toward LESS privacy (`shared → high_performance`): only an elevated
///   administrator. On a shared PC a standard user must never be able to
///   expose other users' file names; on a single-user PC the owner confirms
///   once with UAC (Ferrum runs `ferrum-indexer.exe set-mode` elevated).
pub fn may_set_mode(id: &ClientIdentity, current: Mode, to: Mode) -> Result<(), ErrorCode> {
    accept_client(id)?;
    if current.lowers_privacy_to(to) && !(id.elevated && id.admin) {
        return Err(ErrorCode::NeedsElevation);
    }
    Ok(())
}

/// Whether a client could lower privacy (sent in `HelloOk` so Ferrum knows
/// up front that it will need UAC).
pub fn can_lower_privacy(id: &ClientIdentity) -> bool {
    id.elevated && id.admin
}

/// Token bucket: `burst` requests at once, refilled at `per_sec`.
#[derive(Debug)]
pub struct RateLimiter {
    capacity: f64,
    per_sec: f64,
    tokens: f64,
    last: Instant,
    /// Requests refused so far on this connection.
    pub refused: u32,
}

impl RateLimiter {
    pub fn new(burst: u32, per_sec: u32, now: Instant) -> Self {
        RateLimiter {
            capacity: burst as f64,
            per_sec: per_sec as f64,
            tokens: burst as f64,
            last: now,
            refused: 0,
        }
    }

    /// Take one token at `now`. Returns false (and counts the refusal) when
    /// the bucket is empty.
    pub fn allow(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last);
        self.last = now;
        self.tokens = (self.tokens + dt.as_secs_f64() * self.per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            self.refused = self.refused.saturating_add(1);
            false
        }
    }
}

/// Service-wide limits.
pub mod limits {
    use super::Duration;
    /// Concurrent connections served at once; more are answered `Busy` and
    /// closed.
    pub const MAX_CONNECTIONS: usize = 8;
    /// Requests per connection: burst and sustained rate.
    pub const RATE_BURST: u32 = 20;
    pub const RATE_PER_SEC: u32 = 10;
    /// Refusals tolerated on one connection before it is dropped.
    pub const MAX_REFUSALS: u32 = 20;
    /// The client must send `Hello` within this time after connecting.
    pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
    /// A connection with no request for this long is closed.
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
    /// One write to the client may take at most this long.
    pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
    /// A connection lives at most this long, whatever it does.
    pub const MAX_CONNECTION_LIFETIME: Duration = Duration::from_secs(10 * 60);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> ClientIdentity {
        ClientIdentity {
            user_sid: vec![1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0],
            integrity_rid: INTEGRITY_MEDIUM,
            elevated: false,
            admin: false,
            interactive: true,
            restricted: false,
            app_container: false,
        }
    }

    fn admin() -> ClientIdentity {
        ClientIdentity {
            integrity_rid: 0x3000,
            elevated: true,
            admin: true,
            ..user()
        }
    }

    #[test]
    fn interactive_medium_users_are_accepted() {
        assert!(accept_client(&user()).is_ok());
        assert!(accept_client(&admin()).is_ok());
    }

    #[test]
    fn non_interactive_low_il_and_sandboxed_clients_are_refused() {
        let cases = [
            ClientIdentity {
                interactive: false,
                ..user()
            },
            ClientIdentity {
                integrity_rid: 0x1000,
                ..user()
            },
            ClientIdentity {
                integrity_rid: 0,
                ..user()
            },
            ClientIdentity {
                restricted: true,
                ..user()
            },
            ClientIdentity {
                app_container: true,
                ..user()
            },
            ClientIdentity {
                user_sid: vec![],
                ..user()
            },
            // Even an elevated admin must be interactive.
            ClientIdentity {
                interactive: false,
                ..admin()
            },
        ];
        for c in cases {
            assert_eq!(accept_client(&c), Err(ErrorCode::Unauthenticated), "{c:?}");
        }
    }

    #[test]
    fn lowering_privacy_needs_an_elevated_admin() {
        use Mode::*;
        // More privacy / no change: anyone accepted.
        for (from, to) in [
            (HighPerformance, Shared),
            (Shared, Shared),
            (HighPerformance, HighPerformance),
        ] {
            assert!(may_set_mode(&user(), from, to).is_ok(), "{from:?}->{to:?}");
        }
        // Less privacy: standard user, filtered admin token, and an
        // elevated non-admin (should not exist, but) are all refused.
        for c in [
            user(),
            ClientIdentity {
                elevated: true,
                ..user()
            },
            ClientIdentity {
                admin: true,
                ..user()
            },
        ] {
            assert_eq!(
                may_set_mode(&c, Shared, HighPerformance),
                Err(ErrorCode::NeedsElevation),
                "{c:?}"
            );
            assert!(!can_lower_privacy(&c));
        }
        assert!(may_set_mode(&admin(), Shared, HighPerformance).is_ok());
        assert!(can_lower_privacy(&admin()));
        // An unacceptable client is refused before the mode rule.
        let bad = ClientIdentity {
            interactive: false,
            ..admin()
        };
        assert_eq!(
            may_set_mode(&bad, HighPerformance, Shared),
            Err(ErrorCode::Unauthenticated)
        );
    }

    #[test]
    fn rate_limiter_bursts_then_refills() {
        let t0 = Instant::now();
        let mut r = RateLimiter::new(3, 2, t0);
        assert!(r.allow(t0));
        assert!(r.allow(t0));
        assert!(r.allow(t0));
        assert!(!r.allow(t0));
        assert_eq!(r.refused, 1);
        // 0.5 s at 2/s = one token.
        let t1 = t0 + Duration::from_millis(500);
        assert!(r.allow(t1));
        assert!(!r.allow(t1));
        // A long pause refills to the burst, never above.
        let t2 = t1 + Duration::from_secs(60);
        for _ in 0..3 {
            assert!(r.allow(t2));
        }
        assert!(!r.allow(t2));
        assert_eq!(r.refused, 3);
    }
}
