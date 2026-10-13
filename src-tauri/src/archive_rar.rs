//! RAR/RAR5 provider (F2-06): listing, bounded read, and extraction through
//! the shared F2-03 safe pipeline. Read-only by design — creating RAR
//! archives is license-restricted and deliberately unsupported.
//!
//! Engine: the `unrar` crate (UnRAR freeware license — free for handling RAR
//! archives in any software, forbidden for re-creating the RAR compression
//! algorithm). RAR creation therefore stays impossible in Ferrum regardless
//! of backend.
//!
//! UnRAR's API decodes a whole member into memory in one call, so RAR
//! members are buffered in RAM bounded by the entry size limit — a
//! documented difference from the ZIP provider's true streaming.

use crate::archive_contract::{
    ArchiveCapabilities, ArchiveEntry, ArchiveError, ArchiveErrorCode, ArchiveFormat,
    ArchiveListRequest, ArchiveListResponse, ArchiveProvider, ArchiveReadRequest, Location,
    MemberPath,
};
use std::path::Path;

// unrar_sys's pathfn.o references the Windows Registry API; MSVC link needs
// advapi32.lib which is not in unrar_sys's metadata. Force the link here so
// every binary that includes this module resolves the symbols.
#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "C" {}

pub struct RarProvider;

impl RarProvider {
    pub fn new() -> Self {
        Self
    }

    /// Signature sniff: `Rar!\x1a\x07\x00` (RAR4) or `Rar!\x1a\x07\x01\x00` (RAR5).
    pub fn sniff(path: &Path) -> Result<(), ArchiveError> {
        let mut file = std::fs::File::open(path).map_err(|e| {
            let mut err = ArchiveError::new(ArchiveErrorCode::Io, e.to_string());
            err.archive_path = Some(path.to_string_lossy().into_owned());
            err
        })?;
        use std::io::Read;
        let mut magic = [0u8; 7];
        let mut n = 0;
        while n < magic.len() {
            match file.read(&mut magic[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(ArchiveError::new(ArchiveErrorCode::Io, e.to_string()));
                }
            }
        }
        let ok = n >= 7 && magic[..6] == *b"Rar!\x1a\x07" && (magic[6] == 0 || magic[6] == 1);
        if !ok {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedFormat,
                "Not a RAR archive (bad signature)",
            ));
        }
        Ok(())
    }

    pub fn list_with_password(
        &self,
        request: &ArchiveListRequest,
        password: Option<&str>,
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
        Self::sniff(Path::new(archive_path))?;
        let prefix = if member_path.as_str().is_empty() {
            String::new()
        } else {
            format!("{}/", member_path.as_str())
        };

        let listing = match password {
            Some(pw) => unrar::Archive::with_password(archive_path.as_str(), pw)
                .open_for_listing()
                .map_err(map_unrar)?,
            None => unrar::Archive::new(archive_path.as_str())
                .open_for_listing()
                .map_err(map_unrar)?,
        };

        if listing.volume_info() != unrar::VolumeInfo::None {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedCodec,
                "Multi-volume RAR not supported",
            ));
        }

        // Pass 1: walk the forward-only listing, collecting raw rows for the
        // direct children of `member_path` (implicit directories included).
        let mut rows: Vec<(String, bool, u64)> = Vec::new();
        for header in listing {
            let header = header.map_err(map_unrar)?;
            if header.is_split() {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::UnsupportedCodec,
                    "Multi-volume RAR not supported",
                ));
            }
            let raw = member_string(&header.filename);
            let is_dir = header.is_directory();
            if let Some(row) = level_row(&prefix, &raw, is_dir, header.unpacked_size) {
                rows.push(row);
            }
        }
        // FER-504: the dedup is type-aware — folders with the exact same name
        // merge, a name shared (case-insensitively) by a file and a folder or
        // by two files is the same `Conflict` the extract preflight would
        // reach, so listing and extract agree.
        let rows = dedup_level(member_path.as_str(), rows)?;

        // Pass 2: build contract entries referencing owned names.
        let mut entries: Vec<ArchiveEntry> = rows
            .into_iter()
            .map(|(name, is_dir, size)| {
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
                    modified_ms: None,
                })
            })
            .collect::<Result<_, ArchiveError>>()?;
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
}

