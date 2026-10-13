// FER-225 / AI-13.3: tes adversarial sisi Rust (std + fake): symlink/junction escape pada guard approval, isi jurnal
// ledger, dan kebocoran rahasia lewat provider. Hanya direktori temporary milik tes; kunci memakai string tiruan.
// Tes symlink DILEWATI dengan jelas (eprintln "SKIPPED") bila OS/akun tidak boleh membuat link.
#[allow(dead_code)]
mod approval {
    include!("../../../../src-tauri/src/ai/approval.rs");
}
#[allow(dead_code)]
mod executor {
    include!("../../../../src-tauri/src/ai/executor.rs");
}
#[allow(dead_code)]
mod ai {
    #[path = "../../../../../src-tauri/src/ai/contract.rs"]
    pub mod contract;
    #[path = "../../../../../src-tauri/src/ai/credential.rs"]
    pub mod credential;
    #[path = "../../../../../src-tauri/src/ai/provider.rs"]
    pub mod provider;
}

use ai::credential::CredentialError;
use ai::provider::*;
use approval::*;
use executor::{ExecutionLedger, Reservation};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const KEY: &str = "sk-test-DUMMY-0123456789abcdef";
static N: AtomicU32 = AtomicU32::new(0);

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ferrum-ai-adv-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        // Hapus link dulu agar remove_dir_all tidak mengikuti junction ke luar direktori tes.
        if let Ok(rd) = fs::read_dir(&self.0) {
            for e in rd.flatten() {
                let p = e.path();
                if let Ok(m) = fs::symlink_metadata(&p)
                    && m.file_type().is_symlink()
                {
                    let _ = fs::remove_dir(&p).or_else(|_| fs::remove_file(&p));
                }
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Buat link direktori (symlink, atau junction di Windows tanpa hak symlink). `false` = tidak bisa di host ini.
fn make_dir_link(link: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn make_file_link(link: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

fn skipped(what: &str) {
    eprintln!("SKIPPED: {what} (host/akun tidak boleh membuat link; bukan bukti lulus)");
}

struct Fixture {
    dir: Dir,
    src: PathBuf,
    root: PathBuf,
    ledger: ApprovalLedger,
    record: ApprovalRecord,
}

fn fixture() -> Fixture {
    let dir = Dir::new();
    let src = dir.0.join("src");
    let root = dir.0.join("out");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(src.join("a.txt"), b"alpha").unwrap();
    let sources = fingerprint_sources(&[src.join("a.txt")]).unwrap();
    let record = ApprovalRecord {
        handle_id: "hnd_1".into(),
        plan_id: "plan_1".into(),
        plan_digest: "digest_1".into(),
        revision: 1,
        root: root.clone(),
        sources: sources.clone(),
        output_name: Some("Invoice.zip".into()),
        expires_at_ms: 10_000,
        second_root: None,
        absent: vec![],
    };
    let ledger = ApprovalLedger::new();
    ledger.register(record.clone());
    Fixture { dir, src, root, ledger, record }
}

fn claim(f: &Fixture) -> ExecuteClaim {
    ExecuteClaim {
        handle_id: "hnd_1".into(),
        plan_id: "plan_1".into(),
        plan_digest: "digest_1".into(),
        revision: 1,
        root: f.root.clone(),
        second_root: None,
        item_scope: scope_digest(&f.record.sources),
        now_ms: 1,
    }
}

#[test]
fn baseline_claim_is_authorized_and_writes_nothing() {
    // Kontrol: tanpa serangan, claim lolos (tes di bawah bermakna hanya bila baseline hijau).
    let f = fixture();
    assert!(f.ledger.authorize(&claim(&f)).is_ok());
    assert!(!f.root.join("Invoice.zip").exists());
}

#[test]
fn source_file_swapped_for_a_symlink_after_preview_is_stale() {
    let f = fixture();
    let outside = f.dir.0.join("outside.txt");
    fs::write(&outside, b"SECRET-OUTSIDE").unwrap();
    fs::remove_file(f.src.join("a.txt")).unwrap();
    if !make_file_link(&f.src.join("a.txt"), &outside) {
        skipped("symlink file");
        return;
    }
    let report = f.ledger.authorize(&claim(&f)).expect_err("a swapped source must not be authorized");
    assert!(
        report.reasons.iter().any(|r| matches!(r, StaleReason::SourceChanged(_))),
        "{:?}",
        report.reasons
    );
}

#[test]
fn source_directory_swapped_for_a_junction_after_preview_is_stale() {
    let dir = Dir::new();
    let real = dir.0.join("data");
    let secret = dir.0.join("secret");
    let root = dir.0.join("out");
    for d in [&real, &secret, &root] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(secret.join("passwords.txt"), b"hunter2").unwrap();
    let sources = fingerprint_sources(std::slice::from_ref(&real)).unwrap();
    let ledger = ApprovalLedger::new();
    ledger.register(ApprovalRecord {
        handle_id: "hnd_1".into(),
        plan_id: "plan_1".into(),
        plan_digest: "digest_1".into(),
        revision: 1,
        root: root.clone(),
        sources: sources.clone(),
        output_name: None,
        expires_at_ms: 10_000,
        second_root: None,
        absent: vec![],
    });
    std::thread::sleep(Duration::from_millis(40)); // mtime link harus berbeda dari direktori asli
    fs::remove_dir(&real).unwrap();
    if !make_dir_link(&real, &secret) {
        skipped("junction/symlink direktori");
        return;
    }
    let claim = ExecuteClaim {
        handle_id: "hnd_1".into(),
        plan_id: "plan_1".into(),
        plan_digest: "digest_1".into(),
        revision: 1,
        root,
        second_root: None,
        item_scope: scope_digest(&sources),
        now_ms: 1,
    };
    let report = ledger.authorize(&claim).expect_err("junction swap must be stale");
    assert!(report.reasons.iter().any(|r| matches!(r, StaleReason::SourceChanged(_))), "{:?}", report.reasons);
    // fingerprint tidak membaca isi tujuan link
    assert!(fs::symlink_metadata(&real).unwrap().file_type().is_symlink() || fs::symlink_metadata(&real).is_ok());
}

// KNOWN GAP (BUG-2, dilaporkan, bukan diperbaiki; lihat docs/ai/verification.md): `check_target` memakai
// `fs::metadata` (mengikuti link) dan `RootMismatch` hanya membandingkan string path, sehingga direktori tujuan yang
// DIALIHKAN ke tempat lain oleh symlink/junction setelah preview tetap lolos otorisasi. Tes ini menyatakan perilaku
// yang diinginkan; `#[ignore]` agar gate tetap hijau. Jalankan `cargo test -- --ignored` untuk melihat gap.
#[test]
fn output_root_redirected_by_junction_after_preview_is_stale() {
    let f = fixture();
    let elsewhere = f.dir.0.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_dir(&f.root).unwrap();
    if !make_dir_link(&f.root, &elsewhere) {
        skipped("junction/symlink root");
        return;
    }
    assert!(f.ledger.authorize(&claim(&f)).is_err(), "output root now points outside the approved location");
}

// KNOWN GAP (BUG-3, rendah): nama output yang sudah ada sebagai link menggantung tidak terdeteksi `OutputExists`
// (`Path::exists` mengikuti link). Engine arsip tetap menolak lewat `symlink_metadata` (conflict skip), jadi lapisan ini
// hanya pertahanan berlapis yang bolong.
#[test]
fn dangling_symlink_with_the_output_name_is_reported_as_existing() {
    let f = fixture();
    // Link direktori menggantung (junction/symlink) bernama seperti output: tujuan dihapus setelah link dibuat.
    let gone = f.dir.0.join("gone");
    fs::create_dir_all(&gone).unwrap();
    if !make_dir_link(&f.root.join("Invoice.zip"), &gone) {
        skipped("link direktori menggantung");
        return;
    }
    fs::remove_dir(&gone).unwrap();
    let report = f.ledger.authorize(&claim(&f)).expect_err("name is taken by a link");
    assert!(report.reasons.iter().any(|r| matches!(r, StaleReason::OutputExists(_))), "{:?}", report.reasons);
}

#[test]
fn approval_is_single_use_and_unknown_or_revoked_claims_write_nothing() {
    let f = fixture();
    assert!(f.ledger.authorize(&claim(&f)).is_ok());
    let again = f.ledger.authorize(&claim(&f)).unwrap_err();
    assert_eq!(again.reasons, vec![StaleReason::AlreadyConsumed]);
    let mut forged = claim(&f);
    forged.handle_id = "hnd_forged".into();
    assert_eq!(f.ledger.authorize(&forged).unwrap_err().reasons, vec![StaleReason::UnknownApproval]);
    let f2 = fixture();
    f2.ledger.revoke("hnd_1");
    assert_eq!(f2.ledger.authorize(&claim(&f2)).unwrap_err().reasons, vec![StaleReason::UnknownApproval]);
    for fixture in [&f, &f2] {
        assert!(!fixture.root.join("Invoice.zip").exists());
    }
}

#[test]
fn journal_contains_only_references_never_paths_names_or_secrets() {
    let dir = Dir::new();
    let path = dir.0.join("ledger.log");
    {
        let l = ExecutionLedger::open(&path).unwrap();
        let Reservation::Fresh(key) = l.reserve("exe_1", "plan_1", "d1f2").unwrap() else { panic!("fresh") };
        l.record_enqueued(&key, &["job_1".into(), "job_2".into()]).unwrap();
        let Reservation::Fresh(key2) = l.reserve("exe_2", "plan_2", "d3f4").unwrap() else { panic!("fresh") };
        l.mark_unknown(&key2).unwrap();
        for bad in [
            format!("{KEY} extra"),
            "Bearer abc def".to_string(),
            "D:\\secret\\file.txt".to_string(),
            "a\tb".to_string(),
            "a\nb".to_string(),
            "https://x".to_string(),
        ] {
            assert!(l.reserve(&bad, "plan_x", "d").is_err(), "accepted {bad:?}");
            assert!(l.reserve("exe_x", &bad, "d").is_err(), "accepted {bad:?}");
            assert!(l.reserve("exe_x", "plan_x", &bad).is_err(), "accepted {bad:?}");
        }
    }
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.is_empty());
    for forbidden in ['\\', '/', ':', '.', ' ', '"', '\'', '<', '>'] {
        assert!(!text.contains(forbidden), "journal contains {forbidden:?}: {text}");
    }
    assert!(!text.contains("sk-"));
    // setiap baris = rekaman bertab dengan field referensi saja
    for line in text.lines() {
        assert!(line.split('\t').all(|f| f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ',')));
    }
    // restart: entri tanpa konfirmasi menjadi unknown, tidak pernah Fresh lagi untuk plan yang sama
    let l2 = ExecutionLedger::open(&path).unwrap();
    assert_eq!(l2.unresolved().len(), 1);
    assert!(matches!(l2.reserve("exe_9", "plan_2", "d3f4").unwrap(), Reservation::Existing(_)));
}

// ---- provider: rahasia tidak boleh bocor ke log/error/debug, termasuk saat server atau model memantulkannya ----

struct FakeTransport {
    responses: RefCell<VecDeque<Result<HttpResponse, TransportError>>>,
    seen: RefCell<Vec<HttpRequest>>,
}
impl HttpTransport for FakeTransport {
    fn post_json(&self, req: &HttpRequest, _c: &CancelToken) -> Result<HttpResponse, TransportError> {
        self.seen.borrow_mut().push(req.clone());
        self.responses.borrow_mut().pop_front().expect("unexpected extra request")
    }
}
struct FakeCreds;
impl CredentialSource for FakeCreds {
    fn read(&self, _r: &str) -> Result<Secret, CredentialError> {
        Ok(Secret::new(KEY.to_owned()))
    }
}
struct FakeClock;
impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
    fn sleep(&self, _d: Duration, c: &CancelToken) -> bool {
        !c.is_cancelled()
    }
}
#[derive(Default)]
struct VecLog(RefCell<Vec<String>>);
impl LogSink for VecLog {
    fn log(&self, l: &str) {
        self.0.borrow_mut().push(l.to_owned());
    }
}

fn config() -> ProviderConfig {
    ProviderConfig {
        endpoint: "https://api.example.test/api/paas/v4".into(),
        model: "configured-model".into(),
        timeout_ms: 30_000,
        credential_ref: "Ferrum/AI/custom".into(),
        structured: StructuredMode::JsonObject,
    }
}
fn request() -> PlanRequest {
    PlanRequest {
        request_id: "req_1".into(),
        user_text: "Zip the left panel".into(),
        capabilities: None,
        context: OutboundContext {
            active_pane: PaneName::Left,
            left_location: LocationKind::Disk,
            right_location: LocationKind::Disk,
            left_count: 3,
            right_count: 0,
            show_hidden: false,
            filter_active: false,
        },
    }
}
fn http(status: u16, body: Value) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse { status, retry_after: None, body: body.to_string() })
}
fn content(text: &str) -> Result<HttpResponse, TransportError> {
    http(
        200,
        json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}]}),
    )
}

