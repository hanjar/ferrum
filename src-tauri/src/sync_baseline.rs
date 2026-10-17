// AI-10 / FER-414 (H2): penyimpanan lokal BERVERSI untuk baseline sync dua arah.
//
// Aturan:
// - Satu file per pasangan root (kiri, kanan). Kunci = path KANONIK terurut (kiri lalu kanan),
//   hasil normalisasi yang sama dengan `compare::validate_sync_roots` (canonicalize), jadi alias
//   symlink/junction dan beda huruf besar-kecil di Windows menghasilkan kunci yang sama. Panel
//   yang ditukar = pasangan lain (first-run), sesuai `isBaselineTrusted` di sisi TS.
// - Tulis ATOMIK: file sementara eksklusif (`create_new`) + fsync + rename ke file milik pasangan
//   itu sendiri. Crash di tengah menulis tidak merusak baseline lama.
// - File korup / versi tak dikenal / kunci tak cocok / root pindah / volume terlepas / file
//   terlalu besar => `FirstRun` (tidak menebak). Tidak ada yang diperbaiki diam-diam.
// - Batas: jumlah entri per baseline, ukuran file, jumlah pasangan. Terlampaui => penyimpanan
//   DITOLAK dengan kode `limit_exceeded` (baseline lama tetap utuh), bukan dipotong diam-diam.
// - Error yang menyeberang IPC hanya `{ code, message }` tanpa path.
//
// Modul ini TIDAK memutuskan isi baseline: rekonsiliasi hanya terjadi di `reconcileBaseline`
// (TS) dari hasil terverifikasi; di sini hanya validasi bentuk + persistensi.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::automation::commands::AutomationState;
use crate::automation::lease::{LeaseEnv, LeaseGuard, require_cover};
use crate::automation::store::Store as AutomationStore;
use crate::compare::validate_sync_roots;
use crate::domain::ErrorCode;

pub const BASELINE_VERSION: u32 = 1;
/// Maksimum entri per baseline.
pub const MAX_ENTRIES: usize = 200_000;
/// Maksimum ukuran file baseline (byte) yang ditulis maupun dibaca.
pub const MAX_FILE_BYTES: u64 = 48 * 1024 * 1024;
/// Maksimum jumlah pasangan root yang disimpan.
pub const MAX_PAIRS: usize = 256;
const MAX_NAME_BYTES: usize = 4096;
const FILE_EXT: &str = "json";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

// ── DTO (strict) ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootPair {
    pub left_root: String,
    pub right_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactsDto {
    pub size: u64,
    pub modified_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryDto {
    pub left: FactsDto,
    pub right: FactsDto,
}

/// Cermin `SyncBaseline` di `src/lib/syncBaseline.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaselineDto {
    pub version: u32,
    pub left_root: String,
    pub right_root: String,
    pub entries: BTreeMap<String, EntryDto>,
    /// FER-477b: journal marks of the run that verified EVERY entry by
    /// content (written by `reconcileBaseline` only after a fully clean
    /// settle compare). Absent = the next compare reads all content.
    /// Boxed: keeps `LoadResult` small (clippy large_enum_variant); serde
    /// sees through the box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<Box<crate::sync_incremental::JournalMarksDto>>,
}

/// Bentuk di disk. `key` dicatat agar file yang dipindah/ditimpa dengan nama lain terdeteksi.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredFile {
    version: u32,
    key: String,
    saved_at_ms: u64,
    baseline: BaselineDto,
}

/// Mengapa sebuah pemuatan menjadi first-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FirstRunReason {
    /// Belum pernah disimpan untuk pasangan ini.
    None,
    Corrupt,
    UnknownVersion,
    /// Root tidak ada / volume terlepas / tidak terbaca.
    RootsUnavailable,
    /// Isi file tidak cocok dengan pasangan yang diminta.
    Mismatch,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum LoadResult {
    Ok { baseline: BaselineDto },
    FirstRun { reason: FirstRunReason },
}

/// Satu-satunya bentuk error yang menyeberang IPC: kode + pesan tetap, tanpa path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselineError {
    pub code: &'static str,
    pub message: &'static str,
}

const fn err(code: &'static str, message: &'static str) -> BaselineError {
    BaselineError { code, message }
}

// ── Kunci pasangan ─────────────────────────────────────────────────────

enum KeyError {
    /// Root sama / bersarang / alias: harus DITOLAK, bukan first-run.
    Rejected,
    /// Root tidak ada / volume terlepas: perlakukan sebagai first-run.
    Unavailable,
}

/// Kunci identitas pasangan: path kanonik terurut. `validate_sync_roots` dipanggil lebih dulu,
/// jadi root yang sama/bersarang/alias tidak pernah mendapat kunci.
fn pair_key(pair: &RootPair) -> Result<String, KeyError> {
    match validate_sync_roots(&pair.left_root, &pair.right_root) {
        Ok(()) => {}
        Err(e) if e.code == ErrorCode::InvalidPath => return Err(KeyError::Rejected),
        Err(_) => return Err(KeyError::Unavailable),
    }
    let canon = |root: &str| {
        fs::canonicalize(root)
            .map(|p| fold(&p))
            .map_err(|_| KeyError::Unavailable)
    };
    Ok(format!(
        "L={}\nR={}",
        canon(&pair.left_root)?,
        canon(&pair.right_root)?
    ))
}