impl Default for RarProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn member_string(filename: &Path) -> String {
    filename.to_string_lossy().replace('\\', "/")
}

/// One raw member path → the row it contributes to the level under `prefix`
/// (`""` for the root, otherwise `"<dir>/"`), or `None` when the member lies
/// outside that level. A deeper member implies its first path segment as a
/// folder row. The prefix match is exact (case-sensitive), so `Docs/x` is
/// never listed under `docs`.
fn level_row(prefix: &str, raw: &str, is_dir: bool, size: u64) -> Option<(String, bool, u64)> {
    let rest = raw.strip_prefix(prefix)?;
    if rest.is_empty() {
        return None;
    }
    match rest.find('/') {
        Some(slash) => {
            let child = &rest[..slash];
            (!child.is_empty()).then(|| (child.to_owned(), true, 0))
        }
        None => Some((rest.to_owned(), is_dir, if is_dir { 0 } else { size })),
    }
}

/// FER-504: type-aware dedup of one directory level's raw listing rows.
///
/// A folder is listed once whether it came from an explicit directory entry
/// or was implied by a deeper member path — but only when the names match
/// **exactly**. Folders differing only in case (`docs` / `Docs`) stay two
/// rows, because the subfolder listing matches its prefix case-sensitively:
/// merging them would hide `Docs/*` members that extraction still writes
/// (same behaviour as ZIP/7z and as before FER-504).
///
/// A name shared by two files, or by a file and a folder, is a `Conflict` —
/// the same verdict the extract preflight reaches (`archive_extract::preflight`
/// rejects any repeated normalized name) — carrying the clashing member path.
/// That comparison folds to lowercase like the pipeline's `normalize()`, so
/// `X` and `x` clash on the case-insensitive Windows target. First occurrence
/// wins for merged folder rows.
fn dedup_level(
    directory: &str,
    rows: Vec<(String, bool, u64)>,
) -> Result<Vec<(String, bool, u64)>, ArchiveError> {
    // lowercased name → the exact names and kinds of the rows kept so far.
    let mut seen: std::collections::HashMap<String, Vec<(String, bool)>> =
        std::collections::HashMap::with_capacity(rows.len());
    let mut merged: Vec<(String, bool, u64)> = Vec::with_capacity(rows.len());
    for (name, is_dir, size) in rows {
        let kept = seen.entry(name.to_lowercase()).or_default();
        if kept.is_empty() {
            kept.push((name.clone(), is_dir));
            merged.push((name, is_dir, size));
            continue;
        }
        let all_dirs = kept.iter().all(|(_, d)| *d);
        if is_dir && all_dirs {
            // Folder + folder: merge only on an exact (case-sensitive) match;
            // a case variant is its own row so its members stay reachable.
            if !kept.iter().any(|(n, _)| *n == name) {
                kept.push((name.clone(), true));
                merged.push((name, true, size));
            }
            continue;
        }
        // File + file, or file + folder (case-insensitive): extraction would
        // refuse this archive, so the listing refuses to show a quiet row.
        let mut err = ArchiveError::new(ArchiveErrorCode::Conflict, "Duplicate archive member");
        err.member_path = Some(if directory.is_empty() {
            name
        } else {
            format!("{directory}/{name}")
        });
        return Err(err);
    }
    Ok(merged)
}

