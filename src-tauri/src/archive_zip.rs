//! ZIP/ZIP64 provider (F2-04): listing and bounded read without extraction.
//! Implements the `ArchiveProvider` trait from `archive_contract`.
//!
//! Listing never extracts: the central directory is read, entries are
//! filtered to the direct children of the requested `member_path`, and
//! implicit directory members (missing explicit dir entries) are synthesized.

use crate::archive_contract::{
    ArchiveCapabilities, ArchiveEntry, ArchiveError, ArchiveErrorCode, ArchiveFormat,
    ArchiveListRequest, ArchiveListResponse, ArchiveProvider, ArchiveReadRequest, Location,
    MemberPath,
};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub struct ZipProvider;

impl ZipProvider {
    pub fn new() -> Self {
        Self
    }

    /// Signature sniff (local file header, empty archive, or ZIP64 locator) on an opened source,
    /// read from the start.
    fn sniff_file(file: &mut File) -> Result<(), ArchiveError> {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
        let mut magic = [0u8; 4];
        let read = file
            .read(&mut magic)
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
        let ok = read == 4
            && (magic == [0x50, 0x4B, 0x03, 0x04]      // local file header
                || magic == [0x50, 0x4B, 0x05, 0x06]  // empty archive EOCD
                || magic == [0x50, 0x4B, 0x06, 0x06]) // ZIP64 EOCD
            || magic == [0x50, 0x4B, 0x07, 0x08]; // spanned marker (first disk)
        if !ok {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedFormat,
                "Not a ZIP archive (bad signature)",
            ));
        }
        Ok(())
    }

    /// Central-directory facts for one member, read without decompressing:
    /// `(compressed_size, uncompressed_size, encrypted)`. Used by the preview
    /// to refuse extreme compression ratios before reading anything.
    #[allow(dead_code)] // unused when this file is compiled into the standalone test target
    pub(crate) fn member_raw_info(
        path: &Path,
        member: &str,
    ) -> Result<(u64, u64, bool), ArchiveError> {
        Self::raw_info_in(&mut Self::open_archive(path)?, member)
    }

    /// `member_raw_info` from an archive handle the caller already opened and checked (FER-488).
    #[allow(dead_code)] // unused when this file is compiled into the standalone test target
    pub(crate) fn member_raw_info_opened(
        file: File,
        member: &str,
    ) -> Result<(u64, u64, bool), ArchiveError> {
        Self::raw_info_in(&mut Self::open_file(file)?, member)
    }

    fn raw_info_in(
        archive: &mut zip::ZipArchive<BufReader<File>>,
        member: &str,
    ) -> Result<(u64, u64, bool), ArchiveError> {
        let index = archive
            .index_for_name(member)
            .ok_or_else(|| ArchiveError::new(ArchiveErrorCode::Io, "Member not found"))?;
        let raw = archive
            .by_index_raw(index)
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string()))?;
        Ok((raw.compressed_size(), raw.size(), raw.encrypted()))
    }

    fn open_archive(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>, ArchiveError> {
        let file = File::open(path).map_err(|e| {
            let mut err = ArchiveError::new(ArchiveErrorCode::Io, e.to_string());
            err.archive_path = Some(path.to_string_lossy().into_owned());
            err
        })?;
        Self::open_file(file)
    }

    /// Signature sniff + central directory from an opened source (any position), so the
    /// archive is read through exactly that handle (FER-488).
    fn open_file(mut file: File) -> Result<zip::ZipArchive<BufReader<File>>, ArchiveError> {
        Self::sniff_file(&mut file)?;
        zip::ZipArchive::new(BufReader::new(file)).map_err(|e| {
            let code = match &e {
                zip::result::ZipError::UnsupportedArchive(_) => ArchiveErrorCode::UnsupportedCodec,
                zip::result::ZipError::InvalidArchive(_) => ArchiveErrorCode::CorruptArchive,
                _ => ArchiveErrorCode::Io,
            };
            ArchiveError::new(code, format!("ZIP open failed: {e}"))
        })
    }
}

/// True when the zip error is the structured "password required" refusal —
/// metadata reads degrade instead of failing the whole operation.
fn is_password_error(e: &zip::result::ZipError) -> bool {
    // FER-73: the zip crate's own constant, not a substring match on its message.
    matches!(e, zip::result::ZipError::UnsupportedArchive(m)
        if *m == zip::result::ZipError::PASSWORD_REQUIRED)
}

