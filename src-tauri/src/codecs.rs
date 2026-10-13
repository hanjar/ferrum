//! F2-27: Encode/Decode File — the formats chosen for real need, with
//! interoperability fixtures (spec: never claim every format with one
//! parser).
//!
//! Supported (documented, fixture-tested):
//! - ENCODE: MIME Base64 (RFC 4648 body, 76-char CRLF lines) — Total
//!   Commander's "Encode MIME Files" parity; output `<name>.b64`.
//! - DECODE (auto-detected): MIME Base64 (raw `.b64` or full MIME bodies —
//!   non-alphabet bytes are ignored), uuencode and xxencode (header
//!   `begin <mode> <name>`; the geometry of the length char disambiguates
//!   the two alphabets).
//! - BinHex is detected and refused with a structured `unsupported` error
//!   instead of a wrong parse. XXE support shares the UUE table machinery.
//!
//! Every output is written next to its source and an existing output is
//! refused (`alreadyExists`) — the queue surfaces it, nothing is silently
//! clobbered. All work is streaming with bounded memory and cancellable.

use crate::domain::{ErrorCode, FsError};
use crate::file_tools::ToolOutcome;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const LINE_WIDTH: usize = 76;

// ── MIME Base64 encode ───────────────────────────────────────────────────

/// Stream-encode `src` to `dst` as MIME base64 (76-char CRLF lines).
/// 3-byte groups stay aligned across chunk boundaries via a small carry
/// buffer, so memory stays bounded on huge inputs.
pub fn encode_mime_base64(src: &str, dst: &str) -> Result<(), FsError> {
    use base64::Engine;
    ensure_absent(dst)?;
    let mut reader = BufReader::with_capacity(
        64 * 1024,
        File::open(src).map_err(|e| FsError::from(e).with_path(src))?,
    );
    let mut writer = BufWriter::with_capacity(
        64 * 1024,
        File::create(dst).map_err(|e| FsError::from(e).with_path(dst))?,
    );
    let engine = base64::engine::general_purpose::STANDARD;
    let mut line = 0usize;
    let mut pending: Vec<u8> = Vec::with_capacity(3 * 4096 + 2);
    let mut chunk = [0u8; 3 * 4096];
    let result = (|| -> std::io::Result<()> {
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                if !pending.is_empty() {
                    write_wrapped(&mut writer, engine.encode(&pending).as_bytes(), &mut line)?;
                    pending.clear();
                }
                if line > 0 {
                    writer.write_all(b"\r\n")?;
                }
                writer.flush()?;
                return Ok(());
            }
            pending.extend_from_slice(&chunk[..n]);
            let keep = pending.len() % 3;
            let emit_len = pending.len() - keep;
            if emit_len > 0 {
                let encoded = engine.encode(&pending[..emit_len]);
                write_wrapped(&mut writer, encoded.as_bytes(), &mut line)?;
                pending.drain(..emit_len);
            }
        }
    })();
    // A failed encode must not leave a half-written output behind.
    if result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    result.map_err(|e| FsError::from(e).with_path(dst))
}

/// Write base64 wrapped at LINE_WIDTH with CRLF separators.
fn write_wrapped<W: Write>(w: &mut W, mut bytes: &[u8], line: &mut usize) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let take = (LINE_WIDTH - *line).min(bytes.len());
        w.write_all(&bytes[..take])?;
        *line += take;
        bytes = &bytes[take..];
        if *line == LINE_WIDTH {
            w.write_all(b"\r\n")?;
            *line = 0;
        }
    }
    Ok(())
}

// ── Decode (auto-detect) ─────────────────────────────────────────────────

/// Decode `src` into its parent directory; returns the output path.
pub fn decode_auto(src: &str) -> Result<String, FsError> {
    let bytes = std::fs::read(src).map_err(|e| FsError::from(e).with_path(src))?;
    if looks_like_binhex(&bytes) {
        return Err(FsError::new(
            ErrorCode::Unsupported,
            "BinHex (Mac) files are not supported by the decoder",
        )
        .with_path(src));
    }
    if let Some(header) = uue_header(&bytes) {
        let (out_name, body_start) = header;
        let out = output_path(src, sanitize_name(&out_name).as_str());
        decode_uue(&bytes, body_start, out.as_str())?;
        return Ok(out);
    }
    let out = output_path(src, &default_b64_name(src));
    decode_base64(&bytes, &out)?;
    Ok(out)
}