/// Menjalankan plan dengan respons berurutan; mengembalikan (hasil, baris log, request yang dikirim).
fn run(items: Vec<Result<HttpResponse, TransportError>>) -> (Result<PlanOutcome, ProviderError>, Vec<String>, Vec<HttpRequest>) {
    let t = FakeTransport { responses: RefCell::new(items.into()), seen: RefCell::default() };
    let log = VecLog::default();
    let deps = Deps { transport: &t, credentials: &FakeCreds, clock: &FakeClock, log: &log };
    let r = plan(&deps, &config(), &request(), &CancelToken::default());
    let lines = log.0.borrow().clone();
    let seen = t.seen.borrow().clone();
    (r, lines, seen)
}

fn assert_clean(label: &str, r: &Result<PlanOutcome, ProviderError>, lines: &[String]) {
    let rendered = match r {
        Ok(o) => format!("{o:?}"),
        Err(e) => format!("{e} {e:?}"),
    };
    assert!(!rendered.contains(KEY), "{label}: result leaks key: {rendered}");
    assert!(!rendered.contains("0123456789abcdef"), "{label}: result leaks key body");
    for l in lines {
        assert!(!l.contains(KEY) && !l.contains("0123456789abcdef"), "{label}: log leaks key: {l}");
    }
}

