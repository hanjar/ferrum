//! Small Win32 helpers shared by the service, the installer commands and
//! the client (FER-474): handles, wide strings, SIDs, security descriptors,
//! token identity, and the Program Files location rule.

use crate::policy::ClientIdentity;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WIN32_ERROR,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    CheckTokenMembership, GetLengthSid, GetSidSubAuthority, GetSidSubAuthorityCount,
    GetTokenInformation, IsTokenRestricted, IsValidSid, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenElevation,
    TokenIntegrityLevel, TokenIsAppContainer, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Closes a kernel handle on drop.
pub struct OwnedHandle(pub HANDLE);

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a kernel handle is a process-wide value; the owner moves it
// between threads but never shares it mutably.
unsafe impl Send for OwnedHandle {}
// SAFETY: only the handle value is shared; every API used on it is thread-safe.
unsafe impl Sync for OwnedHandle {}

/// NUL-terminated UTF-16.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// `WIN32_ERROR` → `io::Error`.
pub fn win_err(code: WIN32_ERROR) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// UTF-16 up to the first NUL, from a raw pointer.
///
/// # Safety
/// `p` must point to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide_ptr(p: *const u16) -> OsString {
    let mut n = 0usize;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    OsString::from_wide(unsafe { std::slice::from_raw_parts(p, n) })
}

/// Review I2: restrict later `LoadLibrary` searches to System32 (no
/// application directory, no current directory, no PATH). Called first in
/// `main` of every binary that runs elevated or as SYSTEM.
pub fn harden_dll_search() {
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

// ── well-known SIDs (raw bytes, compared like EqualSid) ─────────────────

/// S-1-5-18 NT AUTHORITY\SYSTEM.
pub const SID_LOCAL_SYSTEM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
/// S-1-5-32-544 BUILTIN\Administrators.
pub const SID_ADMINISTRATORS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 0x02, 0, 0];
/// S-1-5-4 NT AUTHORITY\INTERACTIVE.
pub const SID_INTERACTIVE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 4, 0, 0, 0];

/// Raw bytes of a SID pointer.
///
/// # Safety
/// `sid` must be null or point to a SID that outlives the call.
pub unsafe fn sid_bytes(sid: PSID) -> io::Result<Vec<u8>> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(io::Error::other("invalid SID"));
    }
    let n = unsafe { GetLengthSid(sid) } as usize;
    Ok(unsafe { std::slice::from_raw_parts(sid as *const u8, n) }.to_vec())
}

