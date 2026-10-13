//! Tauri commands untuk setup provider AI non-rahasia (FER-430). Hanya endpoint/model/timeout/
//! structured; key tetap lewat `credential_commands`. Galat ke frontend berupa kode aman saja.

use serde::Serialize;
use tauri::AppHandle;

use super::provider::ProviderConfig;
use super::setup_store::{self, SetupStoreError};

/// Galat aman ke frontend: hanya kode, tanpa nilai atau path.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SetupCommandError {
    pub kind: &'static str,
}

impl From<SetupStoreError> for SetupCommandError {
    fn from(error: SetupStoreError) -> Self {
        Self { kind: error.kind() }
    }
}

fn config_dir(app: &AppHandle) -> std::path::PathBuf {
    crate::profile::config_dir(app).unwrap_or_else(std::env::temp_dir)
}

#[tauri::command]
pub fn ai_setup_load(app: AppHandle) -> Option<ProviderConfig> {
    setup_store::load_from(&config_dir(&app))
}

#[tauri::command]
pub fn ai_setup_save(app: AppHandle, config: ProviderConfig) -> Result<(), SetupCommandError> {
    setup_store::save_to(&config_dir(&app), &config).map_err(Into::into)
}

#[tauri::command]
pub fn ai_setup_clear(app: AppHandle) -> Result<(), SetupCommandError> {
    setup_store::clear_in(&config_dir(&app)).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_store_errors_to_safe_kinds() {
        assert_eq!(
            SetupCommandError::from(SetupStoreError::UnsafeEndpoint).kind,
            "unsafe_endpoint"
        );
        assert_eq!(
            SetupCommandError::from(SetupStoreError::InvalidConfig).kind,
            "invalid_config"
        );
        assert_eq!(
            SetupCommandError::from(SetupStoreError::StorageFailure).kind,
            "storage_failure"
        );
    }
}