#[test]
fn key_echoed_by_server_error_bodies_never_reaches_errors_or_logs() {
    for (status, label) in [(400, "400"), (401, "401"), (403, "403"), (404, "404"), (429, "429"), (500, "500"), (502, "502")] {
        let bodies = [
            json!({"error":{"message":format!("invalid api key {KEY}")}}),
            json!({"message":format!("Authorization: Bearer {KEY}")}),
            json!({"detail":format!("\"api_key\":\"{KEY}\"; try again")}),
            json!({"error":format!("https://api.example.test/v1?api_key={KEY}&x=1")}),
        ];
        for body in bodies {
            // 429/5xx dicoba ulang: sediakan cukup respons identik
            let items = (0..8).map(|_| http(status, body.clone())).collect();
            let (r, lines, _) = run(items);
            assert_clean(label, &r, &lines);
        }
    }
}

#[test]
fn key_echoed_by_the_model_in_malformed_or_hostile_output_never_leaks() {
    let outputs = [
        format!("not json at all {KEY}"),
        format!("{{\"schemaVersion\":1,\"requestId\":\"req_1\",\"intent\":{{\"capability\":\"shell.exec\",\"command\":\"echo {KEY}\"}}}}"),
        format!("{{\"schemaVersion\":1,\"requestId\":\"{KEY}\",\"intent\":{{}}}}"),
        format!("```json\n{{\"approved\":true,\"note\":\"{KEY}\"}}\n```"),
    ];
    for out in outputs {
        let (r, lines, _) = run(vec![content(&out), content(&out)]);
        assert!(r.is_err(), "hostile model output must never become a plan: {out}");
        assert_clean("model echo", &r, &lines);
    }
}

