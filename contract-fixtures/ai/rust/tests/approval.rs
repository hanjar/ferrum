#[path = "../../../../src-tauri/src/ai/approval.rs"]
mod approval;

use approval::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ferrum-ai-approval-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _dir: Dir,
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
    fs::write(src.join("b.txt"), b"beta").unwrap();
    let sources = fingerprint_sources(&[src.join("a.txt"), src.join("b.txt")]).unwrap();
    let record = ApprovalRecord {
        handle_id: "hnd_1".into(),
        plan_id: "plan_1".into(),
        plan_digest: "digest_1".into(),
        revision: 1,
        root: root.clone(),
        sources,
        output_name: Some("Invoice.zip".into()),
        expires_at_ms: 10_000,
        second_root: None,
        absent: vec![],
    };
    let ledger = ApprovalLedger::new();
    ledger.register(record.clone());
    Fixture { _dir: dir, src, root, ledger, record }
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
        now_ms: 5_000,
    }
}

fn reasons(r: Result<Authorized, StaleReport>) -> Vec<StaleReason> {
    r.unwrap_err().reasons
}

#[test]
fn authorizes_matching_claim_once_and_writes_nothing() {
    let f = fixture();
    let before: Vec<_> = fs::read_dir(&f.root).unwrap().collect();
    let ok = f.ledger.authorize(&claim(&f)).unwrap();
    assert_eq!(ok.plan_digest, "digest_1");
    assert_eq!(before.len(), fs::read_dir(&f.root).unwrap().count());
    // single use: a second authorize (double submit) is refused
    assert_eq!(reasons(f.ledger.authorize(&claim(&f))), vec![StaleReason::AlreadyConsumed]);
}

#[test]
fn unknown_or_fabricated_approval_is_refused() {
    let f = fixture();
    let mut c = claim(&f);
    c.handle_id = "hnd_forged".into();
    assert_eq!(reasons(f.ledger.authorize(&c)), vec![StaleReason::UnknownApproval]);
}

#[test]
fn expired_digest_revision_root_scope_and_plan_mismatches_are_reported() {
    let f = fixture();
    let mut c = claim(&f);
    c.now_ms = 10_000;
    assert!(reasons(f.ledger.authorize(&c)).contains(&StaleReason::Expired));

    for (mutate, expected) in [
        (Box::new(|c: &mut ExecuteClaim| c.plan_digest = "other".into()) as Box<dyn Fn(&mut ExecuteClaim)>, StaleReason::DigestMismatch),
        (Box::new(|c: &mut ExecuteClaim| c.revision = 2), StaleReason::RevisionMismatch),
        (Box::new(|c: &mut ExecuteClaim| c.plan_id = "plan_2".into()), StaleReason::PlanMismatch),
        (Box::new(|c: &mut ExecuteClaim| c.root = PathBuf::from("Z:\\elsewhere")), StaleReason::RootMismatch),
        (Box::new(|c: &mut ExecuteClaim| c.item_scope = "0".repeat(32)), StaleReason::ScopeMismatch),
    ] {
        let fresh = fixture();
        let mut c = claim(&fresh);
        mutate(&mut c);
        let got = reasons(fresh.ledger.authorize(&c));
        assert!(got.contains(&expected), "{expected:?} not in {got:?}");
    }
}

#[test]
fn source_changed_or_missing_after_preview_is_stale() {
    let f = fixture();
    fs::write(f.src.join("a.txt"), b"alpha-changed-longer").unwrap();
    fs::remove_file(f.src.join("b.txt")).unwrap();
    let got = reasons(f.ledger.authorize(&claim(&f)));
    assert!(got.contains(&StaleReason::SourceChanged(f.src.join("a.txt"))), "{got:?}");
    assert!(got.contains(&StaleReason::SourceMissing(f.src.join("b.txt"))), "{got:?}");
}

#[test]
fn target_removed_or_output_name_taken_is_stale_and_old_target_untouched() {
    let f = fixture();
    fs::write(f.root.join("Invoice.zip"), b"old-zip").unwrap();
    let got = reasons(f.ledger.authorize(&claim(&f)));
    assert_eq!(got, vec![StaleReason::OutputExists("Invoice.zip".into())]);
    assert_eq!(fs::read(f.root.join("Invoice.zip")).unwrap(), b"old-zip");

    let g = fixture();
    fs::remove_dir_all(&g.root).unwrap();
    assert!(reasons(g.ledger.authorize(&claim(&g))).contains(&StaleReason::TargetMissing));
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn readonly_target_is_reported() {
    let f = fixture();
    let mut perms = fs::metadata(&f.root).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&f.root, perms.clone()).unwrap();
    let got = f.ledger.authorize(&claim(&f));
    perms.set_readonly(false);
    fs::set_permissions(&f.root, perms).unwrap();
    // Windows honours the flag on directories only loosely; Unix semantics differ.
    if let Err(r) = got {
        assert!(r.reasons.contains(&StaleReason::TargetNotWritable));
    }
}

#[test]
fn revoke_after_plan_edit_blocks_old_approval() {
    let f = fixture();
    f.ledger.revoke("hnd_1");
    assert_eq!(reasons(f.ledger.authorize(&claim(&f))), vec![StaleReason::UnknownApproval]);
}

#[test]
fn reregistering_with_new_revision_requires_the_new_claim() {
    let f = fixture();
    let mut rec = f.record.clone();
    rec.revision = 2;
    rec.plan_digest = "digest_2".into();
    f.ledger.register(rec);
    assert!(reasons(f.ledger.authorize(&claim(&f))).contains(&StaleReason::DigestMismatch));
    let mut c = claim(&f);
    c.revision = 2;
    c.plan_digest = "digest_2".into();
    assert!(f.ledger.authorize(&c).is_ok());
}

#[test]
fn scope_digest_is_order_independent_and_change_sensitive() {
    let f = fixture();
    let mut rev = f.record.sources.clone();
    rev.reverse();
    assert_eq!(scope_digest(&rev), scope_digest(&f.record.sources));
    let mut changed = f.record.sources.clone();
    changed[0].size += 1;
    assert_ne!(scope_digest(&changed), scope_digest(&f.record.sources));
    assert!(f.ledger.authorize(&claim(&f)).is_ok());
    assert!(StaleReport { reasons: vec![StaleReason::Expired] }.summary().contains("usang"));
}
