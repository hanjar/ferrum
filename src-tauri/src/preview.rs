//! Safe bounded file preview (F2-17). Reads AT MOST `max_bytes` from the
//! start of a file and classifies it as text (with JSON/Markdown variants),
//! image, or binary (NUL sniff). The renderer on the TS side never executes
//! content: text is displayed as text, images are served through the
//! `ferrum-media` protocol (no base64 round-trip), and anything suspicious is
//! reported as unsupported instead of rendered.

use crate::domain::{ErrorCode, FsError, validate_path};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

/// Absolute ceiling for any preview read, whatever the caller asks for.
pub(crate) const ABSOLUTE_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// NUL sniff window for binary detection.
const SNIFF_BYTES: u64 = 8 * 1024;
/// Largest PDF handed to the webview viewer (FER-103). Above it the result is
/// `too_large` and the UI offers "Open in default app" instead.
pub(crate) const PDF_PREVIEW_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// A real PDF carries `%PDF-` within its first 1024 bytes (spec allows junk
/// before the header); anything else is not fed to the viewer.
const PDF_MAGIC_WINDOW: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewKind {
    Text,
    Json,
    Markdown,
    Image,
    Video,
    Audio,
    /// FER-103: a PDF on disk. Never read here; Quick View shows an
    /// "unavailable" message and Open in default app (no in-app viewer, see
    /// docs/quick-view-pdf.md).
    Pdf,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewData {
    pub kind: PreviewKind,
    /// Base64 of the bytes actually read (head of the file).
    pub data: String,
    /// True when the file is larger than what was read.
    pub truncated: bool,
    /// True when the file is larger than the (clamped) text limit: nothing
    /// was read, `data` is empty and the UI shows metadata only (PRV-03).
    #[serde(default)]
    pub too_large: bool,
    /// Full file size in bytes.
    pub size: u64,
    pub modified_ms: i64,
}

pub(crate) fn image_mime_by_extension(name: &str) -> Option<&'static str> {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// Video/audio kinds by extension. Like images they are never read here:
/// playback (a separate component) goes through the `ferrum-media` protocol.
fn media_kind_by_extension(name: &str) -> Option<PreviewKind> {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "webm" | "mov" | "m4v" | "ogv" => Some(PreviewKind::Video),
        "mp3" | "wav" | "ogg" | "flac" | "m4a" | "aac" => Some(PreviewKind::Audio),
        _ => None,
    }
}

/// `.pdf` extension (case-insensitive).
pub(crate) fn is_pdf_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// True when `head` (first bytes of the file) holds the `%PDF-` header.
pub(crate) fn has_pdf_magic(head: &[u8]) -> bool {
    let window = &head[..head.len().min(PDF_MAGIC_WINDOW)];
    window.windows(5).any(|w| w == b"%PDF-")
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

/// Classify without reading (extension first), then confirm text-ness with
/// a NUL sniff on the actual bytes.
pub(crate) fn classify(name: &str, head: &[u8]) -> PreviewKind {
    if image_mime_by_extension(name).is_some() {
        return PreviewKind::Image;
    }
    if let Some(k) = media_kind_by_extension(name) {
        return k;
    }
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "json" => PreviewKind::Json,
        "md" | "markdown" => PreviewKind::Markdown,
        _ if looks_binary(&head[..head.len().min(SNIFF_BYTES as usize)]) => PreviewKind::Binary,
        _ => PreviewKind::Text,
    }
}

/// Safe, fixed message: the file exists only in the cloud and is not read.
pub(crate) fn offline_error() -> FsError {
    FsError::new(
        ErrorCode::Unsupported,
        "File is stored online only - download it first to preview",
    )
}

// FER-486 B1: the link/target placeholder check (formerly `symlink_target_offline`) now lives
// in `path_guard::open_entry`, which reads both attribute sets from checked handles.

