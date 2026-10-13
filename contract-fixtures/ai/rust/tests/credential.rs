#[path = "../../../../src-tauri/src/ai/credential.rs"]
#[allow(dead_code)] // API target tetap sengaja tidak dipanggil dari tes (credential pengguna)
mod credential;

#[test]
fn validates_ref_before_windows_lookup() {
    assert_eq!(
        credential::read_secret("../invalid"),
        Err(credential::CredentialError::InvalidReference)
    );
}

#[cfg(windows)]
#[test]
fn rejects_dummy_reference_without_returning_secret() {
    let result = credential::read_secret("Ferrum/AI/nonexistent-test-only");
    assert_eq!(result, Err(credential::CredentialError::InvalidReference));
}

#[test]
fn validates_secret_value_shape() {
    use credential::{validate_secret_value as v, CredentialError::InvalidValue};
    assert_eq!(v("short"), Err(InvalidValue));
    assert_eq!(v(&"a".repeat(2049)), Err(InvalidValue));
    assert_eq!(v("has space 12345"), Err(InvalidValue));
    assert_eq!(v("line\nbreak12345"), Err(InvalidValue));
    assert_eq!(v("unicode-kunci-\u{e9}12345"), Err(InvalidValue));
    assert_eq!(v("sk-abcdEFGH1234"), Ok(()));
    assert_eq!(v(&"a".repeat(2048)), Ok(()));
}

#[test]
fn invalid_value_never_touches_the_store() {
    assert_eq!(
        credential::store_verified("Ferrum/AI/test-only-never-written", "bad value"),
        Err(credential::CredentialError::InvalidValue)
    );
}

// Target dummy khusus tes; JANGAN pernah memakai `Ferrum/AI/custom` milik pengguna di sini.
#[cfg(windows)]
#[test]
fn round_trips_a_dummy_target_and_clears_it() {
    let target = "Ferrum/AI/test-only-fixture-roundtrip";
    credential::delete_at(target).unwrap();
    assert!(!credential::exists_at(target));
    credential::store_verified(target, "test-only-value-123").unwrap();
    assert!(credential::exists_at(target));
    credential::store_verified(target, "test-only-value-456").unwrap();
    credential::delete_at(target).unwrap();
    assert!(!credential::exists_at(target));
    credential::delete_at(target).unwrap(); // idempoten
}

#[cfg(not(windows))]
#[test]
fn reports_unsupported_off_windows() {
    assert_eq!(
        credential::store_verified("Ferrum/AI/test-only", "test-only-value-123"),
        Err(credential::CredentialError::Unsupported)
    );
}
