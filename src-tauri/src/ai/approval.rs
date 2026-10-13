// AI-05 / FER-194: revalidasi backend sebelum menulis + laporan stale plan.
//
// Frontend memegang tiket approval, tetapi validasi frontend tidak cukup: sebelum
// executor menulis apa pun, backend memeriksa lagi bahwa (1) approval yang diklaim
// memang terdaftar dan belum kedaluwarsa/terpakai, (2) digest/root/item-scope/revision
// cocok dengan yang disetujui, dan (3) file sumber serta direktori tujuan masih seperti
// saat preview (ada, ukuran + mtime sama, tujuan ada/bisa ditulis, nama output belum
// dipakai). Gagal = `StaleReport` (daftar alasan) dan NOL penulisan.
//
// Modul ini sengaja mandiri (hanya std) agar bisa diuji lewat fixture crate. Registrasi
// ke `ai/mod.rs` dan command Tauri adalah handoff integrator.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// Sidik jari satu sumber saat preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: u128,
}

/// Ambil sidik jari; gagal bila sumber tidak bisa dibaca.
pub fn fingerprint_source(path: &Path) -> std::io::Result<SourceFingerprint> {
    let meta = fs::symlink_metadata(path)?;
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Ok(SourceFingerprint {
        path: path.to_path_buf(),
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified_ms,
    })
}

pub fn fingerprint_sources(paths: &[PathBuf]) -> std::io::Result<Vec<SourceFingerprint>> {
    paths.iter().map(|p| fingerprint_source(p)).collect()
}

/// Digest non-kriptografis (2 x FNV-1a 64) untuk mengikat item scope.
pub fn scope_digest(sources: &[SourceFingerprint]) -> String {
    scope_digest_for(sources, None, &[])
}

/// Item scope untuk operasi dua-root (sync, FER-168): selain sidik jari sumber/target, digest juga
/// mengikat root kedua dan daftar path yang HARUS tetap tidak ada. Tanpa root kedua dan tanpa path
/// absen hasilnya identik dengan `scope_digest` (perilaku ZIP tidak berubah).
pub fn scope_digest_for(
    sources: &[SourceFingerprint],
    second_root: Option<&Path>,
    absent: &[PathBuf],
) -> String {
    let mut parts: Vec<String> = sources
        .iter()
        .map(|s| {
            format!(
                "{}|{}|{}|{}",
                s.path.to_string_lossy(),
                s.is_dir,
                s.size,
                s.modified_ms
            )
        })
        .collect();
    if let Some(root) = second_root {
        parts.push(format!("R2|{}", root.to_string_lossy()));
    }
    parts.extend(absent.iter().map(|p| format!("A|{}", p.to_string_lossy())));
    parts.sort();
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x84222325cbf29ce4;
    for part in &parts {
        for b in part.bytes().chain(std::iter::once(0x1f)) {
            h1 = (h1 ^ u64::from(b)).wrapping_mul(0x100000001b3);
            h2 = (h2 ^ u64::from(b))
                .wrapping_mul(0x100000001b3)
                .rotate_left(5);
        }
    }
    format!("{h1:016x}{h2:016x}")
}

/// Yang disetujui pengguna (didaftarkan saat approval, bukan dari klaim model).
#[derive(Debug, Clone)]
pub struct ApprovalRecord {
    pub handle_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub revision: u32,
    pub root: PathBuf,
    pub sources: Vec<SourceFingerprint>,
    /// Nama output baru (archive); `None` untuk operasi tanpa satu output.
    pub output_name: Option<String>,
    pub expires_at_ms: u64,
    /// Sync (FER-168): root kanan; `root` adalah root kiri. `None` untuk operasi satu-root (ZIP).
    pub second_root: Option<PathBuf>,
    /// Sync: path target salinan BARU yang harus tetap tidak ada sampai eksekusi.
    pub absent: Vec<PathBuf>,
}

/// Klaim yang dibawa permintaan eksekusi; dibandingkan dengan catatan terdaftar.
#[derive(Debug, Clone)]
pub struct ExecuteClaim {
    pub handle_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub revision: u32,
    pub root: PathBuf,
    /// Sync: root kanan yang diklaim; harus sama dengan yang didaftarkan.
    pub second_root: Option<PathBuf>,
    pub item_scope: String,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    UnknownApproval,
    AlreadyConsumed,
    Expired,
    PlanMismatch,
    DigestMismatch,
    RevisionMismatch,
    RootMismatch,
    ScopeMismatch,
    SourceMissing(PathBuf),
    SourceChanged(PathBuf),
    TargetMissing,
    TargetNotWritable,
    OutputExists(String),
    /// Sync: path yang saat preview belum ada kini ada (berkas baru akan tertimpa).
    TargetAppeared(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleReport {
    pub reasons: Vec<StaleReason>,
}

impl StaleReport {
    fn one(reason: StaleReason) -> Self {
        Self {
            reasons: vec![reason],
        }
    }
    /// Pesan singkat untuk pengguna (Indonesia); tanpa path mentah.
    pub fn summary(&self) -> String {
        format!(
            "Rencana usang ({} alasan); tidak ada yang ditulis. Susun ulang dan setujui lagi.",
            self.reasons.len()
        )
    }
}

/// Bukti bahwa claim lolos; satu kali pakai.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized {
    pub handle_id: String,
    pub plan_id: String,
    pub plan_digest: String,
}

#[derive(Default)]
struct Slot {
    record: Option<ApprovalRecord>,
    consumed: bool,
}

#[derive(Default)]
pub struct ApprovalLedger {
    slots: Mutex<HashMap<String, Slot>>,
}

impl ApprovalLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Daftarkan approval pengguna. Mendaftar ulang handle yang sama mengganti catatan
    /// (revision baru) dan menghidupkan lagi slot-nya.
    pub fn register(&self, mut record: ApprovalRecord) {
        // Compare roots by their canonical form: a directory redirected by a symlink/junction after the
        // preview must not keep authorizing (the plain string would still match).
        record.root = canonical(&record.root);
        record.second_root = record.second_root.as_deref().map(canonical);
        let mut slots = self.slots.lock().unwrap();
        slots.insert(
            record.handle_id.clone(),
            Slot {
                record: Some(record),
                consumed: false,
            },
        );
    }

