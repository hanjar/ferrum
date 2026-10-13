//! FER-486 "Add to chat": bounded, read-only extraction of files the user attached to a chat
//! question. Everything here stays inside the backend: the webview only ever receives metadata and
//! flags (`inspect`), never file content. The content is read again at send time (`extract`) and
//! goes straight into the provider request built by `ai::ask`.
//!
//! Limits (design decision, FER-486):
//! - at most `MAX_ATTACHMENTS` items per message;
//! - text/code: at most `MAX_FILE_BYTES` per file and `MAX_TOTAL_BYTES` in total, decoded like the
//!   Quick View preview (lossy UTF-8; UTF-16 and other NUL-carrying files count as binary); a cut
//!   is marked `[dipotong: X dari Y byte]`;
//! - folders: names and sizes of at most `MAX_LISTING_ENTRIES` direct entries, never file content;
//! - Office documents on disk (DOCX/XLSX/PPTX/ODF, FER-493): text extracted by `office_text`
//!   (bounded zip/XML parser) and then treated like a text file (same caps, same secret check on
//!   the extracted text); a document the parser refuses stays metadata only, with the reason;
//! - PDF/images/media/archives/other binaries: metadata only (no parser runs here);
//! - archive members go through the existing bounded member preview (`preview_archive`).
//!
//! Safety rules shared with the preview: structural path validation, no folder reads as a file,
//! cloud placeholders are never hydrated (also through a symlink), symlinks/junctions are not
//! followed into listings, reads are capped before allocation. Nothing in this module logs.

use crate::archive_contract::MemberPath;
use crate::domain::{ErrorCode, FsError};
use crate::preview::{self, PreviewKind};
use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_FILE_BYTES: usize = 100 * 1024;
pub const MAX_TOTAL_BYTES: usize = 300 * 1024;
pub const MAX_LISTING_ENTRIES: usize = 200;
/// A folder with more entries than this is not walked to the end; the listing says "more than".
const MAX_LISTING_SCAN: usize = 10_000;
/// Longest path accepted from the webview (same order as Windows extended paths).
const MAX_PATH_CHARS: usize = 4096;

pub const NOTE_METADATA_ONLY: &str = "isi tidak didukung (metadata saja)";
pub const NOTE_TOO_LARGE_IN_ARCHIVE: &str = "terlalu besar untuk dibaca dari arsip (metadata saja)";
pub const NOTE_BUDGET_SPENT: &str = "isi tidak disertakan: batas total 300 KB sudah terpakai";
pub const NOTE_LINK: &str = "tautan/junction tidak diikuti (metadata saja)";
/// FER-493: content of an Office document is its extracted text.
pub const NOTE_OFFICE_TEXT: &str = "teks diekstrak";
pub const NOTE_OFFICE_EMPTY: &str = "dokumen tanpa teks (metadata saja)";
/// O4: the inspect deadline ran out; the text is extracted at send time.
pub const NOTE_OFFICE_LATER: &str = "akan diekstrak saat kirim";
/// First line of the body sent for an Office document.
pub const OFFICE_TEXT_HEADER: &str =
    "[Teks diekstrak dari dokumen oleh Ferrum; gambar, format dan tata letak tidak disertakan.]\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentKind {
    Text,
    Folder,
    Image,
    Pdf,
    Document,
    Media,
    Archive,
    Binary,
    Link,
}

/// What the provider receives for this item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Support {
    /// Text content (bounded).
    Content,
    /// Folder listing (names + sizes).
    Listing,
    /// Name, kind and size only.
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SensitiveReason {
    Name,
    Content,
}

/// Metadata the webview may see. Never carries content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfo {
    pub path: String,
    pub name: String,
    pub kind: AttachmentKind,
    pub size: u64,
    pub modified_ms: i64,
    pub in_archive: bool,
    pub support: Support,
    /// Set when the name or the content looks like a secret: the user must tick the item first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<SensitiveReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One attachment as the webview names it at send time.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRef {
    pub path: String,
    /// The user ticked "send anyway" for an item flagged sensitive.
    #[serde(default)]
    pub confirmed_sensitive: bool,
}

