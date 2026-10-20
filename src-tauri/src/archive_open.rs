//! FER-575: open ONE member of an archive with the OS default app.
//!
//! The pane shows an archive as a virtual folder (`C:\x\a.rar\docs\report.pdf`), a path Windows
//! cannot open. This module extracts that single member through the normal fail-closed extract
//! pipeline (`archive_jobs::run_extract` -> provider -> `archive_extract::extract`: member-path
//! rules, reserved names, no-follow handles, staging + commit, quotas) into a private temp
//! folder owned by Ferrum, and returns the extracted file so the caller can hand it to the
//! `shell_open` path (native confirmation for executables, scripts and installers stays there).
//!
//! Layout: `<root>\s<unix-secs>-<pid>\<key>\<member relative path>`, where `<root>` is
//! `<app local data>\open-temp` and `<key>` hashes the archive identity (path, size, mtime) and
//! the member. The same member of an unchanged archive therefore reuses its extracted file; a
//! changed archive gets a new key and is extracted again. Sessions older than a day are removed at
//! start. Every deletion is limited to direct children of `<root>` that match the session-name
//! pattern, and reparse points are unlinked, never followed.
//!
//! Mark of the Web: when the archive file carries a `Zone.Identifier` stream, the extracted file
//! gets a copy (as Explorer and 7-Zip do), so Office and SmartScreen treat it as downloaded.
//!
//! Nothing here logs or stores a password; it only travels inside the extract request.