/// Read `path` for a text preview when it fits `max_bytes` (clamped to a
/// 4 MiB ceiling). A file LARGER than the limit is not read at all: the
/// result has `too_large = true` and empty `data` (metadata-only fallback).
/// Classify it. Never follows the content into any renderer on this side.
/// Images are NOT read: they render through the `ferrum-media` protocol
/// (`convertFileSrc` on the TS side), so only their metadata and readability
/// are checked here and `data` comes back empty.
pub fn read_preview(path: &str, max_bytes: u64) -> Result<PreviewData, FsError> {
    validate_path(path)?;
    // FER-486 A1/B1: the path is resolved once by the shared guard (device/NT namespaces, ADS,
    // pipes and character devices refused); metadata and every read come from the checked handle.
    let guarded = match crate::path_guard::open_entry(path)? {
        crate::path_guard::Entry::File(file) => file,
        _ => {
            return Err(FsError::new(
                ErrorCode::NotADirectory,
                "Cannot preview a folder — select a file",
            )
            .with_path(path));
        }
    };
    let path = guarded.path.as_str();
    let limit = max_bytes.clamp(1, ABSOLUTE_MAX_BYTES);
    let p = Path::new(path);
    let meta = &guarded.meta;
    // FER-103: a OneDrive-style cloud placeholder would be downloaded by a read. Never hydrate it
    // from a preview; the guard checks the link and (for a symlink) the target attributes.
    if guarded.offline {
        return Err(offline_error().with_path(path));
    }
    let size = meta.len();
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if image_mime_by_extension(&name).is_some() {
        // Open once so an unreadable file still surfaces as a structured
        // error, then skip the read + base64 round-trip: the webview loads
        // the bytes itself via the `ferrum-media` protocol. `truncated` stays false —
        // the webview renders the whole file, not a head.
        guarded.open_read()?;
        return Ok(PreviewData {
            kind: PreviewKind::Image,
            data: String::new(),
            truncated: false,
            too_large: false,
            size,
            modified_ms: crate::fs_service::modified_ms(meta),
        });
    }
    if let Some(kind) = media_kind_by_extension(&name) {
        // Same as images: readability check only, the webview streams bytes.
        guarded.open_read()?;
        return Ok(PreviewData {
            kind,
            data: String::new(),
            truncated: false,
            too_large: false,
            size,
            modified_ms: crate::fs_service::modified_ms(meta),
        });
    }
    if is_pdf_name(&name) {
        // FER-103: the webview viewer reads the bytes itself via the `ferrum-media`
        // protocol. Here: size cap, then a 1 KiB magic check so a renamed
        // non-PDF is reported as binary instead of reaching the viewer.
        if size > PDF_PREVIEW_MAX_BYTES {
            return Ok(PreviewData {
                kind: PreviewKind::Pdf,
                data: String::new(),
                truncated: false,
                too_large: true,
                size,
                modified_ms: crate::fs_service::modified_ms(meta),
            });
        }
        let file = guarded.open_read()?;
        let mut head = Vec::with_capacity(PDF_MAGIC_WINDOW);
        file.take(PDF_MAGIC_WINDOW as u64)
            .read_to_end(&mut head)
            .map_err(|e| FsError::from(e).with_path(path))?;
        let kind = if has_pdf_magic(&head) {
            PreviewKind::Pdf
        } else {
            PreviewKind::Binary
        };
        return Ok(PreviewData {
            kind,
            data: String::new(),
            truncated: false,
            too_large: false,
            size,
            modified_ms: crate::fs_service::modified_ms(meta),
        });
    }
    if size > limit {
        // Over the limit: do not open or read the payload.
        return Ok(PreviewData {
            kind: classify(&name, &[]),
            data: String::new(),
            truncated: false,
            too_large: true,
            size,
            modified_ms: crate::fs_service::modified_ms(meta),
        });
    }
    let file = guarded.open_read()?;
    let mut bytes: Vec<u8> = Vec::with_capacity(limit.min(1024 * 1024) as usize);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| FsError::from(e).with_path(path))?;
    let truncated = size > bytes.len() as u64;
    Ok(PreviewData {
        kind: classify(&name, &bytes),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        truncated,
        too_large: false,
        size,
        modified_ms: crate::fs_service::modified_ms(meta),
    })
}

