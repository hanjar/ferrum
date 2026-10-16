//! FER-490 E3/F1: an agent executable is only started from a location no other account can
//! change. On a shared PC a folder writable by "Users" would otherwise let another account plant
//! or swap a `claude.exe` that Ferrum runs with this user's rights.
//!
//! Rules (checked at detection and again before every run):
//! - TARGET = the exe (and a script entry) and its own folder:
//!   - the owner is the current user, SYSTEM, Administrators or TrustedInstaller (an owner has
//!     an implicit WRITE_DAC);
//!   - every ACCESS_ALLOWED ACE (type 0) that grants any write-like right
//!     ([`TARGET_WRITE`]) belongs to one of those accounts;
//!   - any other ALLOW ACE type (compound, object, callback, callback-object, …) is treated as a
//!     foreign writer (fail closed); deny ACEs are ignored; inherit-only ACEs do not apply to the
//!     object itself and are skipped; a NULL DACL is refused.
//! - ANCESTORS = every folder above that, up to the volume root:
//!   - owner as above;
//!   - no other account may hold a right that lets it move/replace the folder chain
//!     ([`ANCESTOR_DANGER`]: DELETE, DELETE_CHILD, WRITE_DAC, WRITE_OWNER, GENERIC_ALL,
//!     GENERIC_WRITE, FILE_ADD_SUBDIRECTORY); FILE_ADD_FILE alone is harmless there;
//!   - exception for the VOLUME ROOT only: FILE_ADD_SUBDIRECTORY (and FILE_ADD_FILE) for other
//!     accounts is accepted (Windows grants "Authenticated Users" create-folder on `C:\` by
//!     default), as long as no other dangerous right is present;
//!   - non-type-0 allow ACEs are fail-closed as above;
//!   - AppContainer capability SIDs (`S-1-15-3-…`, sandboxed apps of this same user) are accepted
//!     on ancestors only (Windows puts a Full-control capability grant on `%USERPROFILE%\AppData`
//!     on this PC); on the exe and its folder they count as foreign writers.
//!
//! The default locations pass: the npm global folder in `%APPDATA%` and `%LOCALAPPDATA%` (owned
//! by the user, inherited user/SYSTEM/Administrators), `C:\Program Files` (TrustedInstaller),
//! `C:\Users` (SYSTEM) and the root `C:\` (only "create folder" for Authenticated Users).

use std::path::Path;

/// The security information of a path could not be read (treated as untrusted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclUnreadable;

/// Accounts that may own / write the agent location (besides the current user).
const TRUSTED: &[&str] = &[
    "S-1-5-18",                                                       // SYSTEM
    "S-1-5-32-544",                                                   // BUILTIN\Administrators
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464", // TrustedInstaller
];
/// CREATOR OWNER: only meaningful on inherit-only ACEs (skipped anyway); never an owner.
const CREATOR_OWNER: &str = "S-1-3-0";

const FILE_ADD_FILE: u32 = 0x0000_0002;
const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
const FILE_WRITE_EA: u32 = 0x0000_0010;
const FILE_DELETE_CHILD: u32 = 0x0000_0040;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
const DELETE: u32 = 0x0001_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Rights that let someone change or replace the exe, or put files into its folder.
const TARGET_WRITE: u32 = FILE_ADD_FILE
    | FILE_ADD_SUBDIRECTORY
    | FILE_WRITE_EA
    | FILE_DELETE_CHILD
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_ALL
    | GENERIC_WRITE;

/// Rights on an ancestor folder that let someone rename/replace the folder chain.
const ANCESTOR_DANGER: u32 = FILE_ADD_SUBDIRECTORY
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_ALL
    | GENERIC_WRITE;

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
/// ACCESS_ALLOWED_CALLBACK_ACE_TYPE (conditional ACE): same mask/SID layout as type 0.
const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 9;
/// ACCESS_DENIED, ACCESS_DENIED_OBJECT, ACCESS_DENIED_CALLBACK, ACCESS_DENIED_CALLBACK_OBJECT.
const DENY_TYPES: [u8; 4] = [1, 6, 10, 12];
const INHERIT_ONLY_ACE: u8 = 0x08;