/// Open a member for content reads. An encrypted member without a password is
/// a structured `PasswordRequired`, a wrong one `BadPassword` — never a bare
/// CorruptArchive. Metadata reads (names, sizes) do not go through here; the
/// central directory lists encrypted members without decrypting.
pub(crate) fn open_member<'a>(
    archive: &'a mut zip::ZipArchive<BufReader<File>>,
    index: usize,
    password: Option<&str>,
) -> Result<zip::read::ZipFile<'a, BufReader<File>>, ArchiveError> {
    let decoded = match password {
        Some(pw) => archive.by_index_decrypt(index, pw.as_bytes()),
        None => archive.by_index(index),
    };
    decoded.map_err(|e| match &e {
        zip::result::ZipError::UnsupportedArchive(_) if is_password_error(&e) => ArchiveError::new(
            ArchiveErrorCode::PasswordRequired,
            "Encrypted member requires a password",
        ),
        zip::result::ZipError::InvalidPassword => {
            ArchiveError::new(ArchiveErrorCode::BadPassword, "Wrong archive password")
        }
        _ => ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string()),
    })
}

impl Default for ZipProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn mtime_ms(tm: Option<zip::DateTime>) -> Option<i64> {
    let tm = tm?;
    // zip::DateTime has no timezone; treat as UTC for stable ordering.
    let days = days_from_civil(tm.year() as i64, tm.month() as u32, tm.day() as u32);
    let secs =
        days * 86_400 + tm.hour() as i64 * 3600 + tm.minute() as i64 * 60 + tm.second() as i64;
    Some(secs * 1000)
}

fn zip_entry_mtime_ms<R: Read>(file: &zip::read::ZipFile<'_, R>) -> Option<i64> {
    for field in file.extra_data_fields() {
        if let zip::extra_fields::ExtraField::ExtendedTimestamp(et) = field
            && let Some(secs) = et.mod_time()
        {
            return Some(secs as i64 * 1000);
        }
    }
    mtime_ms(file.last_modified())
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Howard Hinnant's days_from_civil (proleptic Gregorian).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Listing body shared by `list` (by path) and `list_opened` (checked handle, FER-488).
fn list_from(
    source: Option<File>,
    request: &ArchiveListRequest,
) -> Result<ArchiveListResponse, ArchiveError> {
    let Location::Archive {
        archive_path,
        member_path,
    } = &request.location
    else {
        return Err(ArchiveError::new(
            ArchiveErrorCode::InvalidMemberPath,
            "Listing requires archive location",
        ));
    };
    let prefix = if member_path.as_str().is_empty() {
        String::new()
    } else {
        format!("{}/", member_path.as_str())
    };

    let mut archive = match source {
        Some(file) => ZipProvider::open_file(file)?,
        None => ZipProvider::open_archive(Path::new(archive_path))?,
    };
    // Direct children of `member_path`; implicit dirs synthesized and
    // deduped by child-name. Encrypted members list with unknown size /
    // mtime (central-directory name only) instead of failing the whole
    // listing.
    let mut names: Vec<(String, bool, u64, Option<i64>)> = Vec::new();
    let mut seen_dirs = std::collections::HashSet::new();
    for i in 0..archive.len() {
        let opened = archive.by_index(i);
        let opened = opened.map(|file| {
            (
                file.name().to_owned(),
                file.is_dir(),
                if file.is_dir() { 0 } else { file.size() },
                zip_entry_mtime_ms(&file),
            )
        });
        let meta = match opened {
            Ok(meta) => meta,
            Err(e) => {
                if !is_password_error(&e) {
                    return Err(ArchiveError::new(
                        ArchiveErrorCode::CorruptArchive,
                        e.to_string(),
                    ));
                }
                let Some(raw) = archive.name_for_index(i) else {
                    return Err(ArchiveError::new(
                        ArchiveErrorCode::CorruptArchive,
                        e.to_string(),
                    ));
                };
                let name = raw.to_owned();
                (name.clone(), name.ends_with('/'), 0, None)
            }
        };
        let (raw, is_dir, size, modified) = meta;
        let Some(rest) = raw.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue; // the requested dir itself
        }
        if let Some(slash) = rest.find('/') {
            // nested under a child dir: synthesize the direct child
            let child = &rest[..slash];
            if !child.is_empty() && seen_dirs.insert(child.to_owned()) {
                names.push((child.to_owned(), true, 0, None));
            }
        } else {
            names.push((rest.to_owned(), is_dir, size, modified));
        }
    }

    let mut entries: Vec<ArchiveEntry> = names
        .into_iter()
        .map(|(name, is_dir, size, modified_ms)| {
            let full = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", member_path.as_str(), name)
            };
            Ok(ArchiveEntry {
                member_path: MemberPath::parse(&full)?,
                name,
                is_dir,
                size,
                modified_ms,
            })
        })
        .collect::<Result<_, ArchiveError>>()?;
    // Folders first, then name ascending (D13 parity with disk listing).
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let response = ArchiveListResponse {
        location: request.location.clone(),
        request_token: request.request_token,
        entries,
    };
    response.validate()?;
    Ok(response)
}

