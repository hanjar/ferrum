//! Lease commands (P0.4): contract fixtures shared with the TypeScript tests, and the Tauri-free
//! cores driven with real folders. Kept beside `commands.rs` (declared there with `#[path]`).

use std::sync::Arc;

use super::*;
use crate::automation::store::leases::is_lease_id;

const SOURCE: &str = include_str!("commands.rs");

fn fixture(name: &str) -> serde_json::Value {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../contract-fixtures/automation/"
    );
    let text =
        std::fs::read_to_string(format!("{dir}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn camel(snake: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in snake.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// IPC argument keys of a command: its parameters minus the injected `State`, camelCased the
/// way Tauri names them. Both lease commands have flat, generic-free parameters after `State`.
fn wire_keys(command: &str) -> Vec<String> {
    let start = SOURCE
        .find(&format!("pub async fn {command}("))
        .unwrap_or_else(|| panic!("{command} is defined"));
    let open = start + SOURCE[start..].find('(').unwrap();
    let close = open + SOURCE[open..].find(") ->").unwrap();
    // No generic parameter besides the injected `State<'_, ..>` has a comma, so a plain split
    // is enough (and works for LF and CRLF checkouts alike).
    let mut keys: Vec<String> = SOURCE[open + 1..close]
        .split(',')
        .filter_map(|p| p.split_once(':'))
        .map(|(name, _)| name.trim())
        .filter(|name| *name != "state")
        .map(camel)
        .collect();
    keys.sort();
    keys
}

fn fixture_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

fn store() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("automation"), "0.2.0"));
    (dir, store)
}

fn canon(p: &str) -> Result<String, RootError> {
    roots::canonical(p)
}

fn folders(dir: &tempfile::TempDir, a: &str, b: &str) -> (String, String) {
    let left = dir.path().join(a);
    let right = dir.path().join(b);
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    (
        left.to_string_lossy().into_owned(),
        right.to_string_lossy().into_owned(),
    )
}

/// The request fixtures name exactly the command parameters (what Tauri maps by camelCase
/// name), and their values deserialize into the parameter types.
#[test]
fn lease_request_fixtures_match_the_commands() {
    assert_eq!(
        wire_keys("sync_lease_acquire"),
        fixture_keys(&fixture("sync-lease-acquire.request.json"))
    );
    assert_eq!(
        wire_keys("sync_lease_release"),
        fixture_keys(&fixture("sync-lease-release.request.json"))
    );
    let acquire = fixture("sync-lease-acquire.request.json");
    let _: String = serde_json::from_value(acquire["left"].clone()).unwrap();
    let _: String = serde_json::from_value(acquire["right"].clone()).unwrap();
    let release = fixture("sync-lease-release.request.json");
    let id: String = serde_json::from_value(release["leaseId"].clone()).unwrap();
    assert!(is_lease_id(&id));
}

/// The result fixtures are the wire truth for the DTOs, in both directions.
#[test]
fn lease_result_fixtures_roundtrip_through_the_dtos() {
    let acquired: LeaseAcquireDto =
        serde_json::from_value(fixture("sync-lease-acquire.json")).unwrap();
    assert!(
        matches!(&acquired, LeaseAcquireDto::Acquired(LeaseAcquired { lease_id }) if is_lease_id(lease_id))
    );
    assert_eq!(
        serde_json::to_value(&acquired).unwrap(),
        fixture("sync-lease-acquire.json")
    );

    let busy: LeaseAcquireDto =
        serde_json::from_value(fixture("sync-lease-acquire-busy.json")).unwrap();
    assert_eq!(
        busy,
        LeaseAcquireDto::Busy(LeaseBusy {
            status: LeaseBusyStatus::Busy,
            holder: LeaseHolderDto {
                kind: "runner".into(),
                run_id: Some(42)
            }
        })
    );
    assert_eq!(
        serde_json::to_value(&busy).unwrap(),
        fixture("sync-lease-acquire-busy.json")
    );

    let released: LeaseReleaseDto =
        serde_json::from_value(fixture("sync-lease-release.json")).unwrap();
    assert_eq!(
        serde_json::to_value(released).unwrap(),
        fixture("sync-lease-release.json")
    );

    // Unknown fields are refused.
    let extra = serde_json::json!({ "leaseId": "a".repeat(32), "path": "x" });
    assert!(serde_json::from_value::<LeaseAcquireDto>(extra).is_err());
}

