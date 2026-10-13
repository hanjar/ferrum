//! Read-only adapter that lets `compare_files` address archive members
//! (FER-413) without extracting anything to disk.
//!
//! Path convention (the one `compare.rs` already used for its "archive
//! member" hint and that `Location::Archive` models): an OS path whose
//! ancestor is an existing archive FILE, `C:\x\a.zip\dir\inner.txt`. The part
//! up to the archive file is `archive_path`; the rest, joined with '/', is
//! the `MemberPath` (so `..`, reserved names, drive letters etc. are rejected
//! by the same grammar the archive layer uses).
//!
//! Reading goes through the public `ArchiveProvider::read` API only. That API
//! is push-style (callback per chunk, bounded by `max_bytes`), so a worker
//! thread drives it and hands chunks over a small bounded channel; the
//! consumer sees a plain `Read`. At most `QUEUE_DEPTH` chunks (64 KiB each)
//! are buffered, so memory stays constant regardless of member size. Dropping
//! the reader cancels the worker's token and closes the channel, then joins
//! it. The token matters for solid 7z: the members in front of the requested
//! one are drained without sending anything, so a closed channel alone would
//! not stop that drain (FER-498).
//!
//! Supported: ZIP (`.zip`) and 7z (`.7z`, `.7zip`). 7z decodes sequentially:
//! non-solid archives cost about the member size, solid archives re-decode
//! preceding data. RAR is NOT supported: the RAR provider materialises the
//! whole member in memory before its first chunk, which is not a bounded
//! stream. Other archive-like extensions have no read provider at all.
//! Encrypted archives are not supported (compare carries no password).
//!
//! Limits: a member whose declared size exceeds `MEMBER_MAX_BYTES` (the
//! 512 MiB per-entry limit the archive layer already uses) is refused, and a
//! stream that yields more or fewer bytes than the declared size is an error
//! (zip-bomb / lying header), never a verdict.

use crate::archive_contract::{
    ArchiveError, ArchiveErrorCode, ArchiveListRequest, ArchiveProvider, ArchiveReadRequest,
    CancelToken, Location, MemberPath,
};
use crate::domain::{ErrorCode, FsError};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::UNIX_EPOCH;

/// Same per-entry ceiling the archive layer applies to extraction/creation.
pub(crate) const MEMBER_MAX_BYTES: u64 = 512 * 1024 * 1024;
const QUEUE_DEPTH: usize = 4;

const ARCHIVE_EXTS: &[&str] = &[
    "zip", "7z", "7zip", "rar", "tar", "gz", "tgz", "bz2", "xz", "jar", "apk", "cbz",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Zip,
    SevenZ,
}

/// A resolved, existing, non-extracted archive member.
#[derive(Debug, Clone)]
pub(crate) struct MemberRef {
    pub archive_path: String,
    pub member: MemberPath,
    format: Format,
    pub size: u64,
    pub is_dir: bool,
    pub archive_size: u64,
    pub archive_mtime_ns: Option<u128>,
}

impl MemberRef {
    /// Cache identity: archive path + archive size + member path, paired with
    /// the archive mtime. `None` when the archive mtime is unknown.
    pub fn cache_ident(&self) -> Option<(String, u128)> {
        let m = self.archive_mtime_ns?;
        Some((
            format!(
                "{}\u{1}{}\u{1}{}",
                self.archive_path,
                self.archive_size,
                self.member.as_str()
            ),
            m,
        ))
    }

    pub fn open(&self) -> Result<Box<dyn Read + Send>, FsError> {
        Ok(Box::new(MemberReader::spawn(self)))
    }
}

fn unsupported(msg: impl Into<String>, path: &str) -> FsError {
    FsError::new(ErrorCode::Unsupported, msg).with_path(path)
}

/// Archive-layer error -> `FsError`. Format/codec/password problems are
/// `Unsupported` for compare (no password channel); the rest are `Io`.
pub(crate) fn archive_err(e: &ArchiveError, archive_path: &str) -> FsError {
    let code = match e.code {
        ArchiveErrorCode::UnsupportedFormat
        | ArchiveErrorCode::UnsupportedCodec
        | ArchiveErrorCode::PasswordRequired
        | ArchiveErrorCode::BadPassword => ErrorCode::Unsupported,
        ArchiveErrorCode::InvalidMemberPath => ErrorCode::InvalidPath,
        ArchiveErrorCode::Cancelled => ErrorCode::Cancelled,
        _ => ErrorCode::Io,
    };
    FsError::new(code, e.message.clone()).with_path(archive_path)
}

/// Map an I/O error from a side's reader: archive errors carried inside the
/// `io::Error` keep their typed code, anything else is a plain I/O error.
pub(crate) fn io_to_fs(e: io::Error, path: &str) -> FsError {
    if let Some(a) = e.get_ref().and_then(|i| i.downcast_ref::<ArchiveError>()) {
        return archive_err(a, path);
    }
    FsError::from(e).with_path(path)
}