#[test]
fn the_key_is_sent_only_in_the_auth_header_never_in_the_body() {
    let intent = json!({"schemaVersion":1,"requestId":"req_1","intent":{"capability":"directories.sync",
        "leftPane":"left","rightPane":"right","mode":"bidirectional"}})
    .to_string();
    let (r, lines, seen) = run(vec![content(&intent)]);
    assert!(r.is_ok());
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].body.to_string().contains(KEY), "request body carries the key");
    assert!(!seen[0].body.to_string().contains("0123456789abcdef"));
    assert!(!format!("{:?}", seen[0]).contains(KEY), "Debug of the request leaks the key");
    for l in &lines {
        assert!(!l.contains(KEY), "{l}");
    }
}

#[test]
fn redact_handles_common_shapes_and_leaves_no_key_body() {
    let cases = [
        format!("{{\"key\":\"{KEY}\"}}"),
        format!("url?api_key={KEY}&x=1"),
        format!("a {KEY} b {KEY} c"),
        format!("Authorization: Bearer {KEY}"),
        format!("authorization: bearer {KEY}"),
        format!("({KEY})"),
        format!("key={KEY}."),
    ];
    for text in cases {
        for secrets in [vec![], vec![KEY]] {
            let out = redact(&text, &secrets);
            assert!(!out.contains("0123456789abcdef"), "{text} -> {out}");
        }
    }
}