/// Result of reading one attachment for the prompt (backend only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub info: AttachmentInfo,
    /// Text content or folder listing; `None` for metadata-only items.
    pub body: Option<String>,
    /// Bytes of the source that went into `body` (text only).
    pub sent_bytes: u64,
    /// `Some((sent, total))` when the text was cut.
    pub truncated: Option<(u64, u64)>,
}

// ---------------------------------------------------------------------------------------------
// Sensitive-item guard
// ---------------------------------------------------------------------------------------------

static SENSITIVE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)^(?:
            \.env(?:\..*)? | .*\.env
          | .*\.(?:pem|key|pfx|p12|kdbx|kdb|keystore|jks|ppk|ovpn|asc|gpg|tfstate)
          | id_(?:rsa|dsa|ecdsa|ed25519)(?:[._-].*)?
          | credentials?(?:[._-].*)? | .*[._-]credentials?(?:[._-].*)?
          | secrets?(?:[._-].*)?
          | wallet(?:[._-].*)? | .*wallet\.dat
          | \.npmrc | \.pypirc | \.netrc | _netrc | \.git-credentials | \.pgpass | \.htpasswd
          | .*\.tfvars
        )$",
    )
    .expect("valid sensitive-name pattern")
});

static SENSITIVE_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\bdapi[a-f0-9]{32}\b",
        r"|\bsk-[A-Za-z0-9_\-]{20,}",
        r"|\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
        r"|\bgh[pousr]_[A-Za-z0-9]{30,}",
        r"|\bgithub_pat_[A-Za-z0-9_]{22,}",
        r"|\bglpat-[A-Za-z0-9_\-]{20,}",
        r"|\bxox[abprs]-[A-Za-z0-9\-]{10,}",
        r"|\bAIza[0-9A-Za-z_\-]{35}",
        // FER-486 A2: PEM and PGP ("-----BEGIN PGP PRIVATE KEY BLOCK-----") private keys.
        r"|-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----",
        r"|\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}",
        r"|(?i:\bAccountKey\s*=\s*[A-Za-z0-9+/=]{20,})",
        r"|(?i:\bSharedAccess(?:Signature|Key)\s*=\s*\S{8,})",
        r"|\bnpm_[A-Za-z0-9]{30,}",
        r"|\bhf_[A-Za-z0-9]{30,}",
        r"|\bya29\.[A-Za-z0-9_\-]{20,}",
        r"|\bxapp-[A-Za-z0-9\-]{10,}",
        r"|\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]+",
        r#"|(?i:\b(?:api[_-]?key|secret[_-]?key|client[_-]?secret|access[_-]?token|auth[_-]?token|private[_-]?key|password|passwd|pwd)\b["']?\s*[:=]\s*["']?[^\s"'`]{8,})"#,
    ))
    .expect("valid sensitive-content pattern")
});

pub fn sensitive_name(name: &str) -> bool {
    SENSITIVE_NAME.is_match(name)
}

pub fn sensitive_content(text: &str) -> bool {
    SENSITIVE_CONTENT.is_match(text)
}

// ---------------------------------------------------------------------------------------------
// Location
// ---------------------------------------------------------------------------------------------

enum Located {
    /// Handle-backed facts from the shared guard (B1): nothing is looked up by path again.
    Disk(crate::path_guard::Entry),
    Member {
        archive: String,
        member: MemberPath,
    },
}

fn invalid(message: &str) -> FsError {
    FsError::new(ErrorCode::InvalidPath, message)
}

