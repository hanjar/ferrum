//! Read-only media metadata (PRV-06 / FER-403): EXIF for JPEG, dimensions for
//! PNG/GIF, duration for WAV and MP4/MOV. Own bounded parsers, no new crates.
//!
//! Privacy contract: nothing is written, modified or sent anywhere; values
//! are returned to the caller only and are never logged. At most
//! [`MAX_READ`] bytes are read from the head of the file, every offset is
//! bounds-checked, and malformed input becomes a structured `FsError`.
//! Cloud placeholders / offline files are never hydrated: they are reported
//! as unavailable before any byte is read.

use crate::domain::{ErrorCode, FsError, validate_path};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;

/// Hard ceiling for the head read.
pub const MAX_READ: u64 = 256 * 1024;
/// Max IFD entries visited per IFD (guards crafted counts).
const MAX_IFD_ENTRIES: usize = 512;
const MAX_VALUE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    pub make: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gps {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadata {
    pub kind: Option<MediaKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps: Option<Gps>,
    pub other: Vec<(String, String)>,
}

fn malformed(what: &str) -> FsError {
    FsError::new(ErrorCode::Io, format!("Malformed media data: {what}"))
}

fn unavailable() -> FsError {
    FsError::new(ErrorCode::Unsupported, "unavailable")
}

/// Entry point. Reads ≤256 KiB from the head and parses by magic bytes.
pub fn read_media_metadata(path: &str) -> Result<MediaMetadata, FsError> {
    validate_path(path)?;
    let meta = std::fs::metadata(path).map_err(|e| FsError::from(e).with_path(path))?;
    if !meta.is_file() {
        return Err(FsError::new(ErrorCode::Unsupported, "Not a regular file").with_path(path));
    }
    if is_offline_placeholder(&meta) {
        return Err(unavailable().with_path(path));
    }
    let mut head = Vec::new();
    File::open(path)
        .map_err(|e| FsError::from(e).with_path(path))?
        .take(MAX_READ)
        .read_to_end(&mut head)
        .map_err(|e| FsError::from(e).with_path(path))?;
    parse_media(&head, meta.len()).map_err(|e| e.with_path(path))
}

/// OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS (cloud placeholders).
pub(crate) fn offline_attrs(attrs: u32) -> bool {
    attrs & (0x1000 | 0x0004_0000 | 0x0040_0000) != 0
}

#[cfg(windows)]
pub(crate) fn is_offline_placeholder(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    offline_attrs(meta.file_attributes())
}

#[cfg(not(windows))]
pub(crate) fn is_offline_placeholder(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Parse an in-memory head buffer. `file_len` is the full file size.
pub fn parse_media(head: &[u8], file_len: u64) -> Result<MediaMetadata, FsError> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        parse_jpeg(head)
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        parse_png(head)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        parse_gif(head)
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        parse_wav(head, file_len)
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        parse_mp4(head)
    } else {
        Err(FsError::new(
            ErrorCode::Unsupported,
            "Not a supported media format",
        ))
    }
}