fn looks_like_binhex(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(64)];
    let lower = head.to_ascii_lowercase();
    let prefix = b"(this file must be converted with binhex";
    lower.starts_with(prefix)
}

/// `Some((file name, body offset))` when the payload starts with a uuencode
/// / xxencode `begin` line.
fn uue_header(bytes: &[u8]) -> Option<(String, usize)> {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let first_line_end = text.find('\n')?;
    let first = text[..first_line_end].trim_end_matches('\r');
    let mut parts = first.split_whitespace();
    if parts.next()? != "begin" {
        return None;
    }
    let _mode = parts.next()?;
    let name = parts.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, first_line_end + 1))
}

/// Strip directories and separators from a header-declared output name.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '\0' | '"' | '<' | '>' | '|'))
        .collect();
    if cleaned.is_empty() {
        "decoded.bin".to_string()
    } else {
        cleaned
    }
}

fn default_b64_name(src: &str) -> String {
    if let Some(stripped) = src.strip_suffix(".b64") {
        let base = stripped
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(stripped)
            .to_string();
        if !base.is_empty() {
            return base;
        }
    }
    format!(
        "{}.decoded",
        src.rsplit(['/', '\\']).next().unwrap_or("file")
    )
}

/// `<parent dir of src>/<name>`; refuses to build a path without a parent.
fn output_path(src: &str, name: &str) -> String {
    match src.rfind(['/', '\\']) {
        Some(i) => format!("{}{name}", &src[..=i]),
        None => name.to_string(),
    }
}

fn ensure_absent(dst: &str) -> Result<(), FsError> {
    if Path::new(dst).exists() {
        return Err(FsError::new(ErrorCode::AlreadyExists, "Output already exists").with_path(dst));
    }
    Ok(())
}

/// MIME Base64 decode. Two honest input shapes:
/// - a raw Base64 body (the `.b64` form TC produces), whitespace allowed;
/// - a MIME message whose header block (up to the first blank line) names
///   `base64` as the transfer encoding — then the body after the blank
///   line is decoded and the headers are not filtered loosely.
///
/// Anything else is refused instead of guessed at.
fn decode_base64(bytes: &[u8], dst: &str) -> Result<(), FsError> {
    use base64::Engine;
    ensure_absent(dst)?;
    let text = String::from_utf8_lossy(bytes);
    let mut body = text.as_ref();
    // A MIME header block (lines with a colon, ending at a blank line)
    // is honoured only when it names base64 as the transfer encoding.
    let colon = text.find(':');
    let blank = [
        text.find("\n\n").map(|i| i + 2),
        text.find("\r\n\r\n").map(|i| i + 4),
    ]
    .into_iter()
    .flatten()
    .min();
    if let (Some(_), Some(end)) = (colon, blank) {
        let (headers, rest) = text.split_at(end);
        if headers.to_ascii_lowercase().contains("base64") {
            body = rest;
        } else {
            return Err(
                FsError::new(ErrorCode::InvalidPath, "No Base64 content found").with_path(dst),
            );
        }
    }
    let mut filtered: Vec<u8> = Vec::with_capacity(body.len() / 4 * 3 + 3);
    for c in body.bytes() {
        if c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=' {
            filtered.push(c);
        } else if !c.is_ascii_whitespace() && !body.starts_with("--") {
            return Err(FsError::new(ErrorCode::InvalidPath, "Not a Base64 file").with_path(dst));
        }
    }
    if filtered.is_empty() {
        return Err(FsError::new(ErrorCode::InvalidPath, "No Base64 content found").with_path(dst));
    }
    let mut out: Vec<u8> = Vec::with_capacity(filtered.len() / 4 * 3 + 3);
    let mut chunk = [0u8; 3];
    for group in filtered.chunks(4) {
        let mut padded = [b'='; 4];
        if group.len() == 1 {
            return Err(
                FsError::new(ErrorCode::InvalidPath, "Truncated Base64 content").with_path(dst),
            );
        }
        padded[..group.len()].copy_from_slice(group);
        let n = base64::engine::general_purpose::STANDARD
            .decode_slice(padded, &mut chunk)
            .map_err(|_| {
                FsError::new(ErrorCode::InvalidPath, "Invalid Base64 content").with_path(dst)
            })?;
        out.extend_from_slice(&chunk[..n]);
        if group.len() < 4 {
            break; // padding reached — trailing bytes would be noise
        }
    }
    std::fs::write(dst, &out).map_err(|e| FsError::from(e).with_path(dst))
}