/// Disk path, or archive file + member for a path inside an archive virtual location
/// (`C:\a.zip\dir\f.txt`, the same shape the panes use). The archive part must be a real file.
fn locate(raw: &str) -> Result<Located, FsError> {
    if raw.chars().count() > MAX_PATH_CHARS {
        return Err(invalid("Invalid attachment path"));
    }
    // FER-486 A1: shared read-path guard (device/NT namespaces in every spelling, ADS, pipe
    // shares). Nothing touches the filesystem before it.
    let path = crate::path_guard::check_str(raw)?;
    match crate::path_guard::open_entry(&path) {
        Ok(entry) => return Ok(Located::Disk(entry)),
        // Refused (pipe, character device): never fall through to the archive split.
        Err(error) if error.code == ErrorCode::InvalidPath => return Err(error),
        Err(_) => {}
    }
    let lower = path.to_lowercase();
    let mut best: Option<usize> = None;
    for ext in [".zip", ".rar", ".7z"] {
        let needle = format!("{ext}\\");
        let mut from = 0;
        while let Some(found) = lower[from..].find(&needle) {
            let cut = from + found + ext.len();
            if best.is_none_or(|c| cut < c) && crate::path_guard::checked_file(&path[..cut]).is_ok()
            {
                best = Some(cut);
            }
            from = cut;
        }
    }
    let Some(cut) = best else {
        return Err(FsError::new(ErrorCode::NotFound, "File not found").with_path(raw));
    };
    let member = path[cut + 1..].replace('\\', "/");
    let member = member.trim_end_matches('/');
    let member = MemberPath::parse(member).map_err(|_| invalid("Invalid archive member"))?;
    if member.as_str().is_empty() {
        return crate::path_guard::open_entry(&path[..cut]).map(Located::Disk);
    }
    Ok(Located::Member {
        archive: path[..cut].to_owned(),
        member,
    })
}

