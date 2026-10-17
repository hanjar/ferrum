//! Test run and ledger tests (FER-522 / P1.1: AP-4.1, AP-4.2, AP-4.4, SE-3.2).

use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::automation::test_support::{Fixture, passing_dry_run, with_test_deps};

fn summary(total: usize, overwrites: usize, deletes: usize) -> PlanSummary {
    PlanSummary {
        total,
        overwrites,
        deletes,
        ..Default::default()
    }
}

/// Run the steps for a fresh real profile with the given facts.
fn steps_with(
    automation: AutomationPolicy,
    dry: &dyn Fn(&ProfilePolicy) -> DryRun,
    access: &dyn Fn(&str, bool) -> bool,
    background_ready: bool,
) -> Vec<StepDto> {
    let fx = Fixture::new();
    let (profile, revision) = fx.profile("Photos");
    let policy = fx.profile_policy(&profile);
    let probe = Fixture::probe();
    let deps = TestDeps {
        probe: &probe,
        budget: Duration::from_secs(2),
        dry_run: dry,
        access,
        background_ready,
    };
    run_steps("Photos", revision, &policy, &automation, &deps)
}

fn ids(steps: &[StepDto]) -> Vec<&str> {
    steps.iter().map(|s| s.id.as_str()).collect()
}

fn status(steps: &[StepDto], id: &str) -> StepStatus {
    steps.iter().find(|s| s.id == id).unwrap().status
}

/// AP-4.1 / AP-4.2: a healthy test walks profile, source, target, compare, conflicts, guard and
/// runtime, in that order, and passes.
#[test]
fn a_healthy_test_shows_every_step_in_order_and_passes() {
    let dry = |_: &ProfilePolicy| DryRun::Done {
        summary: summary(6, 2, 0),
        conflicts: 0,
        guard: GuardVerdict::Ok,
    };
    let steps = steps_with(Fixture::daily(), &dry, &|_, _| true, false);
    assert_eq!(
        ids(&steps),
        [
            "profile",
            "source",
            "target",
            "compare",
            "conflicts",
            "guard",
            "runtime"
        ]
    );
    assert!(
        steps.iter().all(|s| s.status == StepStatus::Ok),
        "{steps:?}"
    );
    assert!(passed(&steps));
    // Spec text: "Comparison: 6 operations (2 update, 4 new, 0 delete)".
    let compare = steps.iter().find(|s| s.id == "compare").unwrap();
    assert_eq!(
        compare.text,
        "Comparison: 6 operations (2 update, 4 new, 0 delete)."
    );
}

/// AP-4.2: the FIRST failing step ends the test; later steps (including the dry run) never run.
#[test]
fn the_first_failing_step_stops_the_test() {
    let ran = Cell::new(false);
    let dry = |_: &ProfilePolicy| {
        ran.set(true);
        DryRun::Done {
            summary: summary(1, 0, 0),
            conflicts: 0,
            guard: GuardVerdict::Ok,
        }
    };
    // Source unreadable.
    let steps = steps_with(Fixture::daily(), &dry, &|_, write| write, false);
    assert_eq!(ids(&steps), ["profile", "source"]);
    assert_eq!(status(&steps, "source"), StepStatus::Fail);
    assert!(!passed(&steps));
    assert!(!ran.get(), "the compare must not run after a failed step");
    // Target not writable.
    let steps = steps_with(Fixture::daily(), &dry, &|_, write| !write, false);
    assert_eq!(ids(&steps), ["profile", "source", "target"]);
    assert_eq!(status(&steps, "target"), StepStatus::Fail);
    assert!(!ran.get());
}