// ── byte helpers ─────────────────────────────────────────────────────────

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        d.get(o..o.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        d.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn be64(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        d.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}
fn le16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        d.get(o..o.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn le32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        d.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn clean_text(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let s: String = String::from_utf8_lossy(&raw[..end])
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_VALUE_CHARS)
        .collect();
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

// ── PNG / GIF ────────────────────────────────────────────────────────────

fn parse_png(d: &[u8]) -> Result<MediaMetadata, FsError> {
    // 8 signature + 4 len + "IHDR" + w + h
    if d.get(12..16) != Some(b"IHDR") {
        return Err(malformed("PNG IHDR missing"));
    }
    let w = be32(d, 16).ok_or_else(|| malformed("PNG IHDR truncated"))?;
    let h = be32(d, 20).ok_or_else(|| malformed("PNG IHDR truncated"))?;
    if w == 0 || h == 0 {
        return Err(malformed("PNG zero dimension"));
    }
    Ok(MediaMetadata {
        kind: Some(MediaKind::Image),
        width: Some(w),
        height: Some(h),
        ..Default::default()
    })
}

fn parse_gif(d: &[u8]) -> Result<MediaMetadata, FsError> {
    let w = le16(d, 6).ok_or_else(|| malformed("GIF header truncated"))?;
    let h = le16(d, 8).ok_or_else(|| malformed("GIF header truncated"))?;
    Ok(MediaMetadata {
        kind: Some(MediaKind::Image),
        width: Some(u32::from(w)),
        height: Some(u32::from(h)),
        ..Default::default()
    })
}

// ── WAV ──────────────────────────────────────────────────────────────────

fn parse_wav(d: &[u8], file_len: u64) -> Result<MediaMetadata, FsError> {
    let mut pos = 12usize;
    let mut byte_rate: Option<u32> = None;
    let mut channels = None;
    let mut sample_rate = None;
    let mut data_len: Option<u64> = None;
    while let (Some(id), Some(size)) = (d.get(pos..pos + 4), le32(d, pos + 4)) {
        let body = pos + 8;
        if id == b"fmt " {
            channels = le16(d, body + 2);
            sample_rate = le32(d, body + 4);
            byte_rate = le32(d, body + 8);
        } else if id == b"data" {
            let avail = file_len.saturating_sub(body as u64);
            data_len = Some(u64::from(size).min(avail));
            break;
        }
        let next = body
            .checked_add(size as usize)
            .and_then(|n| n.checked_add(size as usize & 1));
        match next {
            Some(n) if n > pos => pos = n,
            _ => break,
        }
    }
    let br = byte_rate
        .filter(|&b| b > 0)
        .ok_or_else(|| malformed("WAV fmt chunk"))?;
    let mut m = MediaMetadata {
        kind: Some(MediaKind::Audio),
        ..Default::default()
    };
    if let Some(n) = data_len {
        m.duration_ms = Some(n * 1000 / u64::from(br));
    }
    if let Some(c) = channels {
        m.other.push(("Channels".into(), c.to_string()));
    }
    if let Some(s) = sample_rate {
        m.other.push(("Sample rate".into(), format!("{s} Hz")));
    }
    Ok(m)
}

// ── MP4 / MOV ────────────────────────────────────────────────────────────

/// Iterate boxes in `d[start..end]`; returns (type, body_start, body_end).
fn boxes(d: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let end = end.min(d.len());
    let mut pos = start;
    while pos + 8 <= end && out.len() < 64 {
        let Some(size32) = be32(d, pos) else { break };
        let Some(ty) = d.get(pos + 4..pos + 8) else {
            break;
        };
        let ty: [u8; 4] = [ty[0], ty[1], ty[2], ty[3]];
        let (hdr, size) = match size32 {
            0 => (8usize, (end - pos) as u64),
            1 => match be64(d, pos + 8) {
                Some(s) => (16usize, s),
                None => break,
            },
            n => (8usize, u64::from(n)),
        };
        if size < hdr as u64 {
            break;
        }
        let body = pos + hdr;
        let box_end = usize::try_from(size)
            .ok()
            .and_then(|s| pos.checked_add(s))
            .map_or(end, |e| e.min(end));
        out.push((ty, body, box_end));
        if box_end <= pos {
            break;
        }
        pos = box_end;
    }
    out
}

fn parse_mp4(d: &[u8]) -> Result<MediaMetadata, FsError> {
    let mut m = MediaMetadata {
        kind: Some(MediaKind::Video),
        ..Default::default()
    };
    for (ty, bs, be) in boxes(d, 0, d.len()) {
        if &ty != b"moov" {
            continue;
        }
        for (cty, cbs, _) in boxes(d, bs, be) {
            if &cty == b"mvhd" {
                let ver = *d.get(cbs).ok_or_else(|| malformed("mvhd truncated"))?;
                let (ts, dur) = if ver == 1 {
                    (be32(d, cbs + 20), be64(d, cbs + 24))
                } else {
                    (be32(d, cbs + 12), be32(d, cbs + 16).map(u64::from))
                };
                let (ts, dur) = ts.zip(dur).ok_or_else(|| malformed("mvhd truncated"))?;
                if ts > 0 {
                    m.duration_ms = Some(dur.saturating_mul(1000) / u64::from(ts));
                }
            }
        }
    }
    Ok(m)
}

// ── JPEG + EXIF ──────────────────────────────────────────────────────────

fn parse_jpeg(d: &[u8]) -> Result<MediaMetadata, FsError> {
    let mut m = MediaMetadata {
        kind: Some(MediaKind::Image),
        ..Default::default()
    };
    let mut pos = 2usize;
    loop {
        if d.get(pos) != Some(&0xFF) {
            break;
        }
        while d.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let Some(&marker) = d.get(pos) else { break };
        pos += 1;
        if marker == 0xD9 || marker == 0xDA {
            break; // EOI / start of scan: no more headers
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue; // standalone markers
        }
        let Some(len) = be16(d, pos).map(usize::from) else {
            break; // head ended inside a segment header: keep what we have
        };
        if len < 2 {
            return Err(malformed("JPEG segment length"));
        }
        let body_start = pos + 2;
        let body_end = (pos + len).min(d.len());
        let body = d.get(body_start..body_end).unwrap_or(&[]);
        match marker {
            0xE1 if body.starts_with(b"Exif\0\0") => {
                // Truncated / bad EXIF must not hide the rest of the file.
                let _ = parse_tiff(&body[6..], &mut m);
            }
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                if let (Some(h), Some(w)) = (be16(body, 1), be16(body, 3)) {
                    m.width = Some(u32::from(w));
                    m.height = Some(u32::from(h));
                }
            }
            _ => {}
        }
        pos += len;
    }
    Ok(m)
}

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, o: usize) -> Option<u16> {
        if self.le {
            le16(self.d, o)
        } else {
            be16(self.d, o)
        }
    }
    fn u32(&self, o: usize) -> Option<u32> {
        if self.le {
            le32(self.d, o)
        } else {
            be32(self.d, o)
        }
    }
    /// Entries of the IFD at `off`: (tag, type, count, value bytes).
    fn ifd(&self, off: usize) -> Vec<(u16, u16, u32, &[u8])> {
        let mut out = Vec::new();
        let Some(n) = self.u16(off).map(usize::from) else {
            return out;
        };
        for i in 0..n.min(MAX_IFD_ENTRIES) {
            let e = off + 2 + i * 12;
            let (Some(tag), Some(ty), Some(count)) =
                (self.u16(e), self.u16(e + 2), self.u32(e + 4))
            else {
                break;
            };
            let unit = match ty {
                1 | 2 | 6 | 7 => 1usize,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                _ => continue,
            };
            let Some(size) = (count as usize).checked_mul(unit) else {
                continue;
            };
            let start = if size <= 4 {
                e + 8
            } else {
                match self.u32(e + 8) {
                    Some(o) => o as usize,
                    None => continue,
                }
            };
            let Some(bytes) = start
                .checked_add(size)
                .and_then(|end| self.d.get(start..end))
            else {
                continue;
            };
            out.push((tag, ty, count, bytes));
        }
        out
    }
    fn short(&self, b: &[u8]) -> Option<u16> {
        let a: [u8; 2] = b.get(0..2)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    }
    fn long(&self, b: &[u8]) -> Option<u32> {
        let a: [u8; 4] = b.get(0..4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
    fn rational(&self, b: &[u8], idx: usize) -> Option<f64> {
        let o = idx * 8;
        let n = self.long(b.get(o..o + 4)?)?;
        let d = self.long(b.get(o + 4..o + 8)?)?;
        if d == 0 {
            None
        } else {
            Some(f64::from(n) / f64::from(d))
        }
    }
}

fn parse_tiff(d: &[u8], m: &mut MediaMetadata) -> Result<(), FsError> {
    let le = match d.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(malformed("EXIF byte order")),
    };
    let t = Tiff { d, le };
    if t.u16(2) != Some(42) {
        return Err(malformed("EXIF TIFF magic"));
    }
    let ifd0 = t.u32(4).ok_or_else(|| malformed("EXIF truncated"))? as usize;
    let mut make = None;
    let mut model = None;
    let mut exif_ptr = None;
    let mut gps_ptr = None;
    for (tag, ty, _c, v) in t.ifd(ifd0) {
        match tag {
            0x010F => make = clean_text(v),
            0x0110 => model = clean_text(v),
            0x0112 => m.orientation = t.short(v).filter(|o| (1..=8).contains(o)),
            0x0131 => push_text(m, "Software", v),
            0x0132 => {
                if m.taken_at.is_none() {
                    m.taken_at = exif_date(v);
                }
            }
            0x8769 if ty == 4 => exif_ptr = t.long(v).map(|o| o as usize),
            0x8825 if ty == 4 => gps_ptr = t.long(v).map(|o| o as usize),
            _ => {}
        }
    }
    if let Some(p) = exif_ptr {
        for (tag, ty, _c, v) in t.ifd(p) {
            match tag {
                0x9003 => {
                    if let Some(s) = exif_date(v) {
                        m.taken_at = Some(s);
                    }
                }
                0xA002 | 0xA003 => {
                    let val = if ty == 3 {
                        t.short(v).map(u32::from)
                    } else {
                        t.long(v)
                    };
                    // The JPEG frame header wins; EXIF only fills gaps.
                    if let Some(val) = val {
                        if tag == 0xA002 && m.width.is_none() {
                            m.width = Some(val);
                        } else if tag == 0xA003 && m.height.is_none() {
                            m.height = Some(val);
                        }
                    }
                }
                0x829A => {
                    if let (Some(n), Some(dn)) = (t.long(v), v.get(4..).and_then(|x| t.long(x)))
                        && n > 0
                        && dn > 0
                    {
                        let s = if dn > n {
                            format!("1/{} s", (f64::from(dn) / f64::from(n)).round() as u64)
                        } else {
                            format!("{} s", f64::from(n) / f64::from(dn))
                        };
                        m.other.push(("Exposure".into(), s));
                    }
                }
                0x829D => {
                    if let Some(f) = t.rational(v, 0) {
                        m.other.push(("Aperture".into(), format!("f/{f:.1}")));
                    }
                }
                0x8827 => {
                    if let Some(i) = t.short(v) {
                        m.other.push(("ISO".into(), i.to_string()));
                    }
                }
                0x920A => {
                    if let Some(f) = t.rational(v, 0) {
                        m.other.push(("Focal length".into(), format!("{f:.0} mm")));
                    }
                }
                0xA434 => push_text(m, "Lens", v),
                _ => {}
            }
        }
    }
    if let Some(p) = gps_ptr {
        m.gps = parse_gps(&t, p);
    }
    if make.is_some() || model.is_some() {
        m.camera = Some(Camera { make, model });
    }
    Ok(())
}

