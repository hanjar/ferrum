//! FER-460 fast NTFS scan: everything that runs inside the elevated
//! `ferrum-scan-helper.exe`, plus the wire protocol Ferrum decodes.
//!
//! - [`args`]: strict argv validation (drive letter + pipe name + nonce).
//! - [`protocol`]: bounded frames; the decoder is Ferrum's trust boundary.
//! - [`mft`]: read-only MFT parser over a raw-volume reader (pure, tested
//!   on synthetic images).
//! - [`helper`] (Windows): the helper's `main` — connect, verify, scan,
//!   stream, exit. No writes, no network, no child processes.

pub mod args;
pub mod mft;
pub mod protocol;

#[cfg(windows)]
pub mod helper;
#[cfg(windows)]
pub mod token;
