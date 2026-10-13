#[path = "../../../../src-tauri/src/ai/contract.rs"]
pub mod contract;

#[cfg(test)]
mod tests {
    use super::contract::{
        assert_prepared_for_plan, parse_execution_receipt, parse_intent, parse_pane_snapshot,
        parse_prepared_handle, parse_resolved_plan, Intent,
    };
    use serde_json::{json, Value};

    fn archive() -> Value {
        json!({"schemaVersion":1,"requestId":"req_1","intent":{
            "capability":"archive.create","sourcePane":"left",
            "selector":{"kind":"allVisible","entries":"filesOnly"},
            "format":"zip","target":{"kind":"pane","pane":"right"},
            "outputName":"backup.zip"}})
    }

    fn sync() -> Value {
        json!({"schemaVersion":1,"requestId":"req_1","intent":{
            "capability":"directories.sync","leftPane":"left","rightPane":"right",
            "mode":"bidirectional"}})
    }

    fn snapshot() -> Value {
        json!({"schemaVersion":1,"requestId":"req_1","activePaneAtSubmit":"left","panes":[
            {"paneId":"left","tabId":"tab_left","location":{"kind":"disk","path":"C:\\left"},
             "contextRevision":2,"selectionRevision":3,"selectionHandle":"sel_left","filter":"",
             "showHidden":false,"sort":{"key":"name","dir":1}},
            {"paneId":"right","tabId":"tab_right","location":{"kind":"disk","path":"C:\\right"},
             "contextRevision":4,"selectionRevision":1,"selectionHandle":"sel_right","filter":"",
             "showHidden":false,"sort":{"key":"modified","dir":-1}}]})
    }

    fn resolved_sync() -> Value {
        json!({"schemaVersion":1,"planId":"plan_1","requestId":"req_1","revision":1,
            "capability":"directories.sync","sourceSnapshot":snapshot(),
            "operation":{"kind":"directories.sync","leftRoot":"C:\\left","rightRoot":"C:\\right",
                "leftManifest":"manifest_left","rightManifest":"manifest_right"},
            "preview":{"itemCount":2,"conflictCount":0,"totalBytes":1024},
            "steps":[{"id":"step_1","capability":"directories.sync","dependsOn":[]},
                {"id":"step_2","capability":"directories.sync","dependsOn":["step_1"]}],
            "previewDigest":"digest_1","risk":"mutating","requiresApproval":true,"stepCount":2})
    }

    #[test]
    fn snapshot_sort_matches_ui_shape() {
        let value = snapshot();
        let parsed = parse_pane_snapshot(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        for sort in [json!("name"), json!({"key":"extension","dir":1}),
            json!({"key":"size","dir":0}), json!({"key":"modified","dir":2}),
            json!({"key":"name","dir":1,"extra":true})] {
            let mut bad = snapshot();
            bad["panes"][0]["sort"] = sort.clone();
            assert!(parse_pane_snapshot(bad).is_err(), "accepted {sort}");
        }
        for key in ["name", "size", "modified"] {
            for dir in [-1, 1] {
                let mut valid = snapshot();
                valid["panes"][0]["sort"] = json!({"key":key,"dir":dir});
                assert!(parse_pane_snapshot(valid).is_ok());
            }
        }
    }

    #[test]
    fn resolved_sync_roundtrip_and_required_fields() {
        let value = resolved_sync();
        assert_eq!(serde_json::to_value(parse_resolved_plan(value.clone()).unwrap()).unwrap(), value);
        for field in ["operation", "preview", "steps"] {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(parse_resolved_plan(bad).is_err(), "missing {field}");
        }
        for (pointer, replacement) in [
            ("/operation/kind", json!("filesystem.delete")),
            ("/operation/leftRoot", json!("C:\\elsewhere")),
            ("/operation/rightRoot", json!("..\\right")),
            ("/operation/leftManifest", json!("../bad")),
            ("/preview/itemCount", json!(1000001)),
            ("/preview/conflictCount", json!(1000001)),
            ("/preview/totalBytes", json!(9007199254740992_u64)),
            ("/steps/0/id", json!("../step")),
            ("/steps/0/capability", json!("archive.create")),
            ("/steps/0/dependsOn", json!(["step_2"])),
            ("/steps/1/dependsOn", json!(["step_1", "step_1"])),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_resolved_plan(bad).is_err(), "accepted {pointer}");
        }
        for pointer in ["/operation", "/preview", "/steps/0"] {
            let mut bad = value.clone();
            bad.pointer_mut(pointer).unwrap().as_object_mut().unwrap().insert("extra".into(), json!(true));
            assert!(parse_resolved_plan(bad).is_err(), "accepted extra at {pointer}");
        }
        let mut bad = value.clone();
        bad["steps"][1]["id"] = json!("step_1");
        assert!(parse_resolved_plan(bad).is_err());
        let mut bad = value.clone();
        bad["stepCount"] = json!(1);
        assert!(parse_resolved_plan(bad).is_err());
        let mut bad = value.clone();
        bad["steps"] = json!([]);
        bad["stepCount"] = json!(0);
        assert!(parse_resolved_plan(bad).is_err());
    }

