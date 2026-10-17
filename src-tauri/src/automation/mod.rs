//! Automation backend (Sync Profiles, Automation design §4/§7): Tauri-free core modules
//! (`model`, `confirm_text`, `store`) plus the thin command layer. Everything under this tree is
//! linked by the future headless runner, so only `commands.rs` may touch the app runtime — a
//! source test below keeps that guarantee from rotting.

pub mod approval;
pub mod commands;
pub mod confirm_text;
pub mod dpapi;
pub mod dto;
pub mod lease;
pub mod manual_run;
pub mod model;
pub mod policy;
pub mod profile_baseline;
pub mod read;
pub mod status;
pub mod store;
pub mod test_run;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod source_tests {
    /// Grep gate (design §2, work package P0.3): nothing under `sync_engine/**` or
    /// `automation/**` may reference the app runtime module except `automation/commands.rs` —
    /// the runner must stay free of a GUI runtime dependency.
    #[test]
    fn only_commands_may_reference_the_app_runtime() {
        let needle = concat!("tauri", "::");
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let commands_rs = root.join("automation").join("commands.rs");
        let mut offenders = Vec::new();
        let mut stack = vec![root.join("sync_engine"), root.join("automation")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("source tree present") {
                let path = entry.expect("readable").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path == commands_rs {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("readable source");
                if text.contains(needle) {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(offenders.is_empty(), "found `{needle}` in: {offenders:?}");
    }
}
