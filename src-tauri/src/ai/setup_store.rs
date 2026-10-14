//! Penyimpanan setup provider AI yang TIDAK rahasia (FER-430): endpoint, model, timeout, mode
//! structured. File `ai-setup.json` di app config dir (sama dengan `config.json` milik prefs),
//! ditulis atomik dan divalidasi dengan `ProviderConfig::validate` saat simpan DAN saat muat.
//!
//! Sengaja bukan field di `Prefs`: Prefs ditulis ulang utuh pada setiap perubahan UI, sehingga
//! frontend lama yang tidak mengirim setup AI akan menimpanya. Key tidak pernah lewat sini; ia
//! tetap di Windows Credential Manager (`Ferrum/AI/custom`).

use std::fs;
use std::path::Path;

use super::provider::{ProviderConfig, ProviderErrorKind};

pub const FILE_NAME: &str = "ai-setup.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStoreError {
    /// Endpoint ditolak (bukan HTTPS dan bukan localhost+port eksplisit).
    UnsafeEndpoint,
    /// Field lain tidak valid.
    InvalidConfig,
    /// Disk tidak bisa ditulis.
    StorageFailure,
    /// FER-512 P3-1: pengguna menjawab tidak di dialog konfirmasi native; tidak ada yang
    /// tersimpan.
    NotConfirmed,
}

impl SetupStoreError {
    pub fn kind(self) -> &'static str {
        match self {
            Self::UnsafeEndpoint => "unsafe_endpoint",
            Self::InvalidConfig => "invalid_config",
            Self::StorageFailure => "storage_failure",
            Self::NotConfirmed => "not_confirmed",
        }
    }
}

/// Setup tersimpan yang valid, atau `None` (tidak ada file, rusak, field asing, atau tidak valid).
pub fn load_from(dir: &Path) -> Option<ProviderConfig> {
    let raw = fs::read_to_string(dir.join(FILE_NAME)).ok()?;
    let config: ProviderConfig = serde_json::from_str(&raw).ok()?;
    config.validate().ok()?;
    Some(config)
}

/// Validasi lalu tulis atomik (temp + rename) agar crash tidak meninggalkan setengah file.
pub fn save_to(dir: &Path, config: &ProviderConfig) -> Result<(), SetupStoreError> {
    config.validate().map_err(|kind| match kind {
        ProviderErrorKind::UnsafeEndpoint => SetupStoreError::UnsafeEndpoint,
        _ => SetupStoreError::InvalidConfig,
    })?;
    let text = serde_json::to_string_pretty(config).map_err(|_| SetupStoreError::StorageFailure)?;
    fs::create_dir_all(dir).map_err(|_| SetupStoreError::StorageFailure)?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    fs::write(&tmp, text).map_err(|_| SetupStoreError::StorageFailure)?;
    fs::rename(&tmp, dir.join(FILE_NAME)).map_err(|_| {
        let _ = fs::remove_file(&tmp);
        SetupStoreError::StorageFailure
    })
}

/// Hapus setup tersimpan; file yang memang tidak ada dianggap sukses (idempoten).
pub fn clear_in(dir: &Path) -> Result<(), SetupStoreError> {
    match fs::remove_file(dir.join(FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SetupStoreError::StorageFailure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::StructuredMode;

    fn config() -> ProviderConfig {
        ProviderConfig {
            endpoint: "http://127.0.0.1:20128/v1".into(),
            model: "cx/gpt-6-luna".into(),
            timeout_ms: 30_000,
            credential_ref: "Ferrum/AI/custom".into(),
            structured: StructuredMode::JsonObject,
        }
    }

    #[test]
    fn missing_file_loads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(dir.path()), None);
    }

    #[test]
    fn round_trips_a_valid_setup_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("fresh");
        save_to(&nested, &config()).unwrap();
        assert_eq!(load_from(&nested), Some(config()));
        let text = fs::read_to_string(nested.join(FILE_NAME)).unwrap();
        assert!(text.contains("\"timeoutMs\""));
        assert!(
            !nested.read_dir().unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-"))
        );
    }

    #[test]
    fn rejects_invalid_setups_without_touching_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        save_to(dir.path(), &config()).unwrap();
        let mut unsafe_endpoint = config();
        unsafe_endpoint.endpoint = "http://example.com/v1".into();
        assert_eq!(
            save_to(dir.path(), &unsafe_endpoint),
            Err(SetupStoreError::UnsafeEndpoint)
        );
        let mut bad_model = config();
        bad_model.model = "bad model!".into();
        assert_eq!(
            save_to(dir.path(), &bad_model),
            Err(SetupStoreError::InvalidConfig)
        );
        let mut bad_timeout = config();
        bad_timeout.timeout_ms = 5;
        assert_eq!(
            save_to(dir.path(), &bad_timeout),
            Err(SetupStoreError::InvalidConfig)
        );
        let mut bad_ref = config();
        bad_ref.credential_ref = "Other/Target".into();
        assert_eq!(
            save_to(dir.path(), &bad_ref),
            Err(SetupStoreError::InvalidConfig)
        );
        assert_eq!(load_from(dir.path()), Some(config()));
    }

    #[test]
    fn ignores_garbage_unknown_fields_and_hand_edited_unsafe_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "{not json").unwrap();
        assert_eq!(load_from(dir.path()), None);
        fs::write(
            &path,
            r#"{"endpoint":"http://127.0.0.1:20128/v1","model":"m","timeoutMs":30000,"credentialRef":"Ferrum/AI/custom","structured":"none","apiKey":"sk-should-not-be-here"}"#,
        )
        .unwrap();
        assert_eq!(
            load_from(dir.path()),
            None,
            "unknown field (a key) must not load"
        );
        fs::write(
            &path,
            r#"{"endpoint":"http://evil.example/v1","model":"m","timeoutMs":30000,"credentialRef":"Ferrum/AI/custom","structured":"none"}"#,
        )
        .unwrap();
        assert_eq!(load_from(dir.path()), None);
    }

    #[test]
    fn clear_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        save_to(dir.path(), &config()).unwrap();
        clear_in(dir.path()).unwrap();
        assert_eq!(load_from(dir.path()), None);
        clear_in(dir.path()).unwrap();
    }

    #[test]
    fn error_kinds_are_safe_codes() {
        assert_eq!(SetupStoreError::UnsafeEndpoint.kind(), "unsafe_endpoint");
        assert_eq!(SetupStoreError::InvalidConfig.kind(), "invalid_config");
        assert_eq!(SetupStoreError::StorageFailure.kind(), "storage_failure");
    }
}