/// One ACE as read from a DACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub kind: u8,
    pub flags: u8,
    pub mask: u32,
    /// String SID; `None` for ACE types whose SID is not at the fixed offset.
    pub sid: Option<String>,
}

/// Owner and DACL of one path (`dacl == None` = NULL DACL, everyone has full access).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Security {
    pub owner: String,
    pub dacl: Option<Vec<Ace>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The exe/script itself or its own folder.
    Target,
    /// A folder above; `root` = the volume root.
    Ancestor { root: bool },
}

fn trusted(sid: &str, me: &str) -> bool {
    sid == me || TRUSTED.contains(&sid)
}

/// Pure rule check. `Some(reason)` = untrusted.
pub fn evaluate(sec: &Security, me: &str, role: Role) -> Option<String> {
    evaluate_with(sec, me, role, false)
}

/// [`evaluate`] with one switch (FER-514). `callback_as_plain = true` judges a conditional
/// allow ACE (type 9) by its mask and SID exactly like a plain allow ACE (a condition can only
/// narrow what it grants, so this stays conservative): a condition that grants no write right to
/// a foreign account is accepted, one that does is reported. Every other non-plain allow ACE
/// type (object, compound, callback-object) still fails closed. The default (`false`) is the
/// FER-490 rule for agent CLIs, unchanged.
pub fn evaluate_with(
    sec: &Security,
    me: &str,
    role: Role,
    callback_as_plain: bool,
) -> Option<String> {
    if !trusted(&sec.owner, me) {
        return Some(format!("pemilik {}", sec.owner));
    }
    let Some(dacl) = &sec.dacl else {
        return Some("NULL DACL".to_owned());
    };
    for ace in dacl {
        if ace.flags & INHERIT_ONLY_ACE != 0 || DENY_TYPES.contains(&ace.kind) {
            continue;
        }
        let plain = ace.kind == ACCESS_ALLOWED_ACE_TYPE
            || (callback_as_plain && ace.kind == ACCESS_ALLOWED_CALLBACK_ACE_TYPE);
        if !plain {
            // Callback / conditional / object / compound allow ACEs: fail closed.
            return Some(format!(
                "ACE tipe {} ({})",
                ace.kind,
                ace.sid.as_deref().unwrap_or("?")
            ));
        }
        let Some(sid) = ace.sid.as_deref() else {
            return Some("ACE tanpa SID".to_owned());
        };
        if trusted(sid, me) || sid == CREATOR_OWNER {
            continue;
        }
        // AppContainer capability SIDs (S-1-15-3-…) are not accounts: only sandboxed apps of
        // THIS user that hold the capability use them. Windows/Store apps put such grants on
        // `%USERPROFILE%\AppData`; accepted on ancestors, never on the exe or its folder.
        if matches!(role, Role::Ancestor { .. }) && sid.starts_with("S-1-15-3-") {
            continue;
        }
        let bad = match role {
            Role::Target => ace.mask & TARGET_WRITE,
            Role::Ancestor { root: false } => ace.mask & ANCESTOR_DANGER,
            // Root: "create folder" for Authenticated Users etc. is the Windows default.
            Role::Ancestor { root: true } => ace.mask & ANCESTOR_DANGER & !FILE_ADD_SUBDIRECTORY,
        };
        if bad != 0 {
            return Some(sid.to_owned());
        }
    }
    None
}

#[cfg(windows)]
unsafe fn sid_string(sid: windows_sys::Win32::Security::PSID) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    let mut out: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `sid` points into a live security descriptor / token buffer; `out` freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 || out.is_null() {
        return None;
    }
    let mut len = 0;
    // SAFETY: NUL-terminated wide string from the OS.
    while unsafe { *out.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) });
    unsafe { LocalFree(out as _) };
    Some(s)
}

/// String SID of the current process user.
#[cfg(windows)]
pub fn current_user_sid() -> Result<String, AclUnreadable> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: token handle checked and closed; buffer sized for TOKEN_USER + SID.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(AclUnreadable);
        }
        let mut buf = vec![0u64; 64];
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(AclUnreadable);
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        sid_string(user.User.Sid).ok_or(AclUnreadable)
    }
}

