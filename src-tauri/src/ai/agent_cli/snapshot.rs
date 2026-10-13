//! FER-490 E1: attachments are COPIED into `<workdir>\files\` before an agent runs, and the agent
//! only ever sees that copy (Claude: cwd = workdir, no `--add-dir`; Codex: `-C workdir`). Sibling
//! files of an attachment, links inside an attached folder and later swaps of the original path
//! are therefore out of the agent's reach.
//!
//! Copy rules (design decision):
//! - every source is opened with the FER-486 `path_guard` (handle-checked, no reparse point
//!   followed) and read through `GuardedFile::open_read` (ReOpenFile of the checked handle);
//! - inside a folder: symlinks/junctions are skipped, cloud placeholders are skipped, every child's
//!   handle must really sit inside the checked folder (`final_path`), at most `MAX_FILES` entries (files and sub-folders together, F2),
//!   `MAX_DEPTH` folder levels and `MAX_BYTES` in total, otherwise the question is refused;
//! - every copied file is checked for secrets (name + whole content, chunk by chunk). A sensitive
//!   file the user did not tick is not copied and is reported in `skipped`.
//!
//! FER-493 (additive): a top-level Office attachment (DOCX/XLSX/PPTX/ODF) also gets
//! `files\<name>.txt` with the text extracted by `office_text` (bounded zip/XML parser, read
//! through the same checked handle), and DOCX/PPTX get `files\<name>_media\` with at most
//! `office_text::MAX_MEDIA` PNG/JPEG/GIF pictures (`MAX_MEDIA_BYTES` in total). These companions
//! count toward the same `max_files`/`max_bytes` budget and the extracted text gets the same
//! secret check (an unticked document whose TEXT holds a secret is refused like any other).

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::args::{OfficeCompanion, PromptItem};
use super::commands::AgentError;
use crate::ai::attach::{self, AttachmentRef};
use crate::office_text;
use crate::path_guard::{self, Entry, GuardedDir, GuardedFile};

pub const MAX_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_FILES: usize = 2000;
pub const MAX_DEPTH: usize = 8;
/// Folder name inside the working folder; the prompt names files relative to the working folder.
pub const FILES_DIR: &str = "files";

const CHUNK: usize = 64 * 1024;
/// Bytes of the previous chunk kept for the secret scan (a token split across two chunks).
const OVERLAP: usize = 512;

/// Limits of one snapshot (tests use smaller ones).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_files: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            max_files: MAX_FILES,
            max_depth: MAX_DEPTH,
        }
    }
}

/// An item that was NOT copied (shown under the answer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub path: String,
    /// `sensitive` | `link` | `offline` | `unreadable`
    pub reason: &'static str,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    /// Top-level items, with paths relative to the working folder (`files\name`).
    pub items: Vec<PromptItem>,
    pub skipped: Vec<Skipped>,
    pub files: usize,
    /// Sub-folders created inside attached folders (F2: they count toward `max_files` too).
    pub dirs: usize,
    pub bytes: u64,
}

fn limit(what: &str) -> AgentError {
    AgentError::with_message("snapshot_limit", what.to_owned())
}

fn norm(p: &str) -> String {
    p.trim_start_matches(r"\\?\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// True when `child` (a final path) lies directly inside `parent` (a final path).
fn directly_inside(parent: &str, child: &str) -> bool {
    let (p, c) = (norm(parent), norm(child));
    c.strip_prefix(&p)
        .and_then(|rest| rest.strip_prefix('\\'))
        .is_some_and(|name| !name.is_empty() && !name.contains('\\'))
}

/// A free name in `dir` (`name`, `name (2)`, …).
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{name} ({i})")))
        .find(|p| !p.exists())
        .expect("a free name")
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['\\', '/', ':'])
}

struct Copier<'a> {
    dest_root: &'a Path,
    limits: Limits,
    out: Snapshot,
}

enum FileResult {
    Copied,
    Sensitive,
}

