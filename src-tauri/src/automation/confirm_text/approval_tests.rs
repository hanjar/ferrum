//! Prompt texts of approve / re-approve / resume (FER-522 / P1.1: SE-2, SE-2.1, SE-2.2, §10.3).

use super::*;
use crate::automation::model::{
    CompareMode, Conditions, DeleteGuard, DriveKind, Filters, RootIdentity, RootSpec, Side,
    WatchTrigger,
};

fn profile(delete: DeletePolicy, guard_on: bool, mode: SyncMode) -> ProfilePolicy {
    let root = |p: &str| RootSpec {
        path: p.into(),
        identity: RootIdentity::Local {
            volume_serial: "0123abcd".into(),
            file_id: "f".repeat(32),
            fs_name: "NTFS".into(),
            drive: DriveKind::Fixed,
        },
    };
    ProfilePolicy {
        left: root(r"D:\Photos"),
        right: root(r"E:\Backup"),
        mode,
        delete_policy: delete,
        compare: CompareMode::SizeAndDate,
        filters: Filters {
            include: vec![],
            exclude: vec![],
        },
        guard: DeleteGuard {
            enabled: guard_on,
            max_count: 50,
            max_percent: 20,
        },
    }
}

fn one_way() -> SyncMode {
    SyncMode::OneWay { from: Side::Left }
}

fn daily(runtime: Runtime) -> AutomationPolicy {
    AutomationPolicy {
        trigger: Trigger::Schedule(crate::automation::model::ScheduleTrigger {
            freq: Freq::Daily {},
            time: "02:00".into(),
            tz: "Asia/Jakarta".into(),
            missed: MissedPolicy::RunOnce,
        }),
        runtime,
        retries: 2,
        conditions: Conditions::default(),
    }
}

fn at(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing in {text:?}"))
}

/// §10.3: warnings first, then action, profile, trigger, runtime, closing.
#[test]
fn the_parts_come_in_the_documented_order() {
    let p = profile(
        DeletePolicy::Permanent,
        false,
        SyncMode::TwoWay {
            propagate_deletes: true,
        },
    );
    let prompt = approval_prompt(
        ApprovalAction::Enable,
        "Nightly photos",
        "Photos to NAS",
        3,
        &p,
        &daily(Runtime::Background),
    );
    let t = &prompt.text;
    let order = [
        at(t, PERMANENT_WARNING),
        at(t, GUARD_OFF_WARNING),
        at(t, PROPAGATE_WARNING),
        at(t, "Enable automation \"Nightly photos\"?"),
        at(t, "\"Photos to NAS\" rev 3"),
        at(t, "Every day at 02:00 (Asia/Jakarta)"),
        at(
            t,
            "Runs while you're signed in, even when Ferrum is closed.",
        ),
        at(t, AUTOMATION_CLOSING),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}\n{t}");
    assert!(t.starts_with(PERMANENT_WARNING), "SE-2.2: {t}");
    assert_eq!(prompt.caption, APPROVE_CAPTION);
}

/// SE-2.2: the permanent warning is there for `permanent` and ONLY then; a plain policy has no
/// warnings and starts with the action.
#[test]
fn the_permanent_warning_appears_exactly_for_permanent_policies() {
    for (delete, expect) in [
        (DeletePolicy::Permanent, true),
        (DeletePolicy::RecycleBin, false),
        (
            DeletePolicy::Versioning {
                root: r"E:\V".into(),
            },
            false,
        ),
    ] {
        let prompt = approval_prompt(
            ApprovalAction::Resume,
            "N",
            "P",
            1,
            &profile(delete, true, one_way()),
            &daily(Runtime::App),
        );
        assert_eq!(prompt.text.contains(PERMANENT_WARNING), expect);
        assert_eq!(prompt.text.starts_with(PERMANENT_WARNING), expect);
        assert_eq!(prompt.text.contains("WARNING"), expect, "{}", prompt.text);
    }
}

/// SE-2.1: Resume names the automation, the profile and its revision, the folders, the mode, the
/// delete policy, the trigger and the runtime; captions follow the action.
#[test]
fn resume_follows_the_se_2_1_shape() {
    let p = profile(DeletePolicy::RecycleBin, true, one_way());
    let prompt = approval_prompt(
        ApprovalAction::Resume,
        "Nightly photos",
        "Photos to NAS",
        3,
        &p,
        &daily(Runtime::App),
    );
    let t = &prompt.text;
    assert!(t.starts_with("Resume \"Nightly photos\"?"), "{t}");
    for part in [
        "\"Photos to NAS\" rev 3",
        r"D:\Photos → E:\Backup",
        "Update left → right",
        "Recycle Bin",
        "Every day at 02:00 (Asia/Jakarta); a missed run happens once when possible.",
        "Runs only while Ferrum is open.",
    ] {
        assert!(t.contains(part), "{part:?} missing in {t}");
    }
    assert_eq!(prompt.caption, RESUME_CAPTION);
    let again = approval_prompt(
        ApprovalAction::Reapprove,
        "N",
        "P",
        1,
        &p,
        &daily(Runtime::App),
    );
    assert_eq!(again.caption, REAPPROVE_CAPTION);
    assert!(again.text.starts_with("Re-approve automation \"N\"?"));
}

