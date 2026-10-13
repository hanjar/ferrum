//! Tauri commands untuk onboarding credential AI (FER-412). Nilai key hanya MASUK (save) dan tidak
//! pernah dikembalikan: status hanya `configured`. Target tetap `Ferrum/AI/custom`.

use serde::Serialize;

use super::credential::{self, CredentialError};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CredentialStatus {
    pub configured: bool,
}

/// Galat aman ke frontend: hanya kode, tanpa nilai atau path.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CredentialCommandError {
    pub kind: &'static str,
}

impl From<CredentialError> for CredentialCommandError {
    fn from(error: CredentialError) -> Self {
        let kind = match error {
            CredentialError::InvalidValue => "invalid_value",
            CredentialError::Unsupported | CredentialError::Unavailable => "unsupported",
            _ => "credential_failure",
        };
        Self { kind }
    }
}

#[tauri::command]
pub fn ai_credential_status() -> CredentialStatus {
    CredentialStatus {
        configured: credential::secret_exists(),
    }
}

#[tauri::command]
pub fn ai_credential_save(value: String) -> Result<CredentialStatus, CredentialCommandError> {
    if crate::profile::is_active() {
        return Err(CredentialCommandError {
            kind: "test_profile",
        });
    }
    let result = credential::save_secret(&value);
    // Timpa salinan String yang diterima dari IPC sebelum dibuang.
    let mut bytes = value.into_bytes();
    bytes.fill(0);
    result?;
    Ok(CredentialStatus { configured: true })
}

#[tauri::command]
pub fn ai_credential_clear() -> Result<CredentialStatus, CredentialCommandError> {
    if crate::profile::is_active() {
        return Err(CredentialCommandError {
            kind: "test_profile",
        });
    }
    credential::clear_secret()?;
    Ok(CredentialStatus { configured: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_errors_to_safe_kinds() {
        assert_eq!(
            CredentialCommandError::from(CredentialError::InvalidValue).kind,
            "invalid_value"
        );
        assert_eq!(
            CredentialCommandError::from(CredentialError::Unsupported).kind,
            "unsupported"
        );
        assert_eq!(
            CredentialCommandError::from(CredentialError::WriteFailed).kind,
            "credential_failure"
        );
    }

    #[test]
    fn invalid_value_is_rejected_before_any_write() {
        let error = ai_credential_save("short".into()).unwrap_err();
        assert_eq!(error.kind, "invalid_value");
    }
}
