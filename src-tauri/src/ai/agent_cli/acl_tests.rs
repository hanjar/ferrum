//! FER-490 F1: pure rule checks of the executable-location ACL policy.

use super::acl::{Ace, Role, Security, evaluate};

const ME: &str = "S-1-5-21-1-2-3-1001";
const OTHER: &str = "S-1-5-21-1-2-3-1002";
const USERS: &str = "S-1-5-32-545";
const AUTH_USERS: &str = "S-1-5-11";
const SYSTEM: &str = "S-1-5-18";
const ADMINS: &str = "S-1-5-32-544";
const TI: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const CAP: &str = "S-1-15-3-3557520199-3666692283-3112367039";

const FULL: u32 = 0x001F_01FF;
const READ_EXEC: u32 = 0x0012_00A9;
const ADD_FILE: u32 = 0x2;
const ADD_SUBDIR: u32 = 0x4;
const DELETE_CHILD: u32 = 0x40;
const WRITE_DAC: u32 = 0x0004_0000;

fn allow(sid: &str, mask: u32) -> Ace {
    Ace {
        kind: 0,
        flags: 0,
        mask,
        sid: Some(sid.to_owned()),
    }
}

fn sec(owner: &str, aces: Vec<Ace>) -> Security {
    Security {
        owner: owner.to_owned(),
        dacl: Some(aces),
    }
}

fn standard() -> Vec<Ace> {
    vec![
        allow(SYSTEM, FULL),
        allow(ADMINS, FULL),
        allow(ME, FULL),
        allow(USERS, READ_EXEC),
    ]
}

const ALL_ROLES: [Role; 3] = [
    Role::Target,
    Role::Ancestor { root: false },
    Role::Ancestor { root: true },
];

#[test]
fn standard_locations_pass() {
    for owner in [ME, SYSTEM, ADMINS, TI] {
        for role in ALL_ROLES {
            assert_eq!(
                evaluate(&sec(owner, standard()), ME, role),
                None,
                "{owner} {role:?}"
            );
        }
    }
}

#[test]
fn a_foreign_owner_is_refused_everywhere() {
    for owner in [OTHER, USERS, AUTH_USERS, "S-1-1-0"] {
        for role in ALL_ROLES {
            let why = evaluate(&sec(owner, standard()), ME, role).expect("refused");
            assert!(why.contains(owner), "{why}");
        }
    }
}

#[test]
fn null_dacl_is_refused() {
    let s = Security {
        owner: ME.to_owned(),
        dacl: None,
    };
    for role in ALL_ROLES {
        assert_eq!(evaluate(&s, ME, role).as_deref(), Some("NULL DACL"));
    }
}

#[test]
fn non_basic_allow_aces_fail_closed() {
    // 4 compound, 5 object, 9 callback (conditional), 11 callback object: even read-only and
    // even for a trusted SID, the rule cannot be evaluated, so the location is refused.
    for kind in [4u8, 5, 9, 11] {
        let mut aces = standard();
        aces.push(Ace {
            kind,
            flags: 0,
            mask: READ_EXEC,
            sid: Some(SYSTEM.to_owned()),
        });
        for role in ALL_ROLES {
            assert!(
                evaluate(&sec(ME, aces.clone()), ME, role).is_some(),
                "type {kind}"
            );
        }
    }
}

#[test]
fn deny_and_inherit_only_aces_are_ignored() {
    let mut aces = standard();
    for kind in [1u8, 6, 10, 12] {
        aces.push(Ace {
            kind,
            flags: 0,
            mask: FULL,
            sid: Some(USERS.to_owned()),
        });
    }
    // Inherit-only (0x08) Modify for Authenticated Users (the `C:\` default): not on this object.
    aces.push(Ace {
        kind: 0,
        flags: 0x08 | 0x01 | 0x02,
        mask: FULL,
        sid: Some(AUTH_USERS.to_owned()),
    });
    // Even an inherit-only conditional ACE does not apply.
    aces.push(Ace {
        kind: 9,
        flags: 0x08,
        mask: FULL,
        sid: Some(USERS.to_owned()),
    });
    for role in ALL_ROLES {
        assert_eq!(
            evaluate(&sec(SYSTEM, aces.clone()), ME, role),
            None,
            "{role:?}"
        );
    }
}