/// `S-1-…` form of raw SID bytes (for logs and SDDL).
pub fn sid_string(sid: &[u8]) -> io::Result<String> {
    let mut buf = sid.to_vec();
    let psid = buf.as_mut_ptr() as PSID;
    if unsafe { IsValidSid(psid) } == 0 {
        return Err(io::Error::other("invalid SID"));
    }
    let mut s: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(psid, &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let out = unsafe { from_wide_ptr(s) }.to_string_lossy().into_owned();
    unsafe { LocalFree(s.cast()) };
    // Defensive: the result is spliced into SDDL.
    if !out.starts_with("S-1-")
        || !out
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
    {
        return Err(io::Error::other("unexpected SID format"));
    }
    Ok(out)
}

/// A security descriptor built from SDDL, freed on drop.
pub struct SecurityDescriptor(pub PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(SecurityDescriptor(psd))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

// SAFETY: the descriptor is immutable after creation.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

// ── tokens ───────────────────────────────────────────────────────────────

fn token_info(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
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

fn token_u32(token: HANDLE, class: i32) -> io::Result<u32> {
    let mut v = 0u32;
    let mut len = 0u32;
    if unsafe { GetTokenInformation(token, class, (&mut v as *mut u32).cast(), 4, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(v)
}

/// Raw bytes of the token's user SID.
pub fn token_user_sid(token: HANDLE) -> io::Result<Vec<u8>> {
    let buf = token_info(token, TokenUser)?;
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    unsafe { sid_bytes(user.User.Sid) }
}

/// Integrity level RID (`SECURITY_MANDATORY_*_RID`).
pub fn token_integrity(token: HANDLE) -> io::Result<u32> {
    let buf = token_info(token, TokenIntegrityLevel)?;
    let label = unsafe { &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
    let sid = label.Label.Sid;
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(io::Error::other("invalid integrity SID"));
    }
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err(io::Error::other("integrity SID without RID"));
    }
    Ok(unsafe { *GetSidSubAuthority(sid, count as u32 - 1) })
}

pub fn token_elevated(token: HANDLE) -> io::Result<bool> {
    let buf = token_info(token, TokenElevation)?;
    let e = unsafe { &*(buf.as_ptr() as *const TOKEN_ELEVATION) };
    Ok(e.TokenIsElevated != 0)
}

/// `CheckTokenMembership` for an IMPERSONATION token: true only when the SID
/// is an enabled group (deny-only groups of a filtered token do not count).
// A HANDLE is an opaque kernel handle value passed to Win32, never
// dereferenced here.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn token_has_enabled_group(token: HANDLE, sid: &[u8]) -> io::Result<bool> {
    let mut sid = sid.to_vec();
    let mut member = 0i32;
    if unsafe { CheckTokenMembership(token, sid.as_mut_ptr() as PSID, &mut member) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(member != 0)
}

/// Everything the policy needs from a client's impersonation token.
// A HANDLE is an opaque kernel handle value passed to Win32, never
// dereferenced here.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn client_identity(token: HANDLE) -> io::Result<ClientIdentity> {
    Ok(ClientIdentity {
        user_sid: token_user_sid(token)?,
        integrity_rid: token_integrity(token)?,
        elevated: token_elevated(token)?,
        admin: token_has_enabled_group(token, &SID_ADMINISTRATORS)?,
        interactive: token_has_enabled_group(token, &SID_INTERACTIVE)?,
        restricted: unsafe { IsTokenRestricted(token) } != 0,
        app_container: token_u32(token, TokenIsAppContainer)? != 0,
    })
}

/// This process's own token (query only).
pub fn process_token() -> io::Result<OwnedHandle> {
    let mut t: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(t))
}

/// Raw user SID of this process.
pub fn current_user_sid() -> io::Result<Vec<u8>> {
    token_user_sid(process_token()?.raw())
}

/// Is this process running as LocalSystem?
pub fn running_as_system() -> bool {
    current_user_sid().is_ok_and(|s| s == SID_LOCAL_SYSTEM)
}

/// Is this process elevated?
pub fn process_elevated() -> bool {
    process_token().is_ok_and(|t| token_elevated(t.raw()).unwrap_or(false))
}

/// Logon session (`TokenStatistics.AuthenticationId`) of a token: the key
/// under which AccessCheck decisions may be shared between connections of
/// the same logon (same groups). FER-476 review L3.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn token_logon_id(token: HANDLE) -> io::Result<u64> {
    use windows_sys::Win32::Security::{TOKEN_STATISTICS, TokenStatistics};
    let buf = token_info(token, TokenStatistics)?;
    let st = unsafe { &*(buf.as_ptr() as *const TOKEN_STATISTICS) };
    Ok(((st.AuthenticationId.HighPart as u32 as u64) << 32) | st.AuthenticationId.LowPart as u64)
}

/// Is `token` a restricted token whose restricting SIDs include `sid`?
/// FER-476 review H1: the service checks its OWN token at start; the
/// query surface (search/subtree) is only served when this holds, whatever
/// the SCM configuration says (`sc sidtype` can change it at any time).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn token_restricted_by(token: HANDLE, sid: &[u8]) -> io::Result<bool> {
    use windows_sys::Win32::Security::{TOKEN_GROUPS, TokenRestrictedSids};
    if unsafe { IsTokenRestricted(token) } == 0 {
        return Ok(false);
    }
    let buf = token_info(token, TokenRestrictedSids)?;
    let groups = unsafe { &*(buf.as_ptr() as *const TOKEN_GROUPS) };
    let n = groups.GroupCount as usize;
    // The SID_AND_ATTRIBUTES array lives inside `buf` (bounded by its size).
    let entries =
        unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), n.min(buf.len() * 8 / 16)) };
    for g in entries {
        if unsafe { sid_bytes(g.Sid) }.is_ok_and(|b| b == sid) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// (SID bytes, attributes) of a `TOKEN_GROUPS` class, bounded by the
/// returned buffer.
fn token_groups(token: HANDLE, class: i32) -> io::Result<Vec<(Vec<u8>, u32)>> {
    use windows_sys::Win32::Security::{SID_AND_ATTRIBUTES, TOKEN_GROUPS};
    let buf = token_info(token, class)?;
    let groups = unsafe { &*(buf.as_ptr() as *const TOKEN_GROUPS) };
    let max = (buf.len() * 8).saturating_sub(8) / std::mem::size_of::<SID_AND_ATTRIBUTES>();
    let n = groups.GroupCount as usize;
    if n > max {
        return Err(io::Error::other("token group count out of range"));
    }
    let entries = unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), n) };
    entries
        .iter()
        .map(|g| Ok((unsafe { sid_bytes(g.Sid) }?, g.Attributes)))
        .collect()
}