/// §10.3 / SE-2: bounded to 2 000 characters, longest parts shortened, warnings survive.
#[test]
fn the_text_is_bounded_and_keeps_the_warnings() {
    let long = "x".repeat(5000);
    let mut p = profile(
        DeletePolicy::Permanent,
        false,
        SyncMode::TwoWay {
            propagate_deletes: true,
        },
    );
    p.left.path = long.clone();
    p.right.path = long.clone();
    let mut a = daily(Runtime::Background);
    if let Trigger::Schedule(s) = &mut a.trigger {
        s.tz = long.clone();
        s.time = long.clone();
        s.freq = Freq::Once { date: long.clone() };
    }
    let prompt = approval_prompt(ApprovalAction::Reapprove, &long, &long, 999_999, &p, &a);
    let len = prompt.text.chars().count();
    assert!(len <= PROMPT_MAX_CHARS, "{len} characters");
    assert!(prompt.text.starts_with(PERMANENT_WARNING));
    assert!(prompt.text.contains(GUARD_OFF_WARNING));
    assert!(!prompt.text.contains(&long));
    // Even a text that overflows is cut at the END (the warnings stay first).
    let cut = bound(format!("{PERMANENT_WARNING}\n\n{}", "y".repeat(3000)));
    assert_eq!(cut.chars().count(), PROMPT_MAX_CHARS);
    assert!(cut.starts_with(PERMANENT_WARNING));
    assert!(cut.ends_with('…'));
    assert_eq!(bound("short".into()), "short");
}

/// SE-2 (threat T4): control and bidi characters in any name or path never reach the dialog as
/// themselves - they are replaced by a visible marker.
#[test]
fn control_and_bidi_characters_do_not_reach_the_dialog() {
    let mut p = profile(DeletePolicy::RecycleBin, true, one_way());
    p.left.path = "D:\\Ph\u{202e}otos\u{7}".into();
    p.right.path = "E:\\Back\nup\u{200b}".into();
    let mut a = daily(Runtime::App);
    if let Trigger::Schedule(s) = &mut a.trigger {
        s.tz = "Asia/\u{2066}Jakarta".into();
    }
    let prompt = approval_prompt(
        ApprovalAction::Enable,
        "Night\u{202d}ly\u{0}",
        "Pro\u{feff}file",
        2,
        &p,
        &a,
    );
    for c in prompt.text.chars() {
        let allowed_separator = c == '\n';
        assert!(
            allowed_separator || (!c.is_control() && !is_format_char(c)),
            "{:?} in {:?}",
            c,
            prompt.text
        );
    }
    assert!(prompt.text.contains("<U+202E>"));
    assert!(prompt.text.contains("<U+0007>"));
    assert!(prompt.text.contains("<U+000A>"));
    assert!(prompt.text.contains("<U+0000>"));
}

/// The trigger line states what the user is approving, for every trigger shape.
#[test]
fn trigger_lines_cover_every_trigger() {
    let schedule = |freq, missed| {
        trigger_line(&Trigger::Schedule(
            crate::automation::model::ScheduleTrigger {
                freq,
                time: "06:30".into(),
                tz: "UTC".into(),
                missed,
            },
        ))
    };
    assert_eq!(
        schedule(
            Freq::Once {
                date: "2026-10-20".into()
            },
            MissedPolicy::Skip
        ),
        "Once on 2026-10-20 at 06:30 (UTC); a missed run is skipped."
    );
    assert_eq!(
        schedule(
            Freq::Weekly {
                days: vec![0, 2, 6]
            },
            MissedPolicy::RunOnce
        ),
        "Every Sun, Tue, Sat at 06:30 (UTC); a missed run happens once when possible."
    );
    let watch = |left, right, changes, available| {
        trigger_line(&Trigger::Watch(WatchTrigger {
            watch_left: left,
            watch_right: right,
            on_changes: changes,
            on_available: available,
            idle_secs: 10,
            max_wait_secs: 900,
        }))
    };
    assert_eq!(
        watch(true, true, true, true),
        "Watches both folders for changes and when a folder becomes available; starts after 10 s without changes (waits at most 900 s)."
    );
    assert!(watch(false, true, true, false).contains("the right folder for changes;"));
    assert!(watch(true, false, false, true).contains("a folder becoming available"));
}