fn fold(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn file_name_for(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("{hex}.{FILE_EXT}")
}

fn norm_root(root: &str) -> String {
    let s = root.replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

// ── Validasi bentuk ────────────────────────────────────────────────────

fn validate_baseline(pair: &RootPair, b: &BaselineDto) -> Result<(), BaselineError> {
    if b.version != BASELINE_VERSION {
        return Err(err("invalid_baseline", "Unsupported baseline version"));
    }
    if norm_root(&b.left_root) != norm_root(&pair.left_root)
        || norm_root(&b.right_root) != norm_root(&pair.right_root)
    {
        return Err(err(
            "invalid_baseline",
            "Baseline does not belong to this folder pair",
        ));
    }
    if b.entries.len() > MAX_ENTRIES {
        return Err(err(
            "limit_exceeded",
            "Baseline has too many entries to store",
        ));
    }
    for name in b.entries.keys() {
        if name.is_empty() || name.len() > MAX_NAME_BYTES || name.contains('\0') {
            return Err(err(
                "invalid_baseline",
                "Baseline contains an invalid entry name",
            ));
        }
    }
    if b.journal.as_ref().is_some_and(|j| !j.is_valid()) {
        return Err(err(
            "invalid_baseline",
            "Baseline has invalid journal marks",
        ));
    }
    Ok(())
}

// ── Store ──────────────────────────────────────────────────────────────

struct Inner {
    /// None = penyimpanan tidak tersedia (app data dir tidak dapat dibuat).
    dir: Option<PathBuf>,
    /// Serialisasi tulis/hapus dalam proses (dua command bersamaan).
    lock: Mutex<()>,
}

/// State Tauri. Murah di-clone (Arc) agar bisa dipindah ke thread blocking.
#[derive(Clone)]
pub struct SyncBaselineStore {
    inner: Arc<Inner>,
}

impl SyncBaselineStore {
    pub fn open(dir: &Path) -> Self {
        let dir = fs::create_dir_all(dir).ok().map(|()| dir.to_path_buf());
        Self {
            inner: Arc::new(Inner {
                dir,
                lock: Mutex::new(()),
            }),
        }
    }

    /// Penyimpanan tidak tersedia: load = first-run, save = error jujur.
    pub fn unavailable() -> Self {
        Self {
            inner: Arc::new(Inner {
                dir: None,
                lock: Mutex::new(()),
            }),
        }
    }

    fn path_for(&self, key: &str) -> Option<PathBuf> {
        self.inner.dir.as_ref().map(|d| d.join(file_name_for(key)))
    }

    /// Muat baseline untuk pasangan. Root sama/bersarang/alias => Err; selain itu apa pun yang
    /// tidak bisa dipercaya => `FirstRun` dengan alasan.
    pub fn load(&self, pair: &RootPair) -> Result<LoadResult, BaselineError> {
        let key = match pair_key(pair) {
            Ok(k) => k,
            Err(KeyError::Rejected) => {
                return Err(err("invalid_roots", "Folder pair is not valid for sync"));
            }
            Err(KeyError::Unavailable) => {
                return Ok(LoadResult::FirstRun {
                    reason: FirstRunReason::RootsUnavailable,
                });
            }
        };
        let first = |reason| Ok(LoadResult::FirstRun { reason });
        let Some(path) = self.path_for(&key) else {
            return first(FirstRunReason::None);
        };
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return first(FirstRunReason::None),
        };
        if meta.len() > MAX_FILE_BYTES {
            return first(FirstRunReason::TooLarge);
        }
        let Ok(bytes) = fs::read(&path) else {
            return first(FirstRunReason::Corrupt);
        };
        // Versi dibaca dulu secara longgar agar versi baru tidak dikira korup.
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return first(FirstRunReason::Corrupt);
        };
        match value.get("version").and_then(|v| v.as_u64()) {
            Some(v) if v == u64::from(BASELINE_VERSION) => {}
            Some(_) => return first(FirstRunReason::UnknownVersion),
            None => return first(FirstRunReason::Corrupt),
        }
        let Ok(stored) = serde_json::from_value::<StoredFile>(value) else {
            return first(FirstRunReason::Corrupt);
        };
        if stored.key != key {
            return first(FirstRunReason::Mismatch);
        }
        if stored.baseline.version != BASELINE_VERSION
            || stored.baseline.entries.len() > MAX_ENTRIES
        {
            return first(FirstRunReason::Corrupt);
        }
        // Identitas dibuktikan oleh kunci kanonik; roots yang dikembalikan = string yang diminta
        // (alias/case boleh berbeda) agar `isBaselineTrusted` di TS tetap cocok.
        let mut baseline = stored.baseline;
        baseline.left_root = pair.left_root.clone();
        baseline.right_root = pair.right_root.clone();
        Ok(LoadResult::Ok { baseline })
    }

    /// Simpan ATOMIK. Menolak root tak valid, baseline yang bukan milik pasangan ini, dan
    /// batas terlampaui (baseline lama dibiarkan).
    /// UNGUARDED write, private to this module (a baseline is written only under a lease). Production code goes through
    /// [`save_with_guard`] (a run) or [`save_with_lease`] (a command): only the lease holder writes
    /// a baseline (design §5.7). Integration tests use the leased path too.
    fn save(&self, pair: &RootPair, baseline: &BaselineDto) -> Result<(), BaselineError> {
        let key = match pair_key(pair) {
            Ok(k) => k,
            Err(KeyError::Rejected) => {
                return Err(err("invalid_roots", "Folder pair is not valid for sync"));
            }
            Err(KeyError::Unavailable) => {
                return Err(err("roots_unavailable", "Folders are not available"));
            }
        };
        validate_baseline(pair, baseline)?;
        let dir = self
            .inner
            .dir
            .as_ref()
            .ok_or(err("unavailable", "Baseline storage is not available"))?;
        let target = dir.join(file_name_for(&key));
        let stored = StoredFile {
            version: BASELINE_VERSION,
            key,
            saved_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            baseline: baseline.clone(),
        };
        let bytes =
            serde_json::to_vec(&stored).map_err(|_| err("io", "Could not encode baseline"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(err("limit_exceeded", "Baseline is too large to store"));
        }
        let _guard = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        if !target.exists() && count_pairs(dir) >= MAX_PAIRS {
            return Err(err(
                "limit_exceeded",
                "Too many stored folder pairs; clear some first",
            ));
        }
        write_atomic(dir, &target, &bytes).map_err(|_| err("io", "Could not write baseline"))
    }

    /// Test-only seam for OTHER modules' tests that need a baseline on disk without taking a
    /// lease. Production code cannot call it (it does not exist outside `cfg(test)`); `save`
    /// itself is private to this module, so every non-test write goes through
    /// [`save_with_lease`] or [`save_with_guard`].
    #[cfg(test)]
    pub(crate) fn save_unguarded_for_tests(
        &self,
        pair: &RootPair,
        baseline: &BaselineDto,
    ) -> Result<(), BaselineError> {
        self.save(pair, baseline)
    }

    /// Hapus baseline pasangan (kembali ke first-run). Idempotent. Private like `save`:
    /// callers use [`clear_with_lease`].
    fn clear(&self, pair: &RootPair) -> Result<(), BaselineError> {
        let key = match pair_key(pair) {
            Ok(k) => k,
            Err(KeyError::Rejected) => {
                return Err(err("invalid_roots", "Folder pair is not valid for sync"));
            }
            // Root tidak ada lagi: kunci tak bisa dihitung, tidak ada yang dihapus.
            Err(KeyError::Unavailable) => {
                return Err(err("roots_unavailable", "Folders are not available"));
            }
        };
        let Some(path) = self.path_for(&key) else {
            return Ok(());
        };
        let _guard = self.inner.lock.lock().unwrap_or_else(|p| p.into_inner());
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(err("io", "Could not remove baseline")),
        }
    }
}