/// Split `path` into (archive file, member) when an ancestor is an existing
/// archive file. `Ok(None)` = not an archive-member address.
fn split(path: &str) -> Result<Option<(String, String)>, FsError> {
    let p = Path::new(path);
    for anc in p.ancestors().skip(1) {
        let is_archive = anc.is_file()
            && anc
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ARCHIVE_EXTS.contains(&e.to_ascii_lowercase().as_str()));
        if !is_archive {
            continue;
        }
        let rel = p.strip_prefix(anc).map_err(|_| {
            FsError::new(ErrorCode::InvalidPath, "Invalid archive member address").with_path(path)
        })?;
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(s) => parts.push(s.to_str().ok_or_else(|| {
                    FsError::new(ErrorCode::InvalidPath, "Archive member path is not UTF-8")
                        .with_path(path)
                })?),
                _ => {
                    return Err(FsError::new(
                        ErrorCode::InvalidPath,
                        "Invalid archive member path",
                    )
                    .with_path(path));
                }
            }
        }
        return Ok(Some((anc.to_string_lossy().into_owned(), parts.join("/"))));
    }
    Ok(None)
}

fn format_of(archive_path: &str) -> Result<Format, FsError> {
    let ext = Path::new(archive_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" => Ok(Format::Zip),
        "7z" | "7zip" => Ok(Format::SevenZ),
        "rar" => Err(unsupported(
            "Comparing RAR members is not supported: the RAR reader loads the whole member \
             into memory (no bounded stream); extract it first",
            archive_path,
        )),
        other => Err(unsupported(
            format!(
                "Comparing members of .{other} archives is not supported (no read provider); extract them first"
            ),
            archive_path,
        )),
    }
}

fn provider(format: Format) -> Box<dyn ArchiveProvider + Send + Sync> {
    match format {
        Format::Zip => Box::new(crate::archive_zip::ZipProvider::new()),
        Format::SevenZ => Box::new(crate::archive_sevenz::SevenZProvider::new()),
    }
}

/// If `path` addresses a member of an existing archive, resolve it (metadata
/// only — the central directory/header is listed, no member data is read).
/// `Ok(None)` = `path` is not inside an archive.
pub(crate) fn resolve(path: &str) -> Result<Option<MemberRef>, FsError> {
    let Some((archive_path, rel)) = split(path)? else {
        return Ok(None);
    };
    let format = format_of(&archive_path).map_err(|e| e.with_path(path))?;
    let member = MemberPath::parse(&rel)
        .map_err(|e| FsError::new(ErrorCode::InvalidPath, e.message).with_path(path))?;
    let ameta = fs::metadata(&archive_path)
        .map_err(|e| FsError::from(e).with_path(archive_path.as_str()))?;
    let parent = member.parent().ok_or_else(|| {
        FsError::new(ErrorCode::InvalidPath, "Archive member path is empty").with_path(path)
    })?;
    let listing = provider(format)
        .list(&ArchiveListRequest {
            location: Location::Archive {
                archive_path: archive_path.clone(),
                member_path: parent,
            },
            request_token: 0,
            // Compare of header-encrypted archives stays out of FER-504 scope.
            password: None,
        })
        .map_err(|e| archive_err(&e, &archive_path))?;
    let entry = listing
        .entries
        .iter()
        .find(|e| e.member_path == member)
        .ok_or_else(|| {
            FsError::new(ErrorCode::NotFound, "Archive member not found").with_path(path)
        })?;
    if !entry.is_dir && entry.size > MEMBER_MAX_BYTES {
        return Err(FsError::new(
            ErrorCode::Io,
            format!(
                "Archive member is {} bytes, over the {} byte compare limit",
                entry.size, MEMBER_MAX_BYTES
            ),
        )
        .with_path(path));
    }
    Ok(Some(MemberRef {
        archive_path,
        member,
        format,
        size: entry.size,
        is_dir: entry.is_dir,
        archive_size: ameta.len(),
        archive_mtime_ns: ameta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos()),
    }))
}

enum Msg {
    Data(Vec<u8>),
    End,
    Fail(ArchiveError),
}

/// Pull-style `Read` over the provider's push-style bounded read.
struct MemberReader {
    rx: Option<Receiver<Msg>>,
    cur: Vec<u8>,
    pos: usize,
    finished: bool,
    worker: Option<JoinHandle<()>>,
    /// Installed on the worker thread; the 7z skip/read loops poll it per 64 KiB chunk.
    token: CancelToken,
}

impl MemberReader {
    fn spawn(m: &MemberRef) -> Self {
        Self::spawn_with(m, CancelToken::new())
    }

