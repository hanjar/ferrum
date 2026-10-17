//! Approval, resume, pause, delete and SP-6.4 tests (FER-522 / P1.1: AP-5, AP-6, SE-1, SE-2,
//! SE-3.2, SP-6.4). Every dialog is an injected closure; time is an injected clock.

use std::cell::{Cell, RefCell};

use super::*;
use crate::automation::commands::{self, SaveRequest, SaveResultDto};
use crate::automation::dpapi::KeyLoad;
use crate::automation::model::{Draft, Runtime};
use crate::automation::test_run::TEST_TTL_MS;
use crate::automation::test_support::{Fixture, T0, with_test_deps};

/// A confirmation that must never be shown.
fn no_dialog(_: &Prompt) -> bool {
    panic!("no dialog expected")
}

fn count(fx: &Fixture, table: &str) -> i64 {
    fx.store
        .with_read(|conn| -> Result<i64, StoreError> {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .map_err(|_| StoreError::Unavailable)
        })
        .unwrap()
}

fn exec(fx: &Fixture, sql: &str) {
    fx.store
        .with_write(|conn| -> Result<(), StoreError> {
            conn.execute_batch(sql).map_err(|_| StoreError::Unavailable)
        })
        .unwrap();
}

fn record(fx: &Fixture, id: &str) -> automations::AutomationRecord {
    fx.store
        .with_read(|conn| -> Result<_, StoreError> { Ok(automations::get(conn, id)?.unwrap()) })
        .unwrap()
}

fn events(fx: &Fixture, id: &str) -> Vec<String> {
    fx.store
        .with_read(|conn| -> Result<Vec<String>, StoreError> {
            Ok(automations::events(conn, id, 50)?
                .into_iter()
                .map(|e| e.text)
                .collect())
        })
        .unwrap()
}

fn verdict(fx: &Fixture, id: &str) -> Verdict {
    fx.store
        .with_read(|conn| policy::verify(conn, fx.keys.as_ref(), id))
        .unwrap()
}

fn approve_req(
    id: Option<&str>,
    expected: Option<i64>,
    draft: &AutomationDraft,
    test_id: &str,
) -> ApproveRequest {
    ApproveRequest {
        id: id.map(str::to_string),
        expected_config_rev: expected,
        draft: draft.clone(),
        test_id: test_id.into(),
    }
}

fn draft_of(fx: &Fixture, name: &str) -> (String, i64, AutomationDraft) {
    let (profile, revision) = fx.profile(&format!("Profile of {name}"));
    let draft = Fixture::auto_draft(name, &profile, Fixture::daily());
    (profile, revision, draft)
}

// ── Approve ──

/// AP-5 / design §8.2: the happy path writes the automation, its approval, the runtime row, the
/// event and `change_seq`, enabled, and the approval verifies.
#[test]
fn approve_happy_path_writes_everything_and_verifies() {
    let fx = Fixture::new();
    let (profile, revision, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    assert!(test.passed);
    let seq_before: String = fx
        .store
        .with_read(|c| -> Result<String, StoreError> {
            c.query_row("SELECT value FROM meta WHERE key='change_seq'", [], |r| {
                r.get(0)
            })
            .map_err(|_| StoreError::Unavailable)
        })
        .unwrap();
    let asked = RefCell::new(Vec::new());
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        |p| {
            asked.borrow_mut().push(p.clone());
            true
        },
    )
    .unwrap();
    let ApproveResultDto::Approved { automation } = out else {
        panic!("approved expected");
    };
    assert_eq!(automation.id, test.automation_id);
    assert!(automation.enabled);
    assert_eq!(automation.config_rev, 1);
    assert_eq!(automation.profile_revision, revision);
    assert_eq!(automation.review, None);
    assert_eq!(asked.borrow().len(), 1, "exactly one native confirmation");
    assert_eq!(asked.borrow()[0].caption, confirm_text::APPROVE_CAPTION);

    assert_eq!(count(&fx, "automations"), 1);
    assert_eq!(count(&fx, "approvals"), 1);
    assert_eq!(count(&fx, "automation_runtime"), 1);
    assert_eq!(count(&fx, "events"), 1);
    let approval = fx
        .store
        .with_read(|c| approvals::get(c, &automation.id))
        .unwrap()
        .unwrap();
    assert_eq!(approval.policy_hash, test.policy_hash);
    assert_eq!(approval.profile_id, profile);
    assert_eq!(approval.profile_revision, revision);
    assert_eq!(approval.hash_version, 1);
    assert_eq!(approval.mac.len(), 64);
    assert_eq!(approval.approved_ms, T0);
    assert!(
        events(&fx, &automation.id)[0]
            .starts_with("Approved \"Nightly\" rev 1 with the reviewed policies")
    );
    let seq_after: String = fx
        .store
        .with_read(|c| -> Result<String, StoreError> {
            c.query_row("SELECT value FROM meta WHERE key='change_seq'", [], |r| {
                r.get(0)
            })
            .map_err(|_| StoreError::Unavailable)
        })
        .unwrap();
    assert_ne!(seq_before, seq_after, "hosts are told to re-read the store");
    assert_eq!(verdict(&fx, &automation.id), Verdict::Valid);
    // A test is single use.
    let again = fx.approve(None, None, &draft, &test.test_id, true);
    assert_eq!(again, ApproveResultDto::Stale);
}

