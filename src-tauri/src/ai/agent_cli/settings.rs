//! Agent CLI settings (`agent-cli.json` in the app config dir, or the test profile's config dir):
//! the detection result with the resolved executable paths, the default agent and the agents
//! whose disclosure the user acknowledged. No secrets. Written atomically.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::AgentId;
use super::detect::{AgentStatus, DetectedAgent};

pub const FILE_NAME: &str = "agent-cli.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    #[serde(default)]
    pub agents: Vec<DetectedAgent>,
    #[serde(default)]
    pub detected_at_ms: i64,
    #[serde(default)]
    pub default_agent: Option<AgentId>,
    /// Agents whose "file content goes to <vendor>" disclosure was acknowledged.
    #[serde(default)]
    pub acknowledged: Vec<AgentId>,
    /// FER-490 E2: broad-read agents (Codex) the user explicitly switched on in Settings.
    #[serde(default)]
    pub opted_in: Vec<AgentId>,
    /// FER-494: agent opened by the composer's "Buka di terminal" button.
    #[serde(default)]
    pub terminal_default: Option<AgentId>,
    /// FER-494: stored detections older than [`SCHEMA`] lack the new agents and the terminal
    /// check, and are detected again (preferences kept).
    #[serde(default)]
    pub schema: u32,
}

pub const SCHEMA: u32 = 2;

impl AgentSettings {
    pub fn agent(&self, id: AgentId) -> Option<&DetectedAgent> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// Detected with an enforceable read-only mode (may still need an opt-in).
    pub fn detected_supported(&self, id: AgentId) -> bool {
        self.agent(id)
            .is_some_and(|a| a.status == AgentStatus::Supported && a.launch.is_some())
    }

    /// Usable now: supported and, for a broad-read agent, explicitly switched on (default OFF).
    pub fn supported(&self, id: AgentId) -> bool {
        self.detected_supported(id) && (!id.broad_read() || self.opted_in.contains(&id))
    }

    /// FER-494: may be opened in a terminal now (found, valid exe, ACL passed at detection).
    pub fn terminal_ready(&self, id: AgentId) -> bool {
        self.agent(id)
            .is_some_and(|a| a.terminal_ready && a.launch.is_some())
    }

    /// FER-494: the chosen terminal agent if still ready, else the first ready agent.
    pub fn effective_terminal_default(&self) -> Option<AgentId> {
        self.terminal_default
            .filter(|id| self.terminal_ready(*id))
            .or_else(|| {
                self.agents
                    .iter()
                    .map(|a| a.id)
                    .find(|id| self.terminal_ready(*id))
            })
    }

    /// The chosen default if it is still usable, else the first usable agent.
    pub fn effective_default(&self) -> Option<AgentId> {
        self.default_agent
            .filter(|id| self.supported(*id))
            .or_else(|| {
                self.agents
                    .iter()
                    .map(|a| a.id)
                    .find(|id| self.supported(*id))
            })
    }
}

pub fn load_from(dir: &Path) -> Option<AgentSettings> {
    let raw = fs::read_to_string(dir.join(FILE_NAME)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_to(dir: &Path, settings: &AgentSettings) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, dir.join(FILE_NAME)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}
