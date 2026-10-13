//! Production `HttpTransport` for the AI provider policy layer (FER-187/188).
//!
//! Blocking `ureq` client: call it from `spawn_blocking`. The bearer secret is only placed in the
//! `Authorization` header and never logged. In-flight HTTP cannot be interrupted by a
//! `CancelToken`; the policy layer re-checks the token when the call returns.

use std::time::Duration;

use super::provider::{CancelToken, HttpRequest, HttpResponse, HttpTransport, TransportError};

const MAX_BODY_BYTES: u64 = 256 * 1024;

pub struct UreqTransport;

impl HttpTransport for UreqTransport {
    fn post_json(
        &self,
        req: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(req.timeout))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .build()
            .into();
        let body = serde_json::to_vec(&req.body).map_err(|_| TransportError::Network)?;
        let mut response = agent
            .post(&req.url)
            .header("Authorization", &format!("Bearer {}", req.bearer.expose()))
            .header("Content-Type", "application/json")
            .send(&body[..])
            .map_err(map_error)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_string()
            .map_err(map_error)?;
        Ok(HttpResponse {
            status,
            retry_after,
            body,
        })
    }
}

fn map_error(error: ureq::Error) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => TransportError::Unreachable,
        ureq::Error::Io(ref io) if is_unreachable_io(io) => TransportError::Unreachable,
        _ => TransportError::Network,
    }
}

fn is_unreachable_io(error: &std::io::Error) -> bool {
    use std::io::ErrorKind::{ConnectionRefused, HostUnreachable, NetworkUnreachable, NotFound};
    matches!(
        error.kind(),
        ConnectionRefused | HostUnreachable | NetworkUnreachable | NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::super::provider::{
        CancelToken, Deps, NullLog, OsCredentials, ProviderConfig, StructuredMode, SystemClock,
        test_connection,
    };
    use super::super::provider::{HttpRequest, HttpTransport, Secret, TransportError};
    use super::UreqTransport;
    use std::time::Duration;

    fn request_to(port: u16) -> HttpRequest {
        HttpRequest {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            bearer: Secret::new("sk-test-DUMMY-0123456789".to_owned()),
            body: serde_json::json!({"model": "m"}),
            timeout: Duration::from_secs(5),
        }
    }

    /// FER-465: a port nobody listens on (a stopped local server) is `Unreachable`, not generic `Network`.
    #[test]
    fn closed_local_port_maps_to_unreachable() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let error = UreqTransport
            .post_json(&request_to(port), &CancelToken::default())
            .unwrap_err();
        assert_eq!(error, TransportError::Unreachable);
    }

    /// A server that accepts but never answers hits the request timeout.
    #[test]
    fn silent_server_maps_to_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut req = request_to(listener.local_addr().unwrap().port());
        req.timeout = Duration::from_millis(300);
        let error = UreqTransport
            .post_json(&req, &CancelToken::default())
            .unwrap_err();
        assert_eq!(error, TransportError::Timeout);
        drop(listener);
    }

    /// Opt-in live smoke: `FERRUM_AI_LIVE_ENDPOINT` + `FERRUM_AI_LIVE_MODEL` select a local or HTTPS
    /// endpoint; the key is read from Credential Manager (`Ferrum/AI/custom`) and never printed.
    /// Sends a fixed "ping" only. Run with `cargo test --lib ai::transport -- --ignored --nocapture`.
    #[test]
    #[ignore = "live network + OS credential"]
    fn live_connection_smoke() {
        let (Ok(endpoint), Ok(model)) = (
            std::env::var("FERRUM_AI_LIVE_ENDPOINT"),
            std::env::var("FERRUM_AI_LIVE_MODEL"),
        ) else {
            panic!("set FERRUM_AI_LIVE_ENDPOINT and FERRUM_AI_LIVE_MODEL");
        };
        let config = ProviderConfig {
            endpoint,
            model,
            timeout_ms: 30_000,
            credential_ref: "Ferrum/AI/custom".to_owned(),
            structured: StructuredMode::None,
        };
        let transport = UreqTransport;
        let clock = SystemClock::default();
        let deps = Deps {
            transport: &transport,
            credentials: &OsCredentials,
            clock: &clock,
            log: &NullLog,
        };
        let report = test_connection(&deps, &config, &CancelToken::default());
        println!("live smoke: ok={} kind={:?}", report.ok, report.kind);
        assert!(report.ok, "live smoke failed: {:?}", report.kind);
    }
}
