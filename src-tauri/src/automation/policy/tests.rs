//! Policy hash, MAC and verification tests (FER-522 / P1.1: SE-3.1, design §8.3-§8.5).

use super::*;
use crate::automation::dpapi::{DpapiKeyFile, KeyLoad};
use crate::automation::model::{
    CompareMode, Conditions, DeleteGuard, DeletePolicy, DriveKind, Filters, Freq, MissedPolicy,
    RootIdentity, RootSpec, Runtime, ScheduleTrigger, Side, SyncMode, Trigger, WatchTrigger,
};
use crate::automation::store::automations as auto_store;
use crate::automation::test_support::Fixture;

const AUTO_ID: &str = "11111111-1111-4111-8111-111111111111";
const PROFILE_ID: &str = "22222222-2222-4222-8222-222222222222";

fn root(path: &str, id: &str) -> RootSpec {
    RootSpec {
        path: path.into(),
        identity: RootIdentity::Local {
            volume_serial: "0123abcd".into(),
            file_id: id.repeat(32),
            fs_name: "NTFS".into(),
            drive: DriveKind::Fixed,
        },
    }
}

fn fixture_profile() -> ProfilePolicy {
    ProfilePolicy {
        left: root(r"D:\Photos", "a"),
        right: root(r"E:\Backup\Photos", "b"),
        mode: SyncMode::Mirror { from: Side::Left },
        delete_policy: DeletePolicy::Versioning {
            root: r"E:\Versions".into(),
        },
        compare: CompareMode::SizeAndDate,
        filters: Filters {
            include: vec!["*.jpg".into()],
            exclude: vec!["*.tmp".into(), "Thumbs.db".into()],
        },
        guard: DeleteGuard {
            enabled: true,
            max_count: 50,
            max_percent: 20,
        },
    }
}

fn fixture_automation() -> AutomationPolicy {
    AutomationPolicy {
        trigger: Trigger::Schedule(ScheduleTrigger {
            freq: Freq::Weekly {
                days: vec![1, 3, 5],
            },
            time: "02:00".into(),
            tz: "Asia/Jakarta".into(),
            missed: MissedPolicy::RunOnce,
        }),
        runtime: Runtime::Background,
        retries: 2,
        conditions: Conditions::default(),
    }
}

fn hash(profile: &ProfilePolicy, automation: &AutomationPolicy) -> String {
    policy_hash(AUTO_ID, PROFILE_ID, 3, profile, automation).unwrap()
}

/// Pinned `H` for the fixture policy: a serialization change anywhere in `ProfilePolicy`,
/// `AutomationPolicy` or the hash layout fails here and forces a `HASH_VERSION` bump (§8.3).
/// The value was cross-checked against an independent re-implementation of the layout.
const GOLDEN_HASH: &str = "5902abc04ac2ed79e3dd3a5769fab88747136c312abdfdc8f6aae94d48099dc8";

/// The exact JSON document that is hashed (so the golden value can be re-derived independently
/// of this code).
#[test]
fn the_hashed_document_is_pinned() {
    let doc = serde_json::to_string(&PolicyV1 {
        profile: &fixture_profile(),
        automation: &fixture_automation(),
    })
    .unwrap();
    assert_eq!(
        doc,
        r#"{"profile":{"left":{"path":"D:\\Photos","identity":{"kind":"local","volumeSerial":"0123abcd","fileId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","fsName":"NTFS","drive":"fixed"}},"right":{"path":"E:\\Backup\\Photos","identity":{"kind":"local","volumeSerial":"0123abcd","fileId":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","fsName":"NTFS","drive":"fixed"}},"mode":{"kind":"mirror","from":"left"},"deletePolicy":{"kind":"versioning","root":"E:\\Versions"},"compare":"sizeAndDate","filters":{"include":["*.jpg"],"exclude":["*.tmp","Thumbs.db"]},"guard":{"enabled":true,"maxCount":50,"maxPercent":20}},"automation":{"trigger":{"kind":"schedule","freq":{"kind":"weekly","days":[1,3,5]},"time":"02:00","tz":"Asia/Jakarta","missed":"runOnce"},"runtime":"background","retries":2,"conditions":{"skipOnBattery":false,"skipOnMetered":false}}}"#
    );
}