/// design §8.2 atomicity: when the last write of the transaction fails, NOTHING of the approval
/// remains (automation, approval, runtime row, event).
#[test]
fn approve_is_atomic_when_a_write_fails() {
    let fx = Fixture::new();
    let (_, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    exec(
        &fx,
        "CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    );
    let result = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        |_| true,
    );
    assert!(result.is_err());
    for table in ["automations", "approvals", "automation_runtime", "events"] {
        assert_eq!(count(&fx, table), 0, "{table} rolled back");
    }
}

/// SE-3.2: no passed test -> `stale`, and the user is never asked.
#[test]
fn approve_without_a_passed_test_is_stale_and_never_asks() {
    let fx = Fixture::new();
    let (profile, _, draft) = draft_of(&fx, "Nightly");
    let asked = Cell::new(0);
    let attempt = |test_id: &str| {
        approve_in(
            &fx.store,
            approve_req(None, None, &draft, test_id),
            &fx.deps(),
            |_| {
                asked.set(asked.get() + 1);
                true
            },
        )
        .unwrap()
    };
    // An id that was never issued.
    assert_eq!(
        attempt("99999999-9999-4999-8999-999999999999"),
        ApproveResultDto::Stale
    );
    // A test that ran but did not pass (a background runtime with no runner).
    let mut background = Fixture::daily();
    background.runtime = Runtime::Background;
    let failing = Fixture::auto_draft("Nightly", &profile, background);
    let result = fx.test(None, &failing);
    assert!(!result.passed);
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &failing, &result.test_id),
        &fx.deps(),
        |_| {
            asked.set(asked.get() + 1);
            true
        },
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    assert_eq!(asked.get(), 0);
    assert_eq!(count(&fx, "automations"), 0);
}

/// SE-3.2: a test for ANOTHER policy hash is not accepted.
#[test]
fn a_test_for_another_hash_is_stale() {
    let fx = Fixture::new();
    let (profile, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    let mut other = Fixture::daily();
    if let model::Trigger::Schedule(s) = &mut other.trigger {
        s.time = "03:30".into();
    }
    let changed = Fixture::auto_draft("Nightly", &profile, other);
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &changed, &test.test_id),
        &fx.deps(),
        no_dialog,
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    // The tested draft itself is still approvable.
    assert!(matches!(
        fx.approve(None, None, &draft, &test.test_id, true),
        ApproveResultDto::Approved { .. }
    ));
}

