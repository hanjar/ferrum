//! Tauri commands over the AI approval ledger (FER-410 / AI-SEC-1): the backend guard that runs
//! right before the executor writes anything.
//!
//! Flow: the frontend registers an approval when the preview is built (the BACKEND fingerprints the
//! sources and returns an opaque `scopeDigest`), and asks to authorize right before executing. The
//! clock is the backend's (never taken from the frontend), a slot is single use, and any mismatch is a
//! `stale` rejection with zero writes. Only ids, digests and local paths cross this boundary; nothing
//! here goes to the provider and no rejection carries a raw path.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use super::approval::{
    ApprovalLedger, ApprovalRecord, ExecuteClaim, StaleReason, StaleReport, canonical,
    fingerprint_sources, scope_digest_for,
};

/// Upper bound for one approval's source list (the preview of a 100k-entry panel must still fit).
const MAX_SOURCES: usize = 250_000;
const MAX_ID_LEN: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterApprovalDto {
    pub handle_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub revision: u32,
    /// Output directory the plan writes into.
    pub root: String,
    pub source_paths: Vec<String>,
    /// Basename of the single output (archive); `None` for operations without one.
    pub output_name: Option<String>,
    pub expires_at_ms: u64,
    /// Sync (FER-168): the right root (`root` is then the left root). `None` for single-root operations.
    #[serde(default)]
    pub second_root: Option<String>,
    /// Sync: targets of NEW copies; they must still not exist when the plan runs.
    #[serde(default)]
    pub absent_paths: Vec<String>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredDto {
    pub scope_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizeDto {
    pub handle_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub revision: u32,
    pub root: String,
    /// Sync: the right root of the approved pair.
    #[serde(default)]
    pub second_root: Option<String>,
    /// The `scopeDigest` returned by registration.
    pub item_scope: String,
}

/// Rejection data object: stable codes plus a short summary; never a raw path.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct GuardFailure {
    pub kind: &'static str,
    pub reasons: Vec<&'static str>,
    pub summary: String,
}

fn invalid(summary: &str) -> GuardFailure {
    GuardFailure {
        kind: "invalid_request",
        reasons: vec![],
        summary: summary.to_owned(),
    }
}

fn reason_code(reason: &StaleReason) -> &'static str {
    match reason {
        StaleReason::UnknownApproval => "unknown_approval",
        StaleReason::AlreadyConsumed => "already_consumed",
        StaleReason::Expired => "expired",
        StaleReason::PlanMismatch => "plan_mismatch",
        StaleReason::DigestMismatch => "digest_mismatch",
        StaleReason::RevisionMismatch => "revision_mismatch",
        StaleReason::RootMismatch => "root_mismatch",
        StaleReason::ScopeMismatch => "scope_mismatch",
        StaleReason::SourceMissing(_) => "source_missing",
        StaleReason::SourceChanged(_) => "source_changed",
        StaleReason::TargetMissing => "target_missing",
        StaleReason::TargetNotWritable => "target_not_writable",
        StaleReason::OutputExists(_) => "output_exists",
        StaleReason::TargetAppeared(_) => "target_appeared",
    }
}

impl From<StaleReport> for GuardFailure {
    fn from(report: StaleReport) -> Self {
        let mut reasons: Vec<&'static str> = report.reasons.iter().map(reason_code).collect();
        reasons.dedup();
        Self {
            kind: "stale",
            summary: report.summary(),
            reasons,
        }
    }
}

fn valid_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_path(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0') && std::path::Path::new(value).is_absolute()
}

/// A bare file name: no separators, no traversal, no stream syntax.
fn valid_output_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
}

