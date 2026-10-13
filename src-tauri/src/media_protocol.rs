//! `ferrum-media` custom protocol (FER-452). Replaces the Tauri asset
//! protocol (scope `**` = the whole disk) with an allowlist the backend owns.
//!
//! The webview only ever sees `ferrum-media://localhost/<token>` (Windows:
//! `http://ferrum-media.localhost/<token>`): 128 random bits, no path. A token
//! maps to a canonical file path held HERE. Only image/audio/video extensions
//! are served; HTML, SVG, JS and everything else answer 403 even when a token
//! is forced. Grants are revoked by scope (navigation / selection change),
//! capped by an LRU, and expire after an idle TTL.

use crate::domain::{ErrorCode, FsError, validate_path};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::http::{Method, Response, StatusCode, header};

/// Most grants alive at once; the least recently used one is evicted beyond it.
pub const MAX_GRANTS: usize = 4096;
/// Idle lifetime of a grant. Every served request renews it, so a long video
/// that is still being played (or sought) keeps working.
pub const GRANT_TTL: Duration = Duration::from_secs(10 * 60);
/// Paths accepted by one `media_grant` call.
pub const MAX_PATHS_PER_CALL: usize = 1024;
/// Longest scope label.
const MAX_SCOPE_LEN: usize = 64;
/// Largest body for one Range response; open or larger ranges are clamped to
/// it (Content-Range reports what was really sent).
pub const MAX_CHUNK: u64 = 8 * 1024 * 1024;
/// Largest body for a request WITHOUT Range (images). Media elements always
/// send Range; a bigger whole-file request is refused (413).
pub const MAX_FULL_BODY: u64 = 64 * 1024 * 1024;
/// Requests served at the same time. A grid can ask for dozens of large
/// images at once and each body is held in memory: the rest WAIT for a slot
/// (a thumbnail merely shows up a little later) and only give up with 503
/// after `GATE_WAIT`.
pub const MAX_CONCURRENT_SERVES: usize = 8;
pub const GATE_WAIT: Duration = Duration::from_secs(10);
/// Hard ceiling on a grant's life, however often it is used (sliding TTL).
pub const MAX_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

/// MIME for the extensions that may be served; `None` = never served.
/// Deliberately excludes svg/html/js/xml/pdf: only inert media.
pub fn served_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        _ => return None,
    })
}

struct Grant {
    path: PathBuf,
    scope: String,
    expires: Instant,
    born: Instant,
    last_used: u64,
}

#[derive(Default)]
struct Inner {
    grants: HashMap<String, Grant>,
    tick: u64,
}

/// Counting gate on concurrent serves (std only: Mutex + Condvar).
struct Gate {
    busy: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

/// Held for the whole serve; releases its slot on drop.
struct Permit<'a>(&'a Gate);

impl Gate {
    fn new(max: usize) -> Self {
        Self {
            busy: Mutex::new(0),
            freed: Condvar::new(),
            max,
        }
    }

    /// Wait up to `wait` for a free slot; `None` = still full (answer 503).
    fn acquire(&self, wait: Duration) -> Option<Permit<'_>> {
        let deadline = Instant::now() + wait;
        let mut n = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *n >= self.max {
            let left = deadline.checked_duration_since(Instant::now())?;
            let (g, res) = self
                .freed
                .wait_timeout(n, left)
                .unwrap_or_else(|e| e.into_inner());
            n = g;
            if res.timed_out() && *n >= self.max {
                return None;
            }
        }
        *n += 1;
        Some(Permit(self))
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut n = self.0.busy.lock().unwrap_or_else(|e| e.into_inner());
        *n -= 1;
        self.0.freed.notify_one();
    }
}

/// Token allowlist. One instance lives in Tauri state.
#[derive(Clone)]
pub struct MediaRegistry {
    inner: Arc<Mutex<Inner>>,
    gate: Arc<Gate>,
}

impl Default for MediaRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            gate: Arc::new(Gate::new(MAX_CONCURRENT_SERVES)),
        }
    }
}