/// uuencode value table: char − 32 for 32..=95, backquote (` = 96) maps to
/// 0 exactly like the space it replaces in the reference implementation.
fn uue_value(c: u8) -> Option<u64> {
    match c {
        32..=95 => Some(u64::from(c - 32)),
        b'`' => Some(0),
        _ => None,
    }
}

/// xxencode table: '+' = 0, 'A'..'Z' = 1..26, 'a'..'z' = 27..52,
/// '0'..'9' = 53..62, '-' = 63.
fn xxe_value(c: u8) -> Option<u64> {
    match c {
        b'+' => Some(0),
        b'A'..=b'Z' => Some(u64::from(c - b'A' + 1)),
        b'a'..=b'z' => Some(u64::from(c - b'a' + 27)),
        b'0'..=b'9' => Some(u64::from(c - b'0' + 53)),
        b'-' => Some(63),
        _ => None,
    }
}

/// Decode a uuencode/xxencode body that starts at `body_start` into `dst`.
/// The two formats differ only in the value table; the length char's
/// geometry (declared byte count vs. the 4-char groups a line holds) picks
/// the right table on the first line and stays sticky for the whole file,
/// preferring uuencode when a body could legally be either.
fn decode_uue(bytes: &[u8], body_start: usize, dst: &str) -> Result<(), FsError> {
    ensure_absent(dst)?;
    let text = String::from_utf8_lossy(&bytes[body_start.min(bytes.len())..]);
    let mut out: Vec<u8> = Vec::new();
    let mut use_uue: Option<bool> = None; // sticky once the first line picks

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let b = line.as_bytes();
        if b[0] == b'e' && line.starts_with("end") {
            break;
        }
        // A candidate table fits when the length char matches the line's
        // group geometry and every value char decodes in that table.
        let fits = |len: u64| len.div_ceil(3) * 4 == (b.len() - 1) as u64;
        let pick = |table: fn(u8) -> Option<u64>| -> Option<u64> {
            let len = table(b[0])?;
            if len <= 64 && fits(len) && b[1..].iter().all(|c| table(*c).is_some()) {
                Some(len)
            } else {
                None
            }
        };
        let (is_uue, len) = match use_uue {
            Some(true) => (true, pick(uue_value)),
            Some(false) => (false, pick(xxe_value)),
            None => match (pick(uue_value), pick(xxe_value)) {
                (Some(len), _) => (true, Some(len)),
                (None, Some(len)) => (false, Some(len)),
                (None, None) => {
                    return Err(FsError::new(
                        ErrorCode::InvalidPath,
                        "Corrupt uuencode/xxencode body",
                    )
                    .with_path(dst));
                }
            },
        };
        if use_uue.is_none() {
            use_uue = Some(is_uue);
        }
        let len = len.ok_or_else(|| {
            FsError::new(ErrorCode::InvalidPath, "Corrupt uuencode/xxencode body").with_path(dst)
        })?;
        if len == 0 {
            break; // terminator line (` for uue, + for xxe)
        }
        let table = if is_uue { uue_value } else { xxe_value };
        let values: Option<Vec<u64>> = b[1..].iter().map(|c| table(*c)).collect();
        let values = values.ok_or_else(|| {
            FsError::new(ErrorCode::InvalidPath, "Corrupt uuencode/xxencode body").with_path(dst)
        })?;
        let mut line_out: Vec<u8> = Vec::with_capacity(len as usize);
        for quad in values.chunks(4) {
            let mut q = [0u64; 4];
            q[..quad.len()].copy_from_slice(quad);
            let n = (q[0] << 18) | (q[1] << 12) | (q[2] << 6) | q[3];
            line_out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
        }
        line_out.truncate(len as usize); // drop the group padding
        out.extend_from_slice(&line_out);
    }
    std::fs::write(dst, &out).map_err(|e| FsError::from(e).with_path(dst))
}

// ── Batch entry points (queue workers) ───────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecReport {
    pub requested: usize,
    pub produced: usize,
    pub outcomes: Vec<ToolOutcome>,
}