/// SE-3.2: a test older than 15 minutes (fake clock) is stale; exactly 15 minutes still counts.
#[test]
fn a_test_older_than_fifteen_minutes_is_stale() {
    let fx = Fixture::new();
    let (_, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    fx.advance(TEST_TTL_MS + 1);
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        no_dialog,
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    assert_eq!(count(&fx, "automations"), 0);

    let test = fx.test(None, &draft);
    fx.advance(TEST_TTL_MS);
    assert!(matches!(
        fx.approve(None, None, &draft, &test.test_id, true),
        ApproveResultDto::Approved { .. }
    ));
}

/// The test expires WHILE the dialog is open: the answer arrives late, nothing is stored.
#[test]
fn a_test_that_expires_while_the_dialog_is_open_is_stale() {
    let fx = Fixture::new();
    let (_, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        |_| {
            fx.advance(TEST_TTL_MS + 5);
            true
        },
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    assert_eq!(count(&fx, "automations"), 0);
    assert_eq!(count(&fx, "approvals"), 0);
}

/// AP-5: Cancel writes nothing, and the test stays usable.
#[test]
fn confirm_false_is_canceled_and_nothing_is_written() {
    let fx = Fixture::new();
    let (_, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    let out = fx.approve(None, None, &draft, &test.test_id, false);
    assert_eq!(out, ApproveResultDto::Canceled);
    for table in ["automations", "approvals", "automation_runtime", "events"] {
        assert_eq!(count(&fx, table), 0, "{table}");
    }
    assert!(matches!(
        fx.approve(None, None, &draft, &test.test_id, true),
        ApproveResultDto::Approved { .. }
    ));
}

/// design §8.2: the profile is saved by another connection while the dialog is open -> `stale`.
#[test]
fn a_profile_saved_while_the_dialog_is_open_is_stale() {
    let fx = Fixture::new();
    let (profile, revision, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        |_| {
            fx.bump_profile(&profile, revision, "*.bak");
            true
        },
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    assert_eq!(count(&fx, "automations"), 0);
    assert_eq!(count(&fx, "approvals"), 0);
}

/// An edit of an existing automation races with another save of the same automation while the
/// dialog is open (`config_rev` moved) -> `stale`, nothing stored.
#[test]
fn a_concurrent_save_of_the_automation_during_the_dialog_is_stale() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "Nightly");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    let rev = record(&fx, &id).config_rev;
    let mut edited = Fixture::daily();
    if let model::Trigger::Schedule(s) = &mut edited.trigger {
        s.time = "04:00".into();
    }
    let draft = Fixture::auto_draft("Nightly", &profile, edited);
    let test = fx.test(Some(&id), &draft);
    let out = approve_in(
        &fx.store,
        approve_req(Some(&id), Some(rev), &draft, &test.test_id),
        &fx.deps(),
        |_| {
            exec(&fx, "UPDATE automations SET config_rev = config_rev + 1");
            true
        },
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    assert_eq!(
        record(&fx, &id).config_rev,
        rev + 1,
        "only the other writer's change"
    );
}

/// design §8.2: editing re-approves under `config_rev + 1`; a wrong `expectedConfigRev` is stale
/// BEFORE the dialog.
#[test]
fn editing_an_automation_reapproves_with_a_new_config_rev() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "Nightly");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    let mut edited = Fixture::daily();
    edited.retries = 3;
    let draft = Fixture::auto_draft("Nightly renamed", &profile, edited);
    let test = fx.test(Some(&id), &draft);

    let stale = approve_in(
        &fx.store,
        approve_req(Some(&id), Some(7), &draft, &test.test_id),
        &fx.deps(),
        no_dialog,
    )
    .unwrap();
    assert_eq!(stale, ApproveResultDto::Stale);

    let captured = RefCell::new(String::new());
    let out = approve_in(
        &fx.store,
        approve_req(Some(&id), Some(1), &draft, &test.test_id),
        &fx.deps(),
        |p| {
            *captured.borrow_mut() = p.caption.to_string();
            true
        },
    )
    .unwrap();
    let ApproveResultDto::Approved { automation } = out else {
        panic!("approved")
    };
    assert_eq!(automation.config_rev, 2);
    assert_eq!(automation.name, "Nightly renamed");
    assert_eq!(*captured.borrow(), confirm_text::REAPPROVE_CAPTION);
    assert!(events(&fx, &id)[0].starts_with("Re-approved"));
    assert_eq!(count(&fx, "automations"), 1);
    assert_eq!(
        count(&fx, "approvals"),
        1,
        "one approval row per automation"
    );
    assert_eq!(verdict(&fx, &id), Verdict::Valid);
}

/// A test made for one automation cannot approve another, and a new automation's candidate id
/// cannot collide with an existing one.
#[test]
fn a_test_belongs_to_its_automation_id() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "A");
    let a = fx.new_automation("A", &profile, Fixture::daily());
    let b = fx.new_automation("B", &profile, Fixture::watch());
    let draft = Fixture::auto_draft("B", &profile, Fixture::watch());
    // A test for B, replayed against A's id.
    let test_for_b = fx.test(Some(&b), &draft);
    let rev_a = record(&fx, &a).config_rev;
    let out = approve_in(
        &fx.store,
        approve_req(Some(&a), Some(rev_a), &draft, &test_for_b.test_id),
        &fx.deps(),
        no_dialog,
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
    // A test of an edit (which names an existing id) replayed as a NEW automation (no id): the
    // candidate id already exists.
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test_for_b.test_id),
        &fx.deps(),
        no_dialog,
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Stale);
}

