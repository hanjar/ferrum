//! Archive member preview (FER-104). Reads ONE member of a zip/7z/rar into
//! memory, bounded by the same byte limit as the text/image preview, and
//! classifies it like `preview::read_preview`. Nothing is ever extracted to
//! disk or a temp file, and the bytes are only returned as data (the UI shows
//! text as text and images as `<img>` data URLs; nothing is executed).
//!
//! Safety rules:
//! - member path goes through `MemberPath::parse` (no `..`, absolute, drive,
//!   control chars, reserved names) before anything is opened;
//! - encrypted members/archives are refused (no password is ever supplied);
//! - a zip member whose declared ratio is extreme is refused before reading;
//! - a member whose declared size exceeds the limit is not read at all
//!   (`too_large`, metadata only); a lying header is still stopped by the
//!   output cap, which aborts collection at `limit` bytes;
//! - errors carry fixed safe messages and never a raw path.

use crate::archive_contract::{
    ArchiveError, ArchiveErrorCode, ArchiveListRequest, ArchiveProvider, ArchiveReadRequest,
    Location, MemberPath,
};
use crate::domain::{ErrorCode, FsError};
use crate::preview::{
    ABSOLUTE_MAX_BYTES, PreviewData, PreviewKind, classify, image_mime_by_extension,
};
use base64::Engine;

/// Declared uncompressed/compressed ratio above which a zip member is refused.
const MAX_RATIO: u64 = 200;
/// Ratio checks only matter for payloads above this declared size.
const RATIO_MIN_SIZE: u64 = 64 * 1024;

fn safe_err(code: ErrorCode, message: &str) -> FsError {
    // Deliberately no `.with_path`: the UI already knows which item it asked about.
    FsError::new(code, message)
}

fn map_archive_err(e: ArchiveError) -> FsError {
    match e.code {
        ArchiveErrorCode::InvalidMemberPath => {
            safe_err(ErrorCode::InvalidPath, "Invalid archive member")
        }
        ArchiveErrorCode::PasswordRequired | ArchiveErrorCode::BadPassword => safe_err(
            ErrorCode::Unsupported,
            "Encrypted archive member cannot be previewed",
        ),
        ArchiveErrorCode::LimitExceeded
            if e.message == crate::archive_contract::SOLID_TOO_LARGE =>
        {
            safe_err(ErrorCode::Unsupported, "Solid block too large to preview")
        }
        ArchiveErrorCode::LimitExceeded
            if e.message == crate::archive_contract::CODEC_TOO_LARGE =>
        {
            safe_err(
                ErrorCode::Unsupported,
                "Archive codec memory too large to preview",
            )
        }
        ArchiveErrorCode::UnsafeEntry | ArchiveErrorCode::LimitExceeded => {
            safe_err(ErrorCode::Unsupported, "Archive member refused for safety")
        }
        ArchiveErrorCode::UnsupportedFormat | ArchiveErrorCode::UnsupportedCodec => safe_err(
            ErrorCode::Unsupported,
            "Archive format not supported for preview",
        ),
        ArchiveErrorCode::Cancelled => safe_err(ErrorCode::Cancelled, "Preview cancelled"),
        ArchiveErrorCode::CorruptArchive => safe_err(ErrorCode::Io, "Archive could not be read"),
        _ => safe_err(ErrorCode::Io, "Archive member could not be read"),
    }
}

fn is_zip(path: &str) -> bool {
    path.to_lowercase().ends_with(".zip")
}

/// Entry point used by the IPC command.
pub fn read_member_preview(
    archive_path: &str,
    member_path: &str,
    max_bytes: u64,
) -> Result<PreviewData, FsError> {
    crate::domain::validate_path(archive_path)?;
    let provider = crate::ipc::archive_provider(archive_path).map_err(|_| {
        safe_err(
            ErrorCode::Unsupported,
            "Archive format not supported for preview",
        )
    })?;
    read_member_preview_with(provider.as_ref(), archive_path, member_path, max_bytes)
}