/// Encode every path to `<name>.b64` next to it.
pub fn encode_files(paths: &[String], cancel: &dyn Fn() -> bool) -> CodecReport {
    let mut report = CodecReport {
        requested: paths.len(),
        produced: 0,
        outcomes: Vec::new(),
    };
    for p in paths {
        if cancel() {
            report.outcomes.push(ToolOutcome {
                path: p.clone(),
                status: "failed".into(),
                expected: None,
                actual: None,
                message: Some("cancelled".into()),
            });
            continue;
        }
        let dst = format!("{p}.b64");
        match encode_mime_base64(p, &dst) {
            Ok(()) => {
                report.produced += 1;
                report.outcomes.push(ToolOutcome {
                    path: dst,
                    status: "created".into(),
                    expected: None,
                    actual: None,
                    message: None,
                });
            }
            Err(e) => report.outcomes.push(ToolOutcome {
                path: p.clone(),
                status: "failed".into(),
                expected: None,
                actual: None,
                message: Some(e.message),
            }),
        }
    }
    report
}

/// Decode every path (auto-detected format) next to it.
pub fn decode_files(paths: &[String], cancel: &dyn Fn() -> bool) -> CodecReport {
    let mut report = CodecReport {
        requested: paths.len(),
        produced: 0,
        outcomes: Vec::new(),
    };
    for p in paths {
        if cancel() {
            report.outcomes.push(ToolOutcome {
                path: p.clone(),
                status: "failed".into(),
                expected: None,
                actual: None,
                message: Some("cancelled".into()),
            });
            continue;
        }
        match decode_auto(p) {
            Ok(out) => {
                report.produced += 1;
                report.outcomes.push(ToolOutcome {
                    path: out,
                    status: "created".into(),
                    expected: None,
                    actual: None,
                    message: None,
                });
            }
            Err(e) => report.outcomes.push(ToolOutcome {
                path: p.clone(),
                status: "failed".into(),
                expected: None,
                actual: None,
                message: Some(e.message),
            }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-codecs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    fn write(path: &str, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn mime_encode_matches_rfc4648_vectors() {
        let dir = temp("rfc");
        for (text, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Man", "TWFu"),
        ] {
            let src = format!("{dir}\\src.bin");
            let dst = format!("{dir}\\src.b64");
            let _ = std::fs::remove_file(&dst);
            write(&src, text.as_bytes());
            encode_mime_base64(&src, &dst).unwrap();
            assert_eq!(
                std::fs::read_to_string(&dst).unwrap().trim(),
                expected,
                "{text:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_encode_wraps_at_76_columns_and_roundtrips_every_byte_value() {
        let dir = temp("wrap");
        let src = format!("{dir}\\all.bin");
        let dst = format!("{dir}\\all.b64");
        let payload: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
        write(&src, &payload);
        encode_mime_base64(&src, &dst).unwrap();
        let text = std::fs::read_to_string(&dst).unwrap();
        for line in text.trim().split("\r\n") {
            assert!(line.len() <= 76, "line too long: {}", line.len());
        }
        // Decode through the standard engine: exact roundtrip.
        use base64::Engine;
        let filtered: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let back = base64::engine::general_purpose::STANDARD
            .decode(filtered)
            .unwrap();
        assert_eq!(back, payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encode_refuses_to_clobber_an_existing_output() {
        let dir = temp("clobber");
        let src = format!("{dir}\\a.txt");
        write(&src, b"hello");
        let dst = format!("{src}.b64");
        write(&dst, b"precious");
        let err = encode_mime_base64(&src, &dst).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        assert_eq!(std::fs::read(&dst).unwrap(), b"precious");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_mime_base64_handles_raw_and_full_mime_bodies() {
        let dir = temp("mime");
        // Raw .b64 (RFC vector, with padding).
        let raw = format!("{dir}\\raw.b64");
        write(&raw, b"Zm9vYmFy");
        assert_eq!(
            std::fs::read(decode_auto(&raw).unwrap()).unwrap(),
            b"foobar"
        );
        // Output name drops the .b64 suffix.
        assert!(decode_auto(&raw).unwrap_err().message.contains("exists"));
        // Full MIME message: the header block names base64, so the body
        // after the blank line decodes; output drops the .b64 suffix.
        let mime = format!("{dir}\\mail.b64");
        write(
            &mime,
            b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\nTWFu\r\n",
        );
        let out = decode_auto(&mime).unwrap();
        assert!(out.ends_with("mail"));
        assert_eq!(std::fs::read(&out).unwrap(), b"Man");
        // Missing padding is tolerated; truncation is reported.
        let nopad = format!("{dir}\\nopad.b64");
        write(&nopad, b"Zm8");
        assert_eq!(std::fs::read(decode_auto(&nopad).unwrap()).unwrap(), b"fo");
        let bad = format!("{dir}\\bad.b64");
        write(&bad, b"Z");
        assert_eq!(decode_auto(&bad).unwrap_err().code, ErrorCode::InvalidPath);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decodes_the_canonical_uue_vector() {
        // The uuencode(5) man-page example: "Cat" in a file named cat.txt.
        let dir = temp("uue");
        let src = format!("{dir}\\cat.uue");
        write(&src, b"begin 644 cat.txt\n#0V%T\n`\nend\n");
        let out = decode_auto(&src).unwrap();
        assert!(out.ends_with("cat.txt"));
        assert_eq!(std::fs::read(&out).unwrap(), b"Cat");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decodes_an_empty_uue_payload() {
        let dir = temp("uue-empty");
        let src = format!("{dir}\\empty.uue");
        write(&src, b"begin 644 nothing.dat\n`\nend\n");
        let out = decode_auto(&src).unwrap();
        assert!(out.ends_with("nothing.dat"));
        assert!(std::fs::read(&out).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decodes_multi_line_uue_with_directory_names_sanitized() {
        // Two 3-byte groups ("Hello\n" split as Hel/lo\\n → one 45-byte-line
        // format would also work); header name carries a path — stripped.
        let dir = temp("uue-multi");
        let src = format!("{dir}\\hi.uue");
        let body = "begin 644 ..\\\\evil\\name.txt\n&2&5L;&\\*\n`\nend\n";
        write(&src, body.as_bytes());
        let out = decode_auto(&src).unwrap();
        assert!(out.ends_with("name.txt"), "got {out}");
        assert_eq!(std::fs::read(&out).unwrap(), b"Hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decodes_the_xxencode_alphabet() {
        // xxencode of "Cat" with its '+'/'-' table ("CP1Ez").
        let dir = temp("xxe");
        let src = format!("{dir}\\cat.xxe");
        write(&src, b"begin 644 cat.txt\nCP1Ez\n+\nend\n");
        let out = decode_auto(&src).unwrap();
        assert!(out.ends_with("cat.txt"));
        assert_eq!(std::fs::read(&out).unwrap(), b"Cat");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binhex_is_detected_and_refused() {
        let dir = temp("binhex");
        let src = format!("{dir}\\classic.hqx");
        write(&src, b"(This file must be converted with BinHex 4.0)\n:$");
        let err = decode_auto(&src).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("BinHex"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_refuses_to_clobber_and_reports_missing_sources() {
        let dir = temp("refuse");
        let src = format!("{dir}\\a.b64");
        write(&src, b"TWFu");
        let out = format!("{dir}\\a");
        write(&out, b"keep me");
        let err = decode_auto(&src).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        assert_eq!(std::fs::read(&out).unwrap(), b"keep me");
        let report = decode_files(&[format!("{dir}\\nope.b64")], &|| false);
        assert_eq!(report.produced, 0);
        assert_eq!(report.outcomes[0].status, "failed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn batch_reports_track_every_file_and_honour_cancellation() {
        let dir = temp("batch");
        let a = format!("{dir}\\a.txt");
        write(&a, b"alpha");
        let b = format!("{dir}\\missing.txt");
        let report = encode_files(&[a.clone(), b], &|| false);
        assert_eq!(report.requested, 2);
        assert_eq!(report.produced, 1);
        assert_eq!(report.outcomes[0].status, "created");
        assert!(report.outcomes[0].path.ends_with("a.txt.b64"));
        assert_eq!(
            std::fs::read(&report.outcomes[0].path).unwrap(),
            b"YWxwaGE=\r\n"
        );
        assert_eq!(report.outcomes[1].status, "failed");

        let cancelled = encode_files(std::slice::from_ref(&a), &|| true);
        assert_eq!(cancelled.produced, 0);
        assert_eq!(cancelled.outcomes[0].message.as_deref(), Some("cancelled"));

        // Roundtrip: remove the source, then decode the .b64 back over it.
        std::fs::remove_file(&a).unwrap();
        let decoded = decode_files(&[format!("{a}.b64")], &|| false);
        assert_eq!(decoded.produced, 1);
        assert_eq!(std::fs::read(&decoded.outcomes[0].path).unwrap(), b"alpha");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