#[test]
fn a_busy_native_dialog_stores_nothing_and_reports_busy() {
    let fx = Fixture::new();
    let (_, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    let err = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        |_| -> Result<bool, crate::domain::FsError> {
            Err(crate::domain::FsError::new(
                crate::domain::ErrorCode::Busy,
                "busy",
            ))
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "busy");
    assert_eq!(count(&fx, "automations"), 0);
}

/// SE-7 / LC-3: malformed ids, invalid drafts and a read-only store are refused BEFORE any dialog.
#[test]
fn input_errors_and_a_read_only_store_are_refused_before_the_dialog() {
    let fx = Fixture::new();
    let (profile, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    let never = |_: &Prompt| -> bool { panic!("no dialog") };

    for bad in [
        approve_req(Some("not-an-id"), Some(1), &draft, &test.test_id),
        approve_req(None, None, &draft, "x"),
    ] {
        let err = approve_in(&fx.store, bad, &fx.deps(), never).unwrap_err();
        assert_eq!(err.code, "invalid_input");
    }
    let mut broken = draft.clone();
    broken.policy.retries = 9;
    broken.name = "  ".into();
    let err = approve_in(
        &fx.store,
        approve_req(None, None, &broken, &test.test_id),
        &fx.deps(),
        never,
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_input");
    // Unknown automation / profile.
    let err = approve_in(
        &fx.store,
        approve_req(
            Some("99999999-9999-4999-8999-999999999999"),
            Some(1),
            &draft,
            &test.test_id,
        ),
        &fx.deps(),
        never,
    )
    .unwrap_err();
    assert_eq!(err.code, "not_found");
    let mut orphan = draft.clone();
    orphan.profile_id = "99999999-9999-4999-8999-999999999999".into();
    let err = approve_in(
        &fx.store,
        approve_req(None, None, &orphan, &test.test_id),
        &fx.deps(),
        never,
    )
    .unwrap_err();
    assert_eq!(err.code, "not_found");
    let _ = profile;

    // A newer-schema store is read-only: nothing is asked.
    let dir = tempfile::tempdir().unwrap();
    Store::open(dir.path(), "0.2.0");
    {
        let conn = rusqlite::Connection::open(dir.path().join("automation.db")).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    let read_only = Store::open(dir.path(), "0.2.0");
    let err = approve_in(
        &read_only,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        never,
    )
    .unwrap_err();
    assert_eq!(err.code, "store_read_only");
}

/// SE-1: no input field behaves like a "confirmed" flag - extra keys are refused.
#[test]
fn se_1_requests_refuse_unknown_fields() {
    let draft = r#"{"name":"N","profileId":"22222222-2222-4222-8222-222222222222","policy":{"trigger":{"kind":"schedule","freq":{"kind":"daily"},"time":"02:00","tz":"UTC","missed":"skip"},"runtime":"app","retries":2,"conditions":{}}}"#;
    let ok = format!(r#"{{"draft":{draft},"testId":"t"}}"#);
    assert!(serde_json::from_str::<ApproveRequest>(&ok).is_ok());
    for extra in ["confirmed", "approved", "force"] {
        let bad = format!(r#"{{"draft":{draft},"testId":"t","{extra}":true}}"#);
        assert!(
            serde_json::from_str::<ApproveRequest>(&bad).is_err(),
            "{extra} must be refused"
        );
    }
    let bad_draft = draft.replace("\"name\":\"N\"", "\"name\":\"N\",\"confirmed\":true");
    assert!(serde_json::from_str::<AutomationDraft>(&bad_draft).is_err());
    let bad_policy = draft.replace("\"retries\":2", "\"retries\":2,\"approved\":true");
    assert!(serde_json::from_str::<AutomationDraft>(&bad_policy).is_err());
    let bad_test = format!(r#"{{"draft":{draft},"confirmed":true}}"#);
    assert!(serde_json::from_str::<TestRequest>(&bad_test).is_err());
}

// ── Resume / enable ──

fn paused(fx: &Fixture) -> (String, String, i64) {
    let (profile, revision, _) = draft_of(fx, "Nightly");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    pause_in(&fx.store, &id, PauseMode::Finish, fx.clock()).unwrap();
    (id, profile, revision)
}

/// SE-2.1 / AP-5: resume with a valid approval and a confirmation enables it.
#[test]
fn resume_with_a_valid_approval_and_confirmation_enables() {
    let fx = Fixture::new();
    let (id, _, _) = paused(&fx);
    assert!(!record(&fx, &id).enabled);
    let asked = RefCell::new(Vec::new());
    let out = set_enabled_in(&fx.store, &id, true, &fx.deps(), |p| {
        asked.borrow_mut().push(p.clone());
        true
    })
    .unwrap();
    assert_eq!(out, SetEnabledResultDto::Enabled);
    assert!(record(&fx, &id).enabled);
    assert_eq!(asked.borrow().len(), 1);
    assert_eq!(asked.borrow()[0].caption, confirm_text::RESUME_CAPTION);
    assert!(asked.borrow()[0].text.contains("Resume \"Nightly\"?"));
    assert!(events(&fx, &id)[0].starts_with("Resumed \"Nightly\""));
    assert_eq!(verdict(&fx, &id), Verdict::Valid);
}

#[test]
fn resume_canceled_changes_nothing_and_enabled_automations_do_not_ask() {
    let fx = Fixture::new();
    let (id, _, _) = paused(&fx);
    let out = set_enabled_in(&fx.store, &id, true, &fx.deps(), |_| false).unwrap();
    assert_eq!(out, SetEnabledResultDto::Canceled);
    assert!(!record(&fx, &id).enabled);
    // Enable for real, then enabling again asks nothing.
    set_enabled_in(&fx.store, &id, true, &fx.deps(), |_| true).unwrap();
    let out = set_enabled_in(&fx.store, &id, true, &fx.deps(), no_dialog).unwrap();
    assert_eq!(out, SetEnabledResultDto::Enabled);
}

/// AP-6: a changed profile -> `needs_review` WITHOUT showing the dialog, and it stays paused.
#[test]
fn resume_after_a_profile_change_is_needs_review_without_a_dialog() {
    let fx = Fixture::new();
    let (id, profile, revision) = paused(&fx);
    fx.bump_profile(&profile, revision, "*.bak");
    let err = set_enabled_in(&fx.store, &id, true, &fx.deps(), |_| -> bool {
        panic!("no dialog for a stale approval")
    })
    .unwrap_err();
    assert_eq!(err.code, "needs_review");
    assert!(!record(&fx, &id).enabled);
}

/// design §8.4: a missing key means every approval is unverifiable -> `needs_review`, no dialog.
#[test]
fn resume_without_the_key_is_needs_review_without_a_dialog() {
    let fx = Fixture::new();
    let (id, _, _) = paused(&fx);
    fx.memory.as_ref().unwrap().set(KeyLoad::Missing);
    let err = set_enabled_in(&fx.store, &id, true, &fx.deps(), |_| -> bool {
        panic!("no dialog")
    })
    .unwrap_err();
    assert_eq!(err.code, "needs_review");
}

/// The approval is verified AGAIN after the dialog: a profile saved while it was open means the
/// automation is not enabled.
#[test]
fn resume_verifies_again_after_the_dialog() {
    let fx = Fixture::new();
    let (id, profile, revision) = paused(&fx);
    let err = set_enabled_in(&fx.store, &id, true, &fx.deps(), |_| {
        fx.bump_profile(&profile, revision, "*.bak");
        true
    })
    .unwrap_err();
    assert_eq!(err.code, "needs_review");
    assert!(!record(&fx, &id).enabled);
}

/// The automation re-approved by someone else while the dialog showed the old policy: the resume
/// does not enable something the user did not see.
#[test]
fn resume_refuses_a_policy_other_than_the_one_shown() {
    let fx = Fixture::new();
    let (id, profile, _) = paused(&fx);
    let mut other = Fixture::daily();
    other.retries = 0;
    let draft = Fixture::auto_draft("Nightly", &profile, other);
    let err = set_enabled_in(&fx.store, &id, true, &fx.deps(), |_| {
        // Another connection re-approves a different policy (config_rev 2), leaving it disabled.
        let test = fx.test(Some(&id), &draft);
        approve_in(
            &fx.store,
            approve_req(Some(&id), Some(1), &draft, &test.test_id),
            &fx.deps(),
            |_| true,
        )
        .unwrap();
        exec(&fx, "UPDATE automations SET enabled = 0");
        true
    })
    .unwrap_err();
    assert_eq!(err.code, "stale");
    assert!(!record(&fx, &id).enabled);
}

// ── Pause / disable / delete ──

/// RQ-8.3 / design §7.2: pause is the safe direction, no dialog, clears the queue.
#[test]
fn pause_needs_no_dialog_clears_the_queue_and_is_idempotent() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "Nightly");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    exec(
        &fx,
        &format!(
            "INSERT INTO queue(automation_id, profile_id, host_class, trigger_kind, trigger_text, enqueued_ms) VALUES ('{id}', '{profile}', 'app', 'schedule', 'Scheduled', 1)"
        ),
    );
    assert_eq!(count(&fx, "queue"), 1);
    for mode in [PauseMode::Finish, PauseMode::Stop] {
        let out = pause_in(&fx.store, &id, mode, fx.clock()).unwrap();
        assert_eq!(out, PauseResultDto::Paused);
    }
    assert!(!record(&fx, &id).enabled);
    assert_eq!(count(&fx, "queue"), 0);
    // The second pause did not log a second event.
    assert_eq!(
        events(&fx, &id)
            .iter()
            .filter(|e| e.starts_with("Paused"))
            .count(),
        1
    );
    assert_eq!(
        pause_in(
            &fx.store,
            "99999999-9999-4999-8999-999999999999",
            PauseMode::Stop,
            fx.clock()
        )
        .unwrap_err()
        .code,
        "not_found"
    );
    assert_eq!(
        pause_in(&fx.store, "nope", PauseMode::Stop, fx.clock())
            .unwrap_err()
            .code,
        "invalid_input"
    );
}

/// design §7.2: disabling with a run in progress asks the UI to choose finish or stop.
#[test]
fn disabling_with_a_running_run_needs_a_choice() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "Nightly");
    let id = fx.new_automation("Nightly", &profile, Fixture::daily());
    exec(
        &fx,
        &format!(
            "INSERT INTO runs(automation_id, profile_id, profile_revision, host_class, trigger_kind, trigger_text, started_ms, phase) VALUES ('{id}', '{profile}', 1, 'app', 'schedule', 'Scheduled', 1, 'sync')"
        ),
    );
    let out = set_enabled_in(&fx.store, &id, false, &fx.deps(), |_| -> bool {
        panic!("never a dialog to disable")
    })
    .unwrap();
    assert_eq!(out, SetEnabledResultDto::NeedsChoice);
    assert!(record(&fx, &id).enabled);
    // After the choice, pause goes through even with the run in progress.
    pause_in(&fx.store, &id, PauseMode::Stop, fx.clock()).unwrap();
    assert!(!record(&fx, &id).enabled);
    // And a plain disable without a run is a pause.
    let (profile2, _, _) = draft_of(&fx, "Other");
    let other = fx.new_automation("Other", &profile2, Fixture::daily());
    let out = set_enabled_in(&fx.store, &other, false, &fx.deps(), |_| -> bool {
        panic!("no dialog")
    })
    .unwrap();
    assert_eq!(out, SetEnabledResultDto::Paused);
}

/// AP-5 / SP-9.5: delete removes the automation and everything on it; the profile can then be
/// deleted, and could not be before.
#[test]
fn delete_removes_everything_and_releases_the_profile() {
    let fx = Fixture::new();
    let (profile, revision, _) = draft_of(&fx, "Nightly");
    let id = fx.new_automation("Nightly", &profile, Fixture::watch());
    exec(
        &fx,
        &format!(
            "INSERT INTO queue(automation_id, profile_id, host_class, trigger_kind, trigger_text, enqueued_ms) VALUES ('{id}', '{profile}', 'app', 'watch', 'x', 1)"
        ),
    );
    // In use while the automation exists (SP-9.5).
    let err = commands::delete_in(&fx.store, &profile, revision).unwrap_err();
    assert_eq!(err.code, "in_use");
    assert_eq!(
        delete_in(&fx.store, &id).unwrap(),
        DeleteAutomationDto::Deleted
    );
    for table in [
        "automations",
        "approvals",
        "automation_runtime",
        "queue",
        "events",
    ] {
        assert_eq!(count(&fx, table), 0, "{table}");
    }
    assert_eq!(delete_in(&fx.store, &id).unwrap_err().code, "not_found");
    commands::delete_in(&fx.store, &profile, revision).unwrap();
}

// ── SP-6.4: profile save with dependants ──

/// SP-6.4: a policy-changing profile save makes EVERY dependant `profileChanged{from,to}`, removes
/// its queued entries, resets its watch state and logs one event per dependant, all in the same
/// transaction; automations on other profiles are untouched.
#[test]
fn sp_6_4_profile_save_marks_every_dependant_in_the_same_transaction() {
    let fx = Fixture::new();
    let (profile, revision, _) = draft_of(&fx, "Shared");
    let (other_profile, _, _) = draft_of(&fx, "Unrelated");
    let a = fx.new_automation("Schedule one", &profile, Fixture::daily());
    let b = fx.new_automation("Watch two", &profile, Fixture::watch());
    let c = fx.new_automation("Unrelated one", &other_profile, Fixture::daily());
    exec(
        &fx,
        &format!(
            "INSERT INTO queue(automation_id, profile_id, host_class, trigger_kind, trigger_text, enqueued_ms) VALUES ('{a}', '{profile}', 'app', 'schedule', 'x', 1);
             UPDATE automation_runtime SET trigger_state = 'quiet', quiet_until_ms = 99, burst_started_ms = 5, pending_rerun = 1, check_reason = 'x' WHERE automation_id = '{b}'"
        ),
    );
    assert_eq!(verdict(&fx, &a), Verdict::Valid);

    let new_revision = fx.bump_profile(&profile, revision, "*.bak");
    assert_eq!(new_revision, revision + 1);
    for id in [&a, &b] {
        assert_eq!(
            verdict(&fx, id),
            Verdict::NeedsReview(policy::NeedsReview::ProfileChanged {
                from: revision,
                to: new_revision
            }),
            "dependant {id}"
        );
        let texts = events(&fx, id);
        assert!(
            texts[0].starts_with(&format!("Profile changed to rev {new_revision}")),
            "{texts:?}"
        );
        assert!(texts[0].contains("filters"));
        assert!(texts[0].ends_with("held until you review"));
    }
    assert_eq!(
        verdict(&fx, &c),
        Verdict::Valid,
        "other profiles are untouched"
    );
    assert_eq!(events(&fx, &c).len(), 1, "only its own approval event");
    assert_eq!(count(&fx, "queue"), 0, "queued runs removed");
    let (state, quiet, burst, pending, reason): (String, Option<i64>, Option<i64>, i64, Option<String>) = fx
        .store
        .with_read(|conn| -> Result<_, StoreError> {
            conn.query_row(
                "SELECT trigger_state, quiet_until_ms, burst_started_ms, pending_rerun, check_reason FROM automation_runtime WHERE automation_id = ?1",
                [&b],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(|_| StoreError::Unavailable)
        })
        .unwrap();
    assert_eq!(
        (state.as_str(), quiet, burst, pending, reason),
        ("watching", None, None, 0, None)
    );
    // The DTO reports it too (AP-6).
    let dto = fx
        .store
        .with_read(|conn| {
            dto_of(
                conn,
                fx.keys.as_ref(),
                &automations::get(conn, &a)?.unwrap(),
            )
        })
        .unwrap();
    assert_eq!(
        dto.review,
        Some(crate::automation::dto::NeedsReviewDto::ProfileChanged {
            from: revision,
            to: new_revision
        })
    );
    assert_eq!(dto.profile_revision, new_revision);
}

/// SP-6.4 scope: a rename alone is not a policy change - dependants stay valid, no event.
#[test]
fn sp_6_4_a_profile_rename_leaves_dependants_valid() {
    let fx = Fixture::new();
    let (profile, revision, _) = draft_of(&fx, "Shared");
    let a = fx.new_automation("One", &profile, Fixture::daily());
    let current = commands::get_in(&fx.store, &profile, &Fixture::probe()).unwrap();
    let out = commands::save_in(
        &fx.store,
        SaveRequest {
            id: Some(profile.clone()),
            expected_revision: Some(revision),
            draft: Draft {
                name: "Renamed profile".into(),
                policy: current.policy,
            },
        },
        &Fixture::probe(),
        |_| true,
        T0,
    )
    .unwrap();
    assert!(matches!(out, SaveResultDto::Saved { .. }));
    assert_eq!(verdict(&fx, &a), Verdict::Valid);
    assert_eq!(events(&fx, &a).len(), 1);
}

/// SP-6.4 atomicity: if logging the dependants' events fails, the profile save rolls back too.
#[test]
fn sp_6_4_failing_dependant_bookkeeping_rolls_the_profile_save_back() {
    let fx = Fixture::new();
    let (profile, revision, _) = draft_of(&fx, "Shared");
    let a = fx.new_automation("One", &profile, Fixture::daily());
    exec(
        &fx,
        "CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    );
    let current = commands::get_in(&fx.store, &profile, &Fixture::probe()).unwrap();
    let mut policy = current.policy.clone();
    policy.exclude.push("*.bak".into());
    let result = commands::save_in(
        &fx.store,
        SaveRequest {
            id: Some(profile.clone()),
            expected_revision: Some(revision),
            draft: Draft {
                name: current.name,
                policy,
            },
        },
        &Fixture::probe(),
        |_| true,
        T0,
    );
    assert!(result.is_err());
    let after = commands::get_in(&fx.store, &profile, &Fixture::probe()).unwrap();
    assert_eq!(after.revision, revision, "the profile revision rolled back");
    assert_eq!(verdict(&fx, &a), Verdict::Valid);
}

/// design §7.1 `usedBy`: the profile list counts the automations using each profile.
#[test]
fn the_profile_list_reports_used_by() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "Shared");
    let (unused, _) = fx.profile("Alone");
    fx.new_automation("One", &profile, Fixture::daily());
    fx.new_automation("Two", &profile, Fixture::watch());
    let list = commands::list_in(&fx.store, &Fixture::probe()).unwrap();
    let used = |id: &str| list.iter().find(|p| p.id == id).unwrap().used_by;
    assert_eq!(used(&profile), 2);
    assert_eq!(used(&unused), 0);
}

// ── Validate / test ──

#[test]
fn validate_reports_errors_and_readiness_checks() {
    let fx = Fixture::new();
    let (profile, _, draft) = draft_of(&fx, "Nightly");
    let probe = Fixture::probe();
    let budget = std::time::Duration::from_secs(2);
    let ok = validate_in(&fx.store, &draft, &probe, budget, false).unwrap();
    assert!(ok.errors.is_empty());
    assert_eq!(ok.checks.len(), 2);
    assert!(ok.checks.iter().all(|c| c.status == StepStatus::Ok));
    assert!(ok.next.is_empty());

    let mut bad = draft.clone();
    bad.policy.retries = 5;
    bad.policy.runtime = Runtime::Background;
    let out = validate_in(&fx.store, &bad, &probe, budget, false).unwrap();
    assert!(out.errors.iter().any(|e| e.field == "retries"));
    assert!(
        out.checks
            .iter()
            .any(|c| c.id == "runtime" && c.status == StepStatus::Fail)
    );

    let mut orphan = draft.clone();
    orphan.profile_id = "99999999-9999-4999-8999-999999999999".into();
    let out = validate_in(&fx.store, &orphan, &probe, budget, false).unwrap();
    assert_eq!(out.errors[0].field, "profileId");
    let _ = profile;
}

#[test]
fn test_in_records_the_hash_and_names_the_id_of_a_new_automation() {
    let fx = Fixture::new();
    let (profile, revision, draft) = draft_of(&fx, "Nightly");
    let result = fx.test(None, &draft);
    assert!(result.passed);
    assert!(model::is_profile_id(&result.automation_id));
    assert!(model::is_profile_id(&result.test_id));
    let expected = policy::policy_hash(
        &result.automation_id,
        &profile,
        revision,
        &fx.profile_policy(&profile),
        &draft.policy,
    )
    .unwrap();
    assert_eq!(result.policy_hash, expected);
    assert_eq!(fx.ledger.len(), 1);
    // Editing an unknown automation is `not_found`.
    let err = with_test_deps(|deps| {
        test_in(
            &fx.store,
            &fx.ledger,
            TestRequest {
                id: Some("99999999-9999-4999-8999-999999999999".into()),
                draft: draft.clone(),
            },
            deps,
            fx.clock(),
        )
    })
    .unwrap_err();
    assert_eq!(err.code, "not_found");
}

// ── Prompt text through the real flow (SE-2, SE-2.2) ──

/// The dialog text comes from the stored profile and the validated draft - the permanent warning
/// first, bounded, naming the automation - and the webview never supplied any of its words.
#[test]
fn the_dialog_text_is_built_from_stored_data_with_the_permanent_warning_first() {
    let fx = Fixture::new();
    let (profile, _) = fx.permanent_profile("Danger");
    let draft = Fixture::auto_draft("Danger nightly", &profile, Fixture::daily());
    let test = fx.test(None, &draft);
    let seen = RefCell::new(String::new());
    let out = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        |p| {
            *seen.borrow_mut() = p.text.clone();
            false
        },
    )
    .unwrap();
    assert_eq!(out, ApproveResultDto::Canceled);
    let text = seen.borrow();
    assert!(text.starts_with(confirm_text::PERMANENT_WARNING), "{text}");
    assert!(text.contains("Enable automation \"Danger nightly\"?"));
    assert!(text.contains("\"Danger\" rev 1"));
    assert!(text.chars().count() <= confirm_text::PROMPT_MAX_CHARS);
}

// ── FER-522 rework 1 ──

/// A key that is made again after loss announces itself on EVERY existing automation, whatever
/// the user answers to the new approval's dialog.
#[test]
fn a_recreated_key_is_logged_on_every_automation() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "A");
    let old = fx.new_automation("Old one", &profile, Fixture::daily());
    fx.memory.as_ref().unwrap().set(KeyLoad::Missing);
    assert_eq!(
        verdict(&fx, &old),
        Verdict::NeedsReview(policy::NeedsReview::KeyUnavailable)
    );
    let draft = Fixture::auto_draft("New one", &profile, Fixture::watch());
    let test = fx.test(None, &draft);
    // The user cancels - the key was still created, and the event still appears.
    assert_eq!(
        fx.approve(None, None, &draft, &test.test_id, false),
        ApproveResultDto::Canceled
    );
    assert!(
        events(&fx, &old)
            .iter()
            .any(|e| e.starts_with("The approval key was created again")),
        "{:?}",
        events(&fx, &old)
    );
    // The old approval does not verify under the new key; the new one is made now.
    assert_eq!(
        verdict(&fx, &old),
        Verdict::NeedsReview(policy::NeedsReview::NotApproved)
    );
    // Not announced again for the next approval.
    let before = events(&fx, &old).len();
    let again = fx.test(None, &draft);
    fx.approve(None, None, &draft, &again.test_id, false);
    assert_eq!(events(&fx, &old).len(), before);
}