fn push_text(m: &mut MediaMetadata, label: &str, v: &[u8]) {
    if let Some(s) = clean_text(v) {
        m.other.push((label.into(), s));
    }
}

/// "YYYY:MM:DD HH:MM:SS" -> "YYYY-MM-DD HH:MM:SS"; anything else is dropped.
fn exif_date(v: &[u8]) -> Option<String> {
    let s = clean_text(v)?;
    let b = s.as_bytes();
    if b.len() != 19 {
        return None;
    }
    let ok = b.iter().enumerate().all(|(i, &c)| match i {
        4 | 7 | 13 | 16 => c == b':',
        10 => c == b' ',
        _ => c.is_ascii_digit(),
    });
    if !ok || &s[0..4] == "0000" {
        return None;
    }
    Some(format!(
        "{}-{}-{}{}",
        &s[0..4],
        &s[5..7],
        &s[8..10],
        &s[10..]
    ))
}

fn parse_gps(t: &Tiff<'_>, off: usize) -> Option<Gps> {
    let mut lat_ref = None;
    let mut lon_ref = None;
    let mut lat = None;
    let mut lon = None;
    for (tag, ty, count, v) in t.ifd(off) {
        match tag {
            1 => lat_ref = v.first().copied(),
            3 => lon_ref = v.first().copied(),
            2 | 4 if ty == 5 && count >= 3 => {
                let dms = t.rational(v, 0)? + t.rational(v, 1)? / 60.0 + t.rational(v, 2)? / 3600.0;
                if tag == 2 {
                    lat = Some(dms);
                } else {
                    lon = Some(dms);
                }
            }
            _ => {}
        }
    }
    let mut lat: f64 = lat?;
    let mut lon: f64 = lon?;
    if lat_ref == Some(b'S') {
        lat = -lat;
    }
    if lon_ref == Some(b'W') {
        lon = -lon;
    }
    if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 {
        return None;
    }
    Some(Gps { lat, lon })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    type Entry = (u16, u16, u32, Vec<u8>);

    /// Write an IFD at `at`; values > 4 bytes go after the entry table.
    fn put_ifd(buf: &mut Vec<u8>, at: usize, entries: &[Entry]) {
        let table = 2 + entries.len() * 12 + 4;
        let mut extra_at = at + table;
        let mut tab = Vec::new();
        tab.extend((entries.len() as u16).to_le_bytes());
        let mut extras: Vec<u8> = Vec::new();
        for (tag, ty, count, data) in entries {
            tab.extend(tag.to_le_bytes());
            tab.extend(ty.to_le_bytes());
            tab.extend(count.to_le_bytes());
            if data.len() <= 4 {
                let mut v = data.clone();
                v.resize(4, 0);
                tab.extend(v);
            } else {
                tab.extend((extra_at as u32).to_le_bytes());
                extras.extend(data);
                extra_at += data.len();
            }
        }
        tab.extend(0u32.to_le_bytes());
        if buf.len() < at + tab.len() + extras.len() {
            buf.resize(at + tab.len() + extras.len(), 0);
        }
        buf[at..at + tab.len()].copy_from_slice(&tab);
        buf[at + tab.len()..at + tab.len() + extras.len()].copy_from_slice(&extras);
    }

    fn ascii(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }
    fn rat(pairs: &[(u32, u32)]) -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
            .collect()
    }

    fn tiff_full() -> Vec<u8> {
        let mut t = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        put_ifd(
            &mut t,
            8,
            &[
                (0x010F, 2, 6, ascii("Canon")),
                (0x0110, 2, 7, ascii("EOS-X1")),
                (0x0112, 3, 1, 6u16.to_le_bytes().to_vec()),
                (0x8769, 4, 1, 0x200u32.to_le_bytes().to_vec()),
                (0x8825, 4, 1, 0x400u32.to_le_bytes().to_vec()),
            ],
        );
        put_ifd(
            &mut t,
            0x200,
            &[
                (0x9003, 2, 20, ascii("2024:05:17 13:45:09")),
                (0x829D, 5, 1, rat(&[(28, 10)])),
                (0x8827, 3, 1, 400u16.to_le_bytes().to_vec()),
                (0xA002, 4, 1, 4000u32.to_le_bytes().to_vec()),
                (0xA003, 4, 1, 3000u32.to_le_bytes().to_vec()),
            ],
        );
        put_ifd(
            &mut t,
            0x400,
            &[
                (1, 2, 2, b"S\0".to_vec()),
                (2, 5, 3, rat(&[(6, 1), (30, 1), (0, 1)])),
                (3, 2, 2, b"E\0".to_vec()),
                (4, 5, 3, rat(&[(106, 1), (45, 1), (0, 1)])),
            ],
        );
        t
    }

    fn jpeg_with(tiff: Option<&[u8]>) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        if let Some(t) = tiff {
            let mut body = b"Exif\0\0".to_vec();
            body.extend(t);
            j.extend([0xFF, 0xE1]);
            j.extend(((body.len() + 2) as u16).to_be_bytes());
            j.extend(body);
        }
        // SOF0: len 11, precision 8, h=600, w=800, 1 component
        j.extend([0xFF, 0xC0, 0, 11, 8]);
        j.extend(600u16.to_be_bytes());
        j.extend(800u16.to_be_bytes());
        j.extend([1, 1, 0x11, 0]);
        j.extend([0xFF, 0xDA, 0, 2]);
        j.extend([0xFF, 0xD9]);
        j
    }

    #[test]
    fn jpeg_with_exif_extracts_fields() {
        let m = parse_media(&jpeg_with(Some(&tiff_full())), 1000).unwrap();
        assert_eq!(m.kind, Some(MediaKind::Image));
        assert_eq!((m.width, m.height), (Some(800), Some(600)));
        let c = m.camera.unwrap();
        assert_eq!(c.make.as_deref(), Some("Canon"));
        assert_eq!(c.model.as_deref(), Some("EOS-X1"));
        assert_eq!(m.orientation, Some(6));
        assert_eq!(m.taken_at.as_deref(), Some("2024-05-17 13:45:09"));
        let g = m.gps.unwrap();
        assert!((g.lat + 6.5).abs() < 1e-9);
        assert!((g.lon - 106.75).abs() < 1e-9);
        assert!(m.other.contains(&("ISO".into(), "400".into())));
        assert!(m.other.contains(&("Aperture".into(), "f/2.8".into())));
    }

    #[test]
    fn jpeg_without_exif_has_dimensions_only() {
        let m = parse_media(&jpeg_with(None), 100).unwrap();
        assert_eq!((m.width, m.height), (Some(800), Some(600)));
        assert!(m.camera.is_none() && m.gps.is_none() && m.taken_at.is_none());
        assert!(m.other.is_empty());
    }

    #[test]
    fn truncated_exif_never_panics_at_any_cut() {
        let full = jpeg_with(Some(&tiff_full()));
        for cut in 0..full.len() {
            let _ = parse_media(&full[..cut], cut as u64);
        }
    }

    #[test]
    fn malformed_exif_is_ignored() {
        // garbage byte order inside a valid APP1
        let m = parse_media(&jpeg_with(Some(b"ZZ\x2a\0\x08\0\0\0")), 100).unwrap();
        assert!(m.camera.is_none());
        // IFD offsets far outside the buffer, absurd count
        let mut t = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        put_ifd(
            &mut t,
            8,
            &[
                (0x010F, 2, 0x7FFF_FFFF, vec![0xFF, 0xFF, 0xFF, 0x7F]),
                (0x8769, 4, 1, 0xFFFF_FF00u32.to_le_bytes().to_vec()),
                (0x8825, 4, 1, 0x0FFF_FFFFu32.to_le_bytes().to_vec()),
            ],
        );
        let m = parse_media(&jpeg_with(Some(&t)), 100).unwrap();
        assert!(m.camera.is_none() && m.gps.is_none());
        // segment length lies about its size
        let bad = [0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, b'E'];
        let _ = parse_media(&bad, 7);
        // zero-length segment is a structured error
        let zero = [0xFF, 0xD8, 0xFF, 0xE1, 0, 0];
        assert_eq!(parse_media(&zero, 6).unwrap_err().code, ErrorCode::Io);
    }

    #[test]
    fn gps_out_of_range_dropped() {
        let mut t = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        put_ifd(
            &mut t,
            8,
            &[(0x8825, 4, 1, 0x200u32.to_le_bytes().to_vec())],
        );
        put_ifd(
            &mut t,
            0x200,
            &[
                (1, 2, 2, b"N\0".to_vec()),
                (2, 5, 3, rat(&[(200, 1), (0, 1), (0, 1)])),
                (3, 2, 2, b"E\0".to_vec()),
                (4, 5, 3, rat(&[(10, 1), (0, 1), (0, 1)])),
            ],
        );
        let m = parse_media(&jpeg_with(Some(&t)), 100).unwrap();
        assert!(m.gps.is_none());
    }

    #[test]
    fn png_ihdr_dimensions() {
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        p.extend(13u32.to_be_bytes());
        p.extend(b"IHDR");
        p.extend(640u32.to_be_bytes());
        p.extend(480u32.to_be_bytes());
        p.extend([8, 6, 0, 0, 0]);
        let m = parse_media(&p, 100).unwrap();
        assert_eq!((m.width, m.height), (Some(640), Some(480)));
        assert!(parse_media(&p[..20], 20).is_err());
    }

    #[test]
    fn wav_duration() {
        let mut w = b"RIFF".to_vec();
        w.extend(0u32.to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(44100u32.to_le_bytes());
        w.extend(176400u32.to_le_bytes());
        w.extend([4, 0, 16, 0]);
        w.extend(b"data");
        w.extend(352800u32.to_le_bytes());
        w.extend(vec![0u8; 100]);
        let m = parse_media(&w, 352_800 + 44).unwrap();
        assert_eq!(m.kind, Some(MediaKind::Audio));
        assert_eq!(m.duration_ms, Some(2000));
    }

    #[test]
    fn mp4_duration_from_mvhd() {
        let mut mvhd = vec![0u8; 4];
        mvhd.extend(0u32.to_be_bytes());
        mvhd.extend(0u32.to_be_bytes());
        mvhd.extend(1000u32.to_be_bytes());
        mvhd.extend(5500u32.to_be_bytes());
        let mut moov = ((mvhd.len() + 8) as u32).to_be_bytes().to_vec();
        moov.extend(b"mvhd");
        moov.extend(&mvhd);
        let mut f = 16u32.to_be_bytes().to_vec();
        f.extend(b"ftypisom");
        f.extend([0; 4]);
        f.extend(((moov.len() + 8) as u32).to_be_bytes());
        f.extend(b"moov");
        f.extend(&moov);
        let m = parse_media(&f, 1000).unwrap();
        assert_eq!(m.kind, Some(MediaKind::Video));
        assert_eq!(m.duration_ms, Some(5500));
        for cut in 0..f.len() {
            let _ = parse_media(&f[..cut], cut as u64);
        }
    }

    #[test]
    fn non_media_and_missing_path() {
        let e = parse_media(b"hello world, plain text", 23).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        let e = read_media_metadata("Z:/definitely/not/here.jpg").unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(
            read_media_metadata("  ").unwrap_err().code,
            ErrorCode::InvalidPath
        );
    }

    #[test]
    fn reads_from_disk_and_bounds_head() {
        let dir = std::env::temp_dir().join(format!("ferrum_mm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.jpg");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&jpeg_with(Some(&tiff_full()))).unwrap();
        f.write_all(&vec![0u8; 400 * 1024]).unwrap();
        drop(f);
        let m = read_media_metadata(p.to_str().unwrap()).unwrap();
        assert_eq!(m.camera.unwrap().make.as_deref(), Some("Canon"));
        let txt = dir.join("a.txt");
        std::fs::write(&txt, "not media").unwrap();
        assert_eq!(
            read_media_metadata(txt.to_str().unwrap()).unwrap_err().code,
            ErrorCode::Unsupported
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