/// Two-way reads both folders and writes both folders.
#[test]
fn two_way_checks_both_sides_for_reading_and_writing() {
    let fx = Fixture::new();
    let (profile, revision) = fx.profile("Two way");
    let mut policy = fx.profile_policy(&profile);
    policy.mode = crate::automation::model::SyncMode::TwoWay {
        propagate_deletes: false,
    };
    let probe = Fixture::probe();
    let dry = passing_dry_run();
    let asked = std::cell::RefCell::new(Vec::new());
    let access = |path: &str, write: bool| {
        asked.borrow_mut().push((path.to_string(), write));
        true
    };
    let deps = TestDeps {
        probe: &probe,
        budget: Duration::from_secs(2),
        dry_run: &dry,
        access: &access,
        background_ready: false,
    };
    let steps = run_steps("Two way", revision, &policy, &Fixture::daily(), &deps);
    assert!(passed(&steps));
    let asked = asked.borrow();
    for side in [&policy.left.path, &policy.right.path] {
        assert!(asked.contains(&(side.clone(), false)), "read {side}");
        assert!(asked.contains(&(side.clone(), true)), "write {side}");
    }
}

/// A profile whose folder vanished is not ready: the test fails at the first step and says so.
#[test]
fn a_missing_folder_fails_the_profile_step_without_treating_it_as_empty() {
    let fx = Fixture::new();
    let (profile, revision) = fx.profile("Gone");
    let policy = fx.profile_policy(&profile);
    std::fs::remove_dir_all(&policy.right.path).unwrap();
    let probe = Fixture::probe();
    let dry = passing_dry_run();
    let deps = TestDeps {
        probe: &probe,
        budget: Duration::from_secs(2),
        dry_run: &dry,
        access: &|_, _| true,
        background_ready: false,
    };
    let steps = run_steps("Gone", revision, &policy, &Fixture::daily(), &deps);
    assert_eq!(ids(&steps), ["profile"]);
    assert_eq!(steps[0].status, StepStatus::Fail);
    assert!(
        steps[0].text.contains("not treated as empty"),
        "{}",
        steps[0].text
    );
    assert!(!passed(&steps));
}

/// AP-4.1: conflicts, a tripped guard and an empty source are WARNINGS (a real run is held), so
/// the test still passes; a guard that is off warns too.
#[test]
fn conflicts_and_guard_outcomes_warn_but_do_not_fail() {
    let cases: Vec<(GuardVerdict, u32)> = vec![
        (GuardVerdict::Off, 0),
        (
            GuardVerdict::Tripped {
                side: crate::automation::model::Side::Right,
                deletes: 70,
                entries: 100,
                percent: 70,
            },
            0,
        ),
        (
            GuardVerdict::EmptySource {
                empty: crate::automation::model::Side::Left,
            },
            0,
        ),
        (GuardVerdict::Ok, 1),
    ];
    for (guard, conflicts) in cases {
        let dry = move |_: &ProfilePolicy| DryRun::Done {
            summary: summary(10, 1, 3),
            conflicts,
            guard,
        };
        let steps = steps_with(Fixture::daily(), &dry, &|_, _| true, false);
        assert!(passed(&steps), "{guard:?}: {steps:?}");
        if conflicts > 0 {
            assert_eq!(status(&steps, "conflicts"), StepStatus::Warn);
            assert!(
                steps
                    .iter()
                    .find(|s| s.id == "conflicts")
                    .unwrap()
                    .text
                    .contains("never overwritten")
            );
        } else {
            assert_eq!(status(&steps, "guard"), StepStatus::Warn, "{guard:?}");
        }
    }
}

#[test]
fn a_blocked_or_failed_comparison_fails_the_test() {
    let blocked = |_: &ProfilePolicy| DryRun::Blocked(vec!["truncated".into(), "errors".into()]);
    let steps = steps_with(Fixture::daily(), &blocked, &|_, _| true, false);
    assert_eq!(status(&steps, "compare"), StepStatus::Fail);
    assert!(steps.last().unwrap().text.contains("truncated, errors"));
    assert!(!passed(&steps));
    let failed = |_: &ProfilePolicy| DryRun::Failed("not_ready".into());
    let steps = steps_with(Fixture::daily(), &failed, &|_, _| true, false);
    assert_eq!(status(&steps, "compare"), StepStatus::Fail);
    assert!(!passed(&steps));
}

