// FER-576: `archive.create` may ask for 7z and a closed set of compression levels. RAR stays
// unreachable, an unknown or non-string level is invalid, and nothing but the closed enum ever
// reaches the engine (the model never names an engine flag).
use super::*;
use serde_json::{Value, json};

fn envelope(format: Value, level: Option<Value>) -> Value {
    let mut intent = json!({
        "capability": "archive.create",
        "sourcePane": "activeAtSubmit",
        "selector": { "kind": "selected" },
        "format": format,
        "target": { "kind": "sourceDirectory" }
    });
    if let Some(level) = level {
        intent["level"] = level;
    }
    json!({ "schemaVersion": 1, "requestId": "req_1", "intent": intent })
}

#[test]
fn seven_zip_with_every_level_parses_and_round_trips() {
    for level in ["store", "fast", "normal", "maximum", "ultra"] {
        let value = envelope(json!("7z"), Some(json!(level)));
        let parsed = parse_intent(value.clone(), "req_1").expect(level);
        match &parsed.intent {
            Intent::ArchiveCreate(a) => assert_eq!(a.format, ArchiveFormat::Sevenz),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
}

#[test]
fn level_is_optional_and_zip_still_works() {
    for format in ["zip", "7z"] {
        let value = envelope(json!(format), None);
        let parsed = parse_intent(value.clone(), "req_1").unwrap();
        match &parsed.intent {
            Intent::ArchiveCreate(a) => assert!(a.level.is_none()),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
}

#[test]
fn unknown_formats_and_levels_are_invalid() {
    for format in ["rar", "7zip", "tar", "ZIP", ""] {
        assert!(
            parse_intent(envelope(json!(format), None), "req_1").is_err(),
            "format {format:?}"
        );
    }
    for level in [
        json!("insane"),
        json!("ULTRA"),
        json!("-mx=9"),
        json!(9),
        json!(null),
        json!(["ultra"]),
    ] {
        assert!(
            parse_intent(envelope(json!("7z"), Some(level.clone())), "req_1").is_err(),
            "level {level}"
        );
    }
}

#[test]
fn level_cannot_smuggle_extra_fields() {
    let mut value = envelope(json!("7z"), Some(json!("ultra")));
    value["intent"]["password"] = json!("secret");
    assert!(parse_intent(value, "req_1").is_err());
}