/// FER-476 review N1: the identity of a client token AS ACCESSCHECK SEES
/// IT — the key under which AccessCheck decisions may be shared between
/// connections. Two tokens share decisions only when everything AccessCheck
/// evaluates is equal: logon session, user SID, every group SID with its
/// attributes (enabled / deny-only / mandatory / integrity…, sorted), the
/// restricting SIDs, the AppContainer flag and capabilities, and the
/// integrity level. A token derived from the same logon with a group
/// disabled or deny-only (`CreateRestrictedToken` SidsToDisable,
/// `AdjustTokenGroups`) therefore gets its own key. The key is the
/// canonical byte string itself (no hash, no collisions). Any read error
/// is an error: the caller refuses the client (review N2, fail closed).
///
/// Review N1b: the user SID's ATTRIBUTES are part of the key (a token whose
/// user SID is deny-only — `CreateRestrictedToken` SidsToDisable=[user] —
/// must not reuse decisions of the full token), and so are the device
/// groups. Claims (user/device claim attributes, used by conditional ACEs)
/// are not serialised: a token that HAS any, or whose device groups/claims
/// cannot be read, gets a key unique to this call (no sharing at all).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn token_fingerprint(token: HANDLE) -> io::Result<Vec<u8>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows_sys::Win32::Security::{
        TOKEN_USER, TokenCapabilities, TokenDeviceClaimAttributes, TokenDeviceGroups, TokenGroups,
        TokenRestrictedSids, TokenUser, TokenUserClaimAttributes,
    };
    static UNIQUE: AtomicU64 = AtomicU64::new(1);
    fn put_list(out: &mut Vec<u8>, mut l: Vec<(Vec<u8>, u32)>) {
        l.sort();
        out.extend_from_slice(&(l.len() as u32).to_le_bytes());
        for (sid, attrs) in l {
            out.push(sid.len() as u8);
            out.extend_from_slice(&sid);
            out.extend_from_slice(&attrs.to_le_bytes());
        }
    }
    let mut out = b"FRXK2".to_vec();
    out.extend_from_slice(&token_logon_id(token)?.to_le_bytes());
    let user_buf = token_info(token, TokenUser)?;
    let tu = unsafe { &*(user_buf.as_ptr() as *const TOKEN_USER) };
    let user = unsafe { sid_bytes(tu.User.Sid) }?;
    out.push(user.len() as u8);
    out.extend_from_slice(&user);
    out.extend_from_slice(&tu.User.Attributes.to_le_bytes());
    out.extend_from_slice(&token_integrity(token)?.to_le_bytes());
    put_list(&mut out, token_groups(token, TokenGroups)?);
    let restricted = unsafe { IsTokenRestricted(token) } != 0;
    out.push(restricted as u8);
    if restricted {
        put_list(&mut out, token_groups(token, TokenRestrictedSids)?);
    }
    let app_container = token_u32(token, TokenIsAppContainer)? != 0;
    out.push(app_container as u8);
    if app_container {
        put_list(&mut out, token_groups(token, TokenCapabilities)?);
    }
    // Device groups and claims: shareable only when readable and (claims)
    // absent; otherwise this key matches nothing else.
    let mut shareable = true;
    match token_groups(token, TokenDeviceGroups) {
        Ok(d) => put_list(&mut out, d),
        Err(_) => shareable = false,
    }
    for class in [TokenUserClaimAttributes, TokenDeviceClaimAttributes] {
        // CLAIM_SECURITY_ATTRIBUTES_INFORMATION: u16 version, u16 reserved,
        // u32 AttributeCount, ...
        match token_info(token, class) {
            Ok(b) if !b.is_empty() && (b[0] >> 32) as u32 == 0 => {}
            _ => shareable = false,
        }
    }
    out.push(shareable as u8);
    if !shareable {
        out.extend_from_slice(&UNIQUE.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        out.extend_from_slice(&std::process::id().to_le_bytes());
    }
    Ok(out)
}