/// Read body shared by `read` (by path) and `read_opened` (checked handle, FER-488).
fn read_from(
    source: Option<File>,
    request: &ArchiveReadRequest,
    chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
) -> Result<bool, ArchiveError> {
    let Location::Archive {
        archive_path,
        member_path,
    } = &request.location
    else {
        return Err(ArchiveError::new(
            ArchiveErrorCode::InvalidMemberPath,
            "Read requires archive location",
        ));
    };
    let mut archive = match source {
        Some(file) => ZipProvider::open_file(file)?,
        None => ZipProvider::open_archive(Path::new(archive_path))?,
    };
    let index = archive
        .index_for_name(member_path.as_str())
        .ok_or_else(|| {
            let mut err = ArchiveError::new(ArchiveErrorCode::Io, "Member not found");
            err.member_path = Some(member_path.as_str().to_owned());
            err
        })?;
    let mut zip_file = open_member(&mut archive, index, request.password.as_deref())?;
    if zip_file.is_dir() {
        return Err(ArchiveError::new(
            ArchiveErrorCode::Io,
            "Member is a directory",
        ));
    }

    let mut remaining = request.max_bytes;
    let mut buf = [0u8; 64 * 1024];
    let mut truncated = false;
    loop {
        let to_read = remaining.min(buf.len() as u64) as usize;
        if to_read == 0 {
            // Confirm truncation only if bytes actually remain.
            let mut probe = [0u8; 1];
            truncated = zip_file
                .read(&mut probe)
                .map_err(|e| ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string()))?
                == 1;
            break;
        }
        let n = zip_file
            .read(&mut buf[..to_read])
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string()))?;
        if n == 0 {
            break;
        }
        chunk(&buf[..n])?;
        remaining -= n as u64;
    }
    Ok(truncated)
}