/// SE-3.1 / design §8.3: golden policy hash for the fixture policy.
#[test]
fn golden_policy_hash() {
    assert_eq!(hash(&fixture_profile(), &fixture_automation()), GOLDEN_HASH);
    assert_eq!(HASH_VERSION, 1);
}

/// Changing ANY field of `ProfilePolicy` or `AutomationPolicy` changes the hash (design §8.3).
#[test]
fn changing_any_policy_field_changes_the_hash() {
    let base_profile = fixture_profile();
    let base_auto = fixture_automation();
    let base = hash(&base_profile, &base_auto);

    let mut profile_changes: Vec<(&str, ProfilePolicy)> = Vec::new();
    let mut p = base_profile.clone();
    p.left.path = r"D:\Other".into();
    profile_changes.push(("left.path", p));
    let mut p = base_profile.clone();
    p.left.identity = RootIdentity::Local {
        volume_serial: "0123abcd".into(),
        file_id: "c".repeat(32),
        fs_name: "NTFS".into(),
        drive: DriveKind::Fixed,
    };
    profile_changes.push(("left.identity", p));
    let mut p = base_profile.clone();
    p.right.path = r"F:\Elsewhere".into();
    profile_changes.push(("right.path", p));
    let mut p = base_profile.clone();
    p.mode = SyncMode::Mirror { from: Side::Right };
    profile_changes.push(("mode.from", p));
    let mut p = base_profile.clone();
    p.mode = SyncMode::TwoWay {
        propagate_deletes: true,
    };
    profile_changes.push(("mode.kind", p));
    let mut p = base_profile.clone();
    p.delete_policy = DeletePolicy::Permanent;
    profile_changes.push(("deletePolicy", p));
    let mut p = base_profile.clone();
    p.delete_policy = DeletePolicy::Versioning {
        root: r"E:\Versions2".into(),
    };
    profile_changes.push(("deletePolicy.root", p));
    let mut p = base_profile.clone();
    p.compare = CompareMode::Content;
    profile_changes.push(("compare", p));
    let mut p = base_profile.clone();
    p.filters.include.push("*.png".into());
    profile_changes.push(("filters.include", p));
    let mut p = base_profile.clone();
    p.filters.exclude.clear();
    profile_changes.push(("filters.exclude", p));
    let mut p = base_profile.clone();
    p.guard.enabled = false;
    profile_changes.push(("guard.enabled", p));
    let mut p = base_profile.clone();
    p.guard.max_count = 51;
    profile_changes.push(("guard.maxCount", p));
    let mut p = base_profile.clone();
    p.guard.max_percent = 21;
    profile_changes.push(("guard.maxPercent", p));
    for (what, changed) in &profile_changes {
        assert_ne!(hash(changed, &base_auto), base, "profile field {what}");
    }

    let schedule = |freq: Freq, time: &str, tz: &str, missed: MissedPolicy| AutomationPolicy {
        trigger: Trigger::Schedule(ScheduleTrigger {
            freq,
            time: time.into(),
            tz: tz.into(),
            missed,
        }),
        ..base_auto.clone()
    };
    let weekly = || Freq::Weekly {
        days: vec![1, 3, 5],
    };
    let mut auto_changes: Vec<(&str, AutomationPolicy)> = vec![
        (
            "freq",
            schedule(
                Freq::Daily {},
                "02:00",
                "Asia/Jakarta",
                MissedPolicy::RunOnce,
            ),
        ),
        (
            "weekly days",
            schedule(
                Freq::Weekly { days: vec![1, 3] },
                "02:00",
                "Asia/Jakarta",
                MissedPolicy::RunOnce,
            ),
        ),
        (
            "time",
            schedule(weekly(), "02:01", "Asia/Jakarta", MissedPolicy::RunOnce),
        ),
        (
            "tz",
            schedule(weekly(), "02:00", "UTC", MissedPolicy::RunOnce),
        ),
        (
            "missed",
            schedule(weekly(), "02:00", "Asia/Jakarta", MissedPolicy::Skip),
        ),
    ];
    let mut a = base_auto.clone();
    a.runtime = Runtime::App;
    auto_changes.push(("runtime", a));
    let mut a = base_auto.clone();
    a.retries = 3;
    auto_changes.push(("retries", a));
    let mut a = base_auto.clone();
    a.conditions.skip_on_battery = true;
    auto_changes.push(("conditions.battery", a));
    let mut a = base_auto.clone();
    a.conditions.skip_on_metered = true;
    auto_changes.push(("conditions.metered", a));
    for (what, changed) in &auto_changes {
        assert_ne!(
            hash(&base_profile, changed),
            base,
            "automation field {what}"
        );
    }
    // Each watch field matters too, and so does the trigger kind.
    let watch = |f: &dyn Fn(&mut WatchTrigger)| {
        let mut w = WatchTrigger {
            watch_left: true,
            watch_right: false,
            on_changes: true,
            on_available: true,
            idle_secs: 10,
            max_wait_secs: 900,
        };
        f(&mut w);
        hash(
            &base_profile,
            &AutomationPolicy {
                trigger: Trigger::Watch(w),
                ..base_auto.clone()
            },
        )
    };
    let watch_base = watch(&|_| {});
    assert_ne!(watch_base, base, "trigger kind");
    for (what, h) in [
        ("watchLeft", watch(&|w| w.watch_left = false)),
        ("watchRight", watch(&|w| w.watch_right = true)),
        ("onChanges", watch(&|w| w.on_changes = false)),
        ("onAvailable", watch(&|w| w.on_available = false)),
        ("idleSecs", watch(&|w| w.idle_secs = 11)),
        ("maxWaitSecs", watch(&|w| w.max_wait_secs = 901)),
    ] {
        assert_ne!(h, watch_base, "watch field {what}");
    }

    // The ids and the revision are bound too.
    let other = "33333333-3333-4333-8333-333333333333";
    assert_ne!(
        policy_hash(other, PROFILE_ID, 3, &base_profile, &base_auto).unwrap(),
        base
    );
    assert_ne!(
        policy_hash(AUTO_ID, other, 3, &base_profile, &base_auto).unwrap(),
        base
    );
    assert_ne!(
        policy_hash(AUTO_ID, PROFILE_ID, 4, &base_profile, &base_auto).unwrap(),
        base
    );
}

