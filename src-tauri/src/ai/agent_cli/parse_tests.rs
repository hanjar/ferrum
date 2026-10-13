//! FER-490: event streams become text only.

use super::AgentId;
use super::parse::*;

#[test]
fn claude_stream_yields_text_status_and_the_result() {
    let mut p = StreamParser::new(AgentId::Claude);
    assert!(
        p.line(r#"{"type":"system","subtype":"init","tools":["Read"]}"#)
            .is_empty()
    );
    let ev = p.line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Membaca."},{"type":"tool_use","name":"Read","input":{"file_path":"C:\\secret\\x.txt"}}]}}"#,
    );
    assert_eq!(
        ev,
        vec![
            AgentEvent::Text("Membaca.".into()),
            AgentEvent::Status("membaca file")
        ]
    );
    let ev = p.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__evil__run","input":{}}]}}"#);
    assert_eq!(ev, vec![AgentEvent::Status("memakai tool")]);
    p.line(r#"{"type":"result","subtype":"success","is_error":false,"result":"Final answer."}"#);
    assert_eq!(p.answer(), "Final answer.");
    assert!(p.error().is_none());
}

#[test]
fn claude_error_result_is_an_error() {
    let mut p = StreamParser::new(AgentId::Claude);
    p.line(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Not logged in\nmore"}"#);
    assert_eq!(p.error().as_deref(), Some("Not logged in"));
}

#[test]
fn codex_answer_is_the_last_message() {
    let mut p = StreamParser::new(AgentId::Codex);
    p.line(r#"{"type":"thread.started","thread_id":"t"}"#);
    p.line(r#"{"type":"item.completed","item":{"id":"0","type":"error","message":"Ignoring malformed agent role"}}"#);
    let ev = p.line(r#"{"type":"item.started","item":{"type":"command_execution","command":"Remove-Item C:\\"}}"#);
    assert_eq!(
        ev,
        vec![AgentEvent::Status(
            "menjalankan perintah baca (sandbox read-only)"
        )]
    );
    p.line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"I'll read it."}}"#);
    p.line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"It is a report."}}"#);
    p.line(r#"{"type":"turn.completed","usage":{}}"#);
    assert_eq!(p.answer(), "It is a report.");
    // An item of type "error" is a warning, not a failure.
    assert!(p.error().is_none());
}

#[test]
fn codex_turn_failure_is_an_error() {
    let mut p = StreamParser::new(AgentId::Codex);
    p.line(r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#);
    assert_eq!(p.error().as_deref(), Some("usage limit reached"));
}

#[test]
fn garbage_lines_are_ignored_and_answers_are_capped() {
    let mut p = StreamParser::new(AgentId::Claude);
    assert!(p.line("not json").is_empty());
    assert!(p.line("").is_empty());
    let long = "é".repeat(super::MAX_ANSWER_CHARS + 50);
    let line = serde_json::json!({"type":"result","is_error":false,"result":long}).to_string();
    p.line(&line);
    assert_eq!(p.answer().chars().count(), super::MAX_ANSWER_CHARS);
}

#[test]
fn short_line_drops_control_characters() {
    assert_eq!(short_line("\n  bad\u{1b}[31m line \nnext"), "bad[31m line");
}