    /// Plan diedit / dibatalkan: approval lama tak berlaku.
    pub fn revoke(&self, handle_id: &str) {
        self.slots.lock().unwrap().remove(handle_id);
    }

    /// Periksa semua yang disetujui terhadap klaim dan keadaan disk sekarang, lalu
    /// habiskan slot-nya. Tidak menulis apa pun ke disk.
    pub fn authorize(&self, claim: &ExecuteClaim) -> Result<Authorized, StaleReport> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots
            .get_mut(&claim.handle_id)
            .ok_or_else(|| StaleReport::one(StaleReason::UnknownApproval))?;
        if slot.consumed {
            return Err(StaleReport::one(StaleReason::AlreadyConsumed));
        }
        let rec = slot
            .record
            .as_ref()
            .ok_or_else(|| StaleReport::one(StaleReason::UnknownApproval))?;
        let mut reasons = Vec::new();
        if claim.now_ms >= rec.expires_at_ms {
            reasons.push(StaleReason::Expired);
        }
        if claim.plan_id != rec.plan_id {
            reasons.push(StaleReason::PlanMismatch);
        }
        if claim.plan_digest != rec.plan_digest {
            reasons.push(StaleReason::DigestMismatch);
        }
        if claim.revision != rec.revision {
            reasons.push(StaleReason::RevisionMismatch);
        }
        if canonical(&claim.root) != rec.root {
            reasons.push(StaleReason::RootMismatch);
        }
        // A claim must name exactly the roots that were approved (a second root appearing or vanishing
        // is a different operation); a root redirected by a junction no longer canonicalizes to the
        // recorded path.
        match (&rec.second_root, &claim.second_root) {
            (None, None) => {}
            (Some(approved), Some(claimed)) if canonical(claimed) == *approved => {}
            _ => reasons.push(StaleReason::RootMismatch),
        }
        if claim.item_scope
            != scope_digest_for(&rec.sources, rec.second_root.as_deref(), &rec.absent)
        {
            reasons.push(StaleReason::ScopeMismatch);
        }
        reasons.extend(check_sources(&rec.sources));
        reasons.extend(check_target(&rec.root, rec.output_name.as_deref()));
        if let Some(second) = &rec.second_root {
            reasons.extend(check_target(second, None));
        }
        reasons.extend(check_absent(&rec.absent));
        if !reasons.is_empty() {
            return Err(StaleReport { reasons });
        }
        slot.consumed = true;
        Ok(Authorized {
            handle_id: rec.handle_id.clone(),
            plan_id: rec.plan_id.clone(),
            plan_digest: rec.plan_digest.clone(),
        })
    }
}

/// Canonical path, or the path itself when it cannot be resolved (missing targets are reported by
/// `check_target`).
pub fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn check_sources(approved: &[SourceFingerprint]) -> Vec<StaleReason> {
    let mut out = Vec::new();
    for src in approved {
        match fingerprint_source(&src.path) {
            Err(_) => out.push(StaleReason::SourceMissing(src.path.clone())),
            Ok(now) if now != *src => out.push(StaleReason::SourceChanged(src.path.clone())),
            Ok(_) => {}
        }
    }
    out
}

/// Paths that did not exist at preview time must still not exist; `symlink_metadata` so a dangling link
/// also counts as occupied.
fn check_absent(absent: &[PathBuf]) -> Vec<StaleReason> {
    absent
        .iter()
        .filter(|p| fs::symlink_metadata(p).is_ok())
        .map(|p| StaleReason::TargetAppeared(p.clone()))
        .collect()
}

fn check_target(root: &Path, output_name: Option<&str>) -> Vec<StaleReason> {
    let mut out = Vec::new();
    match fs::metadata(root) {
        Ok(meta) if meta.is_dir() => {
            // Bendera read-only saja (probe tulis akan memutasi); izin akhir tetap
            // ditegakkan engine saat menulis.
            if meta.permissions().readonly() {
                out.push(StaleReason::TargetNotWritable);
            }
        }
        _ => out.push(StaleReason::TargetMissing),
    }
    // `symlink_metadata`: a dangling link named like the output still occupies the name (`exists()`
    // follows links and would call it free).
    if let Some(name) = output_name
        && fs::symlink_metadata(root.join(name)).is_ok()
    {
        out.push(StaleReason::OutputExists(name.to_string()));
    }
    out
}