fn count_pairs(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == FILE_EXT))
                .count()
        })
        .unwrap_or(0)
}

/// Tulis `bytes` ke file sementara eksklusif di direktori yang sama, fsync, lalu rename ke
/// `target` (file milik pasangan ini). Nama sementara unik per proses+urutan dan dibuat dengan
/// `create_new`, jadi tidak pernah menimpa file lain; sisa sementara dibersihkan saat gagal.
fn write_atomic(dir: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    // Nama sementara yang sudah ada (milik pihak lain) TIDAK ditimpa dan TIDAK dihapus: lanjut
    // ke nama berikutnya; hanya file yang kita buat sendiri yang dibersihkan saat gagal.
    let mut created: Option<(PathBuf, std::fs::File)> = None;
    for _ in 0..16 {
        let tmp = dir.join(format!(
            ".tmp-{}-{}-{name}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst),
        ));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => {
                created = Some((tmp, f));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let Some((tmp, mut f)) = created else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no free temporary name",
        ));
    };
    let result = (|| {
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

// ── Lease guard (Automation design §3.4, §5.7) ──────────────────────────

/// Save a baseline only while THIS process holds a fresh lease that covers both roots of the
/// pair (a manual Synchronize Dirs lease or a run lease). Without it nothing is written:
/// `lease_required`. This is the cross-process guard for baseline writes; the check is made here
/// in the backend, never trusted from the webview. The folders are canonicalized exactly as
/// `sync_lease_acquire` did, so aliases (case, mapped drive vs UNC, junctions) match the lease.
pub fn save_with_lease(
    baselines: &SyncBaselineStore,
    automation: &AutomationStore,
    env: &LeaseEnv,
    canonical: &dyn Fn(&str) -> Result<String, crate::sync_engine::roots::RootError>,
    pair: &RootPair,
    baseline: &BaselineDto,
) -> Result<(), BaselineError> {
    require_pair_lease(automation, env, canonical, pair)?;
    baselines.save(pair, baseline)
}

/// Save a baseline from a run that HOLDS a [`LeaseGuard`] (engine path, design §5.7): the guard's
/// own lease must be alive (not lost, heartbeat fresh) and cover both roots of the pair. A lost
/// lease or a guard for other folders writes nothing (`lease_required`). The folders are
/// canonicalized exactly as the lease was acquired.
pub fn save_with_guard(
    baselines: &SyncBaselineStore,
    guard: &LeaseGuard,
    canonical: &dyn Fn(&str) -> Result<String, crate::sync_engine::roots::RootError>,
    pair: &RootPair,
    baseline: &BaselineDto,
) -> Result<(), BaselineError> {
    const REQUIRED: BaselineError = err(
        "lease_required",
        "The baseline can only be changed while this sync holds its folder lease",
    );
    let (Ok(left), Ok(right)) = (canonical(&pair.left_root), canonical(&pair.right_root)) else {
        return Err(REQUIRED);
    };
    if !guard.covers(&[left, right]) {
        return Err(REQUIRED);
    }
    baselines.save(pair, baseline)
}

/// Forget a pair's baseline, under the same lease rule as a save: every writer of a pair's
/// baseline file holds the lease (design §5.7).
pub fn clear_with_lease(
    baselines: &SyncBaselineStore,
    automation: &AutomationStore,
    env: &LeaseEnv,
    canonical: &dyn Fn(&str) -> Result<String, crate::sync_engine::roots::RootError>,
    pair: &RootPair,
) -> Result<(), BaselineError> {
    require_pair_lease(automation, env, canonical, pair)?;
    baselines.clear(pair)
}

fn require_pair_lease(
    automation: &AutomationStore,
    env: &LeaseEnv,
    canonical: &dyn Fn(&str) -> Result<String, crate::sync_engine::roots::RootError>,
    pair: &RootPair,
) -> Result<(), BaselineError> {
    const REQUIRED: BaselineError = err(
        "lease_required",
        "The baseline can only be changed while this sync holds its folder lease",
    );
    let (Ok(left), Ok(right)) = (canonical(&pair.left_root), canonical(&pair.right_root)) else {
        return Err(REQUIRED);
    };
    require_cover(automation, env, &[left, right]).map_err(|_| REQUIRED)
}

// ── Command Tauri ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn sync_baseline_load(
    store: tauri::State<'_, SyncBaselineStore>,
    pair: RootPair,
) -> Result<LoadResult, BaselineError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.load(&pair))
        .await
        .map_err(|_| err("io", "Baseline task failed"))?
}

