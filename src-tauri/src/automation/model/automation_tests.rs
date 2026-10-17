//! Automation policy and draft validation (FER-522 / P1.1: AP-2.1, AP-2.4, SE-7, WA-2.3).

use super::*;
use crate::automation::test_support::Fixture;

fn profile() -> ProfilePolicy {
    let fx = Fixture::new();
    let (id, _) = fx.profile("P");
    fx.profile_policy(&id)
}

fn codes(errors: &[FieldError]) -> Vec<(&str, &str)> {
    errors
        .iter()
        .map(|e| (e.field.as_str(), e.code.as_str()))
        .collect()
}

fn schedule(freq: Freq, time: &str, tz: &str) -> AutomationPolicy {
    AutomationPolicy {
        trigger: Trigger::Schedule(ScheduleTrigger {
            freq,
            time: time.into(),
            tz: tz.into(),
            missed: MissedPolicy::Skip,
        }),
        runtime: Runtime::App,
        retries: 2,
        conditions: Conditions::default(),
    }
}

fn watch(f: impl FnOnce(&mut WatchTrigger)) -> AutomationPolicy {
    let mut w = WatchTrigger {
        watch_left: true,
        watch_right: false,
        on_changes: true,
        on_available: false,
        idle_secs: 10,
        max_wait_secs: DEFAULT_MAX_WAIT_SECS,
    };
    f(&mut w);
    AutomationPolicy {
        trigger: Trigger::Watch(w),
        runtime: Runtime::App,
        retries: 2,
        conditions: Conditions::default(),
    }
}

/// SC-1: schedule field syntax.
#[test]
fn schedule_fields_are_validated() {
    let p = profile();
    assert!(
        schedule(Freq::Daily {}, "02:00", "Asia/Jakarta")
            .validate(&p)
            .is_empty()
    );
    assert!(
        schedule(Freq::Daily {}, "23:59", "UTC")
            .validate(&p)
            .is_empty()
    );
    assert!(
        schedule(Freq::Daily {}, "00:00", "Etc/GMT+7")
            .validate(&p)
            .is_empty()
    );
    assert!(
        schedule(Freq::Daily {}, "10:30", "America/Argentina/Buenos_Aires")
            .validate(&p)
            .is_empty()
    );
    for bad in [
        "",
        "2:00",
        "24:00",
        "02:60",
        "02-00",
        "0200",
        "02:0a",
        "02:00 ",
        "٠٢:٠٠",
    ] {
        assert_eq!(
            codes(&schedule(Freq::Daily {}, bad, "UTC").validate(&p)),
            [("trigger.time", "time_invalid")],
            "time {bad:?}"
        );
    }
    for bad in [
        "",
        "Asia//Jakarta",
        "/UTC",
        "UTC/",
        "../Jakarta",
        "Asia/./Jakarta",
        "Asia\\Jakarta",
        "Asia/Jakarta\n",
        "Asia/Jakarta;",
        "Asia/Jak arta",
        &"A".repeat(TZ_MAX_CHARS + 1),
        "Asia/Jakarta\u{202e}",
    ] {
        assert_eq!(
            codes(&schedule(Freq::Daily {}, "02:00", bad).validate(&p)),
            [("trigger.tz", "tz_invalid")],
            "tz {bad:?}"
        );
    }
}

#[test]
fn once_dates_must_be_real_calendar_dates() {
    let p = profile();
    for ok in ["2026-10-20", "2028-02-29", "2000-01-01", "2026-12-31"] {
        assert!(
            schedule(Freq::Once { date: ok.into() }, "02:00", "UTC")
                .validate(&p)
                .is_empty(),
            "{ok}"
        );
    }
    for bad in [
        "2026-02-29",
        "2026-13-01",
        "2026-00-10",
        "2026-04-31",
        "2026-4-3",
        "26-04-03",
        "2026/04/03",
        "1999-12-31",
        "2026-10-20T00:00",
        "",
        "2026-10-2x",
    ] {
        assert_eq!(
            codes(&schedule(Freq::Once { date: bad.into() }, "02:00", "UTC").validate(&p)),
            [("trigger.date", "date_invalid")],
            "{bad:?}"
        );
    }
}

