//! Client side of the `FerrumIndexer` IPC (FER-474), used by Ferrum and by
//! the `ferrum-indexer.exe status|set-mode` commands.
//!
//! Every response is decoded by `proto` (strict, bounded); the server is
//! verified (owner, and for the service PID/image/token against the SCM,
//! FER-480) before the first byte is sent, and every wait is bounded by a
//! timeout.

use crate::pipe::{self, Endpoint, OpenError, PipeIo};
use crate::proto::{
    self, ErrorCode, HelloInfo, IndexStatusInfo, Mode, ProtocolError, Request, Response, StatusInfo,
};
use crate::query::{
    Changes, FolderSizes, JournalMark, SearchPage, SearchQuery, SubtreeEntry, SubtreeInfo,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Debug)]
pub enum ClientError {
    /// The service is not running (no pipe).
    NotRunning,
    Busy,
    /// FER-480 review F1: the pipe NAME is held by another process (wrong
    /// owner or server PID); `pid` = that process when readable. Nothing
    /// was sent to it.
    Squatted {
        pid: Option<u32>,
        why: String,
    },
    /// The pipe's server failed another check (image, token, SCM;
    /// FER-480). Nothing was sent to it.
    Untrusted(String),
    /// The service answered with an error.
    Refused {
        code: ErrorCode,
        message: String,
    },
    /// I/O, timeout, or a response that failed strict decoding.
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning => write!(f, "service not running"),
            ClientError::Busy => write!(f, "service busy"),
            ClientError::Squatted { pid: Some(p), why } => {
                write!(f, "pipe name taken by another process (pid {p}): {why}")
            }
            ClientError::Squatted { pid: None, why } => {
                write!(f, "pipe name taken by another process: {why}")
            }
            ClientError::Untrusted(why) => write!(f, "untrusted service pipe: {why}"),
            ClientError::Refused { code, message } => {
                write!(f, "refused: {} ({message})", code.as_str())
            }
            ClientError::Protocol(m) => write!(f, "protocol: {m}"),
        }
    }
}

impl From<ProtocolError> for ClientError {
    fn from(e: ProtocolError) -> Self {
        ClientError::Protocol(e.to_string())
    }
}

pub struct Client {
    io: PipeIo,
    pub hello: HelloInfo,
}

impl Client {
    /// Connect, verify the server, and complete the hello.
    pub fn connect(endpoint: &Endpoint, timeout: Duration) -> Result<Client, ClientError> {
        let h = pipe::open_client(endpoint, timeout).map_err(open_error)?;
        Self::hello(h, timeout)
    }

    /// [`Client::connect`] verifying the server against `expect` (FER-480;
    /// tests and dev pipes — the service endpoint always verifies against
    /// the SCM in [`Client::connect`]).
    pub fn connect_expect(
        endpoint: &Endpoint,
        timeout: Duration,
        expect: &pipe::ExpectedServer,
    ) -> Result<Client, ClientError> {
        let h = pipe::open_client_expect(endpoint, timeout, expect).map_err(open_error)?;
        Self::hello(h, timeout)
    }

    fn hello(h: crate::win::OwnedHandle, timeout: Duration) -> Result<Client, ClientError> {
        let mut io = PipeIo::new(h, Arc::new(AtomicBool::new(false)))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        io.read_timeout = timeout;
        io.write_timeout = timeout;
        let mut c = Client {
            io,
            hello: HelloInfo {
                proto: 0,
                can_lower_privacy: false,
            },
        };
        match c.call(&Request::Hello)? {
            Response::HelloOk(h) => c.hello = h,
            other => return Err(unexpected(other)),
        }
        Ok(c)
    }

    /// Per-call timeout for the reads (and writes) that follow — for a
    /// query the service may take longer to answer than the hello (FER-477
    /// `changes` waits for its freshness barrier).
    pub fn set_timeout(&mut self, t: Duration) {
        self.io.read_timeout = t;
        self.io.write_timeout = t;
    }

    /// One request → one response. A `Response::Error` becomes `Refused`.
    pub fn call(&mut self, req: &Request) -> Result<Response, ClientError> {
        self.io
            .write_all(&proto::encode_request(req))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        match proto::read_response(&mut self.io)? {
            Response::Error { code, message } => Err(ClientError::Refused { code, message }),
            r => Ok(r),
        }
    }