fn base_name(path: &str) -> String {
    path.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_owned()
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Kind decided from the name alone; `None` means "sniff the bytes" (text or binary).
fn kind_by_name(name: &str) -> Option<AttachmentKind> {
    if preview::image_mime_by_extension(name).is_some() {
        return Some(AttachmentKind::Image);
    }
    if preview::is_pdf_name(name) {
        return Some(AttachmentKind::Pdf);
    }
    let ext = extension(name);
    Some(match ext.as_str() {
        "docx" | "doc" | "docm" | "dotx" | "dotm" | "xlsx" | "xls" | "xlsm" | "xltx" | "xltm"
        | "pptx" | "ppt" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" | "odt" | "ott" | "ods"
        | "ots" | "odp" | "otp" | "epub" => AttachmentKind::Document,
        "mp4" | "webm" | "mov" | "m4v" | "ogv" | "mkv" | "avi" | "mp3" | "wav" | "ogg" | "flac"
        | "m4a" | "aac" | "wma" => AttachmentKind::Media,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "cab" | "iso" => {
            AttachmentKind::Archive
        }
        "exe" | "dll" | "sys" | "msi" | "bin" | "dat" | "db" | "sqlite" | "kdbx" | "kdb"
        | "pfx" | "p12" | "class" | "jar" | "o" | "obj" | "pdb" => AttachmentKind::Binary,
        _ => return None,
    })
}

fn lossy_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Cut `bytes` to at most `cap` bytes on a UTF-8 boundary (a lone partial char is dropped).
fn cut_utf8(bytes: &[u8], cap: usize) -> &[u8] {
    if bytes.len() <= cap {
        return bytes;
    }
    let mut end = cap;
    // Step back over continuation bytes (10xxxxxx) so a multi-byte char is not split.
    while end > 0 && end > cap.saturating_sub(4) && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    &bytes[..end]
}

fn is_text_kind(kind: PreviewKind) -> bool {
    matches!(
        kind,
        PreviewKind::Text | PreviewKind::Json | PreviewKind::Markdown
    )
}

/// Head of a checked disk file, at most `cap` bytes, read from the checked handle (B1). Cloud
/// placeholders are refused by `GuardedFile::open_read` (never hydrated).
fn read_disk_head(file: &crate::path_guard::GuardedFile, cap: usize) -> Result<Vec<u8>, FsError> {
    let path = file.path.as_str();
    let file = file.open_read()?;
    let mut bytes = Vec::with_capacity(cap.min(MAX_FILE_BYTES) + 1);
    file.take(cap as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| FsError::from(e).with_path(path))?;
    Ok(bytes)
}

// ---------------------------------------------------------------------------------------------
// Listings
// ---------------------------------------------------------------------------------------------

struct Listing {
    rows: Vec<(String, bool, u64)>,
    total: usize,
    more: bool,
}

fn listing_text(listing: &Listing) -> String {
    let mut rows = listing.rows.clone();
    rows.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    let mut out = String::from("Isi folder (nama dan ukuran saja; isi file tidak disertakan):\n");
    if rows.is_empty() {
        out.push_str("(folder kosong)\n");
    }
    for (name, is_dir, size) in &rows {
        if *is_dir {
            out.push_str(&format!("- {name}/ (folder)\n"));
        } else {
            out.push_str(&format!("- {name} ({size} byte)\n"));
        }
    }
    if listing.more {
        out.push_str(&format!(
            "[daftar dipotong: {} dari lebih dari {} entri]\n",
            rows.len(),
            listing.total
        ));
    } else if listing.total > rows.len() {
        out.push_str(&format!(
            "[daftar dipotong: {} dari {} entri]\n",
            rows.len(),
            listing.total
        ));
    }
    out
}

/// Folder listing through the checked directory handle (B1): no per-path lookups, links are
/// listed as entries and never followed.
fn list_disk_dir(dir: &crate::path_guard::GuardedDir) -> Result<Listing, FsError> {
    let (rows, total, more) = dir.list(MAX_LISTING_ENTRIES, MAX_LISTING_SCAN)?;
    Ok(Listing {
        rows: rows
            .into_iter()
            .map(|r| (r.name, r.is_dir, r.size))
            .collect(),
        total,
        more,
    })
}

fn list_archive_dir(archive: &str, member: &MemberPath) -> Result<Listing, FsError> {
    let provider = crate::ipc::archive_provider(archive)
        .map_err(|_| FsError::new(ErrorCode::Unsupported, "Archive format not supported"))?;
    // FER-488: listed through the checked archive file, never by path again.
    let listing = crate::preview_archive::CheckedArchive::open(provider.as_ref(), archive)?
        .list(provider.as_ref(), member.clone())
        .map_err(|_| FsError::new(ErrorCode::Io, "Archive could not be read"))?;
    let total = listing.entries.len();
    let rows = listing
        .entries
        .iter()
        .take(MAX_LISTING_ENTRIES)
        .map(|e| (e.name.clone(), e.is_dir, if e.is_dir { 0 } else { e.size }))
        .collect();
    Ok(Listing {
        rows,
        total,
        more: false,
    })
}

// ---------------------------------------------------------------------------------------------
// Reading one item
// ---------------------------------------------------------------------------------------------

/// FER-493 O4: how an Office document is extracted in this call (deadline, cancel flag).
#[derive(Clone, Copy)]
struct OfficeRun<'a> {
    timeout: Duration,
    stop: &'a AtomicBool,
    inspecting: bool,
}

static NO_STOP: AtomicBool = AtomicBool::new(false);

const SEND_RUN: OfficeRun<'static> = OfficeRun {
    timeout: crate::office_text::TIMEOUT,
    stop: &NO_STOP,
    inspecting: false,
};

/// `cap`: content budget for this item. `0` while inspecting means "scan with the per-file cap
/// but keep nothing"; the caller decides what to keep.
fn read_item(path: &str, cap: usize, run: OfficeRun<'_>) -> Result<Extracted, FsError> {
    match locate(path)? {
        Located::Disk(entry) => read_disk_item(path, entry, cap, run),
        Located::Member { archive, member } => read_member_item(path, &archive, &member, cap),
    }
}