#[test]
fn target_refuses_any_foreign_write_right() {
    for mask in [
        ADD_FILE,
        ADD_SUBDIR,
        DELETE_CHILD,
        WRITE_DAC,
        0x10000,
        0x4000_0000,
    ] {
        let mut aces = standard();
        aces.push(allow(USERS, mask));
        let why = evaluate(&sec(ME, aces), ME, Role::Target).expect("refused");
        assert_eq!(why, USERS, "mask {mask:#x}");
    }
}

#[test]
fn ancestors_refuse_rights_that_move_the_chain_but_allow_add_file() {
    let anc = Role::Ancestor { root: false };
    for mask in [
        ADD_SUBDIR,
        DELETE_CHILD,
        WRITE_DAC,
        0x10000,
        0x0008_0000,
        0x1000_0000,
    ] {
        let mut aces = standard();
        aces.push(allow(USERS, mask));
        assert_eq!(
            evaluate(&sec(ME, aces), ME, anc).as_deref(),
            Some(USERS),
            "mask {mask:#x}"
        );
    }
    let mut aces = standard();
    aces.push(allow(USERS, ADD_FILE));
    assert_eq!(evaluate(&sec(ME, aces), ME, anc), None);
}

#[test]
fn volume_root_accepts_create_folder_for_authenticated_users_only() {
    let root = Role::Ancestor { root: true };
    // The Windows default on C:\ : Authenticated Users "create folders / append data".
    let mut aces = standard();
    aces.push(allow(AUTH_USERS, ADD_SUBDIR));
    assert_eq!(evaluate(&sec(TI, aces.clone()), ME, root), None);
    // ... but not below the root,
    assert_eq!(
        evaluate(&sec(TI, aces), ME, Role::Ancestor { root: false }).as_deref(),
        Some(AUTH_USERS)
    );
    // ... and never together with DELETE_CHILD or WRITE_DAC.
    for extra in [DELETE_CHILD, WRITE_DAC] {
        let mut aces = standard();
        aces.push(allow(AUTH_USERS, ADD_SUBDIR | extra));
        assert_eq!(
            evaluate(&sec(TI, aces), ME, root).as_deref(),
            Some(AUTH_USERS),
            "{extra:#x}"
        );
    }
}

#[test]
fn capability_sids_only_on_ancestors() {
    let mut aces = standard();
    aces.push(allow(CAP, FULL));
    assert_eq!(
        evaluate(&sec(ME, aces.clone()), ME, Role::Ancestor { root: false }),
        None
    );
    assert_eq!(
        evaluate(&sec(ME, aces), ME, Role::Target).as_deref(),
        Some(CAP)
    );
}

/// FER-514: the callback-aware variant (used for Rar.exe) accepts a read-only conditional allow
/// ACE but still reports one that grants a write right, and still fails closed on the other
/// non-plain allow types. The strict FER-490 `evaluate` is unchanged.
#[test]
fn callback_aware_judges_conditional_aces_by_their_rights() {
    use super::acl::evaluate_with;
    let cond = |sid: &str, mask: u32, kind: u8| Ace {
        kind,
        flags: 0,
        mask,
        sid: Some(sid.to_owned()),
    };
    let mut read_only = standard();
    read_only.push(cond(USERS, READ_EXEC, 9));
    for role in ALL_ROLES {
        assert!(
            evaluate(&sec(ME, read_only.clone()), ME, role).is_some(),
            "strict rule still fails closed on type 9 ({role:?})"
        );
        assert_eq!(
            evaluate_with(&sec(ME, read_only.clone()), ME, role, true),
            None,
            "{role:?}"
        );
    }
    let mut writable = standard();
    writable.push(cond(USERS, FULL, 9));
    assert_eq!(
        evaluate_with(&sec(ME, writable), ME, Role::Target, true),
        Some(USERS.to_owned())
    );
    for kind in [4u8, 5, 11] {
        let mut aces = standard();
        aces.push(cond(USERS, READ_EXEC, kind));
        assert!(
            evaluate_with(&sec(ME, aces), ME, Role::Target, true).is_some(),
            "type {kind} stays fail-closed"
        );
    }
}
