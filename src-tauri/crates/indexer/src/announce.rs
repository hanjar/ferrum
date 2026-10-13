//! FER-487: the fallback pipe name, announced in the registry.
//!
//! The service pipe normally lives at the fixed name `\\.\pipe\FerrumIndexer`.
//! A process of a standard user can take that name first (squatting), which
//! stops the index from being served (denial of service only: clients verify
//! the server, see `pipe::verify_server`). When the service finds the fixed
//! name taken, it creates `\\.\pipe\FerrumIndexer-<32 hex>` from the OS RNG
//! and then writes that name to the value `PipeName` under
//! `HKLM\SYSTEM\CurrentControlSet\Services\FerrumIndexer\Parameters`. The
//! pipe exists BEFORE its name is announced, so the announcement never
//! points at a name nobody holds yet.
//!
//! Clients read the value only after the fixed name failed, and only when:
//! - the owner of the service key and of `Parameters` is SYSTEM or
//!   Administrators, and neither key grants a write right to any other SID
//!   (the service SID excepted: it is the restricted service token's only
//!   write grant, FER-476). ACEs marked INHERIT_ONLY do not apply to the key
//!   itself, so they are not counted (the service key carries such ACEs for
//!   its children, e.g. CREATOR OWNER);
//! - the value is read from the same registry handle as the `Parameters` ACL,
//!   and it is exactly `FerrumIndexer-[0-9a-f]{32}`.
//!
//! The announced name is a hint, never a trust decision: the client still
//! runs `pipe::verify_server` (owner, server PID from the SCM, image, token)
//! on the pipe it opens.
//!
//! The registry access sits behind [`KeyStore`] so the trust rules are unit
//! tested with an in-memory store; [`Hklm`] is the real implementation.

use crate::win::{SecurityDescriptor, sid_bytes, sid_string, wide};
use std::ffi::c_void;
use std::io;
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::{
    ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, GetAce,
    GetAclInformation, GetLengthSid, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ, KEY_SET_VALUE, REG_NONE,
    REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW,
    RegGetKeySecurity, RegOpenKeyExW, RegQueryValueExW, RegSetKeySecurity, RegSetValueExW,
};

/// The service's key (created by the SCM at registration).
pub const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services\FerrumIndexer";
/// The key the service announces its fallback name in (created at install).
pub const PARAMETERS_KEY: &str = r"SYSTEM\CurrentControlSet\Services\FerrumIndexer\Parameters";
/// REG_SZ value holding the announced name.
pub const PIPE_NAME_VALUE: &str = "PipeName";
/// Value the install probe writes and deletes under the restricted token.
pub const PROBE_VALUE: &str = "PipeNameProbe";
/// Prefix of the fallback name (the full name is this + 32 lowercase hex).
pub const NAME_PREFIX: &str = "FerrumIndexer-";
const HEX_DIGITS: usize = 32;

const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// Registry rights that change data or structure (a write for this check):
/// KEY_SET_VALUE, KEY_CREATE_SUB_KEY, KEY_CREATE_LINK, DELETE, WRITE_DAC,
/// WRITE_OWNER, GENERIC_WRITE, GENERIC_ALL.
const WRITE_MASK: u32 =
    0x2 | 0x4 | 0x20 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x4000_0000 | 0x1000_0000;
const ACCESS_ALLOWED_ACE: u8 = 0x0;
const ACCESS_DENIED_ACE: u8 = 0x1;
/// ACE_HEADER.AceFlags: the ACE applies to child objects only.
const INHERIT_ONLY_ACE: u8 = 0x08;