/// Tests: an identification-level copy of this process's token with the
/// given groups turned DENY-ONLY (`CreateRestrictedToken` SidsToDisable,
/// no restricting SIDs): same logon session, not a "restricted" token —
/// exactly the case review N1 found sharing a cache entry. Test-only.
#[cfg(test)]
pub fn deny_only_token(disable: &[&str]) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::Security::{
        CreateRestrictedToken, DuplicateTokenEx, SID_AND_ATTRIBUTES, SecurityIdentification,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TokenImpersonation,
    };
    let mut base: HANDLE = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY,
            &mut base,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let base = OwnedHandle(base);
    let mut sids: Vec<Vec<u8>> = disable
        .iter()
        .map(|s| sid_from_string(s))
        .collect::<io::Result<_>>()?;
    let attrs: Vec<SID_AND_ATTRIBUTES> = sids
        .iter_mut()
        .map(|s| SID_AND_ATTRIBUTES {
            Sid: s.as_mut_ptr() as PSID,
            Attributes: 0,
        })
        .collect();
    let mut restricted: HANDLE = std::ptr::null_mut();
    if unsafe {
        CreateRestrictedToken(
            base.raw(),
            0,
            attrs.len() as u32,
            attrs.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            &mut restricted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let restricted = OwnedHandle(restricted);
    let mut imp: HANDLE = std::ptr::null_mut();
    if unsafe {
        DuplicateTokenEx(
            restricted.raw(),
            TOKEN_QUERY,
            std::ptr::null(),
            SecurityIdentification,
            TokenImpersonation,
            &mut imp,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(imp))
}

/// This process's token is restricted by `sid` (see [`token_restricted_by`]).
pub fn process_restricted_by(sid: &[u8]) -> bool {
    process_token().is_ok_and(|t| token_restricted_by(t.raw(), sid).unwrap_or(false))
}

// ── write-restricted token emulation (FER-476) ──────────────────────────

/// S-1-1-0 Everyone.
pub const SID_EVERYONE_STR: &str = "S-1-1-0";
/// S-1-5-33 NT AUTHORITY\WRITE RESTRICTED.
pub const SID_WRITE_RESTRICTED_STR: &str = "S-1-5-33";

/// Raw bytes of a SID given as `S-1-…`.
pub fn sid_from_string(s: &str) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    let w = wide(s);
    let mut p: PSID = std::ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut p) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let out = unsafe { sid_bytes(p) };
    unsafe { LocalFree(p.cast()) };
    out
}