/// Why a path is not eligible for a grant (safe fixed text; no OS detail).
fn refuse(path: &str, msg: &str, code: ErrorCode) -> FsError {
    FsError::new(code, msg).with_path(path)
}

/// Win32 device / extended namespaces (`\\.\`, `\\?\GLOBALROOT`, ...) are
/// refused up front; only the plain drive form `\\?\X:\...` (what
/// `canonicalize` itself produces) is let through. Either slash may be used.
pub(crate) fn is_device_path(path: &str) -> bool {
    let p = path.trim_start().replace('/', "\\");
    if let Some(rest) = p.strip_prefix("\\\\?\\") {
        let b = rest.as_bytes();
        let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        return !drive;
    }
    p.starts_with("\\\\.\\") || p.starts_with("\\??\\")
}

/// FILE_ATTRIBUTE_SYSTEM: operating-system files are not media. (Accepted
/// limit: a hidden+system `Folder.jpg` gets no thumbnail.)
const ATTR_SYSTEM: u32 = 0x4;

#[cfg(windows)]
fn attrs_of(meta: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
fn attrs_of(_meta: &fs::Metadata) -> u32 {
    0
}

fn is_system_file(meta: &fs::Metadata) -> bool {
    attrs_of(meta) & ATTR_SYSTEM != 0
}

/// Serve-time refusal of the granted path: it must not have become a symlink
/// or a SYSTEM file. FILE_ATTRIBUTE_REPARSE_POINT (0x400) alone is NOT a
/// reason: OneDrive/cloud sync files that are already downloaded carry it
/// (typically 0x420) and must keep working. Cloud placeholders that are not
/// local yet are refused separately through `media_meta::offline_attrs`.
/// A swap to a symlink is also caught by `canonicalize(path) == path`.
fn serve_blocked(is_symlink: bool, attrs: u32) -> bool {
    is_symlink || attrs & ATTR_SYSTEM != 0
}

/// Canonicalize and vet `path` for serving. The canonical TARGET decides the
/// extension, so a `.png` symlink to a `.html` is refused; cloud placeholders
/// (also behind a symlink) are refused without being hydrated.
fn vet(path: &str) -> Result<PathBuf, FsError> {
    validate_path(path)?;
    if is_device_path(path) {
        return Err(refuse(
            path,
            "Device paths are not served",
            ErrorCode::InvalidPath,
        ));
    }
    let lmeta = fs::symlink_metadata(path).map_err(|e| FsError::from(e).with_path(path))?;
    if crate::media_meta::is_offline_placeholder(&lmeta) {
        return Err(crate::preview::offline_error().with_path(path));
    }
    let canon = fs::canonicalize(path).map_err(|e| FsError::from(e).with_path(path))?;
    let tmeta = fs::metadata(&canon).map_err(|e| FsError::from(e).with_path(path))?;
    if crate::media_meta::is_offline_placeholder(&tmeta) {
        return Err(crate::preview::offline_error().with_path(path));
    }
    if !tmeta.is_file() {
        return Err(refuse(path, "Not a regular file", ErrorCode::Unsupported));
    }
    if is_system_file(&lmeta) || is_system_file(&tmeta) {
        return Err(refuse(
            path,
            "System files are not served to the preview",
            ErrorCode::PermissionDenied,
        ));
    }
    if served_mime(&canon).is_none() {
        return Err(refuse(
            path,
            "File type is not served to the preview",
            ErrorCode::Unsupported,
        ));
    }
    Ok(canon)
}

fn new_token() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random source unavailable");
    hex::encode(b)
}