#[test]
fn weekly_days_must_be_sorted_unique_weekdays() {
    let p = profile();
    let weekly = |days: Vec<u8>| schedule(Freq::Weekly { days }, "02:00", "UTC").validate(&p);
    assert!(weekly(vec![0]).is_empty());
    assert!(weekly(vec![1, 3, 6]).is_empty());
    assert_eq!(codes(&weekly(vec![])), [("trigger.days", "days_required")]);
    for bad in [vec![7], vec![3, 1], vec![1, 1], vec![0, 9]] {
        assert_eq!(
            codes(&weekly(bad.clone())),
            [("trigger.days", "days_invalid")],
            "{bad:?}"
        );
    }
}

/// WA-1 / WA-2: watch ranges and required choices.
#[test]
fn watch_fields_are_validated() {
    let p = profile();
    assert!(watch(|_| {}).validate(&p).is_empty());
    assert!(watch(|w| w.idle_secs = 2).validate(&p).is_empty());
    assert!(watch(|w| w.idle_secs = 3600).validate(&p).is_empty());
    assert!(watch(|w| w.max_wait_secs = 60).validate(&p).is_empty());
    assert!(watch(|w| w.max_wait_secs = 86_400).validate(&p).is_empty());
    assert_eq!(
        codes(&watch(|w| w.idle_secs = 1).validate(&p)),
        [("trigger.idleSecs", "idle_out_of_range")]
    );
    assert_eq!(
        codes(&watch(|w| w.idle_secs = 3601).validate(&p)),
        [("trigger.idleSecs", "idle_out_of_range")]
    );
    assert_eq!(
        codes(&watch(|w| w.max_wait_secs = 59).validate(&p)),
        [("trigger.maxWaitSecs", "max_wait_out_of_range")]
    );
    assert_eq!(
        codes(&watch(|w| w.max_wait_secs = 86_401).validate(&p)),
        [("trigger.maxWaitSecs", "max_wait_out_of_range")]
    );
    assert_eq!(
        codes(
            &watch(|w| {
                w.watch_left = false;
                w.watch_right = false;
            })
            .validate(&p)
        ),
        [("trigger.watch", "watch_root_required")]
    );
    assert_eq!(
        codes(
            &watch(|w| {
                w.on_changes = false;
                w.on_available = false;
            })
            .validate(&p)
        ),
        [("trigger.on", "watch_event_required")]
    );
}

/// WA-2.3 / Q2: a network root cannot be watched.
#[test]
fn watching_a_network_root_is_refused() {
    let mut p = profile();
    p.right.identity = RootIdentity::Network {
        unc_prefix: r"\\nas\share".into(),
        volume_serial: Some("1".into()),
        file_id: Some("2".into()),
    };
    let right = watch(|w| {
        w.watch_left = false;
        w.watch_right = true;
    });
    assert_eq!(
        codes(&right.validate(&p)),
        [("trigger.watchRight", "watch_unsupported_root")]
    );
    // Watching only the local side is fine, and a schedule does not care.
    assert!(watch(|_| {}).validate(&p).is_empty());
    assert!(
        schedule(Freq::Daily {}, "02:00", "UTC")
            .validate(&p)
            .is_empty()
    );
}

/// RQ-7.1: retries 0..=3.
#[test]
fn retries_are_bounded() {
    let p = profile();
    for ok in 0..=MAX_RETRIES {
        let mut a = schedule(Freq::Daily {}, "02:00", "UTC");
        a.retries = ok;
        assert!(a.validate(&p).is_empty(), "{ok}");
    }
    let mut a = schedule(Freq::Daily {}, "02:00", "UTC");
    a.retries = MAX_RETRIES + 1;
    assert_eq!(
        codes(&a.validate(&p)),
        [("retries", "retries_out_of_range")]
    );
}