/// The automation NAME is cosmetic: it is not an input of the hash at all, and renaming through
/// the real test flow keeps `policyHash` (design §8.3).
#[test]
fn the_automation_name_does_not_change_the_hash() {
    let fx = Fixture::new();
    let (profile, _) = fx.profile("P");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    let renamed = Fixture::auto_draft("Renamed completely", &profile, Fixture::daily());
    let original = Fixture::auto_draft("Nightly", &profile, Fixture::daily());
    let a = fx.test(Some(&id), &original);
    let b = fx.test(Some(&id), &renamed);
    assert_eq!(a.policy_hash, b.policy_hash);
    assert_eq!(a.automation_id, id);
}

fn mac_input(hash: &str) -> MacInput<'_> {
    MacInput {
        hash_version: 1,
        policy_hash: hash,
        automation_id: AUTO_ID,
        profile_id: PROFILE_ID,
        profile_revision: 3,
        config_rev: 2,
        approved_ms: 1_760_000_000_000,
    }
}

/// SE-3.1: MAC verify, valid and every tampered input.
#[test]
fn mac_verifies_and_rejects_every_tampered_input() {
    let key = ApprovalKey::for_tests(5);
    let h = hash(&fixture_profile(), &fixture_automation());
    let mac = compute_mac(&key, &mac_input(&h)).unwrap();
    assert_eq!(mac.len(), 64);
    assert!(mac_matches(&key, &mac_input(&h), &mac));

    let other_hash = "0".repeat(64);
    let other_id = "33333333-3333-4333-8333-333333333333";
    let tampered: Vec<(&str, MacInput<'_>)> = vec![
        ("policy_hash", mac_input(&other_hash)),
        (
            "profile_revision",
            MacInput {
                profile_revision: 4,
                ..mac_input(&h)
            },
        ),
        (
            "config_rev",
            MacInput {
                config_rev: 3,
                ..mac_input(&h)
            },
        ),
        (
            "approved_ms",
            MacInput {
                approved_ms: 1_760_000_000_001,
                ..mac_input(&h)
            },
        ),
        (
            "automation_id",
            MacInput {
                automation_id: other_id,
                ..mac_input(&h)
            },
        ),
        (
            "profile_id",
            MacInput {
                profile_id: other_id,
                ..mac_input(&h)
            },
        ),
        (
            "hash_version",
            MacInput {
                hash_version: 2,
                ..mac_input(&h)
            },
        ),
    ];
    for (what, input) in &tampered {
        assert!(!mac_matches(&key, input, &mac), "tampered {what}");
    }
    // A different key, a truncated MAC and non-hex text all fail.
    assert!(!mac_matches(
        &ApprovalKey::for_tests(6),
        &mac_input(&h),
        &mac
    ));
    assert!(!mac_matches(&key, &mac_input(&h), &mac[..62]));
    assert!(!mac_matches(&key, &mac_input(&h), "not hex at all"));
    assert!(!mac_matches(&key, &mac_input(&h), ""));
}