fn metadata_only(mut info: AttachmentInfo, note: &str) -> Extracted {
    info.support = Support::Metadata;
    info.note = Some(note.to_owned());
    Extracted {
        info,
        body: None,
        sent_bytes: 0,
        truncated: None,
    }
}

fn name_flag(name: &str) -> Option<SensitiveReason> {
    sensitive_name(name).then_some(SensitiveReason::Name)
}

fn read_disk_item(
    path: &str,
    entry: crate::path_guard::Entry,
    cap: usize,
    run: OfficeRun<'_>,
) -> Result<Extracted, FsError> {
    use crate::path_guard::Entry;
    let (disk, meta) = match &entry {
        Entry::File(f) => (f.path.as_str(), &f.meta),
        Entry::Dir(d) => (d.path.as_str(), &d.meta),
        Entry::LinkDir { path, meta } => (path.as_str(), meta),
    };
    let name = base_name(disk);
    let mut info = AttachmentInfo {
        path: path.to_owned(),
        name: name.clone(),
        kind: AttachmentKind::Binary,
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified_ms: crate::fs_service::modified_ms(meta),
        in_archive: false,
        support: Support::Metadata,
        sensitive: name_flag(&name),
        note: None,
    };
    let file = match &entry {
        Entry::LinkDir { .. } => {
            info.kind = AttachmentKind::Link;
            info.size = 0;
            return Ok(metadata_only(info, NOTE_LINK));
        }
        Entry::Dir(dir) => {
            info.kind = AttachmentKind::Folder;
            let listing = list_disk_dir(dir)?;
            info.support = Support::Listing;
            return Ok(Extracted {
                body: Some(listing_text(&listing)),
                info,
                sent_bytes: 0,
                truncated: None,
            });
        }
        Entry::File(file) => file,
    };
    if let Some(kind) = kind_by_name(&name) {
        info.kind = kind;
        if let Some(format) = crate::office_text::format_by_name(&name) {
            return Ok(office_item(
                info,
                file,
                format,
                cap.min(MAX_FILE_BYTES),
                run,
            ));
        }
        return Ok(metadata_only(info, NOTE_METADATA_ONLY));
    }
    let size = meta.len();
    let read_cap = cap.min(MAX_FILE_BYTES);
    // Sniff window even when no content may be kept (inspect / spent budget).
    let head = read_disk_head(file, read_cap.max(8 * 1024))?;
    if !is_text_kind(preview::classify(&name, &head)) {
        info.kind = AttachmentKind::Binary;
        return Ok(metadata_only(info, NOTE_METADATA_ONLY));
    }
    info.kind = AttachmentKind::Text;
    Ok(text_item(info, &head, size, read_cap))
}

fn text_item(mut info: AttachmentInfo, head: &[u8], size: u64, cap: usize) -> Extracted {
    let scan = lossy_text(cut_utf8(head, MAX_FILE_BYTES));
    if info.sensitive.is_none() && sensitive_content(&scan) {
        info.sensitive = Some(SensitiveReason::Content);
    }
    if cap == 0 {
        let mut out = metadata_only(info, NOTE_BUDGET_SPENT);
        out.info.kind = AttachmentKind::Text;
        return out;
    }
    let kept = cut_utf8(head, cap);
    let sent = kept.len() as u64;
    let total = size.max(head.len() as u64);
    info.support = Support::Content;
    Extracted {
        info,
        body: Some(lossy_text(kept)),
        sent_bytes: sent,
        truncated: (sent < total).then_some((sent, total)),
    }
}