    fn spawn_with(m: &MemberRef, token: CancelToken) -> Self {
        let (tx, rx) = sync_channel::<Msg>(QUEUE_DEPTH);
        let request = ArchiveReadRequest {
            location: Location::Archive {
                archive_path: m.archive_path.clone(),
                member_path: m.member.clone(),
            },
            // +1 so a stream longer than the declared size is observable.
            max_bytes: m.size.saturating_add(1),
            password: None,
        };
        let (format, declared) = (m.format, m.size);
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            crate::archive_contract::with_cancel(&worker_token, || {
                run_worker(format, request, declared, tx)
            })
        });
        Self {
            rx: Some(rx),
            cur: Vec::new(),
            pos: 0,
            finished: false,
            worker: Some(worker),
            token,
        }
    }
}

fn run_worker(format: Format, request: ArchiveReadRequest, declared: u64, tx: SyncSender<Msg>) {
    let mut total = 0u64;
    let result = provider(format).read(&request, &mut |chunk| {
        total += chunk.len() as u64;
        if total > declared {
            return Err(ArchiveError::new(
                ArchiveErrorCode::LimitExceeded,
                "Archive member is larger than its declared size",
            ));
        }
        tx.send(Msg::Data(chunk.to_vec()))
            .map_err(|_| ArchiveError::new(ArchiveErrorCode::Cancelled, "reader closed"))
    });
    let last = match result {
        Ok(true) => Msg::Fail(ArchiveError::new(
            ArchiveErrorCode::LimitExceeded,
            "Archive member is larger than its declared size",
        )),
        Ok(false) if total < declared => Msg::Fail(ArchiveError::new(
            ArchiveErrorCode::CorruptArchive,
            "Archive member is shorter than its declared size",
        )),
        Ok(false) => Msg::End,
        Err(e) => Msg::Fail(e),
    };
    let _ = tx.send(last); // receiver may already be gone (cancel)
}

impl Read for MemberReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pos < self.cur.len() {
                let n = buf.len().min(self.cur.len() - self.pos);
                buf[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if self.finished {
                return Ok(0);
            }
            let Some(rx) = &self.rx else { return Ok(0) };
            match rx.recv() {
                Ok(Msg::Data(v)) => {
                    self.cur = v;
                    self.pos = 0;
                }
                Ok(Msg::End) => {
                    self.finished = true;
                    return Ok(0);
                }
                Ok(Msg::Fail(e)) => {
                    self.finished = true;
                    return Err(io::Error::other(e));
                }
                Err(_) => {
                    self.finished = true;
                    return Err(io::Error::other(ArchiveError::new(
                        ArchiveErrorCode::Io,
                        "Archive reader stopped unexpectedly",
                    )));
                }
            }
        }
    }
}

impl Drop for MemberReader {
    fn drop(&mut self) {
        // Token first: stops a solid-7z drain that sends nothing, within one chunk.
        self.token.cancel();
        self.rx.take(); // unblocks a worker waiting in send()
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solid 7z: a big member nobody asked for, then the requested one behind it.
    fn solid_with_tail(tag: &str) -> (std::path::PathBuf, MemberRef) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        let path = std::env::temp_dir().join(format!(
            "ferrum-fer498-compare-{tag}-{}.7z",
            std::process::id()
        ));
        let big = vec![0u8; 4 * 1024 * 1024];
        let tail = b"tail".to_vec();
        let mut w = ArchiveWriter::new(fs::File::create(&path).unwrap()).unwrap();
        w.push_archive_entries(
            vec![
                ArchiveEntry::new_file("big.bin"),
                ArchiveEntry::new_file("tail.txt"),
            ],
            vec![
                SourceReader::new(big.as_slice()),
                SourceReader::new(tail.as_slice()),
            ],
        )
        .unwrap();
        w.finish().unwrap();
        let m = MemberRef {
            archive_path: path.to_string_lossy().into_owned(),
            member: MemberPath::parse("tail.txt").unwrap(),
            format: Format::SevenZ,
            size: 4,
            is_dir: false,
            archive_size: 0,
            archive_mtime_ns: None,
        };
        (path, m)
    }

    #[test]
    fn the_worker_honours_its_cancel_token_while_draining_a_solid_block() {
        // FER-498: the drain of big.bin sends nothing, so only the token can stop it.
        let (path, m) = solid_with_tail("token");
        let token = CancelToken::new();
        token.cancel();
        let mut reader = MemberReader::spawn_with(&m, token);
        let mut out = Vec::new();
        let err = reader.read_to_end(&mut out).unwrap_err();
        let inner = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<ArchiveError>())
            .expect("archive error");
        assert_eq!(inner.code, ArchiveErrorCode::Cancelled);
        assert!(out.is_empty());
        drop(reader);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn dropping_the_reader_cancels_the_worker_token() {
        let (path, m) = solid_with_tail("drop");
        let reader = MemberReader::spawn(&m);
        let token = reader.token.clone();
        drop(reader); // joins the worker
        assert!(token.is_cancelled());
        // Not cancelled: the same member reads normally.
        let mut out = Vec::new();
        MemberReader::spawn(&m).read_to_end(&mut out).unwrap();
        assert_eq!(out, b"tail");
        let _ = fs::remove_file(&path);
    }
}