impl Copier<'_> {
    /// F2: files and sub-folders share one entry budget (`max_files`).
    fn entries_full(&self) -> bool {
        self.out.files + self.out.dirs + 1 > self.limits.max_files
    }

    fn entry_limit(&self) -> AgentError {
        limit(&format!(
            "maksimal {} file dan folder",
            self.limits.max_files
        ))
    }

    fn count_dir(&mut self) -> Result<(), AgentError> {
        if self.entries_full() {
            return Err(self.entry_limit());
        }
        self.out.dirs += 1;
        Ok(())
    }

    fn count_file(&mut self, size: u64) -> Result<(), AgentError> {
        if self.entries_full() {
            return Err(self.entry_limit());
        }
        if self.out.bytes + size > self.limits.max_bytes {
            return Err(limit(&format!(
                "maksimal {} MB",
                self.limits.max_bytes / (1024 * 1024)
            )));
        }
        Ok(())
    }

    /// Copy one checked file; `scan` = look for secrets (false when the user ticked it).
    fn copy_file(
        &mut self,
        file: &GuardedFile,
        dest: &Path,
        scan: bool,
    ) -> Result<FileResult, AgentError> {
        let name = Path::new(&file.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if scan && attach::sensitive_name(&name) {
            return Ok(FileResult::Sensitive);
        }
        self.count_file(file.meta.len())?;
        let mut src = file
            .open_read()
            .map_err(|_| AgentError::at("attachment_unavailable", &file.path))?;
        let mut out = File::create_new(dest).map_err(|_| AgentError::new("storage_failure"))?;
        let mut buf = vec![0u8; CHUNK];
        let mut tail: Vec<u8> = Vec::new();
        let mut copied: u64 = 0;
        loop {
            let n = src
                .read(&mut buf)
                .map_err(|_| AgentError::at("attachment_unavailable", &file.path))?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            // The file may have grown since it was measured.
            if self.out.bytes + copied > self.limits.max_bytes {
                drop(out);
                let _ = fs::remove_file(dest);
                return Err(limit(&format!(
                    "maksimal {} MB",
                    self.limits.max_bytes / (1024 * 1024)
                )));
            }
            if scan {
                let mut window = std::mem::take(&mut tail);
                window.extend_from_slice(&buf[..n]);
                if attach::sensitive_content(&String::from_utf8_lossy(&window)) {
                    drop(out);
                    let _ = fs::remove_file(dest);
                    return Ok(FileResult::Sensitive);
                }
                let keep = window.len().min(OVERLAP);
                tail = window[window.len() - keep..].to_vec();
            }
            out.write_all(&buf[..n])
                .map_err(|_| AgentError::new("storage_failure"))?;
        }
        self.out.files += 1;
        self.out.bytes += copied;
        Ok(FileResult::Copied)
    }

    fn copy_dir(&mut self, dir: &GuardedDir, dest: &Path, depth: usize) -> Result<(), AgentError> {
        if depth > self.limits.max_depth {
            return Err(limit(&format!(
                "kedalaman folder maksimal {}",
                self.limits.max_depth
            )));
        }
        fs::create_dir(dest).map_err(|_| AgentError::new("storage_failure"))?;
        let parent_final = dir
            .final_path()
            .ok_or_else(|| AgentError::at("attachment_unavailable", &dir.path))?;
        let scan_cap = self.limits.max_files + 1;
        let (rows, _, more) = dir
            .list(scan_cap, scan_cap)
            .map_err(|_| AgentError::at("attachment_unavailable", &dir.path))?;
        if more {
            return Err(self.entry_limit());
        }
        for row in rows {
            if !valid_name(&row.name) {
                continue;
            }
            let child = format!("{}\\{}", dir.path.trim_end_matches('\\'), row.name);
            let entry = match path_guard::open_entry(&child) {
                Ok(e) => e,
                Err(_) => {
                    self.skip(&child, "unreadable");
                    continue;
                }
            };
            match entry {
                Entry::LinkDir { .. } => self.skip(&child, "link"),
                Entry::File(f) if f.via_symlink => self.skip(&child, "link"),
                Entry::File(f) if f.offline => self.skip(&child, "offline"),
                Entry::File(f) => {
                    // The handle must be the child of the checked folder, not something a
                    // swapped-in junction pointed at.
                    if !f
                        .final_path()
                        .is_some_and(|fp| directly_inside(&parent_final, &fp))
                    {
                        self.skip(&child, "link");
                        continue;
                    }
                    if let FileResult::Sensitive =
                        self.copy_file(&f, &dest.join(&row.name), true)?
                    {
                        self.skip(&child, "sensitive");
                    }
                }
                Entry::Dir(d) => {
                    if !d
                        .final_path()
                        .is_some_and(|fp| directly_inside(&parent_final, &fp))
                    {
                        self.skip(&child, "link");
                        continue;
                    }
                    // F2: a sub-folder costs one entry of the shared file/folder budget.
                    self.count_dir()?;
                    self.copy_dir(&d, &dest.join(&row.name), depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// FER-493: a file Ferrum made (extracted text, picture), within the snapshot budget.
    fn write_extra(&mut self, dest: &Path, bytes: &[u8]) -> Result<(), AgentError> {
        self.count_file(bytes.len() as u64)?;
        let mut out = File::create_new(dest).map_err(|_| AgentError::new("storage_failure"))?;
        out.write_all(bytes)
            .map_err(|_| AgentError::new("storage_failure"))?;
        self.out.files += 1;
        self.out.bytes += bytes.len() as u64;
        Ok(())
    }

    /// FER-493: text (and pictures) of an Office attachment whose copy is `copy`. `scan` = the
    /// user did not tick it, so a secret in the extracted text refuses the attachment.
    fn office_companions(
        &mut self,
        file: &GuardedFile,
        format: office_text::Format,
        copy: &Path,
        workdir: &Path,
        scan: bool,
        raw: &str,
    ) -> Result<OfficeCompanion, AgentError> {
        let mut out = OfficeCompanion::default();
        let copy_name = copy
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rel = |p: &Path| {
            p.strip_prefix(workdir)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        // O4: one pass over the package for text AND pictures.
        static NO_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let (text, media) = match office_text::content_file(file, format, &NO_STOP) {
            Ok(c) => (c.text, c.media),
            Err(e) => (Err(e), office_text::MediaSet::default()),
        };
        match text {
            Err(error) => out.note = Some(format!("text extraction refused: {error}")),
            Ok(ex) if ex.text.trim().is_empty() => {
                out.note = Some("the document holds no text".to_owned());
            }
            Ok(ex) => {
                if scan && attach::sensitive_content(&ex.text) {
                    return Err(AgentError::at("sensitive_unconfirmed", raw));
                }
                let dest = free_name(self.dest_root, &format!("{copy_name}.txt"));
                self.write_extra(&dest, ex.text.as_bytes())?;
                out.text = Some(rel(&dest));
            }
        }
        // Pictures are optional: a refused package simply has none.
        if media.items.is_empty() {
            return Ok(out);
        }
        let dir = free_name(self.dest_root, &format!("{copy_name}_media"));
        self.count_dir()?;
        fs::create_dir(&dir).map_err(|_| AgentError::new("storage_failure"))?;
        let mut copied = 0;
        for media in media.items {
            if scan && attach::sensitive_content(&String::from_utf8_lossy(&media.bytes)) {
                // Info (review): listed under "not copied", so the user knows.
                self.skip(&format!("{raw} › {}", media.name), "sensitive");
                continue;
            }
            self.write_extra(&dir.join(&media.name), &media.bytes)?;
            copied += 1;
        }
        out.media = Some((rel(&dir), copied));
        Ok(out)
    }

    fn skip(&mut self, path: &str, reason: &'static str) {
        self.out.skipped.push(Skipped {
            path: path.to_owned(),
            reason,
        });
    }
}

/// Copy the checked attachments into `<workdir>\files\`. Top-level refusals (link, placeholder,
/// unreadable, sensitive without tick) are errors; problems inside folders are `skipped`.
pub fn take(
    refs: &[(AttachmentRef, String)],
    workdir: &Path,
    limits: Limits,
) -> Result<Snapshot, AgentError> {
    let root = workdir.join(FILES_DIR);
    fs::create_dir_all(&root).map_err(|_| AgentError::new("storage_failure"))?;
    let mut c = Copier {
        dest_root: &root,
        limits,
        out: Snapshot::default(),
    };
    for (r, path) in refs {
        let raw = r.path.as_str();
        let entry = path_guard::open_entry(path)
            .map_err(|_| AgentError::at("attachment_unavailable", raw))?;
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| valid_name(n))
            .ok_or_else(|| AgentError::at("attachment_unavailable", raw))?;
        let dest = free_name(c.dest_root, &name);
        let rel = dest
            .strip_prefix(workdir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| format!("{FILES_DIR}\\{name}"));
        match entry {
            Entry::LinkDir { .. } => return Err(AgentError::at("attachment_link", raw)),
            Entry::File(f) if f.via_symlink => return Err(AgentError::at("attachment_link", raw)),
            Entry::File(f) if f.offline => return Err(AgentError::at("attachment_offline", raw)),
            Entry::File(f) => {
                let size = f.meta.len();
                match c.copy_file(&f, &dest, !r.confirmed_sensitive)? {
                    FileResult::Copied => {
                        let office = match office_text::format_by_name(&name) {
                            Some(format) => Some(c.office_companions(
                                &f,
                                format,
                                &dest,
                                workdir,
                                !r.confirmed_sensitive,
                                raw,
                            )?),
                            None => None,
                        };
                        c.out.items.push(PromptItem {
                            path: rel,
                            is_dir: false,
                            size,
                            office,
                        });
                    }
                    // Not ticked: nothing of it reaches the agent.
                    FileResult::Sensitive => {
                        return Err(AgentError::at("sensitive_unconfirmed", raw));
                    }
                }
            }
            Entry::Dir(d) => {
                c.copy_dir(&d, &dest, 0)?;
                c.out.items.push(PromptItem {
                    path: rel,
                    is_dir: true,
                    size: 0,
                    office: None,
                });
            }
        }
    }
    Ok(c.out)
}