/// Lexical, separator- and (on Windows) case-insensitive normal form used only for containment checks.
fn normal(path: &str) -> String {
    let flat = path.replace('\\', "/");
    let trimmed = flat.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// True when `path` is `root` or lies under it. `..` segments never count as inside.
fn within(path: &str, root: &str) -> bool {
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return false;
    }
    let (p, r) = (normal(path), normal(root));
    p == r || p.strip_prefix(&r).is_some_and(|rest| rest.starts_with('/'))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Validate the request, fingerprint the sources and store the approval.
pub fn register(
    ledger: &ApprovalLedger,
    dto: RegisterApprovalDto,
) -> Result<RegisteredDto, GuardFailure> {
    if !valid_ref(&dto.handle_id) || !valid_ref(&dto.plan_id) || !valid_ref(&dto.plan_digest) {
        return Err(invalid("Invalid approval reference."));
    }
    if !valid_path(&dto.root)
        || dto.source_paths.is_empty()
        || dto.source_paths.len() > MAX_SOURCES
        || !dto.source_paths.iter().all(|p| valid_path(p))
        || dto
            .output_name
            .as_deref()
            .is_some_and(|n| !valid_output_name(n))
    {
        return Err(invalid("Invalid approval scope."));
    }
    // Two-root (sync) scope: both roots canonical, distinct and not nested; every path lies under one of
    // them; the whole list stays within the same bound.
    let mut second_root: Option<PathBuf> = None;
    if let Some(second) = dto.second_root.as_deref() {
        if !valid_path(second) || dto.output_name.is_some() {
            return Err(invalid("Invalid approval scope."));
        }
        let (left, right) = (
            canonical(Path::new(&dto.root)),
            canonical(Path::new(second)),
        );
        if left == right || left.starts_with(&right) || right.starts_with(&left) {
            return Err(GuardFailure {
                kind: "invalid_roots",
                reasons: vec![],
                summary: "The two folders are the same or overlap; nothing was approved."
                    .to_owned(),
            });
        }
        let all = dto.source_paths.iter().chain(dto.absent_paths.iter());
        if dto.source_paths.len() + dto.absent_paths.len() > MAX_SOURCES
            || !all
                .clone()
                .all(|p| valid_path(p) && (within(p, &dto.root) || within(p, second)))
        {
            return Err(invalid("Invalid approval scope."));
        }
        second_root = Some(right);
    } else if !dto.absent_paths.is_empty() {
        return Err(invalid("Invalid approval scope."));
    }
    let absent: Vec<PathBuf> = dto.absent_paths.iter().map(PathBuf::from).collect();
    let paths: Vec<PathBuf> = dto.source_paths.iter().map(PathBuf::from).collect();
    // Unreadable source = fail closed: an approval is never recorded for something we cannot check.
    let sources = fingerprint_sources(&paths).map_err(|_| GuardFailure {
        kind: "source_unreadable",
        reasons: vec![],
        summary: "A selected item cannot be read; nothing was approved.".to_owned(),
    })?;
    let digest = scope_digest_for(&sources, second_root.as_deref(), &absent);
    ledger.register(ApprovalRecord {
        handle_id: dto.handle_id,
        plan_id: dto.plan_id,
        plan_digest: dto.plan_digest,
        revision: dto.revision,
        root: PathBuf::from(dto.root),
        sources,
        output_name: dto.output_name,
        expires_at_ms: dto.expires_at_ms,
        second_root,
        absent,
    });
    Ok(RegisteredDto {
        scope_digest: digest,
    })
}

/// Check the claim against the registered approval and the disk NOW; consumes the slot on success.
pub fn authorize(
    ledger: &ApprovalLedger,
    dto: AuthorizeDto,
    now_ms: u64,
) -> Result<(), GuardFailure> {
    if !valid_ref(&dto.handle_id) || !valid_ref(&dto.plan_id) || !valid_ref(&dto.plan_digest) {
        return Err(invalid("Invalid approval reference."));
    }
    ledger
        .authorize(&ExecuteClaim {
            handle_id: dto.handle_id,
            plan_id: dto.plan_id,
            plan_digest: dto.plan_digest,
            revision: dto.revision,
            root: PathBuf::from(dto.root),
            second_root: dto.second_root.map(PathBuf::from),
            item_scope: dto.item_scope,
            now_ms,
        })
        .map(|_| ())
        .map_err(GuardFailure::from)
}

#[tauri::command]
pub async fn ai_register_approval(
    app: AppHandle,
    request: RegisterApprovalDto,
) -> Result<RegisteredDto, GuardFailure> {
    // Fingerprinting touches the disk (up to MAX_SOURCES entries): keep it off the async runtime.
    // The state is fetched through the (owned) app handle inside the blocking closure.
    tauri::async_runtime::spawn_blocking(move || {
        let ledger = app.state::<ApprovalLedger>();
        register(&ledger, request)
    })
    .await
    .unwrap_or_else(|_| Err(invalid("Approval registration failed.")))
}

#[tauri::command]
pub fn ai_authorize_execute(
    ledger: State<'_, ApprovalLedger>,
    claim: AuthorizeDto,
) -> Result<(), GuardFailure> {
    authorize(&ledger, claim, now_ms())
}

#[tauri::command]
pub fn ai_revoke_approval(ledger: State<'_, ApprovalLedger>, handle_id: String) {
    if valid_ref(&handle_id) {
        ledger.revoke(&handle_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Fresh temp dir per test (std only; removed on drop).
    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ferrum-approval-{tag}-{}-{}",
                std::process::id(),
                now_ms()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn file(&self, name: &str, body: &str) -> String {
            let p = self.0.join(name);
            fs::write(&p, body).unwrap();
            p.to_string_lossy().into_owned()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn register_dto(dir: &Dir, sources: Vec<String>, output: Option<&str>) -> RegisterApprovalDto {
        RegisterApprovalDto {
            handle_id: "h_1".into(),
            plan_id: "plan_1".into(),
            plan_digest: "digest_1".into(),
            revision: 1,
            root: dir.path().to_string_lossy().into_owned(),
            source_paths: sources,
            output_name: output.map(str::to_owned),
            expires_at_ms: now_ms() + 60_000,
            second_root: None,
            absent_paths: vec![],
        }
    }
    fn claim(dir: &Dir, scope: &str) -> AuthorizeDto {
        AuthorizeDto {
            handle_id: "h_1".into(),
            plan_id: "plan_1".into(),
            plan_digest: "digest_1".into(),
            revision: 1,
            root: dir.path().to_string_lossy().into_owned(),
            second_root: None,
            item_scope: scope.to_owned(),
        }
    }
    fn kinds(failure: &GuardFailure) -> Vec<&'static str> {
        failure.reasons.clone()
    }

    #[test]
    fn untouched_sources_authorize_exactly_once() {
        let dir = Dir::new("ok");
        let a = dir.file("a.txt", "one");
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, register_dto(&dir, vec![a], Some("out.zip")))
            .unwrap()
            .scope_digest;
        assert!(authorize(&ledger, claim(&dir, &scope), now_ms()).is_ok());
        let second = authorize(&ledger, claim(&dir, &scope), now_ms()).unwrap_err();
        assert_eq!(kinds(&second), vec!["already_consumed"]);
    }

    #[test]
    fn a_source_changed_after_the_preview_is_stale() {
        let dir = Dir::new("changed");
        let a = dir.file("a.txt", "one");
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, register_dto(&dir, vec![a.clone()], None))
            .unwrap()
            .scope_digest;
        fs::write(&a, "one but longer now").unwrap();
        let failure = authorize(&ledger, claim(&dir, &scope), now_ms()).unwrap_err();
        assert_eq!(failure.kind, "stale");
        assert!(kinds(&failure).contains(&"source_changed"));
        assert!(
            !failure.summary.contains(dir.path().to_str().unwrap()),
            "no raw path in the message"
        );
    }

    #[test]
    fn a_missing_source_or_a_new_output_is_stale() {
        let dir = Dir::new("missing");
        let a = dir.file("a.txt", "one");
        let ledger = ApprovalLedger::new();
        let scope = register(
            &ledger,
            register_dto(&dir, vec![a.clone()], Some("out.zip")),
        )
        .unwrap()
        .scope_digest;
        fs::remove_file(&a).unwrap();
        dir.file("out.zip", "someone else's zip");
        let failure = authorize(&ledger, claim(&dir, &scope), now_ms()).unwrap_err();
        assert!(kinds(&failure).contains(&"source_missing"));
        assert!(kinds(&failure).contains(&"output_exists"));
        // The unrelated file was never touched.
        assert_eq!(
            fs::read_to_string(dir.path().join("out.zip")).unwrap(),
            "someone else's zip"
        );
    }

    #[test]
    fn forged_or_mismatching_claims_are_rejected() {
        let dir = Dir::new("forged");
        let a = dir.file("a.txt", "one");
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, register_dto(&dir, vec![a], None))
            .unwrap()
            .scope_digest;

        let mut wrong_digest = claim(&dir, &scope);
        wrong_digest.plan_digest = "other_digest".into();
        assert!(
            kinds(&authorize(&ledger, wrong_digest, now_ms()).unwrap_err())
                .contains(&"digest_mismatch")
        );

        let wrong_scope = claim(&dir, "00000000000000000000000000000000");
        assert!(
            kinds(&authorize(&ledger, wrong_scope, now_ms()).unwrap_err())
                .contains(&"scope_mismatch")
        );

        let mut unknown = claim(&dir, &scope);
        unknown.handle_id = "never_registered".into();
        assert_eq!(
            kinds(&authorize(&ledger, unknown, now_ms()).unwrap_err()),
            vec!["unknown_approval"]
        );
    }

    #[test]
    fn an_expired_approval_and_a_revoked_one_do_not_authorize() {
        let dir = Dir::new("expiry");
        let a = dir.file("a.txt", "one");
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, register_dto(&dir, vec![a.clone()], None))
            .unwrap()
            .scope_digest;
        let late = authorize(&ledger, claim(&dir, &scope), now_ms() + 3_600_000).unwrap_err();
        assert!(kinds(&late).contains(&"expired"));

        let scope = register(&ledger, register_dto(&dir, vec![a], None))
            .unwrap()
            .scope_digest;
        ledger.revoke("h_1");
        assert_eq!(
            kinds(&authorize(&ledger, claim(&dir, &scope), now_ms()).unwrap_err()),
            vec!["unknown_approval"]
        );
    }

    #[test]
    fn registration_fails_closed_for_bad_input_and_unreadable_sources() {
        let dir = Dir::new("bad");
        let a = dir.file("a.txt", "one");
        let ledger = ApprovalLedger::new();
        let missing = dir.path().join("nope.txt").to_string_lossy().into_owned();
        assert_eq!(
            register(&ledger, register_dto(&dir, vec![missing], None))
                .unwrap_err()
                .kind,
            "source_unreadable"
        );
        assert_eq!(
            register(&ledger, register_dto(&dir, vec![], None))
                .unwrap_err()
                .kind,
            "invalid_request"
        );
        assert_eq!(
            register(
                &ledger,
                register_dto(&dir, vec!["relative/path.txt".into()], None)
            )
            .unwrap_err()
            .kind,
            "invalid_request"
        );
        for name in ["../x.zip", "a/b.zip", "c:evil.zip", ".."] {
            assert_eq!(
                register(&ledger, register_dto(&dir, vec![a.clone()], Some(name)))
                    .unwrap_err()
                    .kind,
                "invalid_request",
                "{name}"
            );
        }
        let mut bad_ref = register_dto(&dir, vec![a], None);
        bad_ref.handle_id = "bad handle!".into();
        assert_eq!(
            register(&ledger, bad_ref).unwrap_err().kind,
            "invalid_request"
        );
    }

    // ---- Two-root (sync) guard, FER-168 ----

    /// Sync fixture: `l/a.txt` is a NEW copy (absent on the right), `l/b.txt` overwrites `r/b.txt`.
    struct Pair {
        dir: Dir,
        left: PathBuf,
        right: PathBuf,
    }
    fn pair(tag: &str) -> Pair {
        let dir = Dir::new(tag);
        let left = dir.path().join("l");
        let right = dir.path().join("r");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(left.join("a.txt"), "alpha").unwrap();
        fs::write(left.join("b.txt"), "bravo-left").unwrap();
        fs::write(right.join("b.txt"), "bravo-right").unwrap();
        Pair { dir, left, right }
    }
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }
    fn sync_register(p: &Pair) -> RegisterApprovalDto {
        RegisterApprovalDto {
            handle_id: "h_1".into(),
            plan_id: "plan_1".into(),
            plan_digest: "digest_1".into(),
            revision: 1,
            root: s(&p.left),
            source_paths: vec![
                s(&p.left.join("a.txt")),
                s(&p.left.join("b.txt")),
                s(&p.right.join("b.txt")),
                s(&p.left),
                s(&p.right),
            ],
            output_name: None,
            expires_at_ms: now_ms() + 60_000,
            second_root: Some(s(&p.right)),
            absent_paths: vec![s(&p.right.join("a.txt"))],
        }
    }
    fn sync_claim(p: &Pair, scope: &str) -> AuthorizeDto {
        AuthorizeDto {
            handle_id: "h_1".into(),
            plan_id: "plan_1".into(),
            plan_digest: "digest_1".into(),
            revision: 1,
            root: s(&p.left),
            second_root: Some(s(&p.right)),
            item_scope: scope.to_owned(),
        }
    }
    fn tick() {
        std::thread::sleep(std::time::Duration::from_millis(60)); // directory mtime must differ
    }
    fn assert_no_path(f: &GuardFailure, p: &Pair) {
        let dump = format!("{} {:?}", f.summary, f.reasons);
        assert!(
            !dump.contains(p.dir.path().to_str().unwrap()),
            "rejection must not carry a raw path"
        );
    }
    fn approved(p: &Pair) -> (ApprovalLedger, String) {
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, sync_register(p)).unwrap().scope_digest;
        (ledger, scope)
    }
    fn make_dir_link(link: &Path, target: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J", &s(link), &s(target)])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[test]
    fn sync_untouched_scope_authorizes_exactly_once() {
        let p = pair("sync-ok");
        let (ledger, scope) = approved(&p);
        assert!(authorize(&ledger, sync_claim(&p, &scope), now_ms()).is_ok());
        let again = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert_eq!(kinds(&again), vec!["already_consumed"]);
    }

    #[test]
    fn sync_source_changed_after_preview_is_stale_and_writes_nothing() {
        let p = pair("sync-src");
        let (ledger, scope) = approved(&p);
        fs::write(p.left.join("a.txt"), "alpha but different now").unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert_eq!(f.kind, "stale");
        assert!(kinds(&f).contains(&"source_changed"));
        assert_no_path(&f, &p);
        assert!(!p.right.join("a.txt").exists(), "the guard never writes");
    }

    #[test]
    fn sync_overwrite_target_changed_after_preview_is_stale() {
        let p = pair("sync-tgt");
        let (ledger, scope) = approved(&p);
        fs::write(
            p.right.join("b.txt"),
            "someone edited the right side meanwhile",
        )
        .unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert!(kinds(&f).contains(&"source_changed"), "{:?}", f.reasons);
        // The edited target is exactly as the other party left it.
        assert_eq!(
            fs::read_to_string(p.right.join("b.txt")).unwrap(),
            "someone edited the right side meanwhile"
        );
    }

    #[test]
    fn sync_new_file_at_an_approved_new_target_is_stale() {
        let p = pair("sync-appear");
        let (ledger, scope) = approved(&p);
        fs::write(p.right.join("a.txt"), "appeared after the preview").unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert!(kinds(&f).contains(&"target_appeared"), "{:?}", f.reasons);
        assert_eq!(
            fs::read_to_string(p.right.join("a.txt")).unwrap(),
            "appeared after the preview"
        );
    }

    #[test]
    fn sync_a_dangling_link_at_a_new_target_counts_as_appeared() {
        let p = pair("sync-dangling");
        let (ledger, scope) = approved(&p);
        let gone = p.dir.path().join("gone");
        fs::create_dir_all(&gone).unwrap();
        // A junction/symlink named like the target, then its destination removed.
        if !make_dir_link(&p.right.join("a.txt"), &gone) {
            eprintln!("SKIPPED: cannot create a directory link here");
            return;
        }
        fs::remove_dir(&gone).unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert!(kinds(&f).contains(&"target_appeared"), "{:?}", f.reasons);
    }

    /// Documents a KNOWN LIMIT (docs/ai/sync-chat.md): a one-sided folder copied whole is fingerprinted by its
    /// own mtime only. A new direct child moves that mtime (stale); a content edit one level down does not
    /// (authorized; the fresh compare in `runSyncPlan` is the layer that sees it). If the fingerprint is ever
    /// strengthened, this test must be updated together with the doc.
    #[test]
    fn sync_folder_fingerprint_sees_direct_children_but_not_deeper_edits() {
        let p = pair("sync-deepdir");
        let folder = p.left.join("album");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("one.txt"), "first").unwrap();
        tick();
        let dto = || {
            let mut dto = sync_register(&p);
            dto.source_paths.push(s(&folder));
            dto.absent_paths.push(s(&p.right.join("album")));
            dto
        };
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, dto()).unwrap().scope_digest;

        // Content edit of a file inside the folder: the folder's own mtime does not move.
        fs::write(folder.join("one.txt"), "first, edited after the preview").unwrap();
        assert!(authorize(&ledger, sync_claim(&p, &scope), now_ms()).is_ok());

        // A new direct child does move it.
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, dto()).unwrap().scope_digest;
        tick();
        fs::write(folder.join("two.txt"), "added after the preview").unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert!(kinds(&f).contains(&"source_changed"), "{:?}", f.reasons);
    }

    #[test]
    fn sync_a_file_added_to_a_root_after_preview_is_stale() {
        let p = pair("sync-newfile");
        let (ledger, scope) = approved(&p);
        tick();
        fs::write(p.left.join("brand-new.txt"), "not in the approved plan").unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert!(kinds(&f).contains(&"source_changed"), "{:?}", f.reasons);
    }

    #[test]
    fn sync_root_redirected_by_a_junction_after_preview_is_stale() {
        let p = pair("sync-junction");
        let (ledger, scope) = approved(&p);
        let elsewhere = p.dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("b.txt"), "bravo-right").unwrap();
        fs::remove_dir_all(&p.right).unwrap();
        if !make_dir_link(&p.right, &elsewhere) {
            eprintln!("SKIPPED: cannot create a directory link here");
            return;
        }
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        assert_eq!(f.kind, "stale");
        assert!(kinds(&f).contains(&"root_mismatch"), "{:?}", f.reasons);
        assert_no_path(&f, &p);
        assert!(
            !elsewhere.join("a.txt").exists(),
            "nothing was written through the link"
        );
    }

    #[test]
    fn sync_claim_must_name_the_same_roots() {
        let p = pair("sync-roots");
        let (ledger, scope) = approved(&p);
        let mut missing = sync_claim(&p, &scope);
        missing.second_root = None;
        assert!(
            kinds(&authorize(&ledger, missing, now_ms()).unwrap_err()).contains(&"root_mismatch")
        );

        let other = p.dir.path().join("other");
        fs::create_dir_all(&other).unwrap();
        let (ledger, scope) = approved(&p);
        let mut wrong = sync_claim(&p, &scope);
        wrong.second_root = Some(s(&other));
        assert!(
            kinds(&authorize(&ledger, wrong, now_ms()).unwrap_err()).contains(&"root_mismatch")
        );

        // A single-root approval cannot be claimed as a pair either.
        let d = Dir::new("sync-single");
        let a = d.file("a.txt", "x");
        let ledger = ApprovalLedger::new();
        let scope = register(&ledger, register_dto(&d, vec![a], None))
            .unwrap()
            .scope_digest;
        let mut pair_claim = claim(&d, &scope);
        pair_claim.second_root = Some(s(&other));
        assert!(
            kinds(&authorize(&ledger, pair_claim, now_ms()).unwrap_err())
                .contains(&"root_mismatch")
        );
    }

    #[test]
    fn sync_scope_digest_binds_roots_and_new_targets() {
        let p = pair("sync-scope");
        let ledger = ApprovalLedger::new();
        let base = register(&ledger, sync_register(&p)).unwrap().scope_digest;
        let mut more = sync_register(&p);
        more.absent_paths.push(s(&p.right.join("c.txt")));
        assert_ne!(
            base,
            register(&ledger, more).unwrap().scope_digest,
            "an extra new target changes the scope"
        );
        // A scope that does not match what the backend computed is refused.
        let ledger = ApprovalLedger::new();
        register(&ledger, sync_register(&p)).unwrap();
        let forged = authorize(
            &ledger,
            sync_claim(&p, "00000000000000000000000000000000"),
            now_ms(),
        );
        assert!(kinds(&forged.unwrap_err()).contains(&"scope_mismatch"));
    }

    #[test]
    fn sync_registration_fails_closed_for_bad_scopes() {
        let p = pair("sync-bad");
        let ledger = ApprovalLedger::new();
        let kind = |dto: RegisterApprovalDto| register(&ledger, dto).unwrap_err().kind;

        let mut same = sync_register(&p);
        same.second_root = Some(s(&p.left));
        assert_eq!(kind(same), "invalid_roots");

        let mut nested = sync_register(&p);
        let inner = p.left.join("inner");
        fs::create_dir_all(&inner).unwrap();
        nested.second_root = Some(s(&inner));
        assert_eq!(kind(nested), "invalid_roots");

        let mut dotted = sync_register(&p);
        dotted.second_root = Some(format!("{}{}.", s(&p.left), std::path::MAIN_SEPARATOR));
        assert_eq!(
            kind(dotted),
            "invalid_roots",
            "an alias of the same folder is the same folder"
        );

        let mut outside = sync_register(&p);
        let stray = p.dir.path().join("stray.txt");
        fs::write(&stray, "x").unwrap();
        outside.source_paths.push(s(&stray));
        assert_eq!(kind(outside), "invalid_request");

        let mut absent_outside = sync_register(&p);
        absent_outside
            .absent_paths
            .push(s(&p.dir.path().join("elsewhere.txt")));
        assert_eq!(kind(absent_outside), "invalid_request");

        let mut traversal = sync_register(&p);
        let sep = std::path::MAIN_SEPARATOR;
        traversal
            .absent_paths
            .push(format!("{}{sep}..{sep}x.txt", s(&p.right)));
        assert_eq!(kind(traversal), "invalid_request");

        let mut absent_alone = sync_register(&p);
        absent_alone.second_root = None;
        assert_eq!(kind(absent_alone), "invalid_request");

        let mut with_output = sync_register(&p);
        with_output.output_name = Some("x.zip".into());
        assert_eq!(kind(with_output), "invalid_request");

        let mut unreadable = sync_register(&p);
        unreadable.source_paths.push(s(&p.left.join("missing.txt")));
        assert_eq!(kind(unreadable), "source_unreadable");
    }

    #[test]
    fn sync_missing_second_root_is_stale() {
        let p = pair("sync-gone");
        let (ledger, scope) = approved(&p);
        fs::remove_dir_all(&p.right).unwrap();
        let f = authorize(&ledger, sync_claim(&p, &scope), now_ms()).unwrap_err();
        let codes = kinds(&f);
        assert!(
            codes.contains(&"target_missing") || codes.contains(&"source_missing"),
            "{codes:?}"
        );
    }
}