    #[test]
    fn archive_operation_and_windows_path_safety() {
        let mut value = resolved_sync();
        value["capability"] = json!("archive.create");
        value["operation"] = json!({"kind":"archive.create","sourceManifest":"manifest_src",
            "targetDirectory":"C:\\archives","outputName":"backup.zip"});
        value["steps"] = json!([{"id":"step_1","capability":"archive.create","dependsOn":[]}]);
        value["stepCount"] = json!(1);
        assert_eq!(serde_json::to_value(parse_resolved_plan(value.clone()).unwrap()).unwrap(), value);
        for (pointer, replacement) in [
            ("/operation/sourceManifest", json!("../manifest")),
            ("/operation/targetDirectory", json!("C:relative")),
            ("/operation/targetDirectory", json!("C:\\archives\\..\\secret")),
            ("/operation/targetDirectory", json!("/tmp/archives")),
            ("/operation/targetDirectory", json!("C:\\archives\\CON")),
            ("/operation/targetDirectory", json!("C:\\archives\\a?b")),
            ("/operation/targetDirectory", json!("C:\\archives\\\\secret")),
            ("/operation/outputName", json!("CON.zip")),
            ("/operation/outputName", json!("../backup.zip")),
            ("/operation/kind", json!("directories.sync")),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_resolved_plan(bad).is_err(), "accepted {pointer}");
        }
    }

    #[test]
    fn shared_fixtures_match_rust_contract() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../cases.json")).unwrap();
        for case in cases {
            assert_eq!(case["kind"], "intent");
            let actual = parse_intent(case["value"].clone(), "req_1");
            assert_eq!(
                actual.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}: {actual:?}",
                case["name"]
            );
            if let Ok(envelope) = actual {
                assert_eq!(
                    serde_json::to_value(envelope).unwrap(),
                    case["value"],
                    "{}",
                    case["name"]
                );
            }
        }
    }

    #[test]
    fn shared_trusted_fixtures_match_rust_contract() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../trusted-cases.json")).unwrap();
        for case in cases {
            let value = case["value"].clone();
            let actual =
                match case["kind"].as_str().unwrap() {
                    "snapshot" => parse_pane_snapshot(value)
                        .map(|parsed| serde_json::to_value(parsed).unwrap()),
                    "plan" => parse_resolved_plan(value)
                        .map(|parsed| serde_json::to_value(parsed).unwrap()),
                    "handle" => parse_prepared_handle(value)
                        .map(|parsed| serde_json::to_value(parsed).unwrap()),
                    "receipt" => parse_execution_receipt(value)
                        .map(|parsed| serde_json::to_value(parsed).unwrap()),
                    _ => panic!("unknown fixture kind"),
                };
            assert_eq!(
                actual.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}: {actual:?}",
                case["name"]
            );
            if let Ok(parsed) = actual {
                assert_eq!(parsed, case["value"], "{}", case["name"]);
            }
        }
    }

    #[test]
    fn snapshot_filter_uses_utf8_byte_bound() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../trusted-cases.json")).unwrap();
        let mut value = cases[0]["value"].clone();
        value["panes"][0]["filter"] = json!("é".repeat(200));
        assert!(parse_pane_snapshot(value.clone()).is_err());
        value["panes"][0]["filter"] = json!("é".repeat(128));
        assert!(parse_pane_snapshot(value).is_ok());
    }

    #[test]
    fn receipt_message_uses_utf8_byte_bound() {
        let mut value = serde_json::json!({"schemaVersion":1,"executionId":"exec_1",
            "planId":"plan_1","jobIds":[],"state":"partial","completed":0,
            "failed":1,"errors":[{"code":"invalid_payload","message":"é".repeat(300)}]});
        assert!(parse_execution_receipt(value.clone()).is_err());
        value["errors"][0]["message"] = serde_json::json!("é".repeat(256));
        assert!(parse_execution_receipt(value).is_ok());
    }

    #[test]
    fn prepared_handle_matches_plan_and_expiry() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../trusted-cases.json")).unwrap();
        let plan = parse_resolved_plan(cases[1]["value"].clone()).unwrap();
        let handle = parse_prepared_handle(cases[2]["value"].clone()).unwrap();
        assert!(assert_prepared_for_plan(&handle, &plan, 1_999_999_999_999).is_ok());
        assert!(assert_prepared_for_plan(&handle, &plan, 2_000_000_000_000).is_err());
        let mut bad = handle.clone();
        bad.plan_id = "plan_2".into();
        assert!(assert_prepared_for_plan(&bad, &plan, 0).is_err());
        let mut bad = handle.clone();
        bad.request_id = "req_2".into();
        assert!(assert_prepared_for_plan(&bad, &plan, 0).is_err());
        let mut bad = handle.clone();
        bad.revision = 2;
        assert!(assert_prepared_for_plan(&bad, &plan, 0).is_err());
        let mut bad = handle.clone();
        bad.plan_digest = "digest_2".into();
        assert!(assert_prepared_for_plan(&bad, &plan, 0).is_err());
    }

    #[test]
    fn typed_intents_and_roundtrip() {
        let archive = parse_intent(archive(), "req_1").unwrap();
        assert!(matches!(archive.intent, Intent::ArchiveCreate(_)));
        let sync = parse_intent(sync(), "req_1").unwrap();
        assert!(matches!(sync.intent, Intent::DirectoriesSync(_)));
        assert_eq!(serde_json::to_value(sync).unwrap(), self::sync());
    }

    #[test]
    fn rejects_request_and_schema_spoofing() {
        for id in ["", "a/b", "é", "two words", ".", "a".repeat(129).as_str()] {
            let mut value = sync();
            value["requestId"] = json!(id);
            assert!(parse_intent(value, "req_1").is_err(), "accepted {id:?}");
        }
        assert!(parse_intent(sync(), "other_request").is_err());
        assert!(parse_intent(sync(), "../req_1").is_err());
        let mut value = sync();
        value["schemaVersion"] = json!(2);
        assert!(parse_intent(value, "req_1").is_err());
    }

    #[test]
    fn rejects_untrusted_fields_and_invalid_references() {
        for (pointer, replacement) in [
            ("/intent/capability", json!("filesystem.delete")),
            ("/intent/target/pane", json!("third")),
            ("/intent/selector/kind", json!("entryIds")),
            ("/intent/selector/entries", json!("directoriesOnly")),
            ("/intent/format", json!("rar")),
        ] {
            let mut value = archive();
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_intent(value, "req_1").is_err(), "accepted {pointer}");
        }
        for (pointer, replacement) in [
            ("/intent/leftPane", json!("right")),
            ("/intent/rightPane", json!("left")),
            ("/intent/mode", json!("mirror")),
        ] {
            let mut value = sync();
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_intent(value, "req_1").is_err(), "accepted {pointer}");
        }
        for (pointer, replacement) in [
            ("/intent/target/path", json!("C:/secret")),
            ("/intent/selector/count", json!(10000)),
            ("/intent/selector/ids", json!(["made-up"])),
            ("/intent/approval", json!(true)),
            ("/intent/risk", json!("safe")),
        ] {
            let mut value = archive();
            let (parent, name) = pointer.rsplit_once('/').unwrap();
            value
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(name.into(), replacement);
            assert!(parse_intent(value, "req_1").is_err(), "accepted {pointer}");
        }
    }

    #[test]
    fn rejects_unsafe_output_names_at_boundaries() {
        for name in [
            "",
            "../secret.zip",
            "C:\\secret.zip",
            "backup:evil.zip",
            "CON.zip",
            "a/zip",
            "a\\zip",
            "file.",
            "file ",
            "bad?.zip",
            "é.zip",
        ] {
            let mut value = archive();
            value["intent"]["outputName"] = json!(name);
            assert!(parse_intent(value, "req_1").is_err(), "accepted {name:?}");
        }
        let mut value = archive();
        value["intent"]["outputName"] = json!("a".repeat(128));
        assert!(parse_intent(value.clone(), "req_1").is_ok());
        value["intent"]["outputName"] = json!("a".repeat(129));
        assert!(parse_intent(value, "req_1").is_err());
    }

    #[test]
    fn rejects_noncanonical_output_names_and_null_option() {
        for name in [".hidden.zip", "-draft.zip", "~backup.zip", "backup+.zip"] {
            let mut value = archive();
            value["intent"]["outputName"] = json!(name);
            assert!(parse_intent(value, "req_1").is_err(), "accepted {name:?}");
        }
        let mut value = archive();
        value["intent"]["outputName"] = Value::Null;
        assert!(
            parse_intent(value, "req_1").is_err(),
            "accepted null outputName"
        );
    }

    #[test]
    fn limits_input_size_and_depth() {
        let mut value = sync();
        value["intent"]["extra"] = json!("x".repeat(33 * 1024));
        assert!(parse_intent(value, "req_1").is_err());
        let mut value = archive();
        let mut nested = json!(null);
        for _ in 0..30 {
            nested = json!([nested]);
        }
        value["intent"]["untrusted"] = nested;
        assert!(parse_intent(value, "req_1").is_err());
        assert!(parse_intent(json!(null), "req_1").is_err());
    }
}