/// Domain separation: an approval MAC is not a baseline MAC and the other way round.
#[test]
fn subkeys_are_domain_separated() {
    let key = ApprovalKey::for_tests(5);
    let sub_a = derive_subkey(&key, "purpose.a").unwrap();
    let sub_b = derive_subkey(&key, "purpose.b").unwrap();
    assert_ne!(sub_a, sub_b);
    assert_ne!(sub_a, key);
    let mac = hmac_hex(&sub_a, b"payload").unwrap();
    assert!(hmac_matches(&sub_a, b"payload", &mac));
    assert!(!hmac_matches(&sub_b, b"payload", &mac));
    assert!(!hmac_matches(&key, b"payload", &mac));
}

fn verdict(fx: &Fixture, id: &str) -> Verdict {
    fx.store
        .with_read(|conn| verify(conn, fx.keys.as_ref(), id))
        .unwrap()
}

fn exec(fx: &Fixture, sql: &str) {
    fx.store
        .with_write(|conn| -> Result<(), StoreError> {
            conn.execute_batch(sql).map_err(|_| StoreError::Unavailable)
        })
        .unwrap();
}

fn approved_fixture() -> (Fixture, String, String, i64) {
    let fx = Fixture::new();
    let (profile, revision) = fx.profile("P");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    (fx, id, profile, revision)
}

#[test]
fn an_approved_automation_verifies() {
    let (fx, id, _, _) = approved_fixture();
    assert_eq!(verdict(&fx, &id), Verdict::Valid);
}

/// design §8.5: no approval row, unknown automation.
#[test]
fn a_missing_approval_or_automation_is_not_approved() {
    let (fx, id, _, _) = approved_fixture();
    exec(&fx, "DELETE FROM approvals");
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::NotApproved)
    );
    assert_eq!(
        verdict(&fx, "99999999-9999-4999-8999-999999999999"),
        Verdict::NeedsReview(NeedsReview::NotApproved)
    );
}

/// SE-3.1 tampered `policy_hash`: a store edit is caught by the recomputed hash.
#[test]
fn a_tampered_policy_hash_needs_review() {
    let (fx, id, _, _) = approved_fixture();
    exec(
        &fx,
        &format!("UPDATE approvals SET policy_hash = '{}'", "0".repeat(64)),
    );
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::PolicyChanged)
    );
}

/// SE-3.1 tampered `profile_revision`.
#[test]
fn a_tampered_profile_revision_needs_review() {
    let (fx, id, _, revision) = approved_fixture();
    exec(
        &fx,
        &format!("UPDATE approvals SET profile_revision = {}", revision + 5),
    );
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::ProfileChanged {
            from: revision + 5,
            to: revision
        })
    );
}