/// An unreadable key file (transient I/O problem) stops the approval before any dialog: no
/// rotation, nothing stored.
#[test]
fn an_unreadable_key_stops_the_approval_before_the_dialog() {
    let fx = Fixture::new();
    let (_, _, draft) = draft_of(&fx, "Nightly");
    let test = fx.test(None, &draft);
    fx.memory.as_ref().unwrap().set(KeyLoad::Unreadable);
    let err = approve_in(
        &fx.store,
        approve_req(None, None, &draft, &test.test_id),
        &fx.deps(),
        no_dialog,
    )
    .unwrap_err();
    assert_eq!(err.code, "store_unavailable");
    assert_eq!(count(&fx, "automations"), 0);
    assert_eq!(fx.memory.as_ref().unwrap().load(), KeyLoad::Unreadable);
}

/// Re-approval shows the STORED name and states a rename; the text says it also enables.
#[test]
fn reapprove_prompt_uses_the_stored_name_and_says_enable() {
    let fx = Fixture::new();
    let (profile, _, _) = draft_of(&fx, "Shared");
    let id = fx.new_automation("Stored name", &profile, Fixture::daily());
    let rev = record(&fx, &id).config_rev;
    let draft = Fixture::auto_draft("Brand new name", &profile, Fixture::daily());
    let test = fx.test(Some(&id), &draft);
    let seen = RefCell::new(String::new());
    approve_in(
        &fx.store,
        approve_req(Some(&id), Some(rev), &draft, &test.test_id),
        &fx.deps(),
        |p| {
            *seen.borrow_mut() = p.text.clone();
            false
        },
    )
    .unwrap();
    let text = seen.borrow();
    assert!(
        text.contains("Re-approve and enable automation \"Stored name\"?"),
        "{text}"
    );
    assert!(text.contains("It will be renamed to \"Brand new name\"."));
    assert!(text.contains("Delete guard: stops above 50 items and 20%."));
}
