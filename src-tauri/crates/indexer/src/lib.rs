//! `FerrumIndexer`, a Windows service holding a read-only NTFS index.
//! FER-474 (IDX-S1) built the skeleton; FER-475 (IDX-S2) adds the index:
//!
//! - [`index`]: compact in-memory index of one volume.
//! - [`changelog`]: per-volume log of changed directories (FER-477).
//! - [`usn`]: strict parser of `FSCTL_READ_USN_JOURNAL` output.
//! - [`persist`]: compressed, checksummed on-disk format (pure).
//! - [`scope`]: what each privacy mode indexes.
//! - [`query`]: name search and subtree listing (FER-476), bounded, with
//!   the shared-mode AccessCheck filter (Windows: [`access`]).
//! - [`engine`]: MFT build, live USN updates, recovery, the worker loop —
//!   platform-neutral, tested on synthetic volumes.
//! - Windows only: [`volume`] (volume discovery, FSCTLs, profiles, budget).
//!
//! - [`proto`]: bounded, strict request/response frames (trust boundary on
//!   both sides of the pipe).
//! - [`config`]: the `indexer.conf` format and the privacy-safe default.
//! - [`policy`]: who may connect, who may change the mode, rate limits.
//! - Windows only: [`pipe`] (names, DACL, overlapped I/O), [`server`],
//!   [`client`], [`store`] (ProgramData ACL, config, log), [`service`]
//!   (SCM entry + install/uninstall), [`cli`].
//!
//! Design and review notes: `docs/indexer-service.md`.

pub mod changelog;
pub mod cli;
pub mod config;
pub mod engine;
pub mod index;
pub mod persist;
pub mod policy;
pub mod proto;
pub mod query;
pub mod scope;
pub mod usn;

#[cfg(windows)]
pub mod access;
#[cfg(windows)]
pub mod announce;
#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod server;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod store;
#[cfg(windows)]
pub mod volume;
#[cfg(windows)]
pub mod win;

/// File name of the service binary, installed next to `ferrum.exe`.
pub const BINARY_NAME: &str = "ferrum-indexer.exe";
/// SCM service name.
pub const SERVICE_NAME: &str = "FerrumIndexer";

/// Dev pipe suffix rule (1–40 of `[A-Za-z0-9-]`), shared by the CLI parser
/// on every platform.
pub fn pipe_suffix_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