pub(crate) fn read_member_preview_with(
    provider: &dyn ArchiveProvider,
    archive_path: &str,
    member_path: &str,
    max_bytes: u64,
) -> Result<PreviewData, FsError> {
    let limit = max_bytes.clamp(1, ABSOLUTE_MAX_BYTES);
    crate::archive_contract::check_cancelled().map_err(map_archive_err)?;
    let member = MemberPath::parse(member_path).map_err(map_archive_err)?;
    if member.as_str().is_empty() {
        return Err(safe_err(ErrorCode::InvalidPath, "Invalid archive member"));
    }

    // FER-486 A1/B1 + FER-488: the archive file goes through the shared read-path guard
    // (handle-checked: disk file only, no pipe/device/ADS) and the provider reads THAT file.
    let archive = CheckedArchive::open(provider, archive_path)?;
    let archive_path = archive.file.path.as_str();

    // Locate the member via a directory listing (declared size, is_dir, mtime).
    let parent = member
        .parent()
        .unwrap_or_else(|| MemberPath::parse("").expect("empty path is the root"));
    let listing = archive.list(provider, parent).map_err(map_archive_err)?;
    let entry = listing
        .entries
        .iter()
        .find(|e| e.member_path == member)
        .ok_or_else(|| safe_err(ErrorCode::NotFound, "Archive member not found"))?;
    if entry.is_dir {
        return Err(safe_err(
            ErrorCode::NotADirectory,
            "Cannot preview a folder",
        ));
    }
    let size = entry.size;
    let modified_ms = entry.modified_ms.unwrap_or(0);
    let name = entry.name.clone();

    // Zip: refuse encrypted members and extreme ratios from the central directory.
    if is_zip(archive_path) {
        let (compressed, declared, encrypted) =
            crate::archive_zip::ZipProvider::member_raw_info_opened(
                archive.source().map_err(map_archive_err)?,
                member.as_str(),
            )
            .map_err(map_archive_err)?;
        if encrypted {
            return Err(safe_err(
                ErrorCode::Unsupported,
                "Encrypted archive member cannot be previewed",
            ));
        }
        if declared >= RATIO_MIN_SIZE && declared / compressed.max(1) > MAX_RATIO {
            return Err(safe_err(
                ErrorCode::Unsupported,
                "Archive member refused for safety",
            ));
        }
    }

    let is_image = image_mime_by_extension(&name).is_some();
    if size > limit {
        return Ok(PreviewData {
            kind: classify(&name, &[]),
            data: String::new(),
            truncated: false,
            too_large: true,
            size,
            modified_ms,
        });
    }

    let (bytes, truncated) = read_capped(
        &mut |request, chunk| archive.read(provider, request, chunk),
        archive_path,
        &member,
        limit,
    )?;
    let kind = if is_image {
        PreviewKind::Image
    } else {
        classify(&name, &bytes)
    };
    // Audio/video members are never played from memory: metadata only.
    let data = if matches!(kind, PreviewKind::Video | PreviewKind::Audio) {
        String::new()
    } else {
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    };
    Ok(PreviewData {
        kind,
        data,
        truncated,
        too_large: false,
        size: size.max(bytes.len() as u64),
        modified_ms,
    })
}

/// FER-488: an archive file checked by `path_guard`, and how the provider reaches it. Providers
/// that read from a handle (zip, 7z) get a `ReOpenFile` of the checked handle; a path-only
/// backend (unrar) gets the final path of a pin on the same file object (`PathPin`), held for
/// as long as this value lives. Nothing opens the archive by the caller's path again.
pub(crate) struct CheckedArchive {
    pub(crate) file: crate::path_guard::GuardedFile,
    pin: Option<crate::path_guard::PathPin>,
}

impl CheckedArchive {
    /// Errors carry fixed safe messages (no path), like the rest of this module.
    pub(crate) fn open(
        provider: &dyn ArchiveProvider,
        archive_path: &str,
    ) -> Result<Self, FsError> {
        let file = match crate::path_guard::open_entry(archive_path) {
            Ok(crate::path_guard::Entry::File(file)) => file,
            Ok(_) => {
                return Err(safe_err(
                    ErrorCode::Unsupported,
                    "Not a regular archive file",
                ));
            }
            Err(e) if e.code == ErrorCode::InvalidPath => {
                return Err(safe_err(e.code, &e.message));
            }
            Err(e) => return Err(safe_err(e.code, "Archive could not be opened")),
        };
        // Never hydrate a cloud placeholder archive.
        if file.offline {
            return Err(crate::preview::offline_error());
        }
        let pin = if provider.reads_opened() {
            None
        } else {
            Some(file.pin_for_path_api().map_err(|e| {
                if e.code == ErrorCode::InvalidPath {
                    safe_err(e.code, &e.message)
                } else {
                    safe_err(e.code, "Archive could not be opened")
                }
            })?)
        };
        Ok(Self { file, pin })
    }