/// Real folders take the lease, the same folders again are `busy` (a normal result naming the
/// holder kind), the release frees them, and releasing twice is harmless.
#[test]
fn lease_cores_acquire_busy_release() {
    let (dir, store) = store();
    let registry = LeaseRegistry::new(LeaseEnv::system());
    let (left, right) = folders(&dir, "left", "right");

    let first = lease_acquire_in(&store, &registry, &left, &right, &canon).unwrap();
    let LeaseAcquireDto::Acquired(LeaseAcquired { lease_id }) = first else {
        panic!("acquired");
    };
    assert!(is_lease_id(&lease_id));

    // Same folders, and a parent of one of them, are both in use.
    for (l, r) in [
        (left.clone(), right.clone()),
        (right.clone(), left.clone()),
        (
            dir.path().to_string_lossy().into_owned(),
            dir.path().join("elsewhere").to_string_lossy().into_owned(),
        ),
    ] {
        match lease_acquire_in(&store, &registry, &l, &r, &canon).unwrap() {
            LeaseAcquireDto::Busy(LeaseBusy { status, holder }) => {
                assert_eq!(status, LeaseBusyStatus::Busy);
                assert_eq!(holder.kind, "manual");
            }
            other => panic!("expected busy, got {other:?}"),
        }
    }

    assert_eq!(
        lease_release_in(&registry, &lease_id).unwrap(),
        LeaseReleaseDto {}
    );
    lease_release_in(&registry, &lease_id).unwrap();
    assert!(matches!(
        lease_acquire_in(&store, &registry, &left, &right, &canon).unwrap(),
        LeaseAcquireDto::Acquired(_)
    ));
}

/// Disjoint folder pairs are granted side by side.
#[test]
fn disjoint_pairs_get_their_own_leases() {
    let (dir, store) = store();
    let registry = LeaseRegistry::new(LeaseEnv::system());
    let (a, b) = folders(&dir, "data", "mirror");
    let (c, d) = folders(&dir, "data-backup", "mirror-backup");
    let first = lease_acquire_in(&store, &registry, &a, &b, &canon).unwrap();
    let second = lease_acquire_in(&store, &registry, &c, &d, &canon).unwrap();
    assert!(matches!(first, LeaseAcquireDto::Acquired(_)));
    assert!(matches!(second, LeaseAcquireDto::Acquired(_)));
}

/// Bad input is refused without side effects: empty, oversized, control characters and device
/// paths are `invalid_input`; a malformed lease id on release too. Messages carry no paths.
#[test]
fn lease_commands_refuse_bad_input() {
    let (_dir, store) = store();
    let registry = LeaseRegistry::new(LeaseEnv::system());
    let bidi = "C:\\bad\u{202e}name".to_string();
    let long = format!("C:\\{}", "a".repeat(5000));
    for (l, r) in [
        ("", r"C:\y"),
        (r"C:\y", ""),
        (r"C:\y", "relative\\path"),
        (r"\\?\C:\x", r"C:\y"),
        (bidi.as_str(), r"C:\y"),
        (long.as_str(), r"C:\y"),
    ] {
        let e = lease_acquire_in(&store, &registry, l, r, &canon).unwrap_err();
        assert_eq!(e.code, "invalid_input", "{l:?} {r:?}");
        assert!(!e.message.contains('\\'), "no paths in messages");
    }
    assert_eq!(registry.held_count(), 0);
    for bad in ["", "short", &"G".repeat(32), &"a".repeat(33)] {
        assert_eq!(
            lease_release_in(&registry, bad).unwrap_err().code,
            "invalid_input"
        );
    }
}

/// An unusable store refuses a manual lease with the store code and holds nothing.
#[test]
fn lease_acquire_reports_store_problems() {
    let store = Arc::new(Store::unavailable());
    let registry = LeaseRegistry::new(LeaseEnv::system());
    let e = lease_acquire_in(&store, &registry, r"C:\a", r"D:\b", &canon).unwrap_err();
    assert_eq!(e.code, "store_unavailable");
    assert_eq!(registry.held_count(), 0);
}

/// Releasing with a well-formed but unknown token does nothing to a live lease and looks the
/// same as a normal release: the token is the secret, and there is no existence oracle.
#[test]
fn release_with_a_wrong_token_leaves_the_lease() {
    let (dir, store) = store();
    let registry = LeaseRegistry::new(LeaseEnv::system());
    let (left, right) = folders(&dir, "l", "r");
    let LeaseAcquireDto::Acquired(LeaseAcquired { lease_id }) =
        lease_acquire_in(&store, &registry, &left, &right, &canon).unwrap()
    else {
        panic!("acquired");
    };
    let wrong = "0".repeat(32);
    assert_ne!(wrong, lease_id);
    assert_eq!(
        lease_release_in(&registry, &wrong).unwrap(),
        LeaseReleaseDto {}
    );
    assert_eq!(registry.held_count(), 1);
    assert!(matches!(
        lease_acquire_in(&store, &registry, &left, &right, &canon).unwrap(),
        LeaseAcquireDto::Busy(_)
    ));
}