/// FER-493: an Office document on disk, read through the checked handle and turned into text by
/// the bounded parser. From here on it is handled like a text file: per-file `cap`, secret check
/// on the WHOLE extracted text, `[dipotong]` note when cut.
fn office_item(
    mut info: AttachmentInfo,
    file: &crate::path_guard::GuardedFile,
    format: crate::office_text::Format,
    cap: usize,
    run: OfficeRun<'_>,
) -> Extracted {
    use crate::office_text::OfficeError;
    let extraction =
        match crate::office_text::extract_file_cached(file, format, run.timeout, run.stop) {
            Ok(e) => e,
            // O4: the short inspect deadline ran out: the chip says so, send time tries again with
            // the full deadline (and the secret check runs then).
            Err(OfficeError::Timeout) if run.inspecting => {
                return metadata_only(info, NOTE_OFFICE_LATER);
            }
            Err(error) => {
                return metadata_only(
                    info,
                    &format!("teks tidak dapat diekstrak: {error} (metadata saja)"),
                );
            }
        };
    if info.sensitive.is_none() && sensitive_content(&extraction.text) {
        info.sensitive = Some(SensitiveReason::Content);
    }
    if extraction.text.trim().is_empty() {
        return metadata_only(info, NOTE_OFFICE_EMPTY);
    }
    // The header line counts toward the same budget as the text.
    let cap = cap.saturating_sub(OFFICE_TEXT_HEADER.len());
    if cap == 0 {
        return metadata_only(info, NOTE_BUDGET_SPENT);
    }
    let bytes = extraction.text.as_bytes();
    let kept = cut_utf8(bytes, cap);
    let sent = kept.len() as u64;
    let total = bytes.len() as u64;
    info.support = Support::Content;
    info.note = Some(NOTE_OFFICE_TEXT.to_owned());
    let mut body = String::with_capacity(OFFICE_TEXT_HEADER.len() + kept.len());
    body.push_str(OFFICE_TEXT_HEADER);
    body.push_str(&String::from_utf8_lossy(kept));
    Extracted {
        info,
        body: Some(body),
        sent_bytes: sent,
        truncated: (sent < total).then_some((sent, total)),
    }
}

fn read_member_item(
    path: &str,
    archive: &str,
    member: &MemberPath,
    cap: usize,
) -> Result<Extracted, FsError> {
    let name = base_name(member.as_str());
    let mut info = AttachmentInfo {
        path: path.to_owned(),
        name: name.clone(),
        kind: AttachmentKind::Binary,
        size: 0,
        modified_ms: 0,
        in_archive: true,
        support: Support::Metadata,
        sensitive: name_flag(&name),
        note: None,
    };
    // Locate the member (size, folder flag) through its parent listing.
    let parent = member
        .parent()
        .unwrap_or_else(|| MemberPath::parse("").expect("root"));
    let parent_listing = {
        let provider = crate::ipc::archive_provider(archive)
            .map_err(|_| FsError::new(ErrorCode::Unsupported, "Archive format not supported"))?;
        crate::preview_archive::CheckedArchive::open(provider.as_ref(), archive)?
            .list(provider.as_ref(), parent)
            .map_err(|_| FsError::new(ErrorCode::Io, "Archive could not be read"))?
    };
    let entry = parent_listing
        .entries
        .iter()
        .find(|e| &e.member_path == member)
        .ok_or_else(|| FsError::new(ErrorCode::NotFound, "Archive member not found"))?;
    info.size = entry.size;
    info.modified_ms = entry.modified_ms.unwrap_or(0);
    if entry.is_dir {
        info.kind = AttachmentKind::Folder;
        info.size = 0;
        let listing = list_archive_dir(archive, member)?;
        info.support = Support::Listing;
        return Ok(Extracted {
            body: Some(listing_text(&listing)),
            info,
            sent_bytes: 0,
            truncated: None,
        });
    }
    if let Some(kind) = kind_by_name(&name) {
        info.kind = kind;
        return Ok(metadata_only(info, NOTE_METADATA_ONLY));
    }
    // Same bounded, never-extracting reader as the Quick View member preview (4 MiB ceiling,
    // encrypted members and extreme ratios refused).
    let data = crate::preview_archive::read_member_preview(
        archive,
        member.as_str(),
        preview::ABSOLUTE_MAX_BYTES,
    )?;
    if data.too_large {
        info.kind = AttachmentKind::Text;
        return Ok(metadata_only(info, NOTE_TOO_LARGE_IN_ARCHIVE));
    }
    if !is_text_kind(data.kind) {
        return Ok(metadata_only(info, NOTE_METADATA_ONLY));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.data.as_bytes())
        .map_err(|_| FsError::new(ErrorCode::Io, "Archive member could not be read"))?;
    info.kind = AttachmentKind::Text;
    info.size = data.size;
    let size = data.size;
    Ok(text_item(info, &bytes, size, cap.min(MAX_FILE_BYTES)))
}

