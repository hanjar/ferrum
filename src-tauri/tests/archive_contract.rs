// Contract-only module is imported here until lib.rs registration by integration.
#[allow(dead_code)]
#[path = "../src/archive_contract.rs"]
mod archive_contract;

use archive_contract::*;
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../contract-fixtures/archive-contract.json"
    ))
    .expect("fixture JSON")
}

#[test]
fn fixture_roundtrips_as_values_not_line_endings() {
    let f = fixture();
    let disk: Location = serde_json::from_value(f["diskLocation"].clone()).unwrap();
    let location: Location = serde_json::from_value(f["location"].clone()).unwrap();
    let entry: ArchiveEntry = serde_json::from_value(f["entry"].clone()).unwrap();
    let caps: ArchiveCapabilities = serde_json::from_value(f["capabilities"].clone()).unwrap();
    let list: ArchiveListRequest = serde_json::from_value(f["listRequest"].clone()).unwrap();
    let response: ArchiveListResponse = serde_json::from_value(f["listResponse"].clone()).unwrap();
    let read: ArchiveReadRequest = serde_json::from_value(f["readRequest"].clone()).unwrap();
    let extract: ArchiveExtractRequest =
        serde_json::from_value(f["extractRequest"].clone()).unwrap();
    let create: ArchiveCreateRequest = serde_json::from_value(f["createRequest"].clone()).unwrap();
    let test: ArchiveTestRequest = serde_json::from_value(f["testRequest"].clone()).unwrap();
    let read_response: ArchiveReadResponse =
        serde_json::from_value(f["readResponse"].clone()).unwrap();
    let operation_response: ArchiveOperationResponse =
        serde_json::from_value(f["operationResponse"].clone()).unwrap();
    let test_response: ArchiveTestReport =
        serde_json::from_value(f["testResponse"].clone()).unwrap();
    let progress: ArchiveProgress = serde_json::from_value(f["progress"].clone()).unwrap();
    let error: ArchiveError = serde_json::from_value(f["error"].clone()).unwrap();
    for (actual, expected) in [
        (serde_json::to_value(&disk).unwrap(), &f["diskLocation"]),
        (serde_json::to_value(&location).unwrap(), &f["location"]),
        (serde_json::to_value(&entry).unwrap(), &f["entry"]),
        (serde_json::to_value(&caps).unwrap(), &f["capabilities"]),
        (serde_json::to_value(&list).unwrap(), &f["listRequest"]),
        (serde_json::to_value(&response).unwrap(), &f["listResponse"]),
        (serde_json::to_value(&read).unwrap(), &f["readRequest"]),
        (
            serde_json::to_value(&extract).unwrap(),
            &f["extractRequest"],
        ),
        (serde_json::to_value(&create).unwrap(), &f["createRequest"]),
        (serde_json::to_value(&test).unwrap(), &f["testRequest"]),
        (
            serde_json::to_value(&read_response).unwrap(),
            &f["readResponse"],
        ),
        (
            serde_json::to_value(&operation_response).unwrap(),
            &f["operationResponse"],
        ),
        (
            serde_json::to_value(&test_response).unwrap(),
            &f["testResponse"],
        ),
        (serde_json::to_value(&progress).unwrap(), &f["progress"]),
        (serde_json::to_value(&error).unwrap(), &f["error"]),
    ] {
        assert_eq!(&actual, expected);
    }
    assert_eq!(caps.format, ArchiveFormat::Zip);
}

#[test]
fn invalid_member_paths_rejected_before_provider_access() {
    for path in [
        "../escape",
        "a/../b",
        "/root",
        "C:/root",
        "//server/share",
        "a\\b",
        "a//b",
        "a/./b",
        "a/",
        "a:stream",
        "CON",
        "a/NUL.txt",
        "a\0b",
    ] {
        assert!(MemberPath::parse(path).is_err(), "accepted {path:?}");
        assert!(
            serde_json::from_value::<Location>(
                json!({"kind":"archive","archivePath":"C:\\a.zip","memberPath":path})
            )
            .is_err()
        );
    }
    for path in [
        "a<b",
        "a>b",
        "a\"b",
        "a|b",
        "a?b",
        "a*b",
        "COM¹",
        "LPT².txt",
        "a\u{7f}b",
    ] {
        assert!(MemberPath::parse(path).is_err(), "accepted {path:?}");
    }
    assert_eq!(MemberPath::parse("").unwrap().as_str(), "");
    assert_eq!(
        MemberPath::parse("albums/été").unwrap().as_str(),
        "albums/été"
    );
}
#[test]
fn rar_create_request_is_accepted_in_contract() {
    // FER-505: Rar is now a supported format in ArchiveCreateRequest
    let mut create = fixture()["createRequest"].clone();
    create["format"] = json!("rar");
    let req = serde_json::from_value::<ArchiveCreateRequest>(create).unwrap();
    assert_eq!(req.format, ArchiveCreateFormat::Rar);

    // Unknown formats must still be rejected
    let mut bad = fixture()["createRequest"].clone();
    bad["format"] = json!("tar");
    assert!(serde_json::from_value::<ArchiveCreateRequest>(bad).is_err());
}

#[test]
fn parent_exits_archive_at_root_and_child_uses_slashes() {
    let location: Location = serde_json::from_value(fixture()["location"].clone()).unwrap();
    let child = location.archive_child("winter").unwrap();
    assert_eq!(child.member_path().unwrap().as_str(), "albums/été/winter");
    assert_eq!(child.parent().unwrap(), location);
    assert_eq!(
        location.archive_root().unwrap().parent(),
        Some(Location::Disk {
            path: "C:\\Work".into()
        })
    );
    assert!(location.archive_child("another.zip/inside").is_err());
}

#[test]
fn archive_root_parent_accepts_both_disk_separator_styles() {
    let windows = Location::Archive {
        archive_path: "C:\\Work\\photos.zip".into(),
        member_path: MemberPath::parse("").unwrap(),
    };
    let unix = Location::Archive {
        archive_path: "/home/user/photos.zip".into(),
        member_path: MemberPath::parse("").unwrap(),
    };
    assert_eq!(
        windows.parent(),
        Some(Location::Disk {
            path: "C:\\Work".into()
        })
    );
    assert_eq!(
        unix.parent(),
        Some(Location::Disk {
            path: "/home/user".into()
        })
    );
}

#[test]
fn listing_validates_member_containment_and_request_identity() {
    let response: ArchiveListResponse =
        serde_json::from_value(fixture()["listResponse"].clone()).unwrap();
    assert!(response.validate().is_ok());
    assert!(
        serde_json::from_value::<ArchiveListResponse>(
            json!({"location": fixture()["location"], "requestToken": -1, "entries": []})
        )
        .is_err()
    );
    let mut bad = response.clone();
    bad.entries[0].member_path = MemberPath::parse("other/photo.jpg").unwrap();
    assert!(bad.validate().is_err());
    assert!(
        serde_json::from_value::<ArchiveEntry>(
            json!({"memberPath":"../escape","name":"escape","isDir":false,"size":0,"modifiedMs":0})
        )
        .is_err()
    );
}