fn map_unrar(e: unrar::error::UnrarError) -> ArchiveError {
    // FER-73: by the library's structured status, not by message text.
    use unrar::error::Code as C;
    let code = match e.code {
        C::MissingPassword => ArchiveErrorCode::PasswordRequired,
        C::BadPassword => ArchiveErrorCode::BadPassword,
        _ => ArchiveErrorCode::CorruptArchive,
    };
    ArchiveError::new(code, format!("unrar: {e}"))
}

impl ArchiveProvider for RarProvider {
    fn capabilities(&self, _format: ArchiveFormat) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Rar,
            list: true,
            read: true,
            extract: true,        // via F2-03 pipeline
            create: false,        // license-restricted; deliberately unsupported
            test: false,          // wired in F2-08
            encrypted_read: true, // unrar supports passwords
            encrypted_create: false,
            multi_volume: false, // split entries refused; honest until proven
        }
    }

    fn list(&self, request: &ArchiveListRequest) -> Result<ArchiveListResponse, ArchiveError> {
        // FER-504: the request may carry a password for header-encrypted RAR.
        self.list_with_password(request, request.password.as_deref())
    }

    fn read(
        &self,
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
        Self::sniff(Path::new(archive_path))?;
        let target = member_path.as_str().to_owned();

        let mut processing = match &request.password {
            Some(pw) => unrar::Archive::with_password(archive_path.as_str(), pw)
                .open_for_processing()
                .map_err(map_unrar)?,
            None => unrar::Archive::new(archive_path.as_str())
                .open_for_processing()
                .map_err(map_unrar)?,
        };
        if processing.volume_info() != unrar::VolumeInfo::None {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedCodec,
                "Multi-volume RAR not supported",
            ));
        }
        let mut data: Option<Vec<u8>> = None;
        // Solid RAR: skipping a member still decodes it, and the API offers no
        // cancellation inside one `skip()`. Budget by the DECLARED sizes of the
        // members skipped so far and check the clock between members; a single
        // huge member cannot be interrupted once started (documented limit).
        let started = std::time::Instant::now();
        let mut skipped = 0u64;
        while let Some(header) = processing.read_header().map_err(map_unrar)? {
            crate::archive_contract::check_cancelled()?;
            let entry = header.entry();
            if entry.is_split() {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::UnsupportedCodec,
                    "Multi-volume RAR not supported",
                ));
            }
            if entry.is_encrypted() && request.password.is_none() {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::PasswordRequired,
                    "RAR member is encrypted",
                ));
            }
            let name = member_string(&entry.filename);
            let entry_size = entry.unpacked_size;
            if name == target {
                // The member's own header declares what UnRAR will buffer;
                // refuse before `read()` (it cannot be stopped midway).
                if entry.unpacked_size > request.max_bytes {
                    return Err(ArchiveError::new(
                        ArchiveErrorCode::LimitExceeded,
                        "RAR member exceeds the read limit",
                    ));
                }
                let (bytes, _rest) = header.read().map_err(map_unrar)?;
                data = Some(bytes);
                break; // rest dropped: last member or superseded below
            }
            if header.is_solid() {
                skipped = skipped.saturating_add(entry_size);
                if skipped > crate::archive_contract::MAX_SOLID_SKIP_BYTES
                    || started.elapsed()
                        > std::time::Duration::from_secs(
                            crate::archive_contract::MAX_SOLID_SKIP_SECS,
                        )
                {
                    return Err(ArchiveError::new(
                        ArchiveErrorCode::LimitExceeded,
                        crate::archive_contract::SOLID_TOO_LARGE,
                    ));
                }
            }
            processing = header.skip().map_err(map_unrar)?;
        }

        let Some(bytes) = data else {
            return Err(ArchiveError::new(
                ArchiveErrorCode::Io,
                format!("Member not found: {target}"),
            ));
        };

        let mut remaining = request.max_bytes;
        let mut sent = 0usize;
        let mut truncated = false;
        while sent < bytes.len() {
            let take = remaining.min((bytes.len() - sent) as u64) as usize;
            if take == 0 {
                truncated = true;
                break;
            }
            chunk(&bytes[sent..sent + take])?;
            sent += take;
            remaining -= take as u64;
        }
        if sent < bytes.len() {
            truncated = true;
        }
        Ok(truncated)
    }

    fn extract(
        &self,
        request: &crate::archive_contract::ArchiveExtractRequest,
        progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<(), ArchiveError> {
        use crate::archive_contract::{ArchiveOperationKind, ArchiveProgress};
        use crate::archive_extract::{Entry, EntryKind};

        Self::sniff(Path::new(&request.archive_path))?;
        let destination = Path::new(&request.destination_path);
        let listing = match &request.password {
            Some(pw) => unrar::Archive::with_password(&request.archive_path, pw)
                .open_for_listing()
                .map_err(map_unrar)?,
            None => unrar::Archive::new(&request.archive_path)
                .open_for_listing()
                .map_err(map_unrar)?,
        };
        if listing.volume_info() != unrar::VolumeInfo::None {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedCodec,
                "Multi-volume RAR not supported",
            ));
        }

        // Map members to engine entries in archive order; preflight preserves
        // this order so the extraction driver stays in lockstep with the
        // forward-only processing iterator.
        let mut names: Vec<String> = Vec::new();
        let mut sizes: Vec<u64> = Vec::new();
        let mut is_dirs: Vec<bool> = Vec::new();
        for header in listing {
            let header = header.map_err(map_unrar)?;
            if header.is_split() {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::UnsupportedCodec,
                    "Multi-volume RAR not supported",
                ));
            }
            if header.is_encrypted() && request.password.is_none() {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::PasswordRequired,
                    "RAR member is encrypted",
                ));
            }
            names.push(member_string(&header.filename));
            sizes.push(if header.is_directory() {
                0
            } else {
                header.unpacked_size
            });
            is_dirs.push(header.is_directory());
        }
        // names is stable now; build entries referencing into it.
        let entries: Vec<Entry<'_>> = names
            .iter()
            .zip(&sizes)
            .zip(&is_dirs)
            .map(|((name, size), is_dir)| Entry {
                raw_name: name.as_str(),
                kind: if *is_dir {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                expanded_size: Some(*size),
                compressed_size: None,
                modified_ms: None, // DOS time conversion skipped in F2-06
            })
            .collect();
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

        let op_id = format!("rar-extract-{}", std::process::id());
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
            progress(ArchiveProgress {
                operation_id: op_id.clone(),
                kind: ArchiveOperationKind::Extract,
                processed_bytes: track.bytes(),
                total_bytes,
                processed_entries: done,
                total_entries: Some(total),
                current_member_path: track
                    .member()
                    .and_then(|m| crate::archive_contract::MemberPath::parse(&m).ok()),
                written_entries: summary.map(|(w, _)| w),
                skipped_entries: summary.map(|(_, s)| s),
            })
        };

        // Drive the F2-03 pipeline; the stream callback advances the
        // forward-only processing iterator in lockstep with the plan.
        let processing = match &request.password {
            Some(pw) => unrar::Archive::with_password(&request.archive_path, pw)
                .open_for_processing()
                .map_err(map_unrar)?,
            None => unrar::Archive::new(&request.archive_path)
                .open_for_processing()
                .map_err(map_unrar)?,
        };
        if processing.volume_info() != unrar::VolumeInfo::None {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedCodec,
                "Multi-volume RAR not supported",
            ));
        }
        let archive_cell: std::cell::RefCell<
            Option<unrar::OpenArchive<unrar::Process, unrar::CursorBeforeHeader>>,
        > = std::cell::RefCell::new(Some(processing));
        let result = crate::archive_extract::extract(
            &plan,
            destination,
            &request.limits,
            request.conflict,
            &mut |planned: &crate::archive_extract::PlannedEntry,
                  writer: &mut dyn std::io::Write| {
                loop {
                    // read_header consumes the archive; take it out of the cell,
                    // chain through the header, then store the advanced archive back.
                    let processing = archive_cell
                        .borrow_mut()
                        .take()
                        .expect("processing archive present");
                    let header = match processing.read_header().map_err(map_unrar)? {
                        Some(header) => header,
                        None => {
                            // EOF without finding the planned member. The archive
                            // state is consumed — extraction aborts entirely.
                            return Err(ArchiveError::new(
                                ArchiveErrorCode::Io,
                                "member vanished mid-extract",
                            ));
                        }
                    };
                    let entry = header.entry();
                    if entry.is_split() {
                        return Err(ArchiveError::new(
                            ArchiveErrorCode::UnsupportedCodec,
                            "Multi-volume RAR not supported",
                        ));
                    }
                    if entry.is_directory() {
                        let rest = header.skip().map_err(map_unrar)?;
                        *archive_cell.borrow_mut() = Some(rest);
                        continue;
                    }
                    if entry.is_encrypted() && request.password.is_none() {
                        // Extraction aborts here; the archive is dropped mid-state
                        // (the cell stays None) — no further members are read.
                        return Err(ArchiveError::new(
                            ArchiveErrorCode::PasswordRequired,
                            "RAR member is encrypted",
                        ));
                    }
                    let name = member_string(&entry.filename);
                    if name != planned.raw_name {
                        // Unselected member between selected ones: skip.
                        let rest = header.skip().map_err(map_unrar)?;
                        *archive_cell.borrow_mut() = Some(rest);
                        continue;
                    }
                    track.begin_member(&planned.raw_name);
                    let (bytes, rest) = header.read().map_err(map_unrar)?;
                    *archive_cell.borrow_mut() = Some(rest);
                    // Cancel checks between members: unrar decodes a member in one call.
                    crate::archive_contract::check_cancelled()?;
                    use std::io::Write as _;
                    let mut writer = crate::archive_extract::CountingWriter::new(writer, &track);
                    const CHUNK_SIZE: usize = 64 * 1024;
                    for chunk in bytes.chunks(CHUNK_SIZE) {
                        crate::archive_contract::check_cancelled()?;
                        writer
                            .write_all(chunk)
                            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
                    }
                    done.set(done.get() + 1);
                    return Ok(());
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
        _request: &crate::archive_contract::ArchiveCreateRequest,
        _progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<std::path::PathBuf, ArchiveError> {
        // Deliberately unsupported: UnRAR's license forbids re-creating the
        // RAR compression algorithm. Ferrum never creates RAR archives.
        Err(ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            "RAR creation is unsupported and license-restricted",
        ))
    }

    fn test(
        &self,
        request: &crate::archive_contract::ArchiveTestRequest,
        _progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<crate::archive_contract::ArchiveTestReport, ArchiveError> {
        crate::archive_create_test::test_rar(
            Path::new(&request.archive_path),
            request.password.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, is_dir: bool, size: u64) -> (String, bool, u64) {
        (name.to_owned(), is_dir, size)
    }

    fn names(rows: &[(String, bool, u64)]) -> Vec<(&str, bool)> {
        rows.iter().map(|(n, d, _)| (n.as_str(), *d)).collect()
    }

    #[test]
    fn folder_entries_merge_regardless_of_source_or_order() {
        // Implicit (from a deeper member) first, explicit folder after.
        let merged = dedup_level(
            "",
            vec![
                row("docs", true, 0), // implied by "docs/a.txt"
                row("docs", true, 0), // explicit "docs/" entry
            ],
        )
        .unwrap();
        assert_eq!(names(&merged), vec![("docs", true)]);
        // And the reverse order.
        let merged = dedup_level("", vec![row("docs", true, 0), row("docs", true, 0)]).unwrap();
        assert_eq!(names(&merged), vec![("docs", true)]);
    }

    #[test]
    fn distinct_names_survive_untouched() {
        let merged = dedup_level(
            "data",
            vec![
                row("readme.txt", false, 69),
                row("sub", true, 0),
                row("Data", false, 10),
            ],
        )
        .unwrap();
        assert_eq!(
            names(&merged),
            vec![("readme.txt", false), ("sub", true), ("Data", false)]
        );
    }

    #[test]
    fn duplicate_files_conflict_with_member_path() {
        let err = dedup_level("", vec![row("x", false, 1), row("x", false, 2)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("x"));
        // Nested directory: the error carries the full member path.
        let err = dedup_level("data", vec![row("x", false, 1), row("x", false, 2)]).unwrap_err();
        assert_eq!(err.member_path.as_deref(), Some("data/x"));
    }

    #[test]
    fn file_and_folder_with_same_name_conflict_either_order() {
        // File first, then a folder implied by a deeper member.
        let err = dedup_level("", vec![row("x", false, 1), row("x", true, 0)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("x"));
        // Folder first (explicit entry), then a file with the same name.
        let err = dedup_level("", vec![row("x", true, 0), row("x", false, 1)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
    }

    #[test]
    fn name_clash_is_case_insensitive_like_the_extract_pipeline() {
        // archive_extract::preflight folds names to lowercase, so "X" and
        // "x" cannot coexist on a Windows target — the listing must agree.
        let err = dedup_level("", vec![row("X", false, 1), row("x", false, 2)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("x"));
        let err = dedup_level("", vec![row("X", false, 1), row("x", true, 0)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
    }

    #[test]
    fn folder_case_variants_stay_separate_rows() {
        let merged = dedup_level("", vec![row("docs", true, 0), row("Docs", true, 0)]).unwrap();
        assert_eq!(names(&merged), vec![("docs", true), ("Docs", true)]);
        // Exact repeats of either variant still merge.
        let merged = dedup_level(
            "",
            vec![
                row("docs", true, 0),
                row("Docs", true, 0),
                row("docs", true, 0),
                row("Docs", true, 0),
            ],
        )
        .unwrap();
        assert_eq!(names(&merged), vec![("docs", true), ("Docs", true)]);
    }

    #[test]
    fn file_and_folder_case_variants_conflict() {
        let err = dedup_level("", vec![row("docs", false, 1), row("Docs", true, 0)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("Docs"));
        // A file arriving after two folder case variants still conflicts.
        let err = dedup_level(
            "",
            vec![
                row("docs", true, 0),
                row("Docs", true, 0),
                row("DOCS", false, 1),
            ],
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
    }

    /// Runs pass 1 + dedup over raw member paths, like `list_with_password`.
    fn list_level(
        dir: &str,
        members: &[(&str, bool, u64)],
    ) -> Result<Vec<(String, bool, u64)>, ArchiveError> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let rows = members
            .iter()
            .filter_map(|(raw, d, s)| level_row(&prefix, raw, *d, *s))
            .collect();
        dedup_level(dir, rows)
    }

    #[test]
    fn case_variant_folder_members_are_listed_under_their_own_folder() {
        // RAR with implicit parents only: "docs/a.txt" and "Docs/evil.exe".
        let members = [("docs/a.txt", false, 3), ("Docs/evil.exe", false, 9)];
        let root = list_level("", &members).unwrap();
        assert_eq!(names(&root), vec![("docs", true), ("Docs", true)]);
        let upper = list_level("Docs", &members).unwrap();
        assert_eq!(names(&upper), vec![("evil.exe", false)]);
        let lower = list_level("docs", &members).unwrap();
        assert_eq!(names(&lower), vec![("a.txt", false)]);
    }

    #[test]
    fn empty_and_single_row_inputs_are_trivial() {
        assert!(dedup_level("", vec![]).unwrap().is_empty());
        let one = dedup_level("", vec![row("only", false, 5)]).unwrap();
        assert_eq!(names(&one), vec![("only", false)]);
    }
}