/// Is `name` exactly `FerrumIndexer-` followed by 32 lowercase hex digits?
/// Nothing else (no separators, no prefix, no upper case) passes.
pub fn name_is_valid(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX).is_some_and(|hex| {
        hex.len() == HEX_DIGITS
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A fallback name from 16 random bytes.
pub fn name_from_bytes(bytes: &[u8; 16]) -> String {
    let mut s = String::with_capacity(NAME_PREFIX.len() + HEX_DIGITS);
    s.push_str(NAME_PREFIX);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A fresh fallback name from the OS random source (`BCryptGenRandom` with
/// the system-preferred RNG; no user-mode PRNG of our own).
pub fn random_name() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(io::Error::other(format!(
            "OS random source: NTSTATUS {status:#x}"
        )));
    }
    let name = name_from_bytes(&bytes);
    debug_assert!(name_is_valid(&name));
    Ok(name)
}

/// One ACE of a key's DACL, reduced to what the trust check needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub allow: bool,
    /// ACE_HEADER.AceFlags (inheritance flags).
    pub flags: u8,
    pub mask: u32,
    pub sid: String,
}

/// Owner and DACL of a registry key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySecurity {
    pub owner: String,
    /// A NULL DACL grants everyone everything: never trusted.
    pub null_dacl: bool,
    pub aces: Vec<Ace>,
}

/// Pure trust rule for a key the client reads the announcement from: owner
/// SYSTEM or Administrators, no NULL DACL, and no ALLOW ACE that applies to
/// the key itself and grants a write right to any SID other than SYSTEM,
/// Administrators or the service SID. INHERIT_ONLY ACEs are skipped (they
/// govern children, not this key). Deny ACEs grant nothing and are ignored.
pub fn key_is_trusted(sec: &KeySecurity, service_sid: &str) -> bool {
    if sec.null_dacl {
        return false;
    }
    if sec.owner != SYSTEM_SID && sec.owner != ADMINISTRATORS_SID {
        return false;
    }
    sec.aces.iter().all(|a| {
        a.flags & INHERIT_ONLY_ACE != 0
            || !a.allow
            || a.mask & WRITE_MASK == 0
            || [SYSTEM_SID, ADMINISTRATORS_SID, service_sid].contains(&a.sid.as_str())
    })
}

/// Registry access, behind a seam for tests. A missing key or value is
/// `Err` (security) or `Ok(None)` (values); deleting a missing value is `Ok`.
pub trait KeyStore {
    /// Owner and DACL of `key`.
    fn security(&self, key: &str) -> io::Result<KeySecurity>;
    /// Owner and DACL of `key` AND the value `value`, both read through ONE
    /// handle, so the value is the one the checked ACL protects.
    fn snapshot(&self, key: &str, value: &str) -> io::Result<(KeySecurity, Option<String>)>;
    fn write_sz(&self, key: &str, value: &str, data: &str) -> io::Result<()>;
    fn delete(&self, key: &str, value: &str) -> io::Result<()>;
}

/// The name the service announced, for a client, or `None` when there is no
/// announcement or any check failed (the caller then keeps its own error).
pub fn read_announced(store: &dyn KeyStore, service_sid: &str) -> Option<String> {
    let parent = store.security(SERVICES_KEY).ok()?;
    if !key_is_trusted(&parent, service_sid) {
        return None;
    }
    let (params, value) = store.snapshot(PARAMETERS_KEY, PIPE_NAME_VALUE).ok()?;
    if !key_is_trusted(&params, service_sid) {
        return None;
    }
    let name = value?;
    name_is_valid(&name).then_some(name)
}

/// Service, at start: drop any announcement left by an earlier run.
pub fn clear(store: &dyn KeyStore) -> io::Result<()> {
    store.delete(PARAMETERS_KEY, PIPE_NAME_VALUE)
}

/// Service, after the pipe `name` exists: announce it.
pub fn announce(store: &dyn KeyStore, name: &str) -> io::Result<()> {
    if !name_is_valid(name) {
        return Err(io::Error::other(
            "refusing to announce an invalid pipe name",
        ));
    }
    store.write_sz(PARAMETERS_KEY, PIPE_NAME_VALUE, name)
}

/// Install probe: the restricted token must be able to write the key.
pub fn probe_write(store: &dyn KeyStore) -> io::Result<()> {
    store.write_sz(PARAMETERS_KEY, PROBE_VALUE, "probe")?;
    store.delete(PARAMETERS_KEY, PROBE_VALUE)
}