impl ArchiveProvider for ZipProvider {
    fn capabilities(&self, _format: ArchiveFormat) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Zip,
            list: true,
            read: true,
            extract: true,        // via F2-03 pipeline, wired in F2-12
            create: true,         // F2-07
            test: true,           // F2-08
            encrypted_read: true, // zip crate aes-crypto feature
            encrypted_create: false,
            multi_volume: false,
        }
    }

    fn list(&self, request: &ArchiveListRequest) -> Result<ArchiveListResponse, ArchiveError> {
        list_from(None, request)
    }

    fn read(
        &self,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError> {
        read_from(None, request, chunk)
    }

    fn reads_opened(&self) -> bool {
        true
    }

    fn list_opened(
        &self,
        file: File,
        request: &ArchiveListRequest,
    ) -> Result<ArchiveListResponse, ArchiveError> {
        list_from(Some(file), request)
    }

    fn read_opened(
        &self,
        file: File,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError> {
        read_from(Some(file), request, chunk)
    }

    fn extract(
        &self,
        request: &crate::archive_contract::ArchiveExtractRequest,
        progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<(), ArchiveError> {
        use crate::archive_contract::{ArchiveOperationKind, ArchiveProgress};
        use crate::archive_extract::{Entry, EntryKind};
        use std::io::Read;

        let destination = Path::new(&request.destination_path);
        let mut archive = Self::open_archive(Path::new(&request.archive_path))?;

        // Map zip members to engine entries (decoded sizes known up front).
        let names: Vec<String> = (0..archive.len())
            .map(|i| {
                archive
                    .by_index(i)
                    .map(|zf| zf.name().to_owned())
                    .map_err(|e| ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string()))
            })
            .collect::<Result<Vec<_>, ArchiveError>>()?;
        let mut entries: Vec<Entry<'_>> = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            // Member-path safety mirrors preflight; zip names with backslash
            // or traversal are rejected before any plan is built.
            // A directory entry carries one conventional trailing '/'
            // ("docs/"); validate the name without it, with every other
            // traversal/reserved-name rule unchanged.
            let checked = match name.strip_suffix('/') {
                Some("") => name.as_str(), // bare "/" stays invalid
                Some(stripped) => stripped,
                None => name.as_str(),
            };
            MemberPath::parse(checked).map_err(|mut e| {
                e.member_path = Some(name.clone());
                e
            })?;
            // Encrypted members: metadata degrades to central-directory name
            // only; quotas then hold at stream time via the runtime writer.
            let opened = archive.by_index(i);
            let opened = opened.map(|zf| {
                (
                    if zf.is_dir() {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    Some(zf.size()),
                    Some(zf.compressed_size()),
                    zip_entry_mtime_ms(&zf),
                )
            });
            let meta = match opened {
                Ok(meta) => meta,
                Err(e) => {
                    if !is_password_error(&e) {
                        return Err(ArchiveError::new(
                            ArchiveErrorCode::CorruptArchive,
                            e.to_string(),
                        ));
                    }
                    let raw = archive.name_for_index(i).ok_or_else(|| {
                        ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string())
                    })?;
                    (
                        if raw.ends_with('/') {
                            EntryKind::Directory
                        } else {
                            EntryKind::File
                        },
                        None,
                        None,
                        None,
                    )
                }
            };
            let (kind, expanded_size, compressed_size, modified_ms) = meta;
            entries.push(Entry {
                raw_name: name,
                kind,
                expanded_size,
                compressed_size,
                modified_ms,
            });
        }

        // Selection: empty = all; nonempty = exact paths or subtrees.
        let selected = |raw: &str| {
            request.member_paths.is_empty()
                || request
                    .member_paths
                    .iter()
                    .any(|mp| raw == mp.as_str() || raw.starts_with(&format!("{}/", mp.as_str())))
        };
        let selected_entries: Vec<Entry<'_>> = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| selected(&names[*i]))
            .map(|(_, e)| e.clone())
            .collect();
        let plan = crate::archive_extract::preflight(&selected_entries, &request.limits)?;

        let op_id = format!("zip-extract-{}", std::process::id());
        let total = plan
            .entries()
            .iter()
            .filter(|e| e.kind != crate::archive_extract::EntryKind::Directory)
            .count() as u64;
        let done = std::cell::Cell::new(0u64);
        let track = crate::archive_extract::ProgressTrack::default();
        let total_bytes = plan.total_expanded_bytes();
        let emit = |progress: &mut dyn FnMut(ArchiveProgress) -> bool,
                    done: u64,
                    summary: Option<(u64, u64)>| {
            let member = match track.member().as_deref().map(MemberPath::parse) {
                Some(Ok(m)) => Some(m),
                _ => None,
            };
            progress(ArchiveProgress {
                operation_id: op_id.clone(),
                kind: ArchiveOperationKind::Extract,
                processed_bytes: track.bytes(),
                total_bytes,
                processed_entries: done,
                total_entries: Some(total),
                current_member_path: member,
                written_entries: summary.map(|(w, _)| w),
                skipped_entries: summary.map(|(_, s)| s),
            })
        };

        let archive_cell = std::cell::RefCell::new(&mut archive);
        let result = crate::archive_extract::extract(
            &plan,
            destination,
            &request.limits,
            request.conflict,
            &mut |planned: &crate::archive_extract::PlannedEntry,
                  writer: &mut dyn std::io::Write| {
                // Read + write inside the guard scope: ZipFile borrows the
                // archive, so the decoded stream is piped to the staging
                // writer while the borrow is alive.
                let mut guard = archive_cell.borrow_mut();
                let index = guard.index_for_name(&planned.raw_name).ok_or_else(|| {
                    ArchiveError::new(ArchiveErrorCode::Io, "member vanished mid-extract")
                })?;
                let mut zf = open_member(&mut guard, index, request.password.as_deref())?;
                track.begin_member(&planned.raw_name);
                use std::io::Write as _;
                let mut writer = crate::archive_extract::CountingWriter::new(writer, &track);
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = zf.read(&mut buf).map_err(|e| {
                        ArchiveError::new(ArchiveErrorCode::CorruptArchive, e.to_string())
                    })?;
                    if n == 0 {
                        done.set(done.get() + 1);
                        return Ok(());
                    }
                    writer
                        .write_all(&buf[..n])
                        .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
                }
            },
            &mut || {
                let keep_going = emit(progress, done.get(), None);
                !keep_going
            },
        );
        let summary = result
            .as_ref()
            .ok()
            .map(|rep| (rep.written_files, rep.skipped_files));
        emit(progress, done.get(), summary);
        result.map(|_| ())
    }

    fn create(
        &self,
        request: &crate::archive_contract::ArchiveCreateRequest,
        progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<std::path::PathBuf, ArchiveError> {
        let dest = Path::new(&request.destination_path);
        let default_limits = crate::archive_contract::ArchiveLimits {
            max_entries: 100_000,
            max_total_bytes: u64::MAX,
            max_entry_bytes: 512 * 1024 * 1024,
            max_depth: 32,
        };
        crate::archive_create_test::create_zip_progress_with_level(
            &request.source_paths,
            dest,
            &default_limits,
            request.conflict,
            request.level(),
            progress,
        )
    }

    fn test(
        &self,
        request: &crate::archive_contract::ArchiveTestRequest,
        _progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<crate::archive_contract::ArchiveTestReport, ArchiveError> {
        crate::archive_create_test::test_zip(
            Path::new(&request.archive_path),
            request.password.as_deref(),
        )
    }
}
