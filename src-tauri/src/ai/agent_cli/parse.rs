//! Reading an agent's JSON event stream (one JSON object per line) into chat events.
//!
//! Only TEXT leaves this module: assistant text, a fixed status label per tool kind (never the
//! tool input), the final answer and an error line. Nothing is interpreted as a command, a plan
//! or a file operation; tool calls the agent makes are only shown as "membaca file…".

use serde_json::Value;

use super::{AgentId, MAX_ANSWER_CHARS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A piece of the answer, shown while the agent works.
    Text(String),
    /// Fixed app text describing what the agent is doing.
    Status(&'static str),
}

/// Accumulates one run's answer from the agent's event lines.
#[derive(Debug)]
pub struct StreamParser {
    agent: AgentId,
    messages: Vec<String>,
    final_text: Option<String>,
    error: Option<String>,
}

impl StreamParser {
    pub fn new(agent: AgentId) -> Self {
        Self {
            agent,
            messages: Vec::new(),
            final_text: None,
            error: None,
        }
    }

    /// Feed one stdout line; returns what the chat should show now. Non-JSON lines are ignored.
    pub fn line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        match self.agent {
            AgentId::Claude => self.claude(&v),
            AgentId::Codex => self.codex(&v),
            AgentId::Omp
            | AgentId::Antigravity
            | AgentId::Opencode
            | AgentId::Gemini
            | AgentId::Aider => Vec::new(),
        }
    }

    fn claude(&mut self, v: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let content = v.pointer("/message/content").and_then(Value::as_array);
                for block in content.into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str)
                                && !t.trim().is_empty()
                            {
                                self.messages.push(t.to_owned());
                                out.push(AgentEvent::Text(t.to_owned()));
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                            out.push(AgentEvent::Status(tool_label(name)));
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => {
                let text = v.get("result").and_then(Value::as_str).map(str::to_owned);
                if v.get("is_error").and_then(Value::as_bool) == Some(true) {
                    let sub = v.get("subtype").and_then(Value::as_str).unwrap_or("error");
                    self.error = Some(text.unwrap_or_else(|| sub.to_owned()));
                } else if let Some(t) = text {
                    self.final_text = Some(t);
                }
            }
            _ => {}
        }
        out
    }

    fn codex(&mut self, v: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        match v.get("type").and_then(Value::as_str) {
            Some("item.started") => {
                if v.pointer("/item/type").and_then(Value::as_str) == Some("command_execution") {
                    out.push(AgentEvent::Status(
                        "menjalankan perintah baca (sandbox read-only)",
                    ));
                }
            }
            Some("item.completed") => {
                if v.pointer("/item/type").and_then(Value::as_str) == Some("agent_message")
                    && let Some(t) = v.pointer("/item/text").and_then(Value::as_str)
                    && !t.trim().is_empty()
                {
                    self.messages.push(t.to_owned());
                    out.push(AgentEvent::Text(t.to_owned()));
                }
            }
            Some("turn.failed") => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("turn failed");
                self.error = Some(msg.to_owned());
            }
            Some("error") => {
                let msg = v.get("message").and_then(Value::as_str).unwrap_or("error");
                self.error = Some(msg.to_owned());
            }
            _ => {}
        }
        out
    }

    /// Error reported by the agent itself (already cut to one short line).
    pub fn error(&self) -> Option<String> {
        self.error.as_deref().map(short_line)
    }

    /// Final answer: the agent's own result (Claude) or its last message (Codex: earlier messages
    /// are progress notes), else everything it said. Cut to [`MAX_ANSWER_CHARS`].
    pub fn answer(&self) -> String {
        let text = match (&self.final_text, self.agent) {
            (Some(t), _) => t.clone(),
            (None, AgentId::Codex) => self.messages.last().cloned().unwrap_or_default(),
            (None, _) => self.messages.join("\n\n"),
        };
        truncate_chars(&text, MAX_ANSWER_CHARS)
    }
}

/// Fixed label per tool kind. The tool input (path, pattern) is never shown or forwarded.
pub fn tool_label(name: &str) -> &'static str {
    match name {
        "Read" => "membaca file",
        "Grep" => "mencari di dalam file",
        "Glob" => "mendaftar file",
        _ => "memakai tool",
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// One line of at most 300 characters (agent errors, stderr tail).
pub fn short_line(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let clean: String = line.chars().filter(|c| !c.is_control()).collect();
    truncate_chars(&clean, 300)
}