impl MediaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&self, path: PathBuf, scope: &str) -> String {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.tick += 1;
        let tick = g.tick;
        let now = Instant::now();
        g.grants.retain(|_, v| v.expires > now);
        while g.grants.len() >= MAX_GRANTS {
            let oldest = g
                .grants
                .iter()
                .min_by_key(|(_, v)| v.last_used)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => g.grants.remove(&k),
                None => break,
            };
        }
        let token = new_token();
        g.grants.insert(
            token.clone(),
            Grant {
                path,
                scope: scope.to_string(),
                expires: now + GRANT_TTL,
                born: now,
                last_used: tick,
            },
        );
        token
    }

    /// One token per path, in order; `None` for a path that may not be served.
    pub fn grant(&self, paths: &[String], scope: &str) -> Result<Vec<Option<String>>, FsError> {
        if scope.is_empty() || scope.len() > MAX_SCOPE_LEN {
            return Err(FsError::new(ErrorCode::InvalidPath, "Invalid media scope"));
        }
        if paths.len() > MAX_PATHS_PER_CALL {
            return Err(FsError::new(
                ErrorCode::InvalidPath,
                "Too many media paths in one request",
            ));
        }
        Ok(paths
            .iter()
            .map(|p| vet(p).ok().map(|canon| self.insert(canon, scope)))
            .collect())
    }

    /// Drop every grant of `scope`; returns how many.
    pub fn revoke(&self, scope: &str) -> usize {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.grants.len();
        g.grants.retain(|_, v| v.scope != scope);
        before - g.grants.len()
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .grants
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Path for a live token (renews its idle TTL); `None` unknown/expired/revoked.
    fn resolve(&self, token: &str) -> Option<PathBuf> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.tick += 1;
        let tick = g.tick;
        let now = Instant::now();
        let live = g
            .grants
            .get(token)
            .is_some_and(|v| v.expires > now && now.duration_since(v.born) < MAX_LIFETIME);
        if !live {
            g.grants.remove(token);
            return None;
        }
        let v = g.grants.get_mut(token)?;
        v.expires = now + GRANT_TTL;
        v.last_used = tick;
        Some(v.path.clone())
    }

    /// Test hook: register ANY path without the eligibility checks (proves the
    /// serve side still refuses forbidden types).
    #[cfg(test)]
    pub(crate) fn force_insert(&self, path: PathBuf, scope: &str) -> String {
        self.insert(path, scope)
    }

    /// Test hook: pretend the grant was made `by` ago (None = clock too young to subtract).
    #[cfg(test)]
    pub(crate) fn age_by(&self, token: &str, by: Duration) -> Option<()> {
        let mut g = self.inner.lock().unwrap();
        let v = g.grants.get_mut(token)?;
        v.born = Instant::now().checked_sub(by)?;
        Some(())
    }

    /// Test hook: make a token already expired.
    #[cfg(test)]
    pub(crate) fn expire_now(&self, token: &str) {
        let mut g = self.inner.lock().unwrap();
        if let Some(v) = g.grants.get_mut(token) {
            v.expires = Instant::now() - Duration::from_secs(1);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RangeSpec {
    /// No (usable) Range header: whole file.
    None,
    /// Inclusive byte window.
    Window(u64, u64),
    Unsatisfiable,
}

/// Parse a single `bytes=` range against `len`. Another unit is ignored (the
/// whole file is sent); malformed, multi-range or out-of-file is unsatisfiable.
fn parse_range(header: Option<&str>, len: u64) -> RangeSpec {
    let Some(h) = header else {
        return RangeSpec::None;
    };
    let h = h.trim();
    let Some(spec) = h.strip_prefix("bytes=") else {
        return RangeSpec::None;
    };
    if spec.contains(',') || len == 0 {
        return RangeSpec::Unsatisfiable;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeSpec::Unsatisfiable;
    };
    let (a, b) = (a.trim(), b.trim());
    let (start, end) = if a.is_empty() {
        // suffix: last n bytes
        let Ok(n) = b.parse::<u64>() else {
            return RangeSpec::Unsatisfiable;
        };
        if n == 0 {
            return RangeSpec::Unsatisfiable;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let Ok(s) = a.parse::<u64>() else {
            return RangeSpec::Unsatisfiable;
        };
        let e = if b.is_empty() {
            len - 1
        } else {
            match b.parse::<u64>() {
                Ok(e) => e.min(len - 1),
                Err(_) => return RangeSpec::Unsatisfiable,
            }
        };
        (s, e)
    };
    if start >= len || start > end {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Window(start, end)
}

/// Token from the request path: exactly `/<32 lowercase hex>`; anything else
/// (extra segments, query-less variations, path-like input) is not a token.
fn token_from_path(path: &str) -> Option<&str> {
    let t = path.strip_prefix('/')?;
    (t.len() == 32 && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(t)
}

fn plain(status: StatusCode) -> Response<Vec<u8>> {
    base(status).body(Vec::new()).expect("static response")
}

fn base(status: StatusCode) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCEPT_RANGES, "bytes")
}

/// Serve one request. Blocking (file I/O): call it off the UI thread.
pub fn serve(
    registry: &MediaRegistry,
    method: &Method,
    uri_path: &str,
    range: Option<&str>,
) -> Response<Vec<u8>> {
    serve_waiting(registry, method, uri_path, range, GATE_WAIT)
}

fn serve_waiting(
    registry: &MediaRegistry,
    method: &Method,
    uri_path: &str,
    range: Option<&str>,
    gate_wait: Duration,
) -> Response<Vec<u8>> {
    let head_only = method == Method::HEAD;
    if !(method == Method::GET || head_only) {
        return plain(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(token) = token_from_path(uri_path) else {
        return plain(StatusCode::NOT_FOUND);
    };
    let Some(path) = registry.resolve(token) else {
        return plain(StatusCode::NOT_FOUND);
    };
    // Defense in depth: the type is judged again at serve time.
    let Some(mime) = served_mime(&path) else {
        return plain(StatusCode::FORBIDDEN);
    };
    // At most MAX_CONCURRENT_SERVES bodies in memory at once; the rest wait.
    let Some(_permit) = registry.gate.acquire(gate_wait) else {
        return base(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::RETRY_AFTER, "1")
            .body(Vec::new())
            .expect("static response");
    };
    // TOCTOU (FER-452 review L1): the grant holds a canonical path. If the
    // file was swapped for a link since, the path no longer canonicalizes to
    // itself (or is a link/reparse point now): refuse instead of following it.
    match (fs::symlink_metadata(&path), fs::canonicalize(&path)) {
        (Ok(lmeta), Ok(canon))
            if canon == path
                && !serve_blocked(lmeta.file_type().is_symlink(), attrs_of(&lmeta)) => {}
        _ => return plain(StatusCode::NOT_FOUND),
    }
    let Ok(mut file) = fs::File::open(&path) else {
        return plain(StatusCode::NOT_FOUND);
    };
    let Ok(meta) = file.metadata() else {
        return plain(StatusCode::NOT_FOUND);
    };
    if !meta.is_file() {
        return plain(StatusCode::FORBIDDEN);
    }
    // A file turned into a cloud placeholder after the grant must not be recalled.
    if crate::media_meta::is_offline_placeholder(&meta) {
        return plain(StatusCode::FORBIDDEN);
    }
    let len = meta.len();
    let (status, start, end) = match parse_range(range, len) {
        RangeSpec::Unsatisfiable => {
            return base(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Vec::new())
                .expect("static response");
        }
        RangeSpec::None => {
            if len > MAX_FULL_BODY {
                return plain(StatusCode::PAYLOAD_TOO_LARGE);
            }
            (StatusCode::OK, 0, len.saturating_sub(1))
        }
        RangeSpec::Window(s, e) => (
            StatusCode::PARTIAL_CONTENT,
            s,
            e.min(s.saturating_add(MAX_CHUNK) - 1),
        ),
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut b = base(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, count.to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        b = b.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    if head_only || count == 0 {
        return b.body(Vec::new()).expect("static response");
    }
    let mut buf = Vec::with_capacity(count as usize);
    let ok = file.seek(SeekFrom::Start(start)).is_ok()
        && file.take(count).read_to_end(&mut buf).is_ok()
        && buf.len() as u64 == count;
    if !ok {
        return plain(StatusCode::NOT_FOUND);
    }
    b.body(buf).expect("static response")
}

// ---- Tauri wiring -------------------------------------------------------

/// Register the protocol on the builder.
pub fn register<R: tauri::Runtime>(b: tauri::Builder<R>) -> tauri::Builder<R> {
    use tauri::Manager;
    b.register_asynchronous_uri_scheme_protocol("ferrum-media", |ctx, request, responder| {
        let app = ctx.app_handle().clone();
        let method = request.method().clone();
        let uri_path = request.uri().path().to_string();
        let range = request
            .headers()
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        tauri::async_runtime::spawn_blocking(move || {
            let resp = match app.try_state::<MediaRegistry>() {
                Some(reg) => serve(&reg, &method, &uri_path, range.as_deref()),
                None => plain(StatusCode::NOT_FOUND),
            };
            responder.respond(resp);
        });
    })
}

#[tauri::command]
pub async fn media_grant(
    paths: Vec<String>,
    scope: String,
    registry: tauri::State<'_, MediaRegistry>,
) -> Result<Vec<Option<String>>, FsError> {
    // File-system checks for up to MAX_PATHS_PER_CALL paths: off the UI thread.
    let reg = registry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || reg.grant(&paths, &scope))
        .await
        .map_err(|e| FsError::new(ErrorCode::Io, e.to_string()))?
}

#[tauri::command]
pub fn media_revoke(scope: String, registry: tauri::State<'_, MediaRegistry>) -> usize {
    registry.revoke(&scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
        let p = dir.join(name);
        fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn get(reg: &MediaRegistry, token: &str, range: Option<&str>) -> Response<Vec<u8>> {
        serve(reg, &Method::GET, &format!("/{token}"), range)
    }

    fn token_of(reg: &MediaRegistry, path: &str) -> String {
        reg.grant(&[path.to_string()], "t").unwrap()[0]
            .clone()
            .unwrap()
    }

    #[test]
    fn serves_image_with_safe_headers() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"\x89PNGdata");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        let r = get(&reg, &t, None);
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.body(), b"\x89PNGdata");
        assert_eq!(r.headers()["content-type"], "image/png");
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert!(!r.headers().contains_key("access-control-allow-origin"));
    }

    #[test]
    fn token_has_128_bits_and_carries_no_path() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "secret-name.png", b"x");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        assert_eq!(t.len(), 32);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!t.contains("secret"));
        assert_ne!(t, token_of(&reg, &p));
    }

    #[test]
    fn forbidden_types_are_refused_at_grant_and_at_serve() {
        let d = tempfile::tempdir().unwrap();
        let reg = MediaRegistry::new();
        for name in [
            "x.html", "x.htm", "x.svg", "x.js", "x.exe", "x.xml", "x.pdf", "noext",
        ] {
            let p = write(d.path(), name, b"<script>1</script>");
            let r = reg.grant(std::slice::from_ref(&p), "t").unwrap();
            assert!(r[0].is_none(), "{name} must not get a token");
            let t = reg.force_insert(PathBuf::from(&p), "t");
            assert_eq!(
                get(&reg, &t, None).status(),
                StatusCode::FORBIDDEN,
                "{name}"
            );
        }
    }

    #[test]
    fn unknown_malformed_and_expired_tokens_are_404() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"x");
        let reg = MediaRegistry::new();
        assert_eq!(
            get(&reg, &"0".repeat(32), None).status(),
            StatusCode::NOT_FOUND
        );
        for bad in [
            "/",
            "/abc",
            "/../etc",
            &format!("/{}/x", "a".repeat(32)),
            &format!("/{}", "A".repeat(32)),
        ] {
            assert_eq!(
                serve(&reg, &Method::GET, bad, None).status(),
                StatusCode::NOT_FOUND,
                "{bad}"
            );
        }
        let t = token_of(&reg, &p);
        reg.expire_now(&t);
        assert_eq!(get(&reg, &t, None).status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&reg, &t, None).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn revoke_by_scope_makes_token_404() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"x");
        let reg = MediaRegistry::new();
        let a = reg.grant(std::slice::from_ref(&p), "one").unwrap()[0]
            .clone()
            .unwrap();
        let b = reg.grant(&[p], "two").unwrap()[0].clone().unwrap();
        assert_eq!(reg.revoke("one"), 1);
        assert_eq!(get(&reg, &a, None).status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&reg, &b, None).status(), StatusCode::OK);
    }

    #[test]
    fn range_requests_206_and_416() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "v.mp4", b"0123456789");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        let r = get(&reg, &t, Some("bytes=2-5"));
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.body(), b"2345");
        assert_eq!(r.headers()["content-range"], "bytes 2-5/10");
        assert_eq!(r.headers()["content-type"], "video/mp4");
        let r = get(&reg, &t, Some("bytes=7-"));
        assert_eq!(r.body(), b"789");
        assert_eq!(r.headers()["content-range"], "bytes 7-9/10");
        let r = get(&reg, &t, Some("bytes=-3"));
        assert_eq!(r.body(), b"789");
        let r = get(&reg, &t, Some("bytes=8-99"));
        assert_eq!(r.body(), b"89");
        for bad in [
            "bytes=10-",
            "bytes=5-2",
            "bytes=a-b",
            "bytes=0-1,3-4",
            "bytes=-0",
            "bytes=99-100",
        ] {
            let r = get(&reg, &t, Some(bad));
            assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE, "{bad}");
            assert_eq!(r.headers()["content-range"], "bytes */10");
        }
        // Unknown unit: ignored, whole file.
        let r = get(&reg, &t, Some("items=1-2"));
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.body(), b"0123456789");
    }

    #[test]
    fn head_has_headers_without_body_and_post_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"abc");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        let r = serve(&reg, &Method::HEAD, &format!("/{t}"), None);
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.body().is_empty());
        assert_eq!(r.headers()["content-length"], "3");
        let r = serve(&reg, &Method::POST, &format!("/{t}"), None);
        assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn open_ended_range_is_clamped_to_chunk() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("big.mp4");
        let f = fs::File::create(&p).unwrap();
        f.set_len(MAX_CHUNK + 100).unwrap();
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p.to_string_lossy());
        let r = get(&reg, &t, Some("bytes=0-"));
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.body().len() as u64, MAX_CHUNK);
        assert_eq!(
            r.headers()["content-range"].to_str().unwrap(),
            format!("bytes 0-{}/{}", MAX_CHUNK - 1, MAX_CHUNK + 100)
        );
    }

    #[test]
    fn missing_dir_and_nonexistent_are_not_granted() {
        let d = tempfile::tempdir().unwrap();
        let reg = MediaRegistry::new();
        let missing = d.path().join("none.png").to_string_lossy().into_owned();
        let dir = d.path().to_string_lossy().into_owned();
        let r = reg.grant(&[missing, dir, "".into()], "t").unwrap();
        assert!(r.iter().all(Option::is_none));
        assert!(reg.is_empty());
    }

    #[test]
    fn lru_caps_the_registry() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"x");
        let reg = MediaRegistry::new();
        let first = token_of(&reg, &p);
        for _ in 0..MAX_GRANTS {
            reg.insert(PathBuf::from(&p), "t");
        }
        assert!(reg.len() <= MAX_GRANTS);
        assert_eq!(get(&reg, &first, None).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn scope_and_batch_limits() {
        let reg = MediaRegistry::new();
        assert!(reg.grant(&[], "").is_err());
        assert!(reg.grant(&[], &"s".repeat(65)).is_err());
        let many = vec!["x.png".to_string(); MAX_PATHS_PER_CALL + 1];
        assert!(reg.grant(&many, "t").is_err());
    }

    #[test]
    fn symlink_to_forbidden_type_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let target = write(d.path(), "evil.html", b"<html>");
        let link = d.path().join("pic.png");
        #[cfg(unix)]
        let ok = std::os::unix::fs::symlink(&target, &link).is_ok();
        #[cfg(windows)]
        let ok = std::os::windows::fs::symlink_file(&target, &link).is_ok();
        if !ok {
            eprintln!(
                "SKIPPED symlink_to_forbidden_type_is_refused: cannot create a symlink here (no privilege)"
            );
            return;
        }
        let reg = MediaRegistry::new();
        let r = reg
            .grant(&[link.to_string_lossy().into_owned()], "t")
            .unwrap();
        assert!(r[0].is_none());
    }

    #[test]
    fn symlink_to_image_is_served() {
        let d = tempfile::tempdir().unwrap();
        let target = write(d.path(), "real.png", b"PNG");
        let link = d.path().join("l.png");
        #[cfg(unix)]
        let ok = std::os::unix::fs::symlink(&target, &link).is_ok();
        #[cfg(windows)]
        let ok = std::os::windows::fs::symlink_file(&target, &link).is_ok();
        if !ok {
            eprintln!(
                "SKIPPED symlink_to_image_is_served: cannot create a symlink here (no privilege)"
            );
            return;
        }
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &link.to_string_lossy());
        assert_eq!(get(&reg, &t, None).body(), b"PNG");
    }

    #[test]
    fn l3_device_and_extended_paths_are_refused() {
        for p in [
            r"\\.\C:\a.png",
            r"\\.\PhysicalDrive0\a.png",
            r"\\?\GLOBALROOT\Device\x\a.png",
            r"\\?\UNC\evil\share\a.png",
            "//./C:/a.png",
            "//?/GLOBALROOT/a.png",
            r"\??\C:\a.png",
            "/??/C:/a.png",
        ] {
            assert!(is_device_path(p), "{p}");
            let reg = MediaRegistry::new();
            assert!(
                reg.grant(&[p.to_string()], "t").unwrap()[0].is_none(),
                "{p}"
            );
        }
        // The plain drive form that canonicalize itself yields stays allowed.
        assert!(!is_device_path(r"\\?\C:\pics\a.png"));
        assert!(!is_device_path(r"C:\pics\a.png"));
        assert!(!is_device_path("/home/a/a.png"));
    }

    #[cfg(windows)]
    #[test]
    fn l3_system_attribute_files_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "sys.png", b"x");
        let ok = std::process::Command::new("attrib")
            .args(["+S", &p])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "attrib +S failed");
        let reg = MediaRegistry::new();
        assert!(reg.grant(std::slice::from_ref(&p), "t").unwrap()[0].is_none());
        let _ = std::process::Command::new("attrib")
            .args(["-S", &p])
            .status();
    }

    /// R1 regression: OneDrive/cloud sync files that are already downloaded carry
    /// FILE_ATTRIBUTE_REPARSE_POINT (0x400, typically 0x420 with ARCHIVE) but are
    /// not symlinks. They must be served; only symlinks and SYSTEM files are not.
    #[test]
    fn r1_downloaded_cloud_files_with_reparse_attribute_are_not_blocked() {
        assert!(
            !serve_blocked(false, 0x420),
            "hydrated OneDrive/cloud sync file"
        );
        assert!(!serve_blocked(false, 0x400));
        assert!(!serve_blocked(false, 0x20));
        assert!(serve_blocked(true, 0x400), "a symlink is refused");
        assert!(serve_blocked(false, 0x4), "SYSTEM is refused");
        // Not-yet-local placeholders stay refused through the shared helper,
        // even when they also carry the reparse bit.
        assert!(!crate::media_meta::offline_attrs(0x420));
        assert!(crate::media_meta::offline_attrs(0x420 | 0x0040_0000));
        assert!(crate::media_meta::offline_attrs(0x420 | 0x0004_0000));
        assert!(crate::media_meta::offline_attrs(0x420 | 0x1000));
    }

    #[test]
    fn l1_serve_refuses_a_path_that_is_not_canonical_anymore() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        let p = write(d.path(), "a.png", b"x");
        let reg = MediaRegistry::new();
        // Granted canonically: fine.
        let t = token_of(&reg, &p);
        assert_eq!(get(&reg, &t, None).status(), StatusCode::OK);
        // A grant whose path does not canonicalize to itself (".." hop) is refused.
        let hop = d.path().join("sub").join("..").join("a.png");
        let t = reg.force_insert(hop, "t");
        assert_eq!(get(&reg, &t, None).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn l1_file_swapped_for_a_symlink_after_grant_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"original");
        let other = write(d.path(), "other.png", b"swapped-in");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        assert_eq!(get(&reg, &t, None).body(), b"original");
        // The granted file is replaced by a link to another file.
        let canon = reg.resolve(&t).unwrap();
        fs::remove_file(&canon).unwrap();
        #[cfg(unix)]
        let ok = std::os::unix::fs::symlink(&other, &canon).is_ok();
        #[cfg(windows)]
        let ok = std::os::windows::fs::symlink_file(&other, &canon).is_ok();
        if !ok {
            eprintln!(
                "SKIPPED l1_file_swapped_for_a_symlink_after_grant_is_refused: cannot create a symlink here (no privilege)"
            );
            return;
        }
        assert_eq!(get(&reg, &t, None).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn l2_gate_limits_concurrent_serves() {
        let g = Gate::new(2);
        let a = g.acquire(Duration::from_millis(10)).expect("slot 1");
        let _b = g.acquire(Duration::from_millis(10)).expect("slot 2");
        assert!(
            g.acquire(Duration::from_millis(30)).is_none(),
            "full -> gives up"
        );
        drop(a);
        assert!(
            g.acquire(Duration::from_millis(30)).is_some(),
            "freed slot is reusable"
        );
    }

    #[test]
    fn l2_a_waiting_request_gets_the_slot_when_one_frees_up() {
        let g = Arc::new(Gate::new(1));
        let first = g.acquire(Duration::from_millis(10)).unwrap();
        let g2 = Arc::clone(&g);
        let h = std::thread::spawn(move || g2.acquire(Duration::from_secs(5)).is_some());
        std::thread::sleep(Duration::from_millis(50));
        drop(first);
        assert!(h.join().unwrap(), "waiter was served after the slot freed");
    }

    #[test]
    fn l2_serve_answers_503_when_the_gate_stays_full() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"x");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        let held: Vec<_> = (0..MAX_CONCURRENT_SERVES)
            .map(|_| reg.gate.acquire(Duration::from_millis(10)).unwrap())
            .collect();
        let r = serve_waiting(
            &reg,
            &Method::GET,
            &format!("/{t}"),
            None,
            Duration::from_millis(30),
        );
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers()["retry-after"], "1");
        drop(held);
        assert_eq!(get(&reg, &t, None).status(), StatusCode::OK);
    }

    #[test]
    fn grants_have_a_hard_lifetime_even_when_used() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.png", b"x");
        let reg = MediaRegistry::new();
        let t = token_of(&reg, &p);
        assert_eq!(get(&reg, &t, None).status(), StatusCode::OK); // renews the idle TTL
        if reg
            .age_by(&t, MAX_LIFETIME + Duration::from_secs(1))
            .is_none()
        {
            eprintln!(
                "SKIPPED grants_have_a_hard_lifetime_even_when_used: monotonic clock is younger than 12 h"
            );
            return;
        }
        assert_eq!(get(&reg, &t, None).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn range_parser_edge_cases() {
        assert_eq!(parse_range(None, 5), RangeSpec::None);
        assert_eq!(parse_range(Some("bytes=0-0"), 5), RangeSpec::Window(0, 0));
        assert_eq!(parse_range(Some("bytes=0-"), 0), RangeSpec::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-100"), 5), RangeSpec::Window(0, 4));
    }

    #[test]
    fn placeholder_attr_is_refused_by_the_shared_helper() {
        // The vet() path reuses media_meta::offline_attrs; pin its bits here.
        assert!(crate::media_meta::offline_attrs(0x1000));
        assert!(crate::media_meta::offline_attrs(0x0040_0000));
        assert!(!crate::media_meta::offline_attrs(0x20));
    }
}