// ---------------------------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------------------------

/// Outcome of inspecting one path for a chip: metadata + flags, or a safe error message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectResult {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<AttachmentInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Metadata and sensitivity flags for the composer chips. Reads at most the per-file cap of each
/// text file to look for secrets; the content itself is dropped here.
pub fn inspect(paths: &[String]) -> Result<Vec<InspectResult>, FsError> {
    inspect_cancellable(paths, &[])
}

/// `inspect` with one cancel flag per path (FER-493 O4: removing a chip stops its Office
/// extraction). Office documents get the short `INSPECT_TIMEOUT`; the result is cached, so the
/// send that follows does not parse the document again.
pub fn inspect_cancellable(
    paths: &[String],
    stops: &[&AtomicBool],
) -> Result<Vec<InspectResult>, FsError> {
    if paths.len() > MAX_ATTACHMENTS {
        return Err(invalid("Too many attachments"));
    }
    Ok(paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let run = OfficeRun {
                timeout: crate::office_text::INSPECT_TIMEOUT,
                stop: stops.get(i).copied().unwrap_or(&NO_STOP),
                inspecting: true,
            };
            (path, read_item(path, MAX_FILE_BYTES, run))
        })
        .map(|(path, result)| match result {
            Ok(mut extracted) => {
                extracted.body = None;
                // Chips show what WILL happen; the budget note only applies at send time.
                if extracted.info.support == Support::Content
                    && extracted.info.note.as_deref() != Some(NOTE_OFFICE_TEXT)
                {
                    extracted.info.note = None;
                }
                InspectResult {
                    path: path.clone(),
                    info: Some(extracted.info),
                    error: None,
                }
            }
            Err(error) => InspectResult {
                path: path.clone(),
                info: None,
                error: Some(error.message),
            },
        })
        .collect())
}

/// Reads every attachment for the prompt, in order, under the per-file and total budgets.
/// A file that cannot be read any more becomes a metadata-only entry with the reason (the user
/// still gets an answer about the others). Paths must be unique and at most `MAX_ATTACHMENTS`.
pub fn extract(refs: &[AttachmentRef]) -> Vec<Extracted> {
    let mut remaining = MAX_TOTAL_BYTES;
    refs.iter()
        .map(|r| {
            let cap = remaining.min(MAX_FILE_BYTES);
            let extracted = match read_item(&r.path, cap, SEND_RUN) {
                Ok(item) => item,
                Err(error) => Extracted {
                    info: AttachmentInfo {
                        path: r.path.clone(),
                        name: base_name(&r.path),
                        kind: AttachmentKind::Binary,
                        size: 0,
                        modified_ms: 0,
                        in_archive: false,
                        support: Support::Metadata,
                        sensitive: name_flag(&base_name(&r.path)),
                        note: Some(format!("tidak dapat dibaca: {}", error.message)),
                    },
                    body: None,
                    sent_bytes: 0,
                    truncated: None,
                },
            };
            let used = extracted.body.as_ref().map_or(0, String::len);
            remaining = remaining.saturating_sub(used);
            extracted
        })
        .collect()
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