    pub fn ping(&mut self, token: u64) -> Result<(), ClientError> {
        match self.call(&Request::Ping { token })? {
            Response::Pong { token: t } if t == token => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    pub fn status(&mut self) -> Result<StatusInfo, ClientError> {
        match self.call(&Request::Status)? {
            Response::Status(s) => Ok(s),
            other => Err(unexpected(other)),
        }
    }

    pub fn version(&mut self) -> Result<(String, u16), ClientError> {
        match self.call(&Request::Version)? {
            Response::Version { version, proto } => Ok((version, proto)),
            other => Err(unexpected(other)),
        }
    }

    pub fn get_mode(&mut self) -> Result<Mode, ClientError> {
        match self.call(&Request::GetMode)? {
            Response::Mode(m) => Ok(m),
            other => Err(unexpected(other)),
        }
    }

    /// Per-volume index state (FER-475).
    pub fn index_status(&mut self) -> Result<IndexStatusInfo, ClientError> {
        match self.call(&Request::IndexStatus)? {
            Response::IndexStatus(s) => Ok(s),
            other => Err(unexpected(other)),
        }
    }

    /// One page of a name search (FER-476).
    pub fn search(&mut self, q: &SearchQuery) -> Result<SearchPage, ClientError> {
        match self.call(&Request::Search(q.clone()))? {
            Response::SearchPage(p) => Ok(p),
            other => Err(unexpected(other)),
        }
    }

    /// Every visible entry below `path` (FER-476, Disk Usage). `sink` gets
    /// each chunk; returning false stops reading (the connection must then
    /// be dropped). `max_entries` bounds what this client accepts.
    pub fn subtree(
        &mut self,
        path: &str,
        max_entries: u64,
        sink: &mut dyn FnMut(&SubtreeInfo, Vec<SubtreeEntry>) -> bool,
    ) -> Result<SubtreeInfo, ClientError> {
        self.io
            .write_all(&proto::encode_request(&Request::Subtree {
                path: path.to_owned(),
            }))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        let mut first: Option<SubtreeInfo> = None;
        let mut got = 0u64;
        loop {
            match proto::read_response(&mut self.io)? {
                Response::SubtreeChunk(c) => {
                    // The header must not change within one answer.
                    if first.is_some_and(|f| f != c.info) {
                        return Err(ClientError::Protocol("subtree header changed".into()));
                    }
                    first = Some(c.info);
                    got += c.entries.len() as u64;
                    if got > max_entries {
                        return Err(ClientError::Protocol("subtree too large".into()));
                    }
                    if !c.entries.is_empty() && !sink(&c.info, c.entries) {
                        return Err(ClientError::Protocol("stopped".into()));
                    }
                    if c.done {
                        return Ok(c.info);
                    }
                }
                Response::Error { code, message } => {
                    return Err(ClientError::Refused { code, message });
                }
                other => return Err(unexpected(other)),
            }
        }
    }

    /// FER-477a: total size of each folder `parent\name` (high_performance
    /// only: shared mode answers `Refused { Forbidden }`). The answer must
    /// carry exactly one size per name.
    pub fn folder_sizes(
        &mut self,
        parent: &str,
        names: &[String],
    ) -> Result<FolderSizes, ClientError> {
        match self.call(&Request::FolderSizes {
            parent: parent.to_owned(),
            names: names.to_vec(),
        })? {
            Response::FolderSizes(r) if r.sizes.len() == names.len() => Ok(r),
            other => Err(unexpected(other)),
        }
    }

    /// FER-477b: directories below `path` changed since `since` (or only
    /// the current journal position). The service may wait up to
    /// `server::CHANGES_BARRIER_TIME` for its index to catch up: use a
    /// client timeout above that.
    pub fn changes(
        &mut self,
        path: &str,
        since: Option<JournalMark>,
    ) -> Result<Changes, ClientError> {
        match self.call(&Request::Changes {
            path: path.to_owned(),
            since,
        })? {
            Response::Changes(c) => Ok(c),
            other => Err(unexpected(other)),
        }
    }

    /// Returns whether the mode actually changed.
    pub fn set_mode(&mut self, mode: Mode) -> Result<bool, ClientError> {
        match self.call(&Request::SetMode(mode))? {
            Response::ModeSet { mode: m, changed } if m == mode => Ok(changed),
            other => Err(unexpected(other)),
        }
    }
}

fn open_error(e: OpenError) -> ClientError {
    match e {
        OpenError::NotRunning => ClientError::NotRunning,
        OpenError::Busy => ClientError::Busy,
        OpenError::Squatted { pid, why } => ClientError::Squatted { pid, why },
        OpenError::Untrusted(why) => ClientError::Untrusted(why),
        OpenError::Io(e) => ClientError::Protocol(e.to_string()),
    }
}

fn unexpected(r: Response) -> ClientError {
    ClientError::Protocol(format!("unexpected response {r:?}"))
}
