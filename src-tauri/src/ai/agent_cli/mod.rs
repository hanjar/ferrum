//! FER-490 "Ask AI agent": a question about attached files is answered by a LOCAL agent CLI
//! (Claude Code, Codex) that reads the files itself, so PDFs, Office files and large files work
//! where "Add to chat" (FER-486) only sends metadata.
//!
//! Security model (design decision, FER-490):
//! - The agent runs in a read-only mode that the AGENT enforces (tool allowlist / OS sandbox), see
//!   [`args`]. Agents without such a mode (omp, Antigravity) are listed as unsupported and never
//!   started.
//! - The executable is resolved once ([`detect`]), stored in the agent settings and shown to the
//!   user with its full path. Only `.exe` files are started, never `.cmd`/`.bat`/`.ps1` and never
//!   through a shell. Prompt and attachment paths go through STDIN; the only path on the command
//!   line is the working folder / the parent folder of an attachment (`--add-dir`), passed through
//!   `CreateProcessW` argument quoting (no `cmd.exe`, so no BatBadBut).
//! - Clean environment (allowlist), fresh working folder per question under the Ferrum data dir
//!   (deleted afterwards), 5 minute timeout, cancel, the whole process tree in a kill-on-close job
//!   object, output capped at 1 MB ([`runner`]).
//! - The answer is plain text for the chat only. Nothing here can reach the Ferrum executor.
//! - Test profile (`FERRUM_PROFILE_DIR`): real agents are never detected or started; the profile
//!   names fake agents in `<profile>/config/agent-cli-test.json`.
//!
//! FER-494 adds a second, separate capability: [`terminal`] opens a found agent INTERACTIVELY in
//! a visible terminal ("Buka di terminal"); the user leads with the agent's own permissions. The
//! single agent definition list is [`terminal::AGENT_DEFS`].

pub mod acl;
pub mod args;
pub mod commands;
pub mod detect;
pub mod parse;
pub mod runner;
pub mod settings;
pub mod snapshot;
pub mod terminal;

#[cfg(test)]
mod acl_tests;
#[cfg(test)]
mod args_tests;
#[cfg(test)]
mod detect_tests;
#[cfg(test)]
mod parse_tests;
#[cfg(all(test, windows))]
mod runner_tests;
#[cfg(all(test, windows))]
mod snapshot_tests;
#[cfg(test)]
mod terminal_tests;

use serde::{Deserialize, Serialize};

/// Agents Ferrum knows about. The order is the display order and the default-agent preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentId {
    Claude,
    Codex,
    Omp,
    Antigravity,
    /// FER-494: terminal mode only (no read-only mode evaluated).
    Opencode,
    Gemini,
    Aider,
}

pub const ALL_AGENTS: [AgentId; 7] = [
    AgentId::Claude,
    AgentId::Codex,
    AgentId::Omp,
    AgentId::Antigravity,
    AgentId::Opencode,
    AgentId::Gemini,
    AgentId::Aider,
];

impl AgentId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Omp => "omp",
            Self::Antigravity => "antigravity",
            Self::Opencode => "opencode",
            Self::Gemini => "gemini",
            Self::Aider => "aider",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL_AGENTS.into_iter().find(|a| a.as_str() == s)
    }

    /// The agent's entry in the single definition list ([`terminal::AGENT_DEFS`]).
    pub fn def(self) -> &'static terminal::AgentDef {
        terminal::AGENT_DEFS
            .iter()
            .find(|d| d.id == self)
            .expect("every AgentId has a definition")
    }

    pub fn display_name(self) -> &'static str {
        self.def().name
    }

    /// Service that receives the file content (shown in the one-time disclosure).
    pub fn vendor(self) -> &'static str {
        self.def().vendor
    }

    /// FER-490 E2: the agent's read-only mode cannot be limited to the working folder on this
    /// platform (Codex 0.159.1: "Restricted read-only access requires the elevated Windows sandbox
    /// backend", and the elevated backend "requires effective `:root` read access"). Such an
    /// agent is OFF until the user opts in explicitly in Settings.
    pub fn broad_read(self) -> bool {
        matches!(self, Self::Codex)
    }

    /// `Ok(mode description)` when Ferrum can force a read-only mode, else `Err(reason)`.
    pub fn read_only_mode(self) -> Result<&'static str, &'static str> {
        match self {
            Self::Claude => Ok(
                "claude -p (headless), tool hanya Read/Grep/Glob, --restricted tanpa --add-dir (hanya membaca SALINAN lampiran di folder kerja), --permission-mode dontAsk, tanpa MCP, plugin, hook, atau settings user/proyek",
            ),
            Self::Codex => Ok(
                "codex exec --sandbox read-only (sandbox Windows unelevated), approval never, tanpa config user/MCP, tanpa web search, -C folder kerja berisi SALINAN lampiran; BACA LUAS: Codex tetap bisa membaca file lain yang bisa dibaca akun Anda",
            ),
            Self::Omp => Err(
                "tidak didukung: tidak ada mode baca-saja. omp tidak punya sandbox atau mode izin baca-saja; --tools hanya daftar tool, tool read bisa membaca URL dan path mana pun, dan tidak ada flag untuk mematikan MCP/extension dari config user.",
            ),
            Self::Antigravity => Err(
                "tidak didukung: tidak ada mode baca-saja. agy tidak punya daftar tool atau sandbox file baca-saja; --sandbox hanya membatasi terminal, --mode plan tidak dijamin baca-saja, dan aturan allow di settings user tetap berlaku di mode print.",
            ),
            Self::Opencode | Self::Gemini | Self::Aider => Err(
                "tidak didukung untuk baca-saja: mode baca-saja agent ini belum dievaluasi. Agent ini hanya bisa dibuka di terminal (Buka di terminal).",
            ),
        }
    }
}

/// Default timeout of one question (5 minutes, cancellable).
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Stdout bytes read from an agent before it is stopped (about 1 MB).
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Longest answer kept (the chat's Markdown renderer shows at most 20 000 characters).
pub const MAX_ANSWER_CHARS: usize = 20_000;