/// SDDL of the `Parameters` key: owner Administrators (the installer can
/// assign it without SeRestorePrivilege); SYSTEM and Administrators full;
/// the service SID KEY_WRITE (the restricted token's only write grant);
/// Users read. Protected: nothing is inherited from the service key.
pub fn parameters_sddl(service_sid: &str) -> String {
    format!("O:BAD:P(A;CI;KA;;;SY)(A;CI;KA;;;BA)(A;CI;KW;;;{service_sid})(A;CI;KR;;;BU)")
}

/// Install: create `Parameters` (or take an existing one) and set its DACL
/// and owner to [`parameters_sddl`]. Needs an elevated installer.
pub fn create_parameters_key(service_sid: &str) -> io::Result<()> {
    let sd = SecurityDescriptor::from_sddl(&parameters_sddl(service_sid))?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let name = wide(PARAMETERS_KEY);
    let mut h: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            name.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            &sa,
            &mut h,
            std::ptr::null_mut(),
        )
    };
    if rc as u32 != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let key = Key(h);
    // An existing key keeps its old ACL under RegCreateKeyEx: set it again.
    let rc = unsafe {
        RegSetKeySecurity(
            key.0,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            sd.0,
        )
    };
    if rc as u32 != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    Ok(())
}

/// The real registry (HKLM).
pub struct Hklm;

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// Open `key` under HKLM. `Ok(None)`: the key does not exist.
fn open(key: &str, access: u32) -> io::Result<Option<Key>> {
    let name = wide(key);
    let mut h: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, name.as_ptr(), 0, access, &mut h) };
    match rc as u32 {
        ERROR_SUCCESS => Ok(Some(Key(h))),
        ERROR_FILE_NOT_FOUND => Ok(None),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

/// Owner and DACL out of a security descriptor, parsed into [`KeySecurity`].
///
/// # Safety
/// `sd` must point to a valid, self-relative or absolute security descriptor
/// that stays alive for the call.
unsafe fn parse_sd(sd: PSECURITY_DESCRIPTOR) -> io::Result<KeySecurity> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut defaulted: i32 = 0;
    if unsafe { GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut present: i32 = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let dacl = if present != 0 {
        dacl
    } else {
        std::ptr::null_mut()
    };
    unsafe { parse_security(owner, dacl) }
}

/// The security of an open key, read through that same handle.
fn key_security_of(h: HKEY) -> io::Result<KeySecurity> {
    // u64 words: the descriptor is read by pointer, keep it aligned.
    let mut words: Vec<u64> = vec![0; 128];
    let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    for _ in 0..2 {
        let mut len = (words.len() * 8) as u32;
        let rc = unsafe { RegGetKeySecurity(h, info, words.as_mut_ptr() as *mut c_void, &mut len) };
        match rc as u32 {
            ERROR_SUCCESS => {
                return unsafe { parse_sd(words.as_mut_ptr() as PSECURITY_DESCRIPTOR) };
            }
            ERROR_INSUFFICIENT_BUFFER => {
                words = vec![0; (len as usize).div_ceil(8)];
            }
            code => return Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
    Err(io::Error::other("key security descriptor did not fit"))
}

/// REG_SZ `value` of an open key. `Ok(None)`: no such value.
fn query_sz(h: HKEY, value: &str) -> io::Result<Option<String>> {
    let name = wide(value);
    // 256 UTF-16 units: far more than a valid name (46 characters).
    let mut buf = [0u16; 256];
    let mut kind = REG_NONE;
    let mut bytes = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegQueryValueExW(
            h,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut bytes,
        )
    };
    match rc as u32 {
        ERROR_SUCCESS => {
            if kind != REG_SZ {
                return Err(io::Error::other("PipeName is not a REG_SZ value"));
            }
            let units = (bytes as usize / 2).min(buf.len());
            let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
            Ok(Some(String::from_utf16_lossy(&buf[..end])))
        }
        ERROR_FILE_NOT_FOUND => Ok(None),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

/// Parse the DACL into ACEs.
///
/// # Safety
/// `owner` and `dacl` must come from a security descriptor that is alive for
/// the call (`dacl` null = NULL DACL).
unsafe fn parse_security(owner: PSID, dacl: *const ACL) -> io::Result<KeySecurity> {
    let owner = sid_string(&unsafe { sid_bytes(owner) }?)?;
    if dacl.is_null() {
        return Ok(KeySecurity {
            owner,
            null_dacl: true,
            aces: Vec::new(),
        });
    }
    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    let ok = unsafe {
        GetAclInformation(
            dacl,
            &mut info as *mut ACL_SIZE_INFORMATION as *mut c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut aces = Vec::with_capacity(info.AceCount as usize);
    for i in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let p = ace as *const u8;
        // ACE_HEADER: AceType (1), AceFlags (1), AceSize (2).
        let kind = unsafe { *p };
        let flags = unsafe { *p.add(1) };
        // Only plain ALLOW and DENY ACEs are understood; anything else (object
        // ACEs, ...) makes the key untrusted.
        if kind != ACCESS_ALLOWED_ACE && kind != ACCESS_DENIED_ACE {
            return Err(io::Error::other(format!(
                "unsupported ACE type {kind:#x} on a key"
            )));
        }
        // ACCESS_*_ACE: header (4 bytes), Mask (4 bytes), then the SID.
        let mask = unsafe { (p.add(4) as *const u32).read_unaligned() };
        let sid_ptr = unsafe { p.add(8) } as PSID;
        let len = unsafe { GetLengthSid(sid_ptr) } as usize;
        let bytes = unsafe { std::slice::from_raw_parts(p.add(8), len) };
        aces.push(Ace {
            allow: kind == ACCESS_ALLOWED_ACE,
            flags,
            mask,
            sid: sid_string(bytes)?,
        });
    }
    Ok(KeySecurity {
        owner,
        null_dacl: false,
        aces,
    })
}

impl KeyStore for Hklm {
    fn security(&self, key: &str) -> io::Result<KeySecurity> {
        // KEY_READ includes READ_CONTROL, which the security read needs.
        let k = open(key, KEY_READ)?
            .ok_or_else(|| io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32))?;
        key_security_of(k.0)
    }

    fn snapshot(&self, key: &str, value: &str) -> io::Result<(KeySecurity, Option<String>)> {
        // One handle: the ACL and the value are read from the same object.
        let k = open(key, KEY_READ)?
            .ok_or_else(|| io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32))?;
        let sec = key_security_of(k.0)?;
        let v = query_sz(k.0, value)?;
        Ok((sec, v))
    }

    fn write_sz(&self, key: &str, value: &str, data: &str) -> io::Result<()> {
        let k = open(key, KEY_SET_VALUE)?
            .ok_or_else(|| io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32))?;
        let name = wide(value);
        let text = wide(data); // includes the terminating NUL REG_SZ requires
        let rc = unsafe {
            RegSetValueExW(
                k.0,
                name.as_ptr(),
                0,
                REG_SZ,
                text.as_ptr() as *const u8,
                (text.len() * 2) as u32,
            )
        };
        if rc as u32 == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc as i32))
        }
    }

    fn delete(&self, key: &str, value: &str) -> io::Result<()> {
        let Some(k) = open(key, KEY_SET_VALUE)? else {
            return Ok(());
        };
        let name = wide(value);
        match unsafe { RegDeleteValueW(k.0, name.as_ptr()) } as u32 {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const SVC: &str = "S-1-5-80-401986193-3732369529-2668797022-3469762217-2719989880";
    const GOOD: &str = "FerrumIndexer-0123456789abcdef0123456789abcdef";

    /// The real DACL of `HKLM\...\Services\FerrumIndexer` on the test
    /// machine (SCM default, read with GetSecurityInfo). Inherit-only ACEs
    /// (CIIOID) belong to children.
    const REAL_SERVICE_KEY_SDDL: &str = "O:BAG:SYD:AI(A;CIID;KR;;;BU)(A;CIID;KA;;;BA)(A;CIID;KA;;;SY)(A;CIIOID;KA;;;CO)(A;CIID;KR;;;AC)(A;CIID;KR;;;S-1-15-3-1024-1065365936-1281604716-3511738428-1654721687-432734479-3232135806-4053264122-3456934681)";

    fn ace(allow: bool, mask: u32, sid: &str) -> Ace {
        Ace {
            allow,
            flags: 0,
            mask,
            sid: sid.into(),
        }
    }

    fn sec(owner: &str, aces: Vec<Ace>) -> KeySecurity {
        KeySecurity {
            owner: owner.into(),
            null_dacl: false,
            aces,
        }
    }

    /// A key ACL as the installer writes it.
    fn installed(owner: &str) -> KeySecurity {
        sec(
            owner,
            vec![
                ace(true, 0xF_003F, SYSTEM_SID),
                ace(true, 0xF_003F, ADMINISTRATORS_SID),
                ace(true, 0x2_0006, SVC),
                ace(true, 0x2_0019, "S-1-5-32-545"),
            ],
        )
    }

    /// Owner and DACL of a real SDDL, parsed by the same code the service
    /// client runs on a live key.
    fn from_real_sddl(sddl: &str) -> KeySecurity {
        let sd = SecurityDescriptor::from_sddl(sddl).unwrap();
        unsafe { parse_sd(sd.0) }.unwrap()
    }

    #[derive(Default)]
    struct Mock {
        keys: HashMap<String, KeySecurity>,
        values: RefCell<HashMap<(String, String), String>>,
    }

    impl Mock {
        fn trusted() -> Mock {
            let mut m = Mock::default();
            m.keys
                .insert(SERVICES_KEY.into(), installed(ADMINISTRATORS_SID));
            m.keys
                .insert(PARAMETERS_KEY.into(), installed(ADMINISTRATORS_SID));
            m
        }
        fn with_value(self, name: &str) -> Mock {
            self.values
                .borrow_mut()
                .insert((PARAMETERS_KEY.into(), PIPE_NAME_VALUE.into()), name.into());
            self
        }
    }

    impl KeyStore for Mock {
        fn security(&self, key: &str) -> io::Result<KeySecurity> {
            self.keys
                .get(key)
                .cloned()
                .ok_or_else(|| io::Error::from_raw_os_error(2))
        }
        fn snapshot(&self, key: &str, value: &str) -> io::Result<(KeySecurity, Option<String>)> {
            let sec = self.security(key)?;
            let v = self
                .values
                .borrow()
                .get(&(key.into(), value.into()))
                .cloned();
            Ok((sec, v))
        }
        fn write_sz(&self, key: &str, value: &str, data: &str) -> io::Result<()> {
            self.values
                .borrow_mut()
                .insert((key.into(), value.into()), data.into());
            Ok(())
        }
        fn delete(&self, key: &str, value: &str) -> io::Result<()> {
            self.values.borrow_mut().remove(&(key.into(), value.into()));
            Ok(())
        }
    }

    #[test]
    fn names_must_match_exactly() {
        assert!(name_is_valid(GOOD));
        for bad in [
            "FerrumIndexer",
            "FerrumIndexer-",
            "FerrumIndexer-0123456789abcdef0123456789abcde", // 31 digits
            "FerrumIndexer-0123456789abcdef0123456789abcdef0", // 33
            "FerrumIndexer-0123456789ABCDEF0123456789abcdef", // upper case
            "FerrumIndexer-0123456789abcdef0123456789abcdeg", // not hex
            "FerrumIndexer-r-0123456789abcdef0123456789abcd", // old draft shape
            "FerrumIndexer-..\\..\\x0123456789abcdef0123456789",
            r"\\.\pipe\FerrumIndexer-0123456789abcdef0123456789abcdef",
            r"ProtectedPrefix\Administrators\FerrumIndexer-0123456789abcdef0123456789abcdef",
            "FerrumIndexer-0123456789abcdef0123456789abcdef\0x",
            " FerrumIndexer-0123456789abcdef0123456789abcdef",
            "",
        ] {
            assert!(!name_is_valid(bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn generated_names_are_valid() {
        assert_eq!(
            name_from_bytes(&[0xab; 16]),
            format!("FerrumIndexer-{}", "ab".repeat(16))
        );
        assert!(name_is_valid(&name_from_bytes(&[0; 16])));
        let a = random_name().unwrap();
        let b = random_name().unwrap();
        assert!(name_is_valid(&a) && name_is_valid(&b));
        assert_ne!(a, b);
    }

    #[test]
    fn installed_key_is_trusted() {
        assert!(key_is_trusted(&installed(ADMINISTRATORS_SID), SVC));
        assert!(key_is_trusted(&installed(SYSTEM_SID), SVC));
    }

    #[test]
    fn owner_must_be_system_or_administrators() {
        assert!(!key_is_trusted(&installed("S-1-5-32-545"), SVC));
        assert!(!key_is_trusted(&installed("S-1-5-21-1-2-3-1001"), SVC));
    }

    #[test]
    fn write_for_users_or_everyone_is_untrusted() {
        for sid in ["S-1-5-32-545", "S-1-5-11", "S-1-1-0", "S-1-5-4"] {
            let mut k = installed(ADMINISTRATORS_SID);
            k.aces.push(ace(true, 0x2, sid)); // KEY_SET_VALUE
            assert!(!key_is_trusted(&k, SVC), "{sid} may write");
        }
        let mut k = installed(ADMINISTRATORS_SID);
        k.aces.push(ace(true, 0x4000_0000, "S-1-5-32-545")); // GENERIC_WRITE
        assert!(!key_is_trusted(&k, SVC));
        let mut k = installed(ADMINISTRATORS_SID);
        k.aces.push(ace(true, 0x4_0000, "S-1-5-11")); // WRITE_DAC
        assert!(!key_is_trusted(&k, SVC));
    }

    #[test]
    fn deny_aces_and_null_dacl() {
        let mut k = installed(ADMINISTRATORS_SID);
        k.aces.push(ace(false, 0x2, "S-1-5-32-545")); // a deny grants nothing
        assert!(key_is_trusted(&k, SVC));
        let null = KeySecurity {
            owner: ADMINISTRATORS_SID.into(),
            null_dacl: true,
            aces: vec![],
        };
        assert!(!key_is_trusted(&null, SVC));
    }

    /// FER-487 review F1: the real service key of this machine must count as
    /// trusted. Its CREATOR OWNER ACE is inherit-only, so it does not apply
    /// to the key itself.
    #[test]
    fn real_service_key_sddl_is_trusted() {
        let k = from_real_sddl(REAL_SERVICE_KEY_SDDL);
        assert_eq!(k.owner, ADMINISTRATORS_SID);
        assert!(k.aces.iter().any(|a| a.flags & INHERIT_ONLY_ACE != 0));
        assert!(key_is_trusted(&k, SVC));
    }

    /// Read-only: the live `Services\FerrumIndexer` key of this machine must
    /// pass the parent check. Skipped when no service is installed.
    #[test]
    fn live_service_key_is_trusted_when_installed() {
        let Ok(sec) = Hklm.security(SERVICES_KEY) else {
            return;
        };
        assert!(key_is_trusted(&sec, SVC), "{sec:?}");
    }

    #[test]
    fn creator_owner_without_inherit_only_is_untrusted() {
        // Same key, but CREATOR OWNER keeps KEY_ALL_ACCESS on the key itself.
        let sddl = REAL_SERVICE_KEY_SDDL.replace("(A;CIIOID;KA;;;CO)", "(A;CI;KA;;;CO)");
        assert!(!key_is_trusted(&from_real_sddl(&sddl), SVC));
    }

    #[test]
    fn users_with_key_write_are_untrusted_in_the_real_sddl() {
        let sddl = REAL_SERVICE_KEY_SDDL.replace("(A;CIID;KR;;;BU)", "(A;CIID;KW;;;BU)");
        assert!(!key_is_trusted(&from_real_sddl(&sddl), SVC));
        // ...but the same write as inherit-only does not touch this key.
        let io = REAL_SERVICE_KEY_SDDL.replace("(A;CIID;KR;;;BU)", "(A;CIIOID;KW;;;BU)");
        assert!(key_is_trusted(&from_real_sddl(&io), SVC));
    }

    #[test]
    fn announcement_is_read_only_when_every_check_passes() {
        let m = Mock::trusted().with_value(GOOD);
        assert_eq!(read_announced(&m, SVC).as_deref(), Some(GOOD));
    }

    #[test]
    fn unsafe_key_is_ignored() {
        let mut m = Mock::trusted().with_value(GOOD);
        let mut bad = installed(ADMINISTRATORS_SID);
        bad.aces.push(ace(true, 0x2, "S-1-5-11"));
        m.keys.insert(PARAMETERS_KEY.into(), bad);
        assert_eq!(read_announced(&m, SVC), None);

        // The parent key is checked too: a user who may replace the subkey
        // could otherwise swap the whole Parameters key.
        let mut m = Mock::trusted().with_value(GOOD);
        let mut parent = installed(ADMINISTRATORS_SID);
        parent.aces.push(ace(true, 0x4, "S-1-5-32-545")); // KEY_CREATE_SUB_KEY
        m.keys.insert(SERVICES_KEY.into(), parent);
        assert_eq!(read_announced(&m, SVC), None);

        let mut m = Mock::trusted().with_value(GOOD);
        m.keys
            .insert(PARAMETERS_KEY.into(), installed("S-1-5-32-545"));
        assert_eq!(read_announced(&m, SVC), None);
    }

    #[test]
    fn real_parent_key_lets_the_announcement_through() {
        let mut m = Mock::default();
        m.keys
            .insert(SERVICES_KEY.into(), from_real_sddl(REAL_SERVICE_KEY_SDDL));
        m.keys
            .insert(PARAMETERS_KEY.into(), installed(ADMINISTRATORS_SID));
        m.values
            .borrow_mut()
            .insert((PARAMETERS_KEY.into(), PIPE_NAME_VALUE.into()), GOOD.into());
        assert_eq!(read_announced(&m, SVC).as_deref(), Some(GOOD));
    }

    #[test]
    fn invalid_format_is_ignored() {
        for bad in [
            r"\\.\pipe\FerrumIndexer-0123456789abcdef0123456789abcdef",
            "FerrumIndexer-0123456789ABCDEF0123456789ABCDEF",
            "..\\..\\pipe\\x",
            "FerrumIndexer-short",
            "",
        ] {
            let m = Mock::trusted().with_value(bad);
            assert_eq!(read_announced(&m, SVC), None, "{bad:?}");
        }
        assert_eq!(read_announced(&Mock::trusted(), SVC), None, "no value");
    }

    #[test]
    fn missing_keys_mean_no_announcement() {
        let m = Mock::default().with_value(GOOD);
        assert_eq!(read_announced(&m, SVC), None);
    }

    #[test]
    fn service_announces_then_clears() {
        let m = Mock::trusted();
        announce(&m, GOOD).unwrap();
        assert_eq!(read_announced(&m, SVC).as_deref(), Some(GOOD));
        clear(&m).unwrap();
        assert_eq!(read_announced(&m, SVC), None);
        clear(&m).unwrap(); // nothing to clear is fine
        assert!(announce(&m, "FerrumIndexer-x").is_err());
    }

    #[test]
    fn probe_leaves_no_value() {
        let m = Mock::trusted();
        probe_write(&m).unwrap();
        assert!(m.values.borrow().is_empty());
    }

    #[test]
    fn parameters_sddl_grants_write_only_to_trusted_sids() {
        let s = parameters_sddl(SVC);
        assert!(s.starts_with("O:BA"));
        assert!(s.contains(&format!("(A;CI;KW;;;{SVC})")));
        assert!(!s.contains("WD") && !s.contains(";;;AU)") && !s.contains(";;;WD)"));
        // It must parse as a descriptor.
        SecurityDescriptor::from_sddl(&s).unwrap();
    }
}