/// Too many simultaneous manual leases per process is `lease_busy`, not a crash.
#[test]
fn too_many_manual_leases_is_lease_busy() {
    let (dir, store) = store();
    let registry = LeaseRegistry::new(LeaseEnv::system());
    for i in 0..crate::automation::lease::MAX_MANUAL_LEASES {
        let (l, r) = folders(&dir, &format!("l{i}"), &format!("r{i}"));
        assert!(matches!(
            lease_acquire_in(&store, &registry, &l, &r, &canon).unwrap(),
            LeaseAcquireDto::Acquired(_)
        ));
    }
    let (l, r) = folders(&dir, "lx", "rx");
    let e = lease_acquire_in(&store, &registry, &l, &r, &canon).unwrap_err();
    assert_eq!(e.code, "lease_busy");
}

/// Two openers of one store (two processes) see each other's manual leases: the same pair is
/// busy from the second connection.
#[test]
fn a_second_connection_sees_the_manual_lease() {
    let (dir, first) = store();
    let second = Arc::new(Store::open(&dir.path().join("automation"), "0.2.0"));
    let (left, right) = folders(&dir, "left", "right");
    let reg_a = LeaseRegistry::new(LeaseEnv::system());
    let reg_b = LeaseRegistry::new(LeaseEnv::system());
    assert!(matches!(
        lease_acquire_in(&first, &reg_a, &left, &right, &canon).unwrap(),
        LeaseAcquireDto::Acquired(_)
    ));
    assert!(matches!(
        lease_acquire_in(&second, &reg_b, &left, &right, &canon).unwrap(),
        LeaseAcquireDto::Busy(_)
    ));
}

/// P2-1: the page-load hook releases every manual lease of the process and frees the folders.
#[test]
fn page_load_releases_the_manual_leases() {
    let (dir, store) = store();
    let state = AutomationState {
        store: store.clone(),
        leases: Arc::new(LeaseRegistry::new(LeaseEnv::system())),
    };
    let (left, right) = folders(&dir, "left", "right");
    let LeaseAcquireDto::Acquired(_) =
        lease_acquire_in(&store, &state.leases, &left, &right, &canon).unwrap()
    else {
        panic!("acquired");
    };
    assert!(matches!(
        lease_acquire_in(&store, &state.leases, &left, &right, &canon).unwrap(),
        LeaseAcquireDto::Busy(_)
    ));
    assert_eq!(release_manual_leases_on_page_load(&state), 1);
    assert!(matches!(
        lease_acquire_in(&store, &state.leases, &left, &right, &canon).unwrap(),
        LeaseAcquireDto::Acquired(_)
    ));
}

/// R2-P3-1: the UI-thread hook only drains; the guards are dropped on a worker, after which the
/// folders are free again.
#[test]
fn async_page_load_hook_frees_the_folders_off_thread() {
    let (dir, store) = store();
    let state = AutomationState {
        store: store.clone(),
        leases: Arc::new(LeaseRegistry::new(LeaseEnv::system())),
    };
    let (left, right) = folders(&dir, "left", "right");
    let LeaseAcquireDto::Acquired(_) =
        lease_acquire_in(&store, &state.leases, &left, &right, &canon).unwrap()
    else {
        panic!("acquired");
    };
    assert_eq!(release_manual_leases_on_page_load_async(&state), 1);
    assert_eq!(state.leases.held_count(), 0, "drained at once");
    let mut freed = false;
    for _ in 0..200 {
        if matches!(
            lease_acquire_in(&store, &state.leases, &left, &right, &canon).unwrap(),
            LeaseAcquireDto::Acquired(_)
        ) {
            freed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(freed, "the worker released the lease");
}

/// RQ-2 / SP-1.5: a junction and its target are the same folder, so a lease on one makes the
/// other busy. Skipped (with a message) where a junction cannot be created.
#[test]
fn a_junction_alias_overlaps_its_target() {
    let (dir, store) = store();
    let registry = LeaseRegistry::new(LeaseEnv::system());
    let (left, right) = folders(&dir, "real-left", "right");
    let link = dir.path().join("link-to-left");
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&left)
        .output()
        .is_ok_and(|o| o.status.success());
    if !made {
        eprintln!("SKIPPED: a junction could not be created in this environment");
        return;
    }
    let LeaseAcquireDto::Acquired(_) =
        lease_acquire_in(&store, &registry, &left, &right, &canon).unwrap()
    else {
        panic!("acquired");
    };
    let (_, other_right) = folders(&dir, "unused", "other-right");
    let via_link = link.to_string_lossy().into_owned();
    assert!(matches!(
        lease_acquire_in(&store, &registry, &via_link, &other_right, &canon).unwrap(),
        LeaseAcquireDto::Busy(_)
    ));
    // A path inside the junction is covered as well.
    let inside = link.join("sub");
    std::fs::create_dir_all(&inside).unwrap();
    assert!(matches!(
        lease_acquire_in(
            &store,
            &registry,
            &inside.to_string_lossy(),
            &other_right,
            &canon
        )
        .unwrap(),
        LeaseAcquireDto::Busy(_)
    ));
    let _ = std::process::Command::new("cmd")
        .args(["/c", "rmdir"])
        .arg(&link)
        .output();
}
