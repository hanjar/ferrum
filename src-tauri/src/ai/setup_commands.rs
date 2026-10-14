//! Tauri commands untuk setup provider AI non-rahasia (FER-430). Hanya endpoint/model/timeout/
//! structured; key tetap lewat `credential_commands`. Galat ke frontend berupa kode aman saja.
//!
//! FER-512 P3-1: endpoint tersimpan di sini adalah SATU-SATUNYA endpoint yang boleh menerima
//! permintaan AI (lihat `locked_endpoint_in`). Menyimpan/mengubah HOST endpoint wajib konfirmasi
//! native (`MessageBoxW`, pola hotlist/FER-511 — disalin lokal, bukan refactor) yang bisa
//! diinjeksi lewat seam `confirm`; webview tidak bisa menggambar atau menjawab dialog itu.

use serde::Serialize;
use tauri::AppHandle;

use super::provider::{ProviderConfig, ProviderErrorKind};
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

pub(crate) fn config_dir(app: &AppHandle) -> std::path::PathBuf {
    crate::profile::config_dir(app).unwrap_or_else(std::env::temp_dir)
}

#[tauri::command]
pub fn ai_setup_load(app: AppHandle) -> Option<ProviderConfig> {
    setup_store::load_from(&config_dir(&app))
}

#[tauri::command]
pub async fn ai_setup_save(
    app: AppHandle,
    window: tauri::Window,
    config: ProviderConfig,
) -> Result<(), SetupCommandError> {
    let dir = config_dir(&app);
    let owner = owner_hwnd(&window);
    // MessageBox menahan utasnya sendiri: jalankan di thread blocking seperti hotlist.
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        save_setup_in(&dir, &config, |text| native_confirm(owner, text))
    })
    .await
    .map_err(|_| SetupStoreError::StorageFailure)??;
    match outcome {
        SaveOutcome::Saved => Ok(()),
        SaveOutcome::Canceled => Err(SetupStoreError::NotConfirmed.into()),
    }
}

#[tauri::command]
pub fn ai_setup_clear(app: AppHandle) -> Result<(), SetupCommandError> {
    setup_store::clear_in(&config_dir(&app)).map_err(Into::into)
}

/// Hasil `save_setup_in`: `Canceled` = pengguna menjawab tidak di dialog native; tidak ada
/// yang tersimpan.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    Canceled,
}

/// Host dari endpoint yang sudah tervalidasi (`ProviderConfig::validate` memastikan bentuknya),
/// huruf kecil. `None` hanya untuk bentuk yang tidak mengandung authority.
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    let host = authority.split(':').next()?;
    Some(host.to_ascii_lowercase())
}

/// Teks dialog konfirmasi native: HOST baru ditampilkan menonjol, dengan peringatan bahwa kunci
/// API akan dikirim ke host itu. Hanya memakai nilai yang sudah tervalidasi, bukan teks webview.
fn save_confirm_text(endpoint: &str, first: bool) -> String {
    let host = endpoint_host(endpoint).unwrap_or_default();
    let context = if first {
        "Ini setup AI pertama (belum ada endpoint tersimpan)."
    } else {
        "Host ini BERBEDA dari endpoint yang tersimpan sekarang."
    };
    format!(
        "Ferrum akan mengirim KUNCI API (header Authorization) ke host berikut setiap kali Anda memakai fitur AI:\n\n{host}\n\nURL lengkap: {endpoint}\n\n{context}\n\nSimpan dan percayai endpoint ini? (Tanpa konfirmasi, permintaan AI ke endpoint lain ditolak.)"
    )
}

/// FER-512 P3-1: the ONLY way the saved AI setup changes. Saving is allowed silently only when
/// the HOST stays the same; a first setup and every host change ask in a native dialog (the
/// injectable `confirm` seam mirrors the hotlist confirmation — production passes a
/// `MessageBoxW` owned by the Ferrum window, which the webview can neither draw nor answer).
/// No for the dialog stores nothing.
pub fn save_setup_in(
    dir: &std::path::Path,
    config: &ProviderConfig,
    confirm: impl FnOnce(&str) -> bool,
) -> Result<SaveOutcome, SetupStoreError> {
    // Shape first: an invalid config never reaches the dialog or the disk.
    config.validate().map_err(|kind| match kind {
        ProviderErrorKind::UnsafeEndpoint => SetupStoreError::UnsafeEndpoint,
        _ => SetupStoreError::InvalidConfig,
    })?;
    let stored = setup_store::load_from(dir);
    let host_changed = match &stored {
        None => true, // first setup: nothing is trusted yet
        Some(current) => endpoint_host(&current.endpoint) != endpoint_host(&config.endpoint),
    };
    if host_changed && !confirm(&save_confirm_text(&config.endpoint, stored.is_none())) {
        return Ok(SaveOutcome::Canceled);
    }
    setup_store::save_to(dir, config)?;
    Ok(SaveOutcome::Saved)
}

/// FER-512 P3-1: the endpoint of an outgoing AI request NEVER comes from the webview. The saved
/// setup is the single source; a request whose endpoint differs (or a missing saved setup) is
/// refused BEFORE any transport is built, so no byte — and no API key — ever reaches another
/// host. Returns the stored endpoint to send with.
pub fn locked_endpoint_in(
    dir: &std::path::Path,
    request_endpoint: &str,
) -> Result<String, ProviderErrorKind> {
    let stored = setup_store::load_from(dir).ok_or(ProviderErrorKind::EndpointMismatch)?;
    if request_endpoint != stored.endpoint {
        return Err(ProviderErrorKind::EndpointMismatch);
    }
    Ok(stored.endpoint)
}

