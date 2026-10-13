//! Token identity checks shared by the helper and Ferrum (FER-460 review
//! W1/W3). Both ends of the pipe must belong to the SAME user account:
//! - the helper compares its own token's user SID with the pipe SERVER's
//!   process token (over-the-shoulder UAC with another admin account is
//!   refused, so a standard user never receives a volume-wide listing);
//! - Ferrum, before sending anything, impersonates the pipe CLIENT
//!   (identification level) and requires an elevated token of its own user.

use std::io;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, IsValidSid, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE,
    TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenElevationType, TokenUser,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `TokenElevationTypeLimited`: the filtered half of a split admin token.
pub const ELEVATION_TYPE_LIMITED: TOKEN_ELEVATION_TYPE = 3;

/// Closes a kernel handle on drop.
pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn info(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 || len > 64 * 1024 {
        return Err(io::Error::last_os_error());
    }
    // u64 backing keeps the returned structures suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    if unsafe { GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

/// Raw bytes of the token's user SID (compare with `==`, like `EqualSid`).
pub fn user_sid(token: HANDLE) -> io::Result<Vec<u8>> {
    let buf = info(token, TokenUser)?;
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let sid = user.User.Sid;
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(io::Error::other("invalid token SID"));
    }
    let n = unsafe { GetLengthSid(sid) } as usize;
    Ok(unsafe { std::slice::from_raw_parts(sid as *const u8, n) }.to_vec())
}

/// `TokenElevation.TokenIsElevated`.
pub fn is_elevated(token: HANDLE) -> io::Result<bool> {
    let buf = info(token, TokenElevation)?;
    let e = unsafe { &*(buf.as_ptr() as *const TOKEN_ELEVATION) };
    Ok(e.TokenIsElevated != 0)
}

/// `TokenElevationType` (1 default, 2 full, 3 limited).
pub fn elevation_type(token: HANDLE) -> io::Result<TOKEN_ELEVATION_TYPE> {
    let buf = info(token, TokenElevationType)?;
    Ok(unsafe { *(buf.as_ptr() as *const TOKEN_ELEVATION_TYPE) })
}

/// This process's own token.
pub fn current_process_token() -> io::Result<Handle> {
    let mut t: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(t))
}

/// Token of another process (query only).
pub fn process_token(pid: u32) -> io::Result<Handle> {
    let p = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if p.is_null() {
        return Err(io::Error::last_os_error());
    }
    let p = Handle(p);
    let mut t: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(p.0, TOKEN_QUERY, &mut t) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(t))
}

/// Whether this (non-elevated) process may ask for the fast scan: only an
/// administrator account whose own token can be elevated (split token,
/// `Limited`), or a process that already is elevated. A standard user
/// would need ANOTHER account's credentials on the UAC prompt — refused.
pub fn may_elevate_same_account() -> bool {
    let Ok(t) = current_process_token() else {
        return false;
    };
    matches!(is_elevated(t.0), Ok(true))
        || matches!(elevation_type(t.0), Ok(ELEVATION_TYPE_LIMITED))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_token_has_a_sid_and_an_elevation_state() {
        let t = current_process_token().unwrap();
        let sid = user_sid(t.0).unwrap();
        assert!(sid.len() >= 8 && sid[0] == 1, "revision 1 SID");
        is_elevated(t.0).unwrap();
        assert!((1..=3).contains(&elevation_type(t.0).unwrap()));
        // Same process via the pid path yields the same SID.
        let again = process_token(std::process::id()).unwrap();
        assert_eq!(user_sid(again.0).unwrap(), sid);
    }
}