use crate::archive_contract::{
    ArchiveConflictPolicy, ArchiveError, ArchiveErrorCode, ArchiveExtractRequest, ArchiveLimits,
    ArchiveListRequest, ArchiveProvider, Location, MemberPath,
};
use crate::archive_jobs::ArchiveProgressEvent;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Largest member opened this way (same ceiling as the extract bridge's per-entry limit).
pub const MAX_OPEN_BYTES: u64 = 512 * 1024 * 1024;
/// Session folders older than this are removed at start.
pub const STALE_AFTER_SECS: u64 = 24 * 60 * 60;
/// A Zone.Identifier stream is a few bytes; refuse to copy anything absurd.
const MAX_ZONE_BYTES: u64 = 64 * 1024;

fn err(code: ArchiveErrorCode, message: impl Into<String>) -> ArchiveError {
    ArchiveError::new(code, message)
}

fn io_err(what: &str, e: std::io::Error) -> ArchiveError {
    err(ArchiveErrorCode::Io, format!("{what}: {e}"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True for a symlink or any Windows reparse point (junction, mount point, ...).
fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// `path` exists and is a real directory (not a link of any kind).
fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !is_link(&m))
}

/// Ferrum's temp area for opened archive members, with this run's session folder name.
#[derive(Debug, Clone)]
pub struct OpenTemp {
    root: PathBuf,
    session: String,
}

impl OpenTemp {
    /// `root` is the dedicated `open-temp` folder (created on demand).
    pub fn new(root: PathBuf) -> Self {
        Self::with_session(root, now_secs(), std::process::id())
    }

    fn with_session(root: PathBuf, secs: u64, pid: u32) -> Self {
        Self {
            root,
            session: format!("s{secs}-{pid}"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_dir(&self) -> PathBuf {
        self.root.join(&self.session)
    }

    /// Create `root` and this session's folder, refusing links anywhere on the way.
    fn ensure_session(&self) -> Result<PathBuf, ArchiveError> {
        for dir in [self.root.clone(), self.session_dir()] {
            match std::fs::symlink_metadata(&dir) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir_all(&dir).map_err(|e| io_err("temp folder", e))?;
                }
                Err(e) => return Err(io_err("temp folder", e)),
            }
            if !is_plain_dir(&dir) {
                return Err(err(
                    ArchiveErrorCode::UnsafeEntry,
                    "The temp folder is not a plain directory",
                ));
            }
        }
        Ok(self.session_dir())
    }

    /// Remove session folders older than `max_age_secs` (never the current one). Best effort: a
    /// file another program still holds open is skipped and tried again at the next start.
    /// Returns the number of session folders removed completely.
    pub fn cleanup_stale(&self, now: u64, max_age_secs: u64) -> usize {
        // The root itself must be a plain directory, or nothing below it is touched.
        if !is_plain_dir(&self.root) {
            return 0;
        }
        let Ok(read) = std::fs::read_dir(&self.root) else {
            return 0;
        };
        let mut removed = 0;
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(started) = parse_session(&name) else {
                continue; // not ours: leave it alone
            };
            if name == self.session || now.saturating_sub(started) <= max_age_secs {
                continue;
            }
            if remove_tree(&entry.path()) {
                removed += 1;
            }
        }
        removed
    }
}

/// `s<secs>-<pid>` -> secs. Anything else is not a session folder.
fn parse_session(name: &str) -> Option<u64> {
    let rest = name.strip_prefix('s')?;
    let (secs, pid) = rest.split_once('-')?;
    if secs.is_empty()
        || pid.is_empty()
        || !secs.bytes().all(|b| b.is_ascii_digit())
        || !pid.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    secs.parse().ok()
}

/// Delete `path` and everything below it without ever following a link: a symlink/junction is
/// unlinked itself. Keeps going past entries that cannot be removed (locked files). True when
/// `path` is gone afterwards.
fn remove_tree(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return true; // already gone
    };
    if is_link(&meta) {
        // A directory link is removed with remove_dir, a file link with remove_file; neither
        // touches the target.
        return std::fs::remove_dir(path)
            .or_else(|_| std::fs::remove_file(path))
            .is_ok();
    }
    if meta.is_dir() {
        let mut clean = true;
        if let Ok(read) = std::fs::read_dir(path) {
            for entry in read.flatten() {
                clean &= remove_tree(&entry.path());
            }
        }
        return clean && std::fs::remove_dir(path).is_ok();
    }
    if std::fs::remove_file(path).is_ok() {
        return true;
    }
    // A read-only file: clear the flag once, then try again.
    let mut perms = meta.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    let _ = std::fs::set_permissions(path, perms);
    std::fs::remove_file(path).is_ok()
}

/// Key of one (archive identity, member) pair.
fn member_key(archive_path: &str, size: u64, mtime_ms: u128, member: &str) -> String {
    let mut h = Sha256::new();
    h.update(archive_path.to_lowercase().replace('/', "\\").as_bytes());
    h.update([0]);
    h.update(size.to_le_bytes());
    h.update(mtime_ms.to_le_bytes());
    h.update(member.as_bytes());
    hex::encode(&h.finalize()[..12])
}

/// The single regular file below `dir` (the extracted member), skipping staging leftovers.
fn find_extracted(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>, depth: u32) {
        if depth > 40 || found.len() > 1 {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if is_link(&meta) {
                found.push(path); // makes the result ambiguous -> refused below
                found.push(PathBuf::new());
            } else if meta.is_dir() {
                walk(&path, found, depth + 1);
            } else if !entry
                .file_name()
                .to_string_lossy()
                .contains(".ferrum-partial-")
            {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found, 0);
    if found.len() == 1 { found.pop() } else { None }
}

/// Copy the archive's `Zone.Identifier` stream (if any) onto `target`. Best effort for a missing
/// source; an archive WITH a stream whose copy fails is an error, because the file would then
/// open without the internet-origin marking.
#[cfg(windows)]
fn copy_motw(archive: &Path, target: &Path) -> Result<(), ArchiveError> {
    use std::io::Read;
    let stream = |p: &Path| {
        let mut s = p.as_os_str().to_owned();
        s.push(":Zone.Identifier");
        PathBuf::from(s)
    };
    let Ok(file) = std::fs::File::open(stream(archive)) else {
        return Ok(()); // no mark on the archive
    };
    let mut data = Vec::new();
    file.take(MAX_ZONE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| io_err("zone identifier", e))?;
    if data.len() as u64 > MAX_ZONE_BYTES {
        return Err(err(
            ArchiveErrorCode::Io,
            "The archive's Zone.Identifier stream is too large",
        ));
    }
    std::fs::write(stream(target), data).map_err(|e| io_err("zone identifier", e))
}

#[cfg(not(windows))]
fn copy_motw(_archive: &Path, _target: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

/// Extract `member_path` of `archive_path` into `temp` (or reuse an earlier extraction of the same
/// unchanged archive) and return the file. `request_id` makes the job cancellable and tags the
/// progress events passed to `emit`.
pub fn extract_member(
    provider: &dyn ArchiveProvider,
    temp: &OpenTemp,
    archive_path: &str,
    member_path: &str,
    password: Option<&str>,
    request_id: Option<&str>,
    emit: &mut dyn FnMut(ArchiveProgressEvent),
) -> Result<PathBuf, ArchiveError> {
    let member = MemberPath::parse(member_path)?;
    if member.as_str().is_empty() {
        return Err(err(
            ArchiveErrorCode::InvalidMemberPath,
            "No file selected inside the archive",
        ));
    }
    let archive = Path::new(archive_path);
    let meta = std::fs::metadata(archive).map_err(|e| io_err("archive", e))?;
    if !meta.is_file() {
        return Err(err(ArchiveErrorCode::Io, "The archive is not a file"));
    }
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis());

    // The member must exist, be a file, and fit the size ceiling (also lists header-encrypted
    // archives, which answer PasswordRequired / BadPassword here).
    let parent = member
        .parent()
        .unwrap_or_else(|| MemberPath::parse("").expect("empty path is the root"));
    let listing = provider.list(&ArchiveListRequest {
        location: Location::Archive {
            archive_path: archive_path.to_owned(),
            member_path: parent,
        },
        request_token: 0,
        password: password.map(str::to_owned),
    })?;
    let entry = listing
        .entries
        .iter()
        .find(|e| e.member_path == member)
        .ok_or_else(|| {
            err(
                ArchiveErrorCode::InvalidMemberPath,
                "The file was not found inside the archive",
            )
        })?;
    if entry.is_dir {
        return Err(err(
            ArchiveErrorCode::InvalidMemberPath,
            "A folder cannot be opened as a file",
        ));
    }
    if entry.size > MAX_OPEN_BYTES {
        return Err(err(
            ArchiveErrorCode::LimitExceeded,
            format!(
                "The file is larger than {} MB, the limit for opening from an archive. Extract it instead.",
                MAX_OPEN_BYTES / (1024 * 1024)
            ),
        ));
    }

    let session = temp.ensure_session()?;
    let key_dir = session.join(member_key(
        archive_path,
        meta.len(),
        mtime_ms,
        member.as_str(),
    ));

    // Reuse an earlier, complete extraction of this very member of this unchanged archive.
    if is_plain_dir(&key_dir) {
        if let Some(file) = find_extracted(&key_dir) {
            copy_motw(archive, &file)?;
            return Ok(file);
        }
        // Left over from a failed or concurrent attempt: start clean.
        if !remove_tree(&key_dir) {
            return Err(err(
                ArchiveErrorCode::Io,
                "A stale temp folder could not be cleared",
            ));
        }
    } else if std::fs::symlink_metadata(&key_dir).is_ok() {
        return Err(err(
            ArchiveErrorCode::UnsafeEntry,
            "The temp folder is not a plain directory",
        ));
    }
    std::fs::create_dir(&key_dir).map_err(|e| io_err("temp folder", e))?;
    if !is_plain_dir(&key_dir) {
        let _ = remove_tree(&key_dir);
        return Err(err(
            ArchiveErrorCode::UnsafeEntry,
            "The temp folder is not a plain directory",
        ));
    }

    let request = ArchiveExtractRequest {
        archive_path: archive_path.to_owned(),
        member_paths: vec![member.clone()],
        destination_path: key_dir.to_string_lossy().into_owned(),
        conflict: ArchiveConflictPolicy::Overwrite,
        limits: ArchiveLimits {
            max_entries: 100_000,
            max_total_bytes: MAX_OPEN_BYTES,
            max_entry_bytes: MAX_OPEN_BYTES,
            max_depth: 32,
        },
        password: password.map(str::to_owned),
    };
    let outcome = crate::archive_jobs::run_extract(provider, &request, request_id, emit)
        .and_then(|()| {
            find_extracted(&key_dir).ok_or_else(|| {
                err(
                    ArchiveErrorCode::Io,
                    "The file was not extracted from the archive",
                )
            })
        })
        .and_then(|file| copy_motw(archive, &file).map(|()| file));
    match outcome {
        Ok(file) => Ok(file),
        Err(e) => {
            let _ = remove_tree(&key_dir);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in files {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn noop(_: ArchiveProgressEvent) {}

    fn open(temp: &OpenTemp, archive: &Path, member: &str) -> Result<PathBuf, ArchiveError> {
        extract_member(
            &crate::archive_zip::ZipProvider::new(),
            temp,
            &archive.to_string_lossy(),
            member,
            None,
            None,
            &mut noop,
        )
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        archive: PathBuf,
        temp: OpenTemp,
    }

    fn fixture(files: &[(&str, &[u8])]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        write_zip(&archive, files);
        let temp = OpenTemp::new(dir.path().join("open-temp"));
        Fixture {
            _dir: dir,
            archive,
            temp,
        }
    }

    #[test]
    fn extracts_only_the_requested_member_under_the_session_folder() {
        let f = fixture(&[
            ("docs/report.pdf", b"%PDF-1"),
            ("docs/other.txt", b"other"),
            ("top.txt", b"top"),
        ]);
        let out = open(&f.temp, &f.archive, "docs/report.pdf").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"%PDF-1");
        assert!(out.starts_with(f.temp.session_dir()));
        assert_eq!(out.file_name().unwrap(), "report.pdf");
        // Nothing else of the archive was written.
        let mut count = 0;
        let mut stack = vec![f.temp.session_dir()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    count += 1;
                }
            }
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn same_member_of_an_unchanged_archive_reuses_the_file() {
        let f = fixture(&[("a/b.txt", b"one")]);
        let first = open(&f.temp, &f.archive, "a/b.txt").unwrap();
        // The user's app may have saved a change; reuse keeps the same file.
        std::fs::write(&first, b"edited").unwrap();
        let second = open(&f.temp, &f.archive, "a/b.txt").unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&second).unwrap(), b"edited");
    }

    #[test]
    fn changed_archive_extracts_again_to_a_new_folder() {
        let f = fixture(&[("a.txt", b"old")]);
        let first = open(&f.temp, &f.archive, "a.txt").unwrap();
        write_zip(&f.archive, &[("a.txt", b"brand new content")]);
        let second = open(&f.temp, &f.archive, "a.txt").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&second).unwrap(), b"brand new content");
    }

    #[test]
    fn folder_missing_and_hostile_members_are_refused() {
        let f = fixture(&[("dir/x.txt", b"x")]);
        for bad in [
            "",
            "dir",
            "nope.txt",
            "../evil.txt",
            "dir/../x",
            "a\\b",
            "CON",
            "x:y",
        ] {
            assert!(open(&f.temp, &f.archive, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn oversized_member_reports_the_limit() {
        // The declared size comes from the archive listing; a sparse-looking big file is not
        // needed, only the check against the ceiling.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("big.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default().large_file(true);
        zip.start_file("big.bin", opts).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..513 {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();
        let temp = OpenTemp::new(dir.path().join("open-temp"));
        let e = open(&temp, &archive, "big.bin").unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::LimitExceeded);
        assert!(e.message.contains("512"));
        // A refused open leaves no temp folder behind.
        let left = std::fs::read_dir(temp.root())
            .map(|r| r.flatten().count())
            .unwrap_or(0);
        assert!(left <= 1);
    }

    #[test]
    fn encrypted_member_asks_for_a_password() {
        // A ZIP whose only member is AES encrypted.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("enc.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .with_aes_encryption(zip::AesMode::Aes256, "secret");
        zip.start_file("s.txt", opts).unwrap();
        zip.write_all(b"classified").unwrap();
        zip.finish().unwrap();
        let temp = OpenTemp::new(dir.path().join("open-temp"));
        let e = open(&temp, &archive, "s.txt").unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::PasswordRequired);
        // And the right password opens it; the password is only in the request.
        let out = extract_member(
            &crate::archive_zip::ZipProvider::new(),
            &temp,
            &archive.to_string_lossy(),
            "s.txt",
            Some("secret"),
            None,
            &mut noop,
        )
        .unwrap();
        assert_eq!(std::fs::read(out).unwrap(), b"classified");
    }

    fn open_with(
        provider: &dyn ArchiveProvider,
        temp: &OpenTemp,
        archive: &Path,
        member: &str,
        password: Option<&str>,
    ) -> Result<PathBuf, ArchiveError> {
        extract_member(
            provider,
            temp,
            &archive.to_string_lossy(),
            member,
            password,
            None,
            &mut noop,
        )
    }

    #[test]
    fn opens_a_member_of_a_7z() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
        for (name, data) in [
            ("sub/note.txt", &b"seven"[..]),
            ("other.txt", &b"other"[..]),
        ] {
            writer
                .push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), Some(data))
                .unwrap();
        }
        writer.finish().unwrap();
        let temp = OpenTemp::new(dir.path().join("open-temp"));
        let provider = crate::archive_sevenz::SevenZProvider::new();
        let out = open_with(&provider, &temp, &archive, "sub/note.txt", None).unwrap();
        assert_eq!(std::fs::read(out).unwrap(), b"seven");
    }

    fn find_rar_exe() -> Option<PathBuf> {
        if let Some(val) = std::env::var_os("FERRUM_RAR_EXE") {
            let p = PathBuf::from(val);
            return p.is_file().then_some(p);
        }
        [
            r"C:\Program Files\WinRAR\Rar.exe",
            r"C:\Program Files (x86)\WinRAR\Rar.exe",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    }

    /// RAR fixtures are generated at test time with the tester's own Rar.exe (WinRAR licence:
    /// no .rar file is ever stored in the repository). Without it the test is skipped.
    fn make_rar(dir: &Path, extra: &[&str]) -> Option<PathBuf> {
        let rar = find_rar_exe()?;
        let src = dir.join("rar-src");
        std::fs::create_dir_all(src.join("docs")).unwrap();
        std::fs::write(src.join("docs").join("report.pdf"), b"%PDF-rar").unwrap();
        std::fs::write(src.join("readme.txt"), b"readme").unwrap();
        let out = dir.join("a.rar");
        let status = std::process::Command::new(rar)
            .current_dir(&src)
            .args(["a", "-r", "-idq", "-ma5"])
            .args(extra)
            .arg(&out)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success(), "Rar.exe failed");
        Some(out)
    }

    #[test]
    fn opens_a_member_of_a_rar() {
        let dir = tempfile::tempdir().unwrap();
        let Some(archive) = make_rar(dir.path(), &[]) else {
            eprintln!("skipped: Rar.exe not found (set FERRUM_RAR_EXE)");
            return;
        };
        let temp = OpenTemp::new(dir.path().join("open-temp"));
        let provider = crate::archive_rar::RarProvider::new();
        let out = open_with(&provider, &temp, &archive, "docs/report.pdf", None).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"%PDF-rar");
        assert_eq!(out.file_name().unwrap(), "report.pdf");
        // The second open reuses the same file.
        let again = open_with(&provider, &temp, &archive, "docs/report.pdf", None).unwrap();
        assert_eq!(out, again);
    }

    #[test]
    fn header_encrypted_rar_asks_for_a_password_then_opens() {
        let dir = tempfile::tempdir().unwrap();
        let Some(archive) = make_rar(dir.path(), &["-hpFerrumTest123"]) else {
            eprintln!("skipped: Rar.exe not found (set FERRUM_RAR_EXE)");
            return;
        };
        let temp = OpenTemp::new(dir.path().join("open-temp"));
        let provider = crate::archive_rar::RarProvider::new();
        let e = open_with(&provider, &temp, &archive, "docs/report.pdf", None).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::PasswordRequired);
        let e = open_with(&provider, &temp, &archive, "docs/report.pdf", Some("nope")).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::BadPassword);
        let out = open_with(
            &provider,
            &temp,
            &archive,
            "docs/report.pdf",
            Some("FerrumTest123"),
        )
        .unwrap();
        assert_eq!(std::fs::read(out).unwrap(), b"%PDF-rar");
    }

    #[cfg(windows)]
    #[test]
    fn zone_identifier_is_copied_to_the_extracted_file() {
        let f = fixture(&[("a.txt", b"hi")]);
        let mut stream = f.archive.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::write(&stream, b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        let out = open(&f.temp, &f.archive, "a.txt").unwrap();
        let mut copied = out.as_os_str().to_owned();
        copied.push(":Zone.Identifier");
        assert_eq!(
            std::fs::read(&copied).unwrap(),
            b"[ZoneTransfer]\r\nZoneId=3\r\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn archive_without_a_mark_gives_a_file_without_one() {
        let f = fixture(&[("a.txt", b"hi")]);
        let out = open(&f.temp, &f.archive, "a.txt").unwrap();
        let mut copied = out.as_os_str().to_owned();
        copied.push(":Zone.Identifier");
        assert!(std::fs::read(&copied).is_err());
    }

    #[test]
    fn session_name_parsing_only_accepts_our_pattern() {
        assert_eq!(parse_session("s100-42"), Some(100));
        for bad in [
            "", "s", "s-1", "s1-", "x1-2", "s1-2-3", "s1a-2", "S1-2", "s 1-2", "s1-2 ",
        ] {
            assert_eq!(parse_session(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn cleanup_removes_old_sessions_only_and_keeps_foreign_and_current() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("open-temp");
        let temp = OpenTemp::with_session(root.clone(), 1_000_000, 7);
        let old = root.join("s100-1");
        let recent = root.join("s999000-2");
        let foreign = root.join("keep-me");
        for d in [&old, &recent, &foreign, &temp.session_dir()] {
            std::fs::create_dir_all(d.join("k")).unwrap();
            std::fs::write(d.join("k").join("f.txt"), b"x").unwrap();
        }
        // A read-only file must not stop the sweep.
        let ro = old.join("k").join("ro.txt");
        std::fs::write(&ro, b"x").unwrap();
        let mut p = std::fs::metadata(&ro).unwrap().permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&ro, p).unwrap();

        let removed = temp.cleanup_stale(1_000_000, STALE_AFTER_SECS);
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(recent.exists());
        assert!(foreign.exists());
        assert!(temp.session_dir().exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_file_is_skipped_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("open-temp");
        let temp = OpenTemp::with_session(root.clone(), 1_000_000, 7);
        let old = root.join("s100-1");
        std::fs::create_dir_all(&old).unwrap();
        let locked = old.join("held.txt");
        std::fs::write(&locked, b"x").unwrap();
        // An open handle without delete sharing (std opens with FILE_SHARE_DELETE, so ask for none).
        use std::os::windows::fs::OpenOptionsExt;
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked)
            .unwrap();
        assert_eq!(temp.cleanup_stale(1_000_000, STALE_AFTER_SECS), 0);
        assert!(locked.exists());
        drop(_held);
        assert_eq!(temp.cleanup_stale(1_000_000, STALE_AFTER_SECS), 1);
        assert!(!old.exists());
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_never_follows_a_junction_out_of_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("open-temp");
        let temp = OpenTemp::with_session(root.clone(), 1_000_000, 7);
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), b"keep").unwrap();
        let old = root.join("s100-1");
        std::fs::create_dir_all(&old).unwrap();
        let link = old.join("jump");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink /J failed");
        assert_eq!(temp.cleanup_stale(1_000_000, STALE_AFTER_SECS), 1);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read(outside.join("precious.txt")).unwrap(),
            b"keep"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_as_session_folder_or_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let root = dir.path().join("open-temp");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&root)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(status.status.success());
        let temp = OpenTemp::new(root);
        let archive = dir.path().join("a.zip");
        write_zip(&archive, &[("a.txt", b"hi")]);
        let e = open(&temp, &archive, "a.txt").unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::UnsafeEntry);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(temp.cleanup_stale(u64::MAX, 0), 0);
    }
}