/// Owner + DACL of `path` (without following anything special: plain named lookup).
#[cfg(windows)]
pub fn read_security(path: &Path) -> Result<Security, AclUnreadable> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: valid NUL-terminated path; out-parameters are ours; `sd` freed with LocalFree.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(AclUnreadable);
    }
    let result = (|| {
        if owner.is_null() {
            return Err(AclUnreadable);
        }
        // SAFETY: `owner` points into `sd`.
        let owner = unsafe { sid_string(owner) }.ok_or(AclUnreadable)?;
        if dacl.is_null() {
            return Ok(Security { owner, dacl: None });
        }
        let mut aces = Vec::new();
        // SAFETY: `dacl` points into `sd`, alive until LocalFree below.
        let count = unsafe { (*dacl).AceCount };
        for i in 0..u32::from(count) {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: index < AceCount.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
                return Err(AclUnreadable);
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            // ACCESS_ALLOWED / ACCESS_DENIED (and their callback forms 9/10) share the
            // header + mask + SID layout; object ACEs (5, 6, 11, 12) have GUIDs before the SID.
            let fixed_layout = matches!(header.AceType, 0 | 1 | 9 | 10);
            let base = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
            let sid = if fixed_layout {
                let p = std::ptr::addr_of!(base.SidStart) as PSID;
                Some(unsafe { sid_string(p) }.ok_or(AclUnreadable)?)
            } else {
                None
            };
            aces.push(Ace {
                kind: header.AceType,
                flags: header.AceFlags,
                mask: base.Mask,
                sid,
            });
        }
        Ok(Security {
            owner,
            dacl: Some(aces),
        })
    })();
    unsafe { LocalFree(sd as _) };
    result
}

#[cfg(not(windows))]
pub fn current_user_sid() -> Result<String, AclUnreadable> {
    Ok(String::new())
}

#[cfg(not(windows))]
pub fn read_security(_path: &Path) -> Result<Security, AclUnreadable> {
    Ok(Security {
        owner: String::new(),
        dacl: Some(Vec::new()),
    })
}

/// FER-494 T1/T3: the owner of a folder the user opens an agent in is the current user, SYSTEM,
/// Administrators or TrustedInstaller. `Err(owner SID or reason)` otherwise (shown as a warning;
/// nothing is refused here).
pub fn owner_trusted(path: &Path) -> Result<(), String> {
    let me = current_user_sid().map_err(|_| "akun saat ini tidak terbaca".to_owned())?;
    let sec = read_security(path).map_err(|_| "pemilik folder tidak dapat dibaca".to_owned())?;
    if trusted(&sec.owner, &me) {
        Ok(())
    } else {
        Err(format!("folder dimiliki akun lain ({})", sec.owner))
    }
}

/// The file, its folder and every ancestor up to the volume root, by the rules above.
/// `Err(reason)` is shown in Settings / the chat.
pub fn trusted_location(file: &Path) -> Result<(), String> {
    location_check(file, false)
}

/// FER-514: [`trusted_location`] for a program the user installed themselves (Rar.exe): same
/// rules, except that a conditional allow ACE is judged by its rights (see [`evaluate_with`]).
/// The stock `C:\Program Files\WinRAR` folder carries a read-only conditional ACE for Users, which
/// the strict agent rule would refuse although nobody but TrustedInstaller/Administrators can
/// write there.
pub fn trusted_location_callback_aware(file: &Path) -> Result<(), String> {
    location_check(file, true)
}

fn location_check(file: &Path, callback_as_plain: bool) -> Result<(), String> {
    let me = current_user_sid().map_err(|_| "ditolak: akun saat ini tidak terbaca".to_owned())?;
    let mut checks: Vec<(&Path, Role)> = vec![(file, Role::Target)];
    if let Some(parent) = file.parent() {
        checks.push((parent, Role::Target));
        let mut up = parent.parent();
        while let Some(dir) = up {
            let root = dir.parent().is_none();
            checks.push((dir, Role::Ancestor { root }));
            up = dir.parent();
        }
    }
    for (path, role) in checks {
        let sec = read_security(path)
            .map_err(|_| format!("ditolak: izin {} tidak dapat dibaca", path.display()))?;
        if let Some(why) = evaluate_with(&sec, &me, role, callback_as_plain) {
            return Err(format!(
                "ditolak: {} bisa diubah akun lain ({why})",
                path.display()
            ));
        }
    }
    Ok(())
}