/// A write-restricted copy of this process's token, as an IMPERSONATION
/// token, shaped like the token the SCM gives a service with
/// `SERVICE_SID_TYPE_RESTRICTED`: same user and groups, plus restricting
/// SIDs `restricting` (the service SID, Everyone, WRITE RESTRICTED, …).
/// Write access is then granted only when BOTH the normal and the
/// restricting SIDs are granted it; read access is unchanged.
///
/// Used by tests (no admin needed) and by `install` (elevated) to probe
/// that the pipe and the data directory still work under that token.
pub fn write_restricted_token(restricting: &[&str]) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::Security::{
        CreateRestrictedToken, DuplicateTokenEx, SID_AND_ATTRIBUTES, SecurityImpersonation,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TokenImpersonation,
        WRITE_RESTRICTED,
    };
    let mut base: HANDLE = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY,
            &mut base,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let base = OwnedHandle(base);
    let mut sids: Vec<Vec<u8>> = restricting
        .iter()
        .map(|s| sid_from_string(s))
        .collect::<io::Result<_>>()?;
    let mut attrs: Vec<SID_AND_ATTRIBUTES> = sids
        .iter_mut()
        .map(|s| SID_AND_ATTRIBUTES {
            Sid: s.as_mut_ptr() as PSID,
            Attributes: 0,
        })
        .collect();
    let mut restricted: HANDLE = std::ptr::null_mut();
    if unsafe {
        CreateRestrictedToken(
            base.raw(),
            WRITE_RESTRICTED,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            attrs.len() as u32,
            attrs.as_mut_ptr(),
            &mut restricted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let restricted = OwnedHandle(restricted);
    let mut imp: HANDLE = std::ptr::null_mut();
    if unsafe {
        DuplicateTokenEx(
            restricted.raw(),
            TOKEN_IMPERSONATE | TOKEN_QUERY,
            std::ptr::null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut imp,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(imp))
}

/// The calling thread runs as `token` until this guard drops (then it
/// reverts; a failed revert aborts — a thread must never keep a foreign
/// security context).
pub struct Impersonation(());

impl Impersonation {
    pub fn begin(token: &OwnedHandle) -> io::Result<Impersonation> {
        use windows_sys::Win32::System::Threading::SetThreadToken;
        if unsafe { SetThreadToken(std::ptr::null(), token.raw()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Impersonation(()))
    }
}

impl Drop for Impersonation {
    fn drop(&mut self) {
        if unsafe { windows_sys::Win32::Security::RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}

// ── locations ────────────────────────────────────────────────────────────

fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    let mut p: *mut u16 = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut p) };
    let out = (hr >= 0 && !p.is_null()).then(|| PathBuf::from(unsafe { from_wide_ptr(p) }));
    if !p.is_null() {
        unsafe { CoTaskMemFree(p.cast()) };
    }
    out
}

/// `C:\ProgramData` from the Known-Folder API (never from the environment,
/// which the launching process controls).
pub fn program_data() -> io::Result<PathBuf> {
    use windows_sys::Win32::UI::Shell::FOLDERID_ProgramData;
    known_folder(&FOLDERID_ProgramData)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::other("ProgramData folder unknown"))
}

/// Program Files folders (Known-Folder API).
pub fn protected_roots() -> Vec<PathBuf> {
    use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86};
    [FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86]
        .iter()
        .filter_map(known_folder)
        .collect()
}

/// True when `dir` is `root` or below it, for any root (component-wise,
/// case-insensitive; `C:\Program Files Evil` does not match). Same rule as
/// FER-460's `fast_scan::in_protected_dir`.
pub fn in_protected_dir(dir: &Path, roots: &[PathBuf]) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .filter(|c| c != "\\")
            .collect()
    };
    let d = norm(dir);
    roots.iter().any(|r| {
        let r = norm(r);
        !r.is_empty() && d.len() >= r.len() && d[..r.len()] == r[..]
    })
}

// ── server process identity (FER-480: client-side server verification) ──