/// FER-512: native Windows message box owned by the Ferrum window (hotlist pattern, copied
/// locally like FER-511 did in `archive_rar_tool`). The webview can neither draw it nor answer
/// it. Cancel is the default button, so a held Enter key cannot confirm a new host.
fn native_confirm(owner: isize, text: &str) -> bool {
    // One native dialog at a time across the whole app; a busy guard answers "no" (nothing saved).
    crate::native_dialog::confirm(owner, "Konfirmasi endpoint AI", text)
}

#[cfg(windows)]
fn owner_hwnd(window: &tauri::Window) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(not(windows))]
fn owner_hwnd(_window: &tauri::Window) -> isize {
    0
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
        assert_eq!(SetupStoreError::NotConfirmed.kind(), "not_confirmed");
    }

    #[test]
    fn host_of_a_valid_endpoint_is_lowercased_and_port_stripped() {
        assert_eq!(
            endpoint_host("https://Api.Example.com/v1").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            endpoint_host("http://127.0.0.1:20128/v1").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(endpoint_host("not-an-endpoint"), None);
    }

    /// The dialog names the host and warns about the key, never a webview-chosen text.
    #[test]
    fn the_confirm_text_names_the_host_and_warns_about_the_key() {
        let first = save_confirm_text("https://api.example.com/v1", true);
        assert!(first.contains("api.example.com"), "{first}");
        assert!(first.contains("KUNCI API"), "{first}");
        assert!(first.contains("pertama"), "{first}");
        let changed = save_confirm_text("http://127.0.0.1:20128/v1", false);
        assert!(changed.contains("127.0.0.1"), "{changed}");
        assert!(changed.contains("BERBEDA"), "{changed}");
    }

    /// First setup: the native question is mandatory; "no" stores nothing, "yes" stores.
    #[test]
    fn a_first_setup_needs_the_native_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let asked = std::cell::Cell::new(false);
        assert_eq!(
            save_setup_in(dir.path(), &config(), |text| {
                asked.set(true);
                assert!(text.contains("127.0.0.1"), "{text}");
                false
            }),
            Ok(SaveOutcome::Canceled)
        );
        assert!(asked.get());
        assert_eq!(setup_store::load_from(dir.path()), None, "nothing stored");
        assert_eq!(
            save_setup_in(dir.path(), &config(), |_| true),
            Ok(SaveOutcome::Saved)
        );
        assert_eq!(setup_store::load_from(dir.path()), Some(config()));
    }

    /// Same host, different path/model: saved without a dialog. A host change always asks;
    /// "no" keeps the old endpoint, "yes" stores the new one.
    #[test]
    fn only_a_host_change_needs_the_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        // Seed a stored setup first (its own first-save confirmation is covered above).
        save_setup_in(dir.path(), &config(), |_| true).unwrap();
        // Same host: no dialog.
        let mut same_host = config();
        same_host.endpoint = "http://127.0.0.1:20128/v2".into();
        same_host.model = "other/model".into();
        let asked = std::cell::Cell::new(false);
        assert_eq!(
            save_setup_in(dir.path(), &same_host, |_| {
                asked.set(true);
                true
            }),
            Ok(SaveOutcome::Saved)
        );
        assert!(!asked.get());
        assert_eq!(setup_store::load_from(dir.path()), Some(same_host.clone()));
        // New host: "no" keeps the stored setup.
        let mut new_host = config();
        new_host.endpoint = "https://attacker.example/v1".into();
        assert_eq!(
            save_setup_in(dir.path(), &new_host, |_| false),
            Ok(SaveOutcome::Canceled)
        );
        assert_eq!(setup_store::load_from(dir.path()), Some(same_host.clone()));
        // "yes" stores it.
        assert_eq!(
            save_setup_in(dir.path(), &new_host, |_| true),
            Ok(SaveOutcome::Saved)
        );
        assert_eq!(setup_store::load_from(dir.path()), Some(new_host));
        // An invalid config is refused before any dialog.
        let mut bad = config();
        bad.endpoint = "http://example.com/v1".into();
        assert_eq!(
            save_setup_in(dir.path(), &bad, |_| panic!(
                "no dialog for an invalid config"
            )),
            Err(SetupStoreError::UnsafeEndpoint)
        );
    }

    /// The endpoint gate: equal → the stored endpoint; different or no setup → refused,
    /// with no transport involved (this is a pure lookup — the refusal happens before any
    /// request-building code runs in the commands).
    #[test]
    fn the_endpoint_gate_only_accepts_the_saved_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            locked_endpoint_in(dir.path(), "http://127.0.0.1:20128/v1"),
            Err(ProviderErrorKind::EndpointMismatch),
            "no saved setup: every request is refused"
        );
        save_setup_in(dir.path(), &config(), |_| true).unwrap();
        assert_eq!(
            locked_endpoint_in(dir.path(), "http://127.0.0.1:20128/v1"),
            Ok("http://127.0.0.1:20128/v1".to_owned())
        );
        assert_eq!(
            locked_endpoint_in(dir.path(), "http://127.0.0.1:20128/v2"),
            Err(ProviderErrorKind::EndpointMismatch)
        );
        assert_eq!(
            locked_endpoint_in(dir.path(), "https://attacker.example/v1"),
            Err(ProviderErrorKind::EndpointMismatch)
        );
        // The gate is checked with the request string exactly — no normalisation that could
        // let a look-alike through.
        assert_eq!(
            locked_endpoint_in(dir.path(), "http://127.0.0.1:20128/v1/"),
            Err(ProviderErrorKind::EndpointMismatch)
        );
    }
}
