// AI-06 / FER-196/197: ledger idempotensi + pemetaan receipt untuk executor AI.
//
// Aturan (spesifikasi bagian 8):
// - Kunci idempotensi = hash(execution_id, plan_digest), dibuat executor tepercaya.
// - `reserve` MENCATAT niat (journal, di-flush) SEBELUM enqueue. Jika proses mati antara
//   enqueue dan `record_enqueued`, saat restart entri itu menjadi `UnknownOutcome`:
//   TIDAK pernah di-replay otomatis; harus direkonsiliasi (`resolve_unknown`).
// - Satu plan = paling banyak satu eksekusi aktif/tidak-pasti: execution ID baru untuk
//   plan yang sama (provider retry, reconnect) mendapat `Existing`, bukan job kedua.
// - Cancel/progress hanya boleh menyentuh job ID milik execution itu (`owns_job`).
// - Status akhir dihitung dari status job engine (`summarize`), bukan dari klaim lain.
//
// Mandiri (hanya std) agar teruji lewat fixture crate. Registrasi di `ai/mod.rs` dan
// command Tauri = handoff integrator.
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecState {
    /// Niat tercatat; enqueue belum dikonfirmasi (hidup) atau tidak pasti (setelah restart).
    Reserved,
    Enqueued,
    /// Hasil enqueue tidak diketahui; butuh rekonsiliasi, jangan replay.
    UnknownOutcome,
    /// Terkonfirmasi tidak ada operasi yang dimulai; boleh disusun ulang.
    NotStarted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub execution_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub state: ExecState,
    pub job_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// Boleh enqueue tepat satu kali.
    Fresh(String),
    /// Sudah ada; JANGAN enqueue lagi. Kembalikan/rekonsiliasi entri ini.
    Existing(Entry),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    InvalidReference,
    UnknownKey,
    BadState,
    Journal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Job ditemukan di queue (milik execution ini).
    Found(Vec<String>),
    /// Terkonfirmasi tidak ada job yang dibuat.
    NotFound,
}