/// PID of the process that created the server end of a connected pipe
/// (client side). NPFS records it when the instance is created.
pub fn pipe_server_pid(h: &OwnedHandle) -> io::Result<u32> {
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let mut pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(h.raw(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// NT image path (`\Device\HarddiskVolumeN\…`) of a running process, from
/// `NtQuerySystemInformation(SystemProcessIdInformation)`. Unlike
/// `QueryFullProcessImageNameW` this needs no process handle, so a
/// standard user can read it for a SYSTEM service (whose process DACL
/// refuses even `PROCESS_QUERY_LIMITED_INFORMATION` to standard users —
/// measured with `scripts/indexer-pipe-probe.ps1 -Client`).
pub fn process_image_nt(pid: u32) -> io::Result<String> {
    use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;
    use windows_sys::Win32::Foundation::UNICODE_STRING;
    /// `SYSTEM_PROCESS_ID_INFORMATION` (class 88, not in the SDK headers).
    #[repr(C)]
    struct ProcessIdInformation {
        process_id: HANDLE,
        image_name: UNICODE_STRING,
    }
    const SYSTEM_PROCESS_ID_INFORMATION: i32 = 88;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    const MAX_UNITS: usize = 32767;
    let mut cap: usize = 1024; // UTF-16 units
    for _ in 0..2 {
        let mut buf = vec![0u16; cap];
        let mut info = ProcessIdInformation {
            process_id: pid as usize as HANDLE,
            image_name: UNICODE_STRING {
                Length: 0,
                MaximumLength: (cap * 2) as u16,
                Buffer: buf.as_mut_ptr(),
            },
        };
        let st = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_ID_INFORMATION,
                (&mut info as *mut ProcessIdInformation).cast(),
                std::mem::size_of::<ProcessIdInformation>() as u32,
                std::ptr::null_mut(),
            )
        };
        if st == STATUS_INFO_LENGTH_MISMATCH && cap < MAX_UNITS {
            cap = (info.image_name.MaximumLength as usize / 2 + 1).clamp(cap + 1, MAX_UNITS);
            continue;
        }
        if st < 0 {
            return Err(io::Error::other(format!(
                "process {pid} image: NTSTATUS {:#x}",
                st as u32
            )));
        }
        let n = (info.image_name.Length as usize / 2).min(cap);
        if n == 0 {
            return Err(io::Error::other(format!("process {pid} has no image")));
        }
        return Ok(String::from_utf16_lossy(&buf[..n]));
    }
    Err(io::Error::other(format!(
        "process {pid} image name too long"
    )))
}

/// Identity of a file: (volume serial number, file index). Two paths name
/// the same file exactly when these are equal — independent of case, 8.3
/// names, drive letters vs. `\Device\…` paths, and junctions.
pub fn file_identity(path: &OsStr) -> io::Result<(u32, u64)> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
    };
    let name = wide(path);
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let h = OwnedHandle(h);
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.raw(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        info.dwVolumeSerialNumber,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}

/// [`file_identity`] of an NT path (`\Device\…`, as [`process_image_nt`]
/// returns it), opened through `\\?\GLOBALROOT`.
pub fn nt_file_identity(nt_path: &str) -> io::Result<(u32, u64)> {
    if !nt_path.starts_with('\\') || nt_path.contains('\0') {
        return Err(io::Error::other("not an NT path"));
    }
    file_identity(OsStr::new(&format!(r"\\?\GLOBALROOT{nt_path}")))
}