/// RT-1: a background automation passes only with a ready runner.
#[test]
fn the_background_runtime_needs_a_ready_runner() {
    let mut background = Fixture::daily();
    background.runtime = Runtime::Background;
    let dry = passing_dry_run();
    let steps = steps_with(background.clone(), &dry, &|_, _| true, false);
    assert_eq!(status(&steps, "runtime"), StepStatus::Fail);
    assert!(!passed(&steps));
    let steps = steps_with(background, &dry, &|_, _| true, true);
    assert_eq!(status(&steps, "runtime"), StepStatus::Ok);
    assert!(passed(&steps));
}

/// WA-2.3: a watch trigger reports change notifications per watched root.
#[test]
fn a_watch_trigger_checks_notifications_for_each_watched_root() {
    let mut both = Fixture::watch();
    if let crate::automation::model::Trigger::Watch(w) = &mut both.trigger {
        w.watch_right = true;
    }
    let dry = passing_dry_run();
    let steps = steps_with(both, &dry, &|_, _| true, false);
    assert_eq!(steps.iter().filter(|s| s.id == "watch").count(), 2);
    assert!(passed(&steps));
    // Nothing to report for a schedule.
    let steps = steps_with(Fixture::daily(), &dry, &|_, _| true, false);
    assert!(steps.iter().all(|s| s.id != "watch"));
}

// ── Ledger (SE-3.2) ──

fn rec(test_id: &str, at_ms: i64) -> TestRecord {
    TestRecord {
        test_id: test_id.into(),
        automation_id: "a".into(),
        policy_hash: "h".into(),
        passed: true,
        at_ms,
    }
}

#[test]
fn the_ledger_honours_the_fifteen_minute_window_and_a_clock_that_moved_back() {
    let ledger = TestLedger::new();
    ledger.record(rec("t1", 1_000));
    assert!(ledger.find("t1", 1_000).is_some());
    assert!(
        ledger.find("t1", 1_000 + TEST_TTL_MS).is_some(),
        "exactly 15 minutes"
    );
    assert!(ledger.find("t1", 1_000 + TEST_TTL_MS + 1).is_none());
    // Expired entries are forgotten for good, even if the clock goes back.
    assert!(ledger.find("t1", 1_000).is_none());
    // A record "from the future" (clock set back) does not count.
    ledger.record(rec("t2", 50_000));
    assert!(ledger.find("t2", 10_000).is_none());
}

#[test]
fn the_ledger_is_bounded_single_use_and_replaces_equal_ids() {
    let ledger = TestLedger::new();
    for i in 0..(MAX_TESTS + 10) {
        ledger.record(rec(&format!("t{i}"), 1_000));
    }
    assert_eq!(ledger.len(), MAX_TESTS);
    assert!(ledger.find("t0", 1_000).is_none(), "the oldest was dropped");
    assert!(ledger.find(&format!("t{}", MAX_TESTS + 9), 1_000).is_some());
    ledger.consume(&format!("t{}", MAX_TESTS + 9));
    assert!(ledger.find(&format!("t{}", MAX_TESTS + 9), 1_000).is_none());
    ledger.record(rec("same", 1_000));
    ledger.record(TestRecord {
        policy_hash: "other".into(),
        ..rec("same", 1_000)
    });
    assert_eq!(ledger.find("same", 1_000).unwrap().policy_hash, "other");
}

/// The production access check opens the folder with the needed rights and creates nothing.
#[cfg(windows)]
#[test]
fn fs_access_checks_rights_without_touching_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    assert!(fs_access(&path, false));
    assert!(fs_access(&path, true));
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    let missing = dir.path().join("missing");
    assert!(!fs_access(&missing.to_string_lossy(), false));
    with_test_deps(|_| ());
}