/// FER-453: the bytes of one PDF for the sandboxed pdf.js viewer. Same guard as
/// `read_preview` (device/NT namespaces, ADS, placeholders refused; every read comes
/// from the checked handle). Refuses anything over `PDF_PREVIEW_MAX_BYTES` before
/// reading, caps the read at that limit even if the file grows, and requires the
/// `%PDF-` magic in the first 1 KiB. Bytes are never sent to any renderer path here.
pub fn read_pdf_bytes(path: &str) -> Result<Vec<u8>, FsError> {
    validate_path(path)?;
    let guarded = match crate::path_guard::open_entry(path)? {
        crate::path_guard::Entry::File(file) => file,
        _ => {
            return Err(FsError::new(
                ErrorCode::NotADirectory,
                "Cannot preview a folder — select a file",
            )
            .with_path(path));
        }
    };
    let path = guarded.path.as_str();
    if !is_pdf_name(path) {
        return Err(FsError::new(ErrorCode::Unsupported, "Not a PDF file").with_path(path));
    }
    let size = guarded.meta.len();
    if size > PDF_PREVIEW_MAX_BYTES {
        return Err(FsError::new(
            ErrorCode::Unsupported,
            "PDF larger than the preview limit (50 MiB)",
        )
        .with_path(path));
    }
    let file = guarded.open_read()?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(PDF_PREVIEW_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| FsError::from(e).with_path(path))?;
    if bytes.len() as u64 > PDF_PREVIEW_MAX_BYTES {
        return Err(FsError::new(
            ErrorCode::Unsupported,
            "PDF larger than the preview limit (50 MiB)",
        )
        .with_path(path));
    }
    let head_len = bytes.len().min(PDF_MAGIC_WINDOW);
    if !has_pdf_magic(&bytes[..head_len]) {
        return Err(FsError::new(ErrorCode::Unsupported, "Not a PDF file").with_path(path));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f217-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_file(&dir);
        dir
    }

    fn preview(path: &Path, max: u64) -> PreviewData {
        read_preview(path.to_str().unwrap(), max).expect("preview ok")
    }

    #[test]
    fn text_files_classify_and_round_trip() {
        let p = temp_path("text");
        fs::write(&p, "héllo wörld — unicode preview").unwrap();
        let d = preview(&p, 1024);
        assert_eq!(d.kind, PreviewKind::Text);
        assert!(!d.truncated);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&d.data)
            .expect("valid base64");
        assert_eq!(
            String::from_utf8_lossy(&raw),
            "héllo wörld — unicode preview"
        );
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn json_and_markdown_get_their_kinds() {
        let pj = temp_path("cfg").with_file_name("config-ünicode.json");
        fs::write(&pj, br#"{"ok":true}"#).unwrap();
        assert_eq!(preview(&pj, 1024).kind, PreviewKind::Json);
        let pm = temp_path("readme").with_file_name("README.md");
        fs::write(&pm, b"# Title").unwrap();
        assert_eq!(preview(&pm, 1024).kind, PreviewKind::Markdown);
        let _ = fs::remove_file(&pj);
        let _ = fs::remove_file(&pm);
    }

    #[test]
    fn images_classify_by_extension_without_a_base64_round_trip() {
        for name in ["x.png", "y.JPG", "z.webp", "pict.svg"] {
            let p = temp_path("img");
            let target = p.with_file_name(name);
            fs::write(&target, b"\x89PNG fake bytes").unwrap();
            let d = preview(&target, 1024);
            assert_eq!(d.kind, PreviewKind::Image, "{name}");
            // Images render via the ferrum-media protocol: no bytes are shipped.
            assert!(d.data.is_empty(), "{name}");
            assert!(!d.truncated, "{name}");
            let _ = fs::remove_file(&target);
        }
    }

    #[test]
    fn pdf_is_classified_by_magic_and_never_read() {
        let dir = temp_path("pdfdir");
        fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("Doc.PDF");
        fs::write(&ok, b"%PDF-1.7\n%\xE2\xE3\xCF\xD3 body").unwrap();
        // 10 bytes limit must not matter: the viewer streams it.
        let d = preview(&ok, 10);
        assert_eq!(d.kind, PreviewKind::Pdf);
        assert!(d.data.is_empty() && !d.too_large && !d.truncated);
        // junk before the header is tolerated (within 1 KiB)
        let junk = dir.join("junk.pdf");
        let mut b = vec![b' '; 100];
        b.extend_from_slice(b"%PDF-1.4");
        fs::write(&junk, &b).unwrap();
        assert_eq!(preview(&junk, 10).kind, PreviewKind::Pdf);
        // renamed non-PDF is binary, never reaches the viewer
        let fake = dir.join("fake.pdf");
        fs::write(&fake, b"MZ not a pdf at all").unwrap();
        assert_eq!(preview(&fake, 1024).kind, PreviewKind::Binary);
        // header past the 1 KiB window does not count
        let late = dir.join("late.pdf");
        let mut b = vec![b'x'; 2000];
        b.extend_from_slice(b"%PDF-1.4");
        fs::write(&late, &b).unwrap();
        assert_eq!(preview(&late, 1024).kind, PreviewKind::Binary);
        assert_eq!(serde_json::to_value(PreviewKind::Pdf).unwrap(), "pdf");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_over_the_cap_is_too_large_without_reading() {
        let p = temp_path("bigpdf").with_extension("pdf");
        let f = fs::File::create(&p).unwrap();
        f.set_len(PDF_PREVIEW_MAX_BYTES + 1).unwrap(); // sparse
        drop(f);
        let d = preview(&p, u64::MAX);
        assert_eq!(d.kind, PreviewKind::Pdf);
        assert!(d.too_large && d.data.is_empty());
        let _ = fs::remove_file(&p);
        let err = read_preview(r"Z:\nope\missing.pdf", 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn nul_bytes_mean_binary_even_with_a_text_extension() {
        let p = temp_path("bin");
        fs::write(&p, b"start of text\0with a NUL inside").unwrap();
        let d = preview(&p, 1024);
        assert_eq!(d.kind, PreviewKind::Binary);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn files_over_the_limit_are_not_read() {
        let p = temp_path("big");
        let body = vec![b'a'; 10_000];
        fs::write(&p, &body).unwrap();
        let d = preview(&p, 1000);
        assert_eq!(d.size, 10_000);
        assert!(d.too_large);
        assert!(d.data.is_empty()); // payload never read
        assert!(!d.truncated);
        // exactly at the limit still previews fully
        let d = preview(&p, 10_000);
        assert!(!d.too_large);
        assert_eq!(d.data.len(), base64_len(10_000));
        // ceiling: asking for the world still clamps, small file is fine
        let d = preview(&p, u64::MAX);
        assert!(!d.too_large && !d.truncated);
        assert_eq!(d.size, 10_000);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn limit_is_clamped_to_the_absolute_ceiling() {
        let p = temp_path("ceil");
        let f = fs::File::create(&p).unwrap();
        f.set_len(ABSOLUTE_MAX_BYTES + 1).unwrap(); // sparse, 4 MiB + 1
        drop(f);
        let d = preview(&p, u64::MAX);
        assert!(d.too_large);
        assert!(d.data.is_empty());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn video_and_audio_classify_by_extension_without_reading() {
        for (name, kind) in [
            ("a.mp4", PreviewKind::Video),
            ("b.WEBM", PreviewKind::Video),
            ("c.mov", PreviewKind::Video),
            ("d.m4v", PreviewKind::Video),
            ("e.ogv", PreviewKind::Video),
            ("f.mp3", PreviewKind::Audio),
            ("g.wav", PreviewKind::Audio),
            ("h.ogg", PreviewKind::Audio),
            ("i.flac", PreviewKind::Audio),
            ("j.m4a", PreviewKind::Audio),
            ("k.aac", PreviewKind::Audio),
        ] {
            let target = temp_path("media").with_file_name(name);
            fs::write(&target, vec![0u8; 5000]).unwrap();
            // size > limit must not matter: media is never read or too_large
            let d = preview(&target, 10);
            assert_eq!(d.kind, kind, "{name}");
            assert!(d.data.is_empty(), "{name}");
            assert!(!d.too_large && !d.truncated, "{name}");
            let _ = fs::remove_file(&target);
        }
        let err = read_preview(
            // FER-486: a backslash path (the old literal held a stray newline, which the read-path
            // guard now refuses as a control character).
            r"Z:\nope\missing.mp4",
            1024,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn cloud_placeholder_attributes_are_detected() {
        use crate::media_meta::offline_attrs;
        assert!(offline_attrs(0x1000)); // OFFLINE
        assert!(offline_attrs(0x0004_0000)); // RECALL_ON_OPEN
        assert!(offline_attrs(0x0040_0000)); // RECALL_ON_DATA_ACCESS
        assert!(offline_attrs(0x20 | 0x0040_0000)); // ARCHIVE + recall
        assert!(!offline_attrs(0x20)); // plain archive-bit file
        assert!(!offline_attrs(0));
        let e = offline_error();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.path.is_none());
    }

    #[test]
    fn real_symlink_to_a_normal_file_still_previews() {
        let target = temp_path("linktarget");
        fs::write(&target, "plain").unwrap();
        let link = temp_path("link");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&target, &link).is_ok();
        if made {
            let d = preview(&link, 1024);
            assert_eq!(d.kind, PreviewKind::Text);
            let _ = fs::remove_file(&link);
        }
        let _ = fs::remove_file(&target);
    }

    fn base64_len(n: u64) -> usize {
        (n as usize).div_ceil(3) * 4
    }

    #[test]
    fn empty_files_preview_as_empty_text() {
        let p = temp_path("empty");
        fs::write(&p, b"").unwrap();
        let d = preview(&p, 1024);
        assert_eq!(d.kind, PreviewKind::Text);
        assert_eq!(d.size, 0);
        assert!(d.data.is_empty());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn errors_are_structured() {
        let err = read_preview(r"Z:\nope\missing.txt", 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        // The media-protocol short-circuit still checks readability for images.
        let err = read_preview(r"Z:\nope\missing.png", 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let dir = temp_path("dir");
        fs::create_dir_all(&dir).unwrap();
        let err = read_preview(dir.to_str().unwrap(), 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotADirectory);
        assert!(read_preview("", 1024).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_serializes_camel_case() {
        let d = PreviewData {
            kind: PreviewKind::Text,
            data: "aGk=".into(),
            truncated: false,
            too_large: false,
            size: 2,
            modified_ms: 1,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "text");
        assert_eq!(json["modifiedMs"], 1);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["tooLarge"], false);
        assert_eq!(serde_json::to_value(PreviewKind::Video).unwrap(), "video");
    }

    fn pdf_temp(tag: &str) -> PathBuf {
        temp_path(tag).with_extension("pdf")
    }

    #[test]
    fn pdf_bytes_round_trip_a_small_valid_pdf() {
        let p = pdf_temp("pdfbytes-ok");
        let body = b"%PDF-1.4
1 0 obj << /Type /Catalog >> endobj
%%EOF
";
        fs::write(&p, body).unwrap();
        let got = read_pdf_bytes(p.to_str().unwrap()).expect("pdf bytes");
        assert_eq!(got, body);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn pdf_bytes_reject_a_renamed_non_pdf() {
        let p = pdf_temp("pdfbytes-fake");
        fs::write(&p, b"this is not a pdf at all").unwrap();
        let err = read_pdf_bytes(p.to_str().unwrap()).expect_err("must refuse");
        assert_eq!(err.code, ErrorCode::Unsupported);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn pdf_bytes_reject_a_non_pdf_extension() {
        let p = temp_path("pdfbytes-ext");
        fs::write(&p, b"%PDF-1.4 body").unwrap();
        let err = read_pdf_bytes(p.to_str().unwrap()).expect_err("extension gate");
        assert_eq!(err.code, ErrorCode::Unsupported);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn pdf_bytes_reject_over_the_cap_before_reading() {
        let p = pdf_temp("pdfbytes-big");
        let f = fs::File::create(&p).unwrap();
        f.set_len(PDF_PREVIEW_MAX_BYTES + 1).unwrap();
        drop(f);
        let err = read_pdf_bytes(p.to_str().unwrap()).expect_err("over cap");
        assert_eq!(err.code, ErrorCode::Unsupported);
        let _ = fs::remove_file(&p);
    }
}