/// Does process `pid`'s token hold `sid` as an ENABLED group?
/// `Ok(None)`: the caller may not open the process or its token (a standard
/// user and a SYSTEM service) — the check is then unavailable, not failed.
pub fn process_has_group(pid: u32, sid: &[u8]) -> io::Result<Option<bool>> {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    use windows_sys::Win32::Security::TokenGroups;
    use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let denied = |e: &io::Error| e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    let p = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if p.is_null() {
        let e = io::Error::last_os_error();
        return if denied(&e) { Ok(None) } else { Err(e) };
    }
    let p = OwnedHandle(p);
    let mut t: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(p.raw(), TOKEN_QUERY, &mut t) } == 0 {
        let e = io::Error::last_os_error();
        return if denied(&e) { Ok(None) } else { Err(e) };
    }
    let t = OwnedHandle(t);
    Ok(Some(token_groups(t.raw(), TokenGroups)?.iter().any(
        |(s, attr)| s == sid && attr & (SE_GROUP_ENABLED as u32) != 0,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_sids_match_their_strings() {
        assert_eq!(sid_string(&SID_LOCAL_SYSTEM).unwrap(), "S-1-5-18");
        assert_eq!(sid_string(&SID_ADMINISTRATORS).unwrap(), "S-1-5-32-544");
        assert_eq!(sid_string(&SID_INTERACTIVE).unwrap(), "S-1-5-4");
        assert!(sid_string(&[1, 2, 3]).is_err());
    }

    #[test]
    fn own_token_reads() {
        let t = process_token().unwrap();
        let sid = token_user_sid(t.raw()).unwrap();
        assert!(sid_string(&sid).unwrap().starts_with("S-1-5-"));
        assert!(token_integrity(t.raw()).unwrap() >= crate::policy::INTEGRITY_MEDIUM);
        token_elevated(t.raw()).unwrap();
        assert!(!running_as_system());
    }

    #[test]
    fn restricted_self_check() {
        let svc = sid_from_string(crate::store::SERVICE_SID).unwrap();
        // A normal process token is not restricted: queries would be off.
        assert!(!process_restricted_by(&svc));
        // The emulated service token is restricted by the service SID...
        let t = write_restricted_token(&[
            crate::store::SERVICE_SID,
            SID_EVERYONE_STR,
            SID_WRITE_RESTRICTED_STR,
        ])
        .unwrap();
        assert!(token_restricted_by(t.raw(), &svc).unwrap());
        // ...but one restricted by other SIDs only does not qualify.
        let other = write_restricted_token(&[SID_EVERYONE_STR, SID_WRITE_RESTRICTED_STR]).unwrap();
        assert!(!token_restricted_by(other.raw(), &svc).unwrap());
        assert!(token_logon_id(t.raw()).unwrap() != 0);
    }

    /// Review N1: a deny-only derivative keeps the logon id but gets a
    /// different fingerprint; the same token always gets the same one.
    #[test]
    fn fingerprint_tells_deny_only_tokens_apart() {
        let full = crate::access::own_identification_token().unwrap();
        let again = crate::access::own_identification_token().unwrap();
        let deny = deny_only_token(&["S-1-5-11"]).unwrap();
        assert_eq!(
            token_logon_id(full.raw()).unwrap(),
            token_logon_id(deny.raw()).unwrap(),
            "same logon session"
        );
        let (f, a, d) = (
            token_fingerprint(full.raw()).unwrap(),
            token_fingerprint(again.raw()).unwrap(),
            token_fingerprint(deny.raw()).unwrap(),
        );
        assert_eq!(f, a);
        assert_ne!(f, d);
        // Still an acceptable client (not restricted): the cache key is
        // what keeps the two apart.
        let id = client_identity(deny.raw()).unwrap();
        assert!(!id.restricted);
        // Review N1b: the user SID itself deny-only is another identity.
        let user = sid_string(&current_user_sid().unwrap()).unwrap();
        let deny_user = deny_only_token(&[&user]).unwrap();
        assert_ne!(f, token_fingerprint(deny_user.raw()).unwrap());
    }

    #[test]
    fn program_data_resolves() {
        let p = program_data().unwrap();
        assert!(p.is_absolute());
        assert!(!protected_roots().is_empty());
    }

    #[test]
    fn protected_dir_is_component_wise() {
        let roots = vec![PathBuf::from(r"C:\Program Files")];
        assert!(in_protected_dir(
            Path::new(r"C:\Program Files\Ferrum"),
            &roots
        ));
        assert!(in_protected_dir(
            Path::new(r"c:\program files\ferrum"),
            &roots
        ));
        assert!(!in_protected_dir(
            Path::new(r"C:\Program Files Evil\F"),
            &roots
        ));
        assert!(!in_protected_dir(
            Path::new(r"C:\Users\x\AppData\Local\F"),
            &roots
        ));
        assert!(!in_protected_dir(Path::new(r"C:\Program Files"), &[]));
    }
}