fn valid_ref(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Kunci idempotensi 128-bit non-kriptografis, terikat execution ID + plan digest.
pub fn idempotency_key(execution_id: &str, plan_digest: &str) -> String {
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x84222325cbf29ce4;
    for b in execution_id
        .bytes()
        .chain(std::iter::once(0x1f))
        .chain(plan_digest.bytes())
    {
        h1 = (h1 ^ u64::from(b)).wrapping_mul(0x100000001b3);
        h2 = (h2 ^ u64::from(b))
            .wrapping_mul(0x100000001b3)
            .rotate_left(7);
    }
    format!("idem_{h1:016x}{h2:016x}")
}

struct Inner {
    entries: HashMap<String, Entry>,
}

pub struct ExecutionLedger {
    inner: Mutex<Inner>,
    journal: Option<PathBuf>,
}

impl ExecutionLedger {
    /// Ledger hanya-memori (tes / tanpa persistensi).
    pub fn in_memory() -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
            }),
            journal: None,
        }
    }

    /// Buka dengan journal durable. Replay: entri `Reserved` tanpa konfirmasi enqueue menjadi
    /// `UnknownOutcome` (batas crash antara enqueue dan receipt).
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        let mut entries: HashMap<String, Entry> = HashMap::new();
        if path.exists() {
            let text = fs::read_to_string(path).map_err(|e| LedgerError::Journal(e.to_string()))?;
            for line in text.lines() {
                let f: Vec<&str> = line.split('\t').collect();
                // Baris rusak/terpotong (crash saat menulis) diabaikan.
                match (f.first().copied(), f.len()) {
                    (Some("R"), 5)
                        if valid_ref(f[1])
                            && valid_ref(f[2])
                            && valid_ref(f[3])
                            && valid_ref(f[4]) =>
                    {
                        entries.insert(
                            f[1].to_string(),
                            Entry {
                                key: f[1].into(),
                                execution_id: f[2].into(),
                                plan_id: f[3].into(),
                                plan_digest: f[4].into(),
                                state: ExecState::Reserved,
                                job_ids: vec![],
                            },
                        );
                    }
                    (Some("E"), 3) => {
                        if let Some(e) = entries.get_mut(f[1]) {
                            let jobs: Vec<String> = f[2]
                                .split(',')
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect();
                            if jobs.iter().all(|j| valid_ref(j)) {
                                e.state = ExecState::Enqueued;
                                e.job_ids = jobs;
                            }
                        }
                    }
                    (Some("U"), 2) => {
                        if let Some(e) = entries.get_mut(f[1])
                            && e.state == ExecState::Reserved
                        {
                            e.state = ExecState::UnknownOutcome;
                        }
                    }
                    (Some("N"), 2) => {
                        if let Some(e) = entries.get_mut(f[1]) {
                            e.state = ExecState::NotStarted;
                        }
                    }
                    _ => {}
                }
            }
            for e in entries.values_mut() {
                if e.state == ExecState::Reserved {
                    e.state = ExecState::UnknownOutcome;
                }
            }
        }
        Ok(Self {
            inner: Mutex::new(Inner { entries }),
            journal: Some(path.to_path_buf()),
        })
    }

    fn append(&self, line: &str) -> Result<(), LedgerError> {
        if let Some(path) = &self.journal {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| LedgerError::Journal(e.to_string()))?;
            writeln!(f, "{line}").map_err(|e| LedgerError::Journal(e.to_string()))?;
            f.sync_all()
                .map_err(|e| LedgerError::Journal(e.to_string()))?;
        }
        Ok(())
    }

    /// Cadangkan hak enqueue. `Fresh` hanya sekali per plan (selama tidak `NotStarted`).
    pub fn reserve(
        &self,
        execution_id: &str,
        plan_id: &str,
        plan_digest: &str,
    ) -> Result<Reservation, LedgerError> {
        if !valid_ref(execution_id) || !valid_ref(plan_id) || !valid_ref(plan_digest) {
            return Err(LedgerError::InvalidReference);
        }
        let key = idempotency_key(execution_id, plan_digest);
        let mut inner = self.inner.lock().unwrap();
        if let Some(e) = inner.entries.get(&key)
            && e.state != ExecState::NotStarted
        {
            return Ok(Reservation::Existing(e.clone()));
        }
        if let Some(e) = inner
            .entries
            .values()
            .find(|e| e.plan_id == plan_id && e.state != ExecState::NotStarted)
        {
            return Ok(Reservation::Existing(e.clone()));
        }
        // Tulis-dulu: tanpa journal tercatat, tidak ada hak enqueue.
        self.append(&format!(
            "R\t{key}\t{execution_id}\t{plan_id}\t{plan_digest}"
        ))?;
        inner.entries.insert(
            key.clone(),
            Entry {
                key: key.clone(),
                execution_id: execution_id.into(),
                plan_id: plan_id.into(),
                plan_digest: plan_digest.into(),
                state: ExecState::Reserved,
                job_ids: vec![],
            },
        );
        Ok(Reservation::Fresh(key))
    }

    /// Simpan pemetaan key -> job ID setelah enqueue berhasil.
    pub fn record_enqueued(&self, key: &str, job_ids: &[String]) -> Result<(), LedgerError> {
        if !job_ids.iter().all(|j| valid_ref(j)) {
            return Err(LedgerError::InvalidReference);
        }
        let mut inner = self.inner.lock().unwrap();
        let e = inner.entries.get_mut(key).ok_or(LedgerError::UnknownKey)?;
        if !matches!(e.state, ExecState::Reserved | ExecState::UnknownOutcome) {
            return Err(LedgerError::BadState);
        }
        let line = format!("E\t{key}\t{}", job_ids.join(","));
        self.append(&line)?;
        e.state = ExecState::Enqueued;
        e.job_ids = job_ids.to_vec();
        Ok(())
    }

    /// Eksekusi gagal SEBELUM menulis apa pun (stale/validasi): hak boleh dicoba lagi lewat plan baru.
    pub fn record_not_started(&self, key: &str) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().unwrap();
        let e = inner.entries.get_mut(key).ok_or(LedgerError::UnknownKey)?;
        if e.state != ExecState::Reserved && e.state != ExecState::UnknownOutcome {
            return Err(LedgerError::BadState);
        }
        self.append(&format!("N\t{key}"))?;
        e.state = ExecState::NotStarted;
        Ok(())
    }

    /// Enqueue melempar error / hasil tak jelas: tandai tidak pasti; jangan replay.
    pub fn mark_unknown(&self, key: &str) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().unwrap();
        let e = inner.entries.get_mut(key).ok_or(LedgerError::UnknownKey)?;
        if e.state == ExecState::Reserved {
            self.append(&format!("U\t{key}"))?;
            e.state = ExecState::UnknownOutcome;
        }
        Ok(())
    }

    /// Rekonsiliasi eksplisit entri tak pasti setelah memeriksa queue/hasil.
    pub fn resolve_unknown(&self, key: &str, resolution: Resolution) -> Result<Entry, LedgerError> {
        {
            let inner = self.inner.lock().unwrap();
            let e = inner.entries.get(key).ok_or(LedgerError::UnknownKey)?;
            if e.state != ExecState::UnknownOutcome {
                return Err(LedgerError::BadState);
            }
        }
        match resolution {
            Resolution::Found(jobs) => self.record_enqueued(key, &jobs)?,
            Resolution::NotFound => self.record_not_started(key)?,
        }
        Ok(self.inner.lock().unwrap().entries[key].clone())
    }

    pub fn lookup(&self, key: &str) -> Option<Entry> {
        self.inner.lock().unwrap().entries.get(key).cloned()
    }

    pub fn by_execution(&self, execution_id: &str) -> Option<Entry> {
        self.inner
            .lock()
            .unwrap()
            .entries
            .values()
            .find(|e| e.execution_id == execution_id)
            .cloned()
    }

    /// Entri yang butuh rekonsiliasi (dipanggil saat startup).
    pub fn unresolved(&self) -> Vec<Entry> {
        let mut v: Vec<Entry> = self
            .inner
            .lock()
            .unwrap()
            .entries
            .values()
            .filter(|e| e.state == ExecState::UnknownOutcome)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.key.cmp(&b.key));
        v
    }

    /// Cancel/progress hanya untuk job milik execution tersebut.
    pub fn owns_job(&self, execution_id: &str, job_id: &str) -> bool {
        self.by_execution(execution_id)
            .map(|e| e.state == ExecState::Enqueued && e.job_ids.iter().any(|j| j == job_id))
            .unwrap_or(false)
    }

    /// Job yang boleh dibatalkan untuk execution ini (kosong bila bukan miliknya / belum ada).
    pub fn cancel_targets(&self, execution_id: &str) -> Vec<String> {
        self.by_execution(execution_id)
            .filter(|e| e.state == ExecState::Enqueued)
            .map(|e| e.job_ids)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobStatus {
    pub state: JobState,
    pub processed_files: u64,
    pub failure_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Queued,
    Running,
    Completed,
    /// Sebagian selesai (job gagal/dibatalkan setelah ada file tersalin). Tanpa rollback.
    Partial,
    Failed,
    Cancelled,
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub outcome: RunOutcome,
    pub completed_files: u64,
    pub failed: u64,
}

/// Status run dari status job engine milik execution ini. Job yang tidak dikenal queue
/// tidak pernah dihitung sukses; `UnknownOutcome` bila ada entri tak pasti.
pub fn summarize(entry: &Entry, lookup: &dyn Fn(&str) -> Option<JobStatus>) -> Summary {
    if matches!(entry.state, ExecState::UnknownOutcome | ExecState::Reserved) {
        return Summary {
            outcome: RunOutcome::UnknownOutcome,
            completed_files: 0,
            failed: 0,
        };
    }
    if entry.state == ExecState::NotStarted {
        return Summary {
            outcome: RunOutcome::Failed,
            completed_files: 0,
            failed: 0,
        };
    }
    let mut files = 0u64;
    let mut failed = 0u64;
    let (mut queued, mut running, mut completed, mut bad, mut cancelled, mut unknown) =
        (0, 0, 0, 0, 0, 0);
    for id in &entry.job_ids {
        match lookup(id) {
            None => unknown += 1,
            Some(s) => {
                files += s.processed_files;
                failed += s.failure_count;
                match s.state {
                    JobState::Queued => queued += 1,
                    JobState::Running => running += 1,
                    JobState::Completed => completed += 1,
                    JobState::Failed => bad += 1,
                    JobState::Cancelled => cancelled += 1,
                }
            }
        }
    }
    let make = |outcome| Summary {
        outcome,
        completed_files: files,
        failed,
    };
    if unknown > 0 {
        return make(RunOutcome::UnknownOutcome);
    }
    if running > 0 || (queued > 0 && queued < entry.job_ids.len()) {
        return make(RunOutcome::Running);
    }
    if queued > 0 {
        return make(RunOutcome::Queued);
    }
    if entry.job_ids.is_empty() {
        // Tanpa job queue (mis. Pack in-process): hanya hasil service yang menentukan.
        return make(RunOutcome::UnknownOutcome);
    }
    if bad == 0 && cancelled == 0 {
        return make(RunOutcome::Completed);
    }
    if files > 0 || completed > 0 {
        return make(RunOutcome::Partial);
    }
    make(if bad > 0 {
        RunOutcome::Failed
    } else {
        RunOutcome::Cancelled
    })
}