/// The strongest store tamper: the approval row is rewritten CONSISTENTLY (new revision, freshly
/// computed hash) by someone without the key. Only the MAC stops it.
#[test]
fn a_consistently_forged_approval_fails_the_mac() {
    let (fx, id, profile, revision) = approved_fixture();
    let new_revision = fx.bump_profile(&profile, revision, "*.tmp");
    assert!(matches!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::ProfileChanged { .. })
    ));
    // Forge: recompute the hash under the new revision and write it with the old MAC.
    let forged_hash = fx
        .store
        .with_read(|conn| -> Result<String, StoreError> {
            let rec = auto_store::get(conn, &id)?.unwrap();
            let profile_rec = crate::automation::store::profiles::get(conn, &profile)?.unwrap();
            Ok(policy_hash(
                &id,
                &profile,
                new_revision,
                &serde_json::from_str(&profile_rec.policy).unwrap(),
                &serde_json::from_str(&rec.policy).unwrap(),
            )
            .unwrap())
        })
        .unwrap();
    exec(
        &fx,
        &format!(
            "UPDATE approvals SET profile_revision = {new_revision}, policy_hash = '{forged_hash}'"
        ),
    );
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::NotApproved)
    );
}

#[test]
fn a_bumped_config_rev_or_edited_automation_row_needs_review() {
    let (fx, id, _, _) = approved_fixture();
    exec(&fx, "UPDATE automations SET config_rev = config_rev + 1");
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::PolicyChanged)
    );
    let (fx, id, _, _) = approved_fixture();
    // The stored automation policy edited behind the approval's back.
    exec(
        &fx,
        "UPDATE automations SET policy = replace(policy, '\"retries\":2', '\"retries\":3')",
    );
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::PolicyChanged)
    );
    // An unparsable stored row is never repaired silently.
    exec(&fx, "UPDATE automations SET policy = '{not json'");
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::PolicyChanged)
    );
}

#[test]
fn an_unknown_hash_version_is_unsupported() {
    let (fx, id, _, _) = approved_fixture();
    exec(&fx, "UPDATE approvals SET hash_version = 2");
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::UnsupportedVersion)
    );
}

#[test]
fn a_missing_or_unusable_key_means_key_unavailable_for_every_approval() {
    let (fx, id, profile, _) = approved_fixture();
    let second = fx.new_automation("Second", &profile, Fixture::watch());
    let memory = fx.memory.as_ref().unwrap();
    memory.set(KeyLoad::Missing);
    for automation in [&id, &second] {
        assert_eq!(
            verdict(&fx, automation),
            Verdict::NeedsReview(NeedsReview::KeyUnavailable)
        );
    }
    memory.set(KeyLoad::Unusable);
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::KeyUnavailable)
    );
}

/// design §8.4: a key that is recreated after loss does not revive old approvals.
#[test]
fn a_new_key_does_not_validate_old_approvals() {
    let (fx, id, _, _) = approved_fixture();
    fx.memory
        .as_ref()
        .unwrap()
        .set(KeyLoad::Key(ApprovalKey::for_tests(77)));
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::NotApproved)
    );
}

/// SE-3.1 with the REAL DPAPI file: deleted key file -> `keyUnavailable`; the same blob read under
/// another entropy (another DPAPI scope) -> `keyUnavailable`.
#[cfg(windows)]
#[test]
fn dpapi_key_deleted_or_from_another_scope_is_key_unavailable() {
    let fx = Fixture::with_dpapi();
    let (profile, _) = fx.profile("P");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    assert_eq!(verdict(&fx, &id), Verdict::Valid);

    // Another DPAPI scope, simulated with a different entropy over the very same file.
    let other_scope = DpapiKeyFile::with_entropy(&fx.store_dir(), b"another scope");
    let verdict_other = fx
        .store
        .with_read(|conn| verify(conn, &other_scope, &id))
        .unwrap();
    assert_eq!(
        verdict_other,
        Verdict::NeedsReview(NeedsReview::KeyUnavailable)
    );

    // Key file deleted.
    std::fs::remove_file(fx.store_dir().join(crate::automation::dpapi::KEY_FILE)).unwrap();
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::KeyUnavailable)
    );
    // The next approval creates a new key; the OLD approval still does not verify.
    fx.keys.load_or_create().unwrap();
    assert_eq!(
        verdict(&fx, &id),
        Verdict::NeedsReview(NeedsReview::NotApproved)
    );
}