    /// A fresh read handle on the checked file object.
    pub(crate) fn source(&self) -> Result<std::fs::File, ArchiveError> {
        self.file
            .open_read()
            .map_err(|_| ArchiveError::new(ArchiveErrorCode::Io, "Archive could not be opened"))
    }

    fn location(&self, member_path: MemberPath) -> Location {
        Location::Archive {
            archive_path: match &self.pin {
                Some(pin) => pin.path.clone(),
                None => self.file.path.clone(),
            },
            member_path,
        }
    }

    pub(crate) fn list(
        &self,
        provider: &dyn ArchiveProvider,
        member_path: MemberPath,
    ) -> Result<crate::archive_contract::ArchiveListResponse, ArchiveError> {
        let request = ArchiveListRequest {
            location: self.location(member_path),
            request_token: 0,
            // Preview of header-encrypted archives stays out of FER-504
            // scope: it answers PasswordRequired like an encrypted member.
            password: None,
        };
        match self.pin {
            Some(_) => provider.list(&request),
            None => provider.list_opened(self.source()?, &request),
        }
    }

    fn read(
        &self,
        provider: &dyn ArchiveProvider,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError> {
        let Location::Archive { member_path, .. } = &request.location else {
            return Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                "Read requires archive location",
            ));
        };
        let request = ArchiveReadRequest {
            location: self.location(member_path.clone()),
            max_bytes: request.max_bytes,
            password: request.password.clone(),
        };
        match self.pin {
            Some(_) => provider.read(&request, chunk),
            None => provider.read_opened(self.source()?, &request, chunk),
        }
    }
}

/// How `read_capped` reaches the provider (the checked archive, or a test double).
type ReadFn<'a> = dyn FnMut(
        &ArchiveReadRequest,
        &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError>
    + 'a;

