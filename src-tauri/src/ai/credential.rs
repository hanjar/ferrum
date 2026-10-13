use std::fmt;

pub const CREDENTIAL_TARGET: &str = "Ferrum/AI/custom";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    InvalidReference,
    Missing,
    Unavailable,
    InvalidEncoding,
    /// Nilai yang akan disimpan ditolak (panjang/karakter); nilainya tidak pernah dilaporkan.
    InvalidValue,
    /// Penulisan/penghapusan ke penyimpanan OS gagal atau baca-balik tidak cocok.
    WriteFailed,
    /// Platform tanpa penyimpanan credential OS.
    Unsupported,
}

/// Batas nilai key: cukup untuk API key umum, di bawah batas baca (2560 byte).
pub const MIN_SECRET_LEN: usize = 8;
pub const MAX_SECRET_LEN: usize = 2048;

/// Hanya ASCII tercetak tanpa spasi (API key/token); menolak newline, kontrol, dan spasi.
pub fn validate_secret_value(value: &str) -> Result<(), CredentialError> {
    let ok = (MIN_SECRET_LEN..=MAX_SECRET_LEN).contains(&value.len())
        && value.bytes().all(|b| (0x21..=0x7e).contains(&b));
    if ok {
        Ok(())
    } else {
        Err(CredentialError::InvalidValue)
    }
}

/// Ada tidaknya credential tetap; tidak pernah mengembalikan nilainya.
pub fn secret_exists() -> bool {
    // FER-486: a test profile never touches the real credential store.
    if let Some(root) = crate::profile::profile_dir() {
        return read_profile_secret(&root).is_ok();
    }
    exists_at(CREDENTIAL_TARGET)
}

/// Simpan key ke target tetap `Ferrum/AI/custom`, lalu baca-balik untuk verifikasi.
pub fn save_secret(value: &str) -> Result<(), CredentialError> {
    store_verified(CREDENTIAL_TARGET, value)
}

/// Hapus credential target tetap. Tidak ada = sukses (idempoten).
pub fn clear_secret() -> Result<(), CredentialError> {
    delete_at(CREDENTIAL_TARGET)
}

pub(crate) fn exists_at(target: &str) -> bool {
    #[cfg(windows)]
    {
        windows_credential::read(target).is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        false
    }
}

/// Tulis lalu baca-balik; salinan hasil baca-balik ditimpa nol sebelum dibuang.
pub(crate) fn store_verified(target: &str, value: &str) -> Result<(), CredentialError> {
    validate_secret_value(value)?;
    #[cfg(windows)]
    {
        windows_credential::write(target, value.as_bytes())?;
        let back = windows_credential::read(target).map_err(|_| CredentialError::WriteFailed)?;
        let same = back == value;
        let mut bytes = back.into_bytes();
        bytes.fill(0);
        if same {
            Ok(())
        } else {
            Err(CredentialError::WriteFailed)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(CredentialError::Unsupported)
    }
}

pub(crate) fn delete_at(target: &str) -> Result<(), CredentialError> {
    #[cfg(windows)]
    {
        windows_credential::delete(target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(CredentialError::Unsupported)
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CredentialError {}

/// FER-486: dummy key of a test profile, `<FERRUM_PROFILE_DIR>/config/ai-credential.txt`.
/// While a test profile is active the real Credential Manager is never read (nor written): smoke
/// runs use this file only, and a missing file means "no key".
pub const PROFILE_SECRET_FILE: &str = "ai-credential.txt";

pub(crate) fn read_profile_secret(root: &std::path::Path) -> Result<String, CredentialError> {
    let path = crate::profile::config_in(root).join(PROFILE_SECRET_FILE);
    let text = std::fs::read_to_string(path).map_err(|_| CredentialError::Missing)?;
    let value = text.trim().to_owned();
    validate_secret_value(&value).map_err(|_| CredentialError::InvalidEncoding)?;
    Ok(value)
}

pub fn read_secret(reference: &str) -> Result<String, CredentialError> {
    if reference != CREDENTIAL_TARGET {
        return Err(CredentialError::InvalidReference);
    }
    if let Some(root) = crate::profile::profile_dir() {
        return read_profile_secret(&root);
    }
    #[cfg(windows)]
    {
        windows_credential::read(reference)
    }
    #[cfg(not(windows))]
    {
        Err(CredentialError::Unavailable)
    }
}

#[cfg(windows)]
mod windows_credential {
    use super::CredentialError;
    use std::{ffi::c_void, ptr::null_mut, slice};

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: i64,
        blob_len: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "Advapi32")]
    unsafe extern "system" {
        fn CredReadW(
            target: *const u16,
            kind: u32,
            flags: u32,
            credential: *mut *mut Credential,
        ) -> i32;
        fn CredFree(credential: *mut c_void);
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
    }

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }

    const ERROR_NOT_FOUND: u32 = 1168;
    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;

    pub fn write(target: &str, blob: &[u8]) -> Result<(), CredentialError> {
        let mut name: Vec<u16> = target.encode_utf16().chain([0]).collect();
        let mut data = blob.to_vec();
        let credential = Credential {
            flags: 0,
            kind: CRED_TYPE_GENERIC,
            target_name: name.as_mut_ptr(),
            comment: null_mut(),
            last_written: 0,
            blob_len: data.len() as u32,
            blob: data.as_mut_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: null_mut(),
            target_alias: null_mut(),
            user_name: null_mut(),
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        data.fill(0);
        if ok == 0 {
            Err(CredentialError::WriteFailed)
        } else {
            Ok(())
        }
    }

    pub fn delete(target: &str) -> Result<(), CredentialError> {
        let name: Vec<u16> = target.encode_utf16().chain([0]).collect();
        let ok = unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok != 0 || unsafe { GetLastError() } == ERROR_NOT_FOUND {
            Ok(())
        } else {
            Err(CredentialError::WriteFailed)
        }
    }

    pub fn read(reference: &str) -> Result<String, CredentialError> {
        let target: Vec<u16> = reference.encode_utf16().chain([0]).collect();
        let mut ptr: *mut Credential = null_mut();
        let ok = unsafe { CredReadW(target.as_ptr(), 1, 0, &mut ptr) };
        if ok == 0 {
            return Err(CredentialError::Missing);
        }
        if ptr.is_null() {
            return Err(CredentialError::Unavailable);
        }
        let result = unsafe {
            let credential = &*ptr;
            if credential.blob.is_null() || credential.blob_len == 0 || credential.blob_len > 2560 {
                Err(CredentialError::InvalidEncoding)
            } else {
                let bytes = slice::from_raw_parts(credential.blob, credential.blob_len as usize);
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|_| CredentialError::InvalidEncoding)
            }
        };
        unsafe { CredFree(ptr.cast()) };
        result
    }
}

#[cfg(test)]
mod profile_secret_tests {
    use super::*;

    /// FER-486: the test-profile key comes from the profile folder only.
    #[test]
    fn a_test_profile_reads_its_dummy_key_file_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("ferrum-fer486-cred-{}", std::process::id()));
        let config = crate::profile::config_in(&root);
        std::fs::create_dir_all(&config).unwrap();
        assert_eq!(read_profile_secret(&root), Err(CredentialError::Missing));
        std::fs::write(config.join(PROFILE_SECRET_FILE), "dummy-test-key-0001\r\n").unwrap();
        assert_eq!(read_profile_secret(&root).unwrap(), "dummy-test-key-0001");
        std::fs::write(config.join(PROFILE_SECRET_FILE), "has space").unwrap();
        assert_eq!(
            read_profile_secret(&root),
            Err(CredentialError::InvalidEncoding)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