#[tauri::command]
pub async fn sync_baseline_save(
    store: tauri::State<'_, SyncBaselineStore>,
    automation: tauri::State<'_, AutomationState>,
    pair: RootPair,
    baseline: BaselineDto,
) -> Result<(), BaselineError> {
    let store = store.inner().clone();
    let automation_store = automation.store.clone();
    let env = automation.leases.env().clone();
    tauri::async_runtime::spawn_blocking(move || {
        save_with_lease(
            &store,
            &automation_store,
            &env,
            &|p| crate::sync_engine::roots::canonical(p),
            &pair,
            &baseline,
        )
    })
    .await
    .map_err(|_| err("io", "Baseline task failed"))?
}

#[tauri::command]
pub async fn sync_baseline_clear(
    store: tauri::State<'_, SyncBaselineStore>,
    automation: tauri::State<'_, AutomationState>,
    pair: RootPair,
) -> Result<(), BaselineError> {
    let store = store.inner().clone();
    let automation_store = automation.store.clone();
    let env = automation.leases.env().clone();
    tauri::async_runtime::spawn_blocking(move || {
        clear_with_lease(
            &store,
            &automation_store,
            &env,
            &|p| crate::sync_engine::roots::canonical(p),
            &pair,
        )
    })
    .await
    .map_err(|_| err("io", "Baseline task failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure or early return).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "ferrum-sbl-{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }

        #[allow(dead_code)]
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp(tag: &str) -> TempDir {
        TempDir::new(tag)
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn pair(l: &Path, r: &Path) -> RootPair {
        RootPair {
            left_root: s(l),
            right_root: s(r),
        }
    }

    fn facts(size: u64, ms: i64) -> FactsDto {
        FactsDto {
            size,
            modified_ms: ms,
        }
    }

    fn baseline_for(p: &RootPair, names: &[&str]) -> BaselineDto {
        let mut entries = BTreeMap::new();
        for (i, n) in names.iter().enumerate() {
            entries.insert(
                (*n).to_string(),
                EntryDto {
                    left: facts(i as u64 + 1, 1000),
                    right: facts(i as u64 + 1, 1000),
                },
            );
        }
        BaselineDto {
            version: BASELINE_VERSION,
            left_root: p.left_root.clone(),
            right_root: p.right_root.clone(),
            entries,
            journal: None,
        }
    }

    /// Tiga folder: store, kiri, kanan.
    fn setup(tag: &str) -> (TempDir, PathBuf, PathBuf, SyncBaselineStore) {
        let base = temp(tag);
        let l = base.join("left");
        let r = base.join("right");
        let st = base.join("store");
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        (base, l, r, SyncBaselineStore::open(&st))
    }

    fn only_file(store_dir: &Path) -> PathBuf {
        fs::read_dir(store_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == FILE_EXT))
            .expect("one baseline file")
    }

    #[test]
    fn never_saved_is_first_run() {
        let (_b, l, r, store) = setup("none");
        assert_eq!(
            store.load(&pair(&l, &r)).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::None
            }
        );
    }

    #[test]
    fn save_then_reopen_store_restores_baseline() {
        let (b, l, r, store) = setup("restart");
        let p = pair(&l, &r);
        store
            .save(&p, &baseline_for(&p, &["a.txt", "sub/b.txt"]))
            .unwrap();
        // "Restart": store baru di direktori yang sama.
        let reopened = SyncBaselineStore::open(&b.join("store"));
        match reopened.load(&p).unwrap() {
            LoadResult::Ok { baseline } => {
                assert_eq!(baseline.entries.len(), 2);
                assert!(baseline.entries.contains_key("sub/b.txt"));
                assert_eq!(baseline.left_root, p.left_root);
            }
            other => panic!("expected ok, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_file_is_first_run() {
        let (b, l, r, store) = setup("corrupt");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let file = only_file(&b.join("store"));
        fs::write(&file, b"{ not json at all").unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::Corrupt
            }
        );
        // Terpotong di tengah tulis.
        fs::write(&file, b"{\"version\":1,\"key\":\"x\"").unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::Corrupt
            }
        );
    }

    #[test]
    fn unknown_version_is_first_run_and_never_guessed() {
        let (b, l, r, store) = setup("version");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let file = only_file(&b.join("store"));
        let text =
            fs::read_to_string(&file)
                .unwrap()
                .replacen("\"version\":1", "\"version\":99", 1);
        fs::write(&file, text).unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::UnknownVersion
            }
        );
    }

    #[test]
    fn unknown_field_in_file_is_corrupt() {
        let (b, l, r, store) = setup("unknownfield");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let file = only_file(&b.join("store"));
        let text = fs::read_to_string(&file)
            .unwrap()
            .replacen("{", "{\"extra\":1,", 1);
        fs::write(&file, text).unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::Corrupt
            }
        );
    }

    #[test]
    fn file_with_a_different_key_is_a_mismatch() {
        let (b, l, r, store) = setup("mismatch");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let file = only_file(&b.join("store"));
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        v["key"] = serde_json::Value::String("L=/other\nR=/pair".into());
        fs::write(&file, serde_json::to_vec(&v).unwrap()).unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::Mismatch
            }
        );
    }

    #[test]
    fn swapped_panels_are_a_different_pair() {
        let (_b, l, r, store) = setup("swap");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let swapped = pair(&r, &l);
        assert_eq!(
            store.load(&swapped).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::None
            }
        );
    }

    #[test]
    fn two_pairs_do_not_affect_each_other() {
        let (b, l, r, store) = setup("twopairs");
        let l2 = b.join("left2");
        let r2 = b.join("right2");
        fs::create_dir_all(&l2).unwrap();
        fs::create_dir_all(&r2).unwrap();
        let p1 = pair(&l, &r);
        let p2 = pair(&l2, &r2);
        store.save(&p1, &baseline_for(&p1, &["one.txt"])).unwrap();
        store
            .save(&p2, &baseline_for(&p2, &["two.txt", "two2.txt"]))
            .unwrap();
        let LoadResult::Ok { baseline: b1 } = store.load(&p1).unwrap() else {
            panic!("p1")
        };
        let LoadResult::Ok { baseline: b2 } = store.load(&p2).unwrap() else {
            panic!("p2")
        };
        assert_eq!(b1.entries.keys().collect::<Vec<_>>(), vec!["one.txt"]);
        assert_eq!(b2.entries.len(), 2);
        // Menghapus p1 tidak menyentuh p2; menimpa p2 tidak menyentuh p1.
        store.clear(&p1).unwrap();
        assert!(matches!(
            store.load(&p1).unwrap(),
            LoadResult::FirstRun { .. }
        ));
        assert!(matches!(store.load(&p2).unwrap(), LoadResult::Ok { .. }));
    }

    #[test]
    fn moved_or_missing_root_is_first_run() {
        let (b, l, r, store) = setup("moved");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let moved = b.join("left-moved");
        fs::rename(&l, &moved).unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::RootsUnavailable
            }
        );
        // Tempat baru dengan nama lama dibuat lagi: tetap identitas yang sama (path sama), tetapi
        // pemindahan ke path lain = pasangan lain => first-run.
        let other = pair(&moved, &r);
        assert_eq!(
            store.load(&other).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::None
            }
        );
    }

    #[test]
    fn same_and_nested_roots_are_rejected_not_first_run() {
        let (_b, l, r, store) = setup("reject");
        let p = pair(&l, &l);
        assert_eq!(store.load(&p).unwrap_err().code, "invalid_roots");
        let nested = l.join("inner");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            store.load(&pair(&l, &nested)).unwrap_err().code,
            "invalid_roots"
        );
        assert_eq!(
            store.load(&pair(&nested, &l)).unwrap_err().code,
            "invalid_roots"
        );
        let bad = pair(&l, &nested);
        assert_eq!(
            store
                .save(&bad, &baseline_for(&bad, &["a"]))
                .unwrap_err()
                .code,
            "invalid_roots"
        );
        assert_eq!(store.clear(&bad).unwrap_err().code, "invalid_roots");
        let _ = r;
    }

    #[test]
    fn dot_segments_resolve_to_the_same_pair() {
        let (_b, l, r, store) = setup("dots");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let aliased = RootPair {
            left_root: s(&l.join("..").join("left")),
            right_root: s(&r),
        };
        let LoadResult::Ok { baseline } = store.load(&aliased).unwrap() else {
            panic!("alias path of the same pair must find the baseline")
        };
        // Roots yang dikembalikan = string yang diminta (agar tepercaya di sisi TS).
        assert_eq!(baseline.left_root, aliased.left_root);
    }

    #[test]
    fn save_rejects_baseline_of_another_pair_or_version() {
        let (b, l, r, store) = setup("reject-shape");
        let p = pair(&l, &r);
        let other = pair(&r, &l);
        assert_eq!(
            store
                .save(&p, &baseline_for(&other, &["a"]))
                .unwrap_err()
                .code,
            "invalid_baseline"
        );
        let mut bad = baseline_for(&p, &["a"]);
        bad.version = 2;
        assert_eq!(store.save(&p, &bad).unwrap_err().code, "invalid_baseline");
        let mut empty_name = baseline_for(&p, &["a"]);
        empty_name.entries.insert(
            String::new(),
            EntryDto {
                left: facts(1, 1),
                right: facts(1, 1),
            },
        );
        assert_eq!(
            store.save(&p, &empty_name).unwrap_err().code,
            "invalid_baseline"
        );
        // Tidak ada file yang tertulis.
        assert_eq!(count_pairs(&b.join("store")), 0);
    }

    #[test]
    fn too_many_entries_is_refused_and_old_baseline_survives() {
        let (_b, l, r, store) = setup("limit");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["keep.txt"])).unwrap();
        let mut big = baseline_for(&p, &[]);
        for i in 0..=MAX_ENTRIES {
            big.entries.insert(
                format!("f{i}"),
                EntryDto {
                    left: facts(1, 1),
                    right: facts(1, 1),
                },
            );
        }
        assert_eq!(store.save(&p, &big).unwrap_err().code, "limit_exceeded");
        let LoadResult::Ok { baseline } = store.load(&p).unwrap() else {
            panic!("old must survive")
        };
        assert!(baseline.entries.contains_key("keep.txt"));
    }

    #[test]
    fn oversized_file_on_disk_is_first_run_too_large() {
        let (b, l, r, store) = setup("toolarge");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["a.txt"])).unwrap();
        let file = only_file(&b.join("store"));
        let f = OpenOptions::new().write(true).open(&file).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::TooLarge
            }
        );
    }

    #[test]
    fn atomic_write_leaves_no_temp_and_keeps_old_on_failure() {
        let (b, l, r, store) = setup("atomic");
        let p = pair(&l, &r);
        store.save(&p, &baseline_for(&p, &["v1.txt"])).unwrap();
        store.save(&p, &baseline_for(&p, &["v2.txt"])).unwrap();
        let names: Vec<String> = fs::read_dir(b.join("store"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "no temp leftovers: {names:?}");
        assert!(!names[0].starts_with(".tmp"));
        // Penulisan sementara yang gagal (target adalah direktori) tidak merusak baseline lain
        // dan membersihkan file sementara.
        let dir = b.join("store");
        let bad_target = dir.join("blocked");
        fs::create_dir_all(&bad_target).unwrap();
        assert!(write_atomic(&dir, &bad_target, b"x").is_err());
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        let LoadResult::Ok { baseline } = store.load(&p).unwrap() else {
            panic!()
        };
        assert!(baseline.entries.contains_key("v2.txt"));
    }

    #[test]
    fn existing_temp_name_is_never_clobbered() {
        // create_new: file sementara yang sudah ada milik pihak lain tidak ditimpa.
        let base = temp("tmpclobber");
        let dir = base.join("d");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("t.json");
        // Urutan berikutnya tidak pasti (tes lain paralel): isi jendela nama ke depan.
        let next = TMP_SEQ.load(Ordering::SeqCst);
        let pid = std::process::id();
        let victims: Vec<PathBuf> = (next..next + 32)
            .map(|n| dir.join(format!(".tmp-{pid}-{n}-t.json")))
            .collect();
        for v in &victims {
            fs::write(v, b"precious").unwrap();
        }
        let res = write_atomic(&dir, &target, b"new");
        // Berhasil memakai nama lain, atau gagal jujur karena tak ada nama bebas; file
        // pihak lain tidak pernah ditimpa maupun dihapus.
        if let Err(e) = &res {
            assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        }
        for v in &victims {
            assert_eq!(fs::read(v).unwrap(), b"precious");
        }
    }

    #[test]
    fn unavailable_storage_is_honest() {
        let (_b, l, r, _store) = setup("unavail");
        let store = SyncBaselineStore::unavailable();
        let p = pair(&l, &r);
        assert_eq!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::None
            }
        );
        assert_eq!(
            store.save(&p, &baseline_for(&p, &["a"])).unwrap_err().code,
            "unavailable"
        );
    }

    #[test]
    fn error_messages_never_contain_paths() {
        let (b, l, r, store) = setup("nopath");
        let p = pair(&l, &l);
        let e = store.load(&p).unwrap_err();
        let shown = format!("{} {}", e.code, e.message);
        assert!(!shown.contains(&s(&b)));
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("ferrum-sbl"));
        let _ = r;
    }

    #[test]
    fn dto_rejects_unknown_fields() {
        let bad = r#"{"leftRoot":"a","rightRoot":"b","extra":1}"#;
        assert!(serde_json::from_str::<RootPair>(bad).is_err());
        let bad2 = r#"{"version":1,"leftRoot":"a","rightRoot":"b","entries":{"x":{"left":{"size":1,"modifiedMs":1,"z":0},"right":{"size":1,"modifiedMs":1}}}}"#;
        assert!(serde_json::from_str::<BaselineDto>(bad2).is_err());
    }

    #[test]
    fn clear_is_idempotent_and_returns_to_first_run() {
        let (_b, l, r, store) = setup("clear");
        let p = pair(&l, &r);
        store.clear(&p).unwrap();
        store.save(&p, &baseline_for(&p, &["a"])).unwrap();
        store.clear(&p).unwrap();
        store.clear(&p).unwrap();
        assert!(matches!(
            store.load(&p).unwrap(),
            LoadResult::FirstRun { .. }
        ));
    }

    // ── P0.4: baseline writes need a lease (design §3.4, §5.7) ──

    fn lease_rig(
        base: &Path,
    ) -> (
        Arc<AutomationStore>,
        crate::automation::lease::LeaseRegistry,
        LeaseEnv,
    ) {
        let store = Arc::new(AutomationStore::open(&base.join("auto"), "0.2.0"));
        let env = LeaseEnv::system();
        let registry = crate::automation::lease::LeaseRegistry::new(env.clone());
        (store, registry, env)
    }

    fn real_canonical(p: &str) -> Result<String, crate::sync_engine::roots::RootError> {
        crate::sync_engine::roots::canonical(p)
    }

    fn lease_on(
        store: &Arc<AutomationStore>,
        registry: &crate::automation::lease::LeaseRegistry,
        l: &Path,
        r: &Path,
    ) -> String {
        let roots = [
            real_canonical(&s(l)).unwrap(),
            real_canonical(&s(r)).unwrap(),
        ];
        match registry.acquire_manual(store, &roots).unwrap() {
            crate::automation::lease::ManualAcquire::Acquired { lease_id } => lease_id,
            _ => panic!("lease acquired"),
        }
    }

    /// Design §5.7: without a lease nothing is saved (`lease_required`); with a manual lease on
    /// the pair it is saved; after the release it is refused again.
    #[test]
    fn baseline_save_requires_a_lease() {
        let (b, l, r, baselines) = setup("lease-save");
        let (auto, registry, env) = lease_rig(&b);
        let p = pair(&l, &r);
        let bl = baseline_for(&p, &["a"]);
        let save = |p: &RootPair, bl: &BaselineDto| {
            save_with_lease(&baselines, &auto, &env, &real_canonical, p, bl)
        };

        let refused = save(&p, &bl).unwrap_err();
        assert_eq!(refused.code, "lease_required");
        assert!(matches!(
            baselines.load(&p).unwrap(),
            LoadResult::FirstRun { .. }
        ));

        let id = lease_on(&auto, &registry, &l, &r);
        save(&p, &bl).unwrap();
        assert!(matches!(baselines.load(&p).unwrap(), LoadResult::Ok { .. }));
        // Alias spelling (case) of the same folders is covered by the same lease.
        let upper = pair(
            Path::new(&s(&l).to_uppercase()),
            Path::new(&s(&r).to_uppercase()),
        );
        let mut bl_upper = bl.clone();
        bl_upper.left_root = upper.left_root.clone();
        bl_upper.right_root = upper.right_root.clone();
        save(&upper, &bl_upper).unwrap();

        assert!(registry.release(&id));
        assert_eq!(save(&p, &bl).unwrap_err().code, "lease_required");
    }

    /// A lease on other folders, or only on one of the two roots, does not cover the pair.
    #[test]
    fn baseline_save_needs_the_lease_to_cover_both_roots() {
        let (b, l, r, baselines) = setup("lease-cover");
        let other = b.join("other");
        fs::create_dir_all(&other).unwrap();
        let (auto, registry, env) = lease_rig(&b);
        let p = pair(&l, &r);
        let bl = baseline_for(&p, &["a"]);
        let save = || save_with_lease(&baselines, &auto, &env, &real_canonical, &p, &bl);

        lease_on(&auto, &registry, &other, &b.join("elsewhere-missing"));
        assert_eq!(save().unwrap_err().code, "lease_required");
        // Only the left root leased (right overlaps nothing leased): still refused.
        let roots = [real_canonical(&s(&l)).unwrap()];
        assert!(matches!(
            registry.acquire_manual(&auto, &roots).unwrap(),
            crate::automation::lease::ManualAcquire::Acquired { .. }
        ));
        assert_eq!(save().unwrap_err().code, "lease_required");
    }

    /// A lease held by another process (another holder identity) never lets this one save.
    #[test]
    fn baseline_save_ignores_a_lease_of_another_process() {
        let (b, l, r, baselines) = setup("lease-foreign");
        let (auto, registry, env) = lease_rig(&b);
        let p = pair(&l, &r);
        let bl = baseline_for(&p, &["a"]);
        lease_on(&auto, &registry, &l, &r);
        let stranger = LeaseEnv {
            holder: crate::automation::store::leases::Holder {
                pid: env.holder.pid + 1,
                created: env.holder.created + 1,
            },
            ..env.clone()
        };
        let err =
            save_with_lease(&baselines, &auto, &stranger, &real_canonical, &p, &bl).unwrap_err();
        assert_eq!(err.code, "lease_required");
    }

    /// An unusable automation store can hold no lease: fail closed.
    #[test]
    fn baseline_save_without_a_store_is_refused() {
        let (_b, l, r, baselines) = setup("lease-nostore");
        let auto = AutomationStore::unavailable();
        let p = pair(&l, &r);
        let bl = baseline_for(&p, &["a"]);
        let err = save_with_lease(
            &baselines,
            &auto,
            &LeaseEnv::system(),
            &real_canonical,
            &p,
            &bl,
        )
        .unwrap_err();
        assert_eq!(err.code, "lease_required");
    }

    /// The clear command follows the same lease rule as save.
    #[test]
    fn baseline_clear_requires_a_lease() {
        let (b, l, r, baselines) = setup("lease-clear");
        let (auto, registry, env) = lease_rig(&b);
        let p = pair(&l, &r);
        baselines.save(&p, &baseline_for(&p, &["a"])).unwrap();
        let clear = || clear_with_lease(&baselines, &auto, &env, &real_canonical, &p);
        assert_eq!(clear().unwrap_err().code, "lease_required");
        assert!(matches!(baselines.load(&p).unwrap(), LoadResult::Ok { .. }));
        let id = lease_on(&auto, &registry, &l, &r);
        clear().unwrap();
        assert!(matches!(
            baselines.load(&p).unwrap(),
            LoadResult::FirstRun { .. }
        ));
        assert!(registry.release(&id));
        assert_eq!(clear().unwrap_err().code, "lease_required");
    }

    fn guard_on(
        store: &Arc<AutomationStore>,
        env: &LeaseEnv,
        l: &Path,
        r: &Path,
    ) -> crate::automation::lease::LeaseGuard {
        let roots = [
            real_canonical(&s(l)).unwrap(),
            real_canonical(&s(r)).unwrap(),
        ];
        match crate::automation::lease::LeaseGuard::acquire(
            store,
            env,
            crate::automation::store::leases::HolderKind::Manual,
            Some(1),
            &roots,
            None,
        )
        .unwrap()
        {
            crate::automation::lease::GuardOutcome::Acquired(g) => g,
            _ => panic!("lease acquired"),
        }
    }

    /// Engine path (design §5.7): a run saves through the guard of ITS lease; a
    /// guard for other folders, or one whose lease row is gone, saves nothing.
    #[test]
    fn baseline_save_with_guard_needs_the_guards_own_live_lease() {
        let (b, l, r, baselines) = setup("guard-save");
        let other = b.join("other");
        fs::create_dir_all(&other).unwrap();
        let (auto, _registry, env) = lease_rig(&b);
        let p = pair(&l, &r);
        let bl = baseline_for(&p, &["a"]);
        let save = |g: &crate::automation::lease::LeaseGuard| {
            save_with_guard(&baselines, g, &real_canonical, &p, &bl)
        };

        // A guard over unrelated folders does not cover the pair.
        let wrong = guard_on(&auto, &env, &other, &b.join("elsewhere-missing"));
        assert_eq!(save(&wrong).unwrap_err().code, "lease_required");
        assert!(matches!(
            baselines.load(&p).unwrap(),
            LoadResult::FirstRun { .. }
        ));
        drop(wrong);

        // The guard of the pair's own lease saves.
        let guard = guard_on(&auto, &env, &l, &r);
        save(&guard).unwrap();
        assert!(matches!(baselines.load(&p).unwrap(), LoadResult::Ok { .. }));
        baselines.clear(&p).unwrap();

        // The lease row disappears (reclaimed): the guard no longer covers anything.
        auto.with_write(|c| crate::automation::store::leases::release(c, guard.id(), env.holder))
            .unwrap();
        assert_eq!(save(&guard).unwrap_err().code, "lease_required");
        assert!(matches!(
            baselines.load(&p).unwrap(),
            LoadResult::FirstRun { .. }
        ));
    }

    /// Another lease of the SAME process does not satisfy a guard: the guard's own row counts.
    #[test]
    fn baseline_save_with_guard_is_not_satisfied_by_a_sibling_lease() {
        let (b, l, r, baselines) = setup("guard-sibling");
        let (auto, registry, env) = lease_rig(&b);
        let p = pair(&l, &r);
        let bl = baseline_for(&p, &["a"]);
        let guard = guard_on(&auto, &env, &l, &r);
        auto.with_write(|c| crate::automation::store::leases::release(c, guard.id(), env.holder))
            .unwrap();
        // A manual lease of the same process now covers the folders, the dead guard still must not.
        lease_on(&auto, &registry, &l, &r);
        assert_eq!(
            save_with_guard(&baselines, &guard, &real_canonical, &p, &bl)
                .unwrap_err()
                .code,
            "lease_required"
        );
    }
}