/// Collect at most `limit` bytes. The callback aborts the read the moment the
/// cap would be passed, so a lying header or a bomb cannot grow memory.
fn read_capped(
    read: &mut ReadFn<'_>,
    archive_path: &str,
    member: &MemberPath,
    limit: u64,
) -> Result<(Vec<u8>, bool), FsError> {
    let request = ArchiveReadRequest {
        location: Location::Archive {
            archive_path: archive_path.to_owned(),
            member_path: member.clone(),
        },
        max_bytes: limit,
        password: None,
    };
    let mut collected: Vec<u8> = Vec::new();
    let mut over = false;
    let result = read(&request, &mut |chunk| {
        crate::archive_contract::check_cancelled()?;
        let room = (limit as usize).saturating_sub(collected.len());
        if chunk.len() > room {
            collected.extend_from_slice(&chunk[..room]);
            over = true;
            return Err(ArchiveError::new(ArchiveErrorCode::LimitExceeded, "cap"));
        }
        collected.extend_from_slice(chunk);
        Ok(())
    });
    match result {
        Ok(truncated) => Ok((collected, truncated || over)),
        Err(_) if over => Ok((collected, true)),
        Err(e) => Err(map_archive_err(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_contract::{
        ArchiveCapabilities, ArchiveCreateRequest, ArchiveExtractRequest, ArchiveFormat,
        ArchiveListResponse, ArchiveProgress, ArchiveTestReport, ArchiveTestRequest,
    };
    use crate::archive_zip::ZipProvider;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_zip(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ferrum-fer104-{tag}-{}-{}.zip",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn build_zip(tag: &str, entries: &[(&str, Vec<u8>)], password: Option<&str>) -> PathBuf {
        let path = temp_zip(tag);
        let file = std::fs::File::create(&path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        for (name, body) in entries {
            let mut opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            if let Some(pw) = password {
                opts = opts.with_aes_encryption(zip::AesMode::Aes256, pw);
            }
            w.start_file(*name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap();
        path
    }

    fn preview(path: &Path, member: &str, max: u64) -> Result<PreviewData, FsError> {
        read_member_preview_with(&ZipProvider::new(), path.to_str().unwrap(), member, max)
    }

    fn decode(d: &PreviewData) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&d.data)
            .unwrap()
    }

    #[test]
    fn normal_text_member_previews() {
        let p = build_zip(
            "ok",
            &[("docs/readme.txt", b"hello from zip".to_vec())],
            None,
        );
        let d = preview(&p, "docs/readme.txt", 1024).unwrap();
        assert_eq!(d.kind, PreviewKind::Text);
        assert_eq!(decode(&d), b"hello from zip");
        assert!(!d.truncated && !d.too_large);
        assert_eq!(d.size, 14);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn json_and_image_members_classify() {
        let p = build_zip(
            "kinds",
            &[
                ("a.json", b"{}".to_vec()),
                ("pic.png", b"\x89PNGfake".to_vec()),
            ],
            None,
        );
        assert_eq!(preview(&p, "a.json", 1024).unwrap().kind, PreviewKind::Json);
        let img = preview(&p, "pic.png", 1024).unwrap();
        assert_eq!(img.kind, PreviewKind::Image);
        assert_eq!(decode(&img), b"\x89PNGfake");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn member_over_the_limit_is_not_read() {
        let p = build_zip("big", &[("big.txt", vec![b'a'; 10_000])], None);
        let d = preview(&p, "big.txt", 1000).unwrap();
        assert!(d.too_large);
        assert!(d.data.is_empty());
        assert_eq!(d.size, 10_000);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn zip_bomb_is_refused_and_never_exceeds_the_cap() {
        // 100 MiB of zeros deflates to ~100 KiB: ratio ~1000.
        let p = build_zip("bomb", &[("zeros.txt", vec![0u8; 100 * 1024 * 1024])], None);
        let err = preview(&p, "zeros.txt", 4 * 1024 * 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.path.is_none());
        // The output cap itself: reading the bomb member stops at the limit.
        let member = MemberPath::parse("zeros.txt").unwrap();
        let zip = ZipProvider::new();
        let (bytes, truncated) = read_capped(
            &mut |r, c| zip.read(r, c),
            p.to_str().unwrap(),
            &member,
            1000,
        )
        .unwrap();
        assert_eq!(bytes.len(), 1000);
        assert!(truncated);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cap_truncates_when_the_provider_overdelivers() {
        struct Greedy;
        impl ArchiveProvider for Greedy {
            fn capabilities(&self, _f: ArchiveFormat) -> ArchiveCapabilities {
                unimplemented!()
            }
            fn list(&self, _r: &ArchiveListRequest) -> Result<ArchiveListResponse, ArchiveError> {
                unimplemented!()
            }
            fn read(
                &self,
                _r: &ArchiveReadRequest,
                chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
            ) -> Result<bool, ArchiveError> {
                for _ in 0..100 {
                    chunk(&[7u8; 4096])?;
                }
                Ok(false)
            }
            fn extract(
                &self,
                _r: &ArchiveExtractRequest,
                _p: &mut dyn FnMut(ArchiveProgress) -> bool,
            ) -> Result<(), ArchiveError> {
                unimplemented!()
            }
            fn create(
                &self,
                _r: &ArchiveCreateRequest,
                _p: &mut dyn FnMut(ArchiveProgress) -> bool,
            ) -> Result<PathBuf, ArchiveError> {
                unimplemented!()
            }
            fn test(
                &self,
                _r: &ArchiveTestRequest,
                _p: &mut dyn FnMut(ArchiveProgress) -> bool,
            ) -> Result<ArchiveTestReport, ArchiveError> {
                unimplemented!()
            }
        }
        let member = MemberPath::parse("x.txt").unwrap();
        let (bytes, truncated) =
            read_capped(&mut |r, c| Greedy.read(r, c), "a.zip", &member, 5000).unwrap();
        assert_eq!(bytes.len(), 5000);
        assert!(truncated);
    }

    #[test]
    fn encrypted_member_is_refused() {
        let p = build_zip("enc", &[("secret.txt", b"top secret".to_vec())], Some("pw"));
        let err = preview(&p, "secret.txt", 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("Encrypted"));
        assert!(err.path.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn traversal_and_odd_member_names_are_rejected_before_opening() {
        let p = build_zip("trav", &[("../evil.txt", b"x".to_vec())], None);
        for bad in [
            "../evil.txt",
            "/abs.txt",
            "a/../b.txt",
            "C:\\x.txt",
            "a\\b.txt",
            "dir/",
            "",
            "CON.txt",
            "a/./b",
            "nul\0.txt",
        ] {
            let err = preview(&p, bad, 1024).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidPath, "{bad:?}");
            assert!(err.path.is_none(), "{bad:?}");
            assert!(!err.message.contains("evil"), "{bad:?}");
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_member_and_directory_member() {
        let p = build_zip("miss", &[("d/f.txt", b"x".to_vec())], None);
        let err = preview(&p, "d/nope.txt", 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = preview(&p, "d", 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotADirectory);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn corrupt_archive_gives_a_safe_error_without_the_path() {
        let p = temp_zip("corrupt");
        std::fs::write(&p, b"PK\x03\x04 this is not really a zip at all").unwrap();
        let err = preview(&p, "a.txt", 1024).unwrap_err();
        assert!(matches!(err.code, ErrorCode::Io | ErrorCode::Unsupported));
        assert!(err.path.is_none());
        assert!(!err.message.contains("ferrum-fer104"));
        let _ = std::fs::remove_file(&p);
        let err = preview(&temp_zip("gone"), "a.txt", 1024).unwrap_err();
        assert!(err.path.is_none());
        assert!(!err.message.contains("ferrum-fer104"));
    }

    #[test]
    fn solid_7z_members_preview_with_correct_content() {
        use sevenz_rust2::{ArchiveEntry as SzEntry, ArchiveWriter, SourceReader};
        let members: [(&str, &[u8]); 3] = [
            ("a.txt", b"AAAA first member AAAA"),
            ("dir-less-b.txt", b"BBBB second member BBBB"),
            ("c.txt", b"CCCC third member CCCC"),
        ];
        let path = std::env::temp_dir().join(format!(
            "ferrum-fer104-solid7z-{}-{}.7z",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let mut w = ArchiveWriter::new(std::fs::File::create(&path).unwrap()).unwrap();
        w.push_archive_entries(
            members.iter().map(|(n, _)| SzEntry::new_file(n)).collect(),
            members.iter().map(|(_, b)| SourceReader::new(*b)).collect(),
        )
        .unwrap();
        w.finish().unwrap();
        let provider = crate::archive_sevenz::SevenZProvider::new();
        for (name, body) in members {
            let d = read_member_preview_with(&provider, path.to_str().unwrap(), name, 1024)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(decode(&d), body, "{name}");
            assert_eq!(d.kind, PreviewKind::Text);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn limit_errors_map_to_fixed_safe_messages() {
        let solid = map_archive_err(ArchiveError::new(
            ArchiveErrorCode::LimitExceeded,
            crate::archive_contract::SOLID_TOO_LARGE,
        ));
        assert_eq!(solid.message, "Solid block too large to preview");
        let codec = map_archive_err(ArchiveError::new(
            ArchiveErrorCode::LimitExceeded,
            crate::archive_contract::CODEC_TOO_LARGE,
        ));
        assert!(codec.message.contains("codec memory"));
        let other = map_archive_err(ArchiveError::new(
            ArchiveErrorCode::LimitExceeded,
            "secret-path-name",
        ));
        assert!(!other.message.contains("secret"));
    }

    #[test]
    fn unsupported_extension_is_refused() {
        let err = read_member_preview("C:\\x\\a.tar", "a.txt", 10).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[test]
    fn a_cancelled_request_stops_the_preview_with_a_cancelled_error() {
        use crate::archive_contract::{CancelToken, with_cancel};
        let p = build_zip(
            "cancel",
            &[(
                "a.txt",
                (0..200_000u32)
                    .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(),
            )],
            None,
        );
        let token = CancelToken::new();
        token.cancel();
        let err = with_cancel(&token, || preview(&p, "a.txt", 4 * 1024 * 1024)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        // Not cancelled: same call works.
        assert!(preview(&p, "a.txt", 4 * 1024 * 1024).is_ok());
        let _ = std::fs::remove_file(&p);
    }

    fn build_7z(tag: &str, members: &[(&str, &[u8])]) -> PathBuf {
        use sevenz_rust2::{ArchiveEntry as SzEntry, ArchiveWriter, SourceReader};
        let path = std::env::temp_dir().join(format!(
            "ferrum-fer488-{tag}-{}-{}.7z",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let mut w = ArchiveWriter::new(std::fs::File::create(&path).unwrap()).unwrap();
        w.push_archive_entries(
            members.iter().map(|(n, _)| SzEntry::new_file(n)).collect(),
            members.iter().map(|(_, b)| SourceReader::new(*b)).collect(),
        )
        .unwrap();
        w.finish().unwrap();
        path
    }

    fn read_through(
        archive: &CheckedArchive,
        provider: &dyn ArchiveProvider,
        member: &str,
    ) -> Vec<u8> {
        let member = MemberPath::parse(member).unwrap();
        read_capped(
            &mut |r, c| archive.read(provider, r, c),
            "label",
            &member,
            1 << 20,
        )
        .unwrap()
        .0
    }

    /// Opens `original` through the guard, then puts `replacement` at its path.
    fn swap_after_check(
        provider: &dyn ArchiveProvider,
        original: &Path,
        replacement: &Path,
    ) -> CheckedArchive {
        let archive = CheckedArchive::open(provider, original.to_str().unwrap()).unwrap();
        std::fs::rename(original, original.with_extension("moved")).unwrap();
        std::fs::rename(replacement, original).unwrap();
        archive
    }

    /// FER-488: zip and 7z are listed and read through the checked handle. Swapping another
    /// archive in at the same path after the check changes nothing.
    #[test]
    fn zip_and_7z_read_the_checked_archive_even_if_the_path_is_swapped() {
        let root = MemberPath::parse("").unwrap();

        let zip = ZipProvider::new();
        let a = build_zip("swap-a", &[("a.txt", b"checked".to_vec())], None);
        let b = build_zip(
            "swap-b",
            &[("a.txt", b"SWAPPED".to_vec()), ("b.txt", b"x".to_vec())],
            None,
        );
        let archive = swap_after_check(&zip, &a, &b);
        assert_eq!(archive.list(&zip, root.clone()).unwrap().entries.len(), 1);
        assert_eq!(read_through(&archive, &zip, "a.txt"), b"checked");
        let (_, declared, _) =
            ZipProvider::member_raw_info_opened(archive.source().unwrap(), "a.txt").unwrap();
        assert_eq!(declared, 7);
        drop(archive);
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(a.with_extension("moved"));

        let sz = crate::archive_sevenz::SevenZProvider::new();
        let a = build_7z("swap-a", &[("a.txt", b"checked")]);
        let b = build_7z("swap-b", &[("a.txt", b"SWAPPED"), ("b.txt", b"x")]);
        let archive = swap_after_check(&sz, &a, &b);
        assert_eq!(archive.list(&sz, root).unwrap().entries.len(), 1);
        assert_eq!(read_through(&archive, &sz, "a.txt"), b"checked");
        drop(archive);
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(a.with_extension("moved"));
    }

    /// FER-488: a path-only provider (unrar) is handed the final path of the pinned, checked file,
    /// and the file cannot be swapped while the provider may still open it.
    #[cfg(windows)]
    #[test]
    fn a_path_only_provider_gets_the_pinned_final_path() {
        struct PathOnly(std::cell::RefCell<Vec<String>>);
        impl ArchiveProvider for PathOnly {
            fn capabilities(&self, _f: ArchiveFormat) -> ArchiveCapabilities {
                unimplemented!()
            }
            fn list(&self, r: &ArchiveListRequest) -> Result<ArchiveListResponse, ArchiveError> {
                let Location::Archive { archive_path, .. } = &r.location else {
                    unreachable!()
                };
                self.0.borrow_mut().push(archive_path.clone());
                Ok(ArchiveListResponse {
                    location: r.location.clone(),
                    request_token: r.request_token,
                    entries: Vec::new(),
                })
            }
            fn read(
                &self,
                _r: &ArchiveReadRequest,
                _c: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
            ) -> Result<bool, ArchiveError> {
                unimplemented!()
            }
            fn extract(
                &self,
                _r: &ArchiveExtractRequest,
                _p: &mut dyn FnMut(ArchiveProgress) -> bool,
            ) -> Result<(), ArchiveError> {
                unimplemented!()
            }
            fn create(
                &self,
                _r: &ArchiveCreateRequest,
                _p: &mut dyn FnMut(ArchiveProgress) -> bool,
            ) -> Result<PathBuf, ArchiveError> {
                unimplemented!()
            }
            fn test(
                &self,
                _r: &ArchiveTestRequest,
                _p: &mut dyn FnMut(ArchiveProgress) -> bool,
            ) -> Result<ArchiveTestReport, ArchiveError> {
                unimplemented!()
            }
        }
        let provider = PathOnly(Default::default());
        let a = build_zip("pin", &[("a.txt", b"x".to_vec())], None);
        let archive = CheckedArchive::open(&provider, a.to_str().unwrap()).unwrap();
        archive
            .list(&provider, MemberPath::parse("").unwrap())
            .unwrap();
        let seen = provider.0.borrow()[0].clone();
        assert!(seen.starts_with(r"\\?\"), "{seen}");
        assert!(std::fs::rename(&a, a.with_extension("moved")).is_err());
        drop(archive);
        assert!(std::fs::rename(&a, a.with_extension("moved")).is_ok());
        let _ = std::fs::remove_file(a.with_extension("moved"));
    }
}