/// AP-2.1 / AP-2.4: the name rules and that every error is collected in one pass.
#[test]
fn the_draft_collects_every_error_and_trims_the_name() {
    let p = profile();
    let id = "22222222-2222-4222-8222-222222222222";
    let ok = AutomationDraft {
        name: "  Nightly photos  ".into(),
        profile_id: id.into(),
        policy: schedule(Freq::Daily {}, "02:00", "UTC"),
    };
    let validated = ok.validate(&p).unwrap();
    assert_eq!(validated.name, "Nightly photos");

    let bad = AutomationDraft {
        name: "".into(),
        profile_id: "nope".into(),
        policy: {
            let mut a = schedule(Freq::Daily {}, "25:00", "UTC");
            a.retries = 9;
            a
        },
    };
    let errors = bad.validate(&p).unwrap_err();
    let got = codes(&errors);
    for expected in [
        ("name", "name_required"),
        ("profileId", "profile_required"),
        ("trigger.time", "time_invalid"),
        ("retries", "retries_out_of_range"),
    ] {
        assert!(got.contains(&expected), "{expected:?} in {got:?}");
    }
    let long = AutomationDraft {
        name: "x".repeat(NAME_MAX_CHARS + 1),
        ..ok.clone()
    };
    assert_eq!(
        codes(&long.validate(&p).unwrap_err()),
        [("name", "name_too_long")]
    );
    let exact = AutomationDraft {
        name: "x".repeat(NAME_MAX_CHARS),
        ..ok.clone()
    };
    assert!(exact.validate(&p).is_ok());
    for evil in ["a\nb", "a\u{202e}b", "tab\there", "nul\0x", "z\u{200b}w"] {
        let d = AutomationDraft {
            name: evil.into(),
            ..ok.clone()
        };
        assert_eq!(
            codes(&d.validate(&p).unwrap_err()),
            [("name", "name_control")],
            "{evil:?}"
        );
    }
}

/// design §4.4: strict serde - unknown fields anywhere, unknown kinds and out-of-range types are
/// refused; defaults fill `retries` and `conditions`.
#[test]
fn the_wire_shape_is_strict_and_fills_defaults() {
    let json = r#"{"trigger":{"kind":"watch","watchLeft":true,"watchRight":false,"onChanges":true,"onAvailable":true,"idleSecs":10,"maxWaitSecs":900},"runtime":"background"}"#;
    let policy: AutomationPolicy = serde_json::from_str(json).unwrap();
    assert_eq!(policy.retries, DEFAULT_RETRIES);
    assert_eq!(policy.conditions, Conditions::default());
    assert_eq!(policy.trigger.kind(), "watch");
    for bad in [
        r#"{"trigger":{"kind":"watch","watchLeft":true,"watchRight":false,"onChanges":true,"onAvailable":true,"idleSecs":10,"maxWaitSecs":900,"extra":1},"runtime":"app"}"#,
        r#"{"trigger":{"kind":"cron","expr":"* * * * *"},"runtime":"app"}"#,
        r#"{"trigger":{"kind":"schedule","freq":{"kind":"daily","at":1},"time":"02:00","tz":"UTC","missed":"skip"},"runtime":"app"}"#,
        r#"{"trigger":{"kind":"schedule","freq":{"kind":"daily"},"time":"02:00","tz":"UTC","missed":"later"},"runtime":"app"}"#,
        r#"{"trigger":{"kind":"schedule","freq":{"kind":"daily"},"time":"02:00","tz":"UTC","missed":"skip"},"runtime":"service"}"#,
        r#"{"trigger":{"kind":"schedule","freq":{"kind":"daily"},"time":"02:00","tz":"UTC","missed":"skip"},"runtime":"app","retries":300}"#,
        r#"{"trigger":{"kind":"schedule","freq":{"kind":"weekly","days":[-1]},"time":"02:00","tz":"UTC","missed":"skip"},"runtime":"app"}"#,
        r#"{"trigger":{"kind":"schedule","freq":{"kind":"daily"},"time":"02:00","tz":"UTC","missed":"skip"},"runtime":"app","conditions":{"skipOnBattery":true,"approved":true}}"#,
    ] {
        assert!(
            serde_json::from_str::<AutomationPolicy>(bad).is_err(),
            "{bad}"
        );
    }
    // Round trip keeps the camelCase shape.
    let again = serde_json::to_string(&policy).unwrap();
    assert!(again.contains("\"watchLeft\":true"));
    assert!(again.contains("\"conditions\":{\"skipOnBattery\":false,\"skipOnMetered\":false}"));
    assert_eq!(
        serde_json::from_str::<AutomationPolicy>(&again).unwrap(),
        policy
    );
}
