//! FER-591: drag files OUT of Ferrum into a browser upload box, Explorer or any other app.
//!
//! The row drag in the webview is an HTML5 drag that only Ferrum understands. For other apps the
//! drag is started natively instead: the shell builds the same data object Explorer uses
//! (`SHCreateDataObject` from the parent folder PIDL and the child PIDLs: `CF_HDROP`, shell ID
//! list, file contents, ...) and `SHDoDragDrop` runs the modal drag loop with the default drop
//! source.
//!
//! Safety rules of this feature:
//!
//! * COPY only. The effect mask passed to `SHDoDragDrop` is `DROPEFFECT_COPY`, never MOVE or LINK,
//!   so a drop target can never make Ferrum's files disappear or turn into shortcuts.
//! * The paths are validated by the caller (`ipc.rs`, same gates as the clipboard copy); this
//!   module adds the pure count / same-parent rules and does the COM work.
//! * The drag runs on the main (WebView2) thread, which is already OLE-initialised and owns the
//!   window the mouse is captured on. Every PIDL and the data object are released on all paths.

use crate::domain::{ErrorCode, FsError};

/// Most items one drag may carry (same limit as the file clipboard).
pub const MAX_ITEMS: usize = 10_000;

/// Count rule: 1..=`MAX_ITEMS` paths.
pub fn check_count(n: usize) -> Result<(), FsError> {
    if n == 0 {
        return Err(FsError::new(ErrorCode::InvalidPath, "Nothing to drag"));
    }
    if n > MAX_ITEMS {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Too many items to drag (the limit is 10,000)",
        ));
    }
    Ok(())
}

/// Same-parent rule: the shell data object is built from ONE parent folder plus child PIDLs, and a
/// pane selection always lives in one folder. Returns the parent folder (spelling of the first
/// path). A path without a parent (a drive root) cannot be dragged.
pub fn common_parent(paths: &[String]) -> Result<String, FsError> {
    check_count(paths.len())?;
    let parent_of = |p: &str| -> Result<String, FsError> {
        std::path::Path::new(p)
            .parent()
            .and_then(|d| d.to_str())
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                FsError::new(
                    ErrorCode::InvalidPath,
                    "A drive or share root cannot be dragged",
                )
                .with_path(p)
            })
    };
    let first = parent_of(&paths[0])?;
    let key = |s: &str| {
        s.trim_end_matches(['\\', '/'])
            .to_lowercase()
            .replace('/', "\\")
    };
    for p in &paths[1..] {
        if key(&parent_of(p)?) != key(&first) {
            return Err(FsError::new(
                ErrorCode::InvalidPath,
                "All dragged items must be in the same folder",
            )
            .with_path(p));
        }
    }
    Ok(first)
}

/// Runs the native drag. Must be called on the main thread. Returns `true` when the target
/// accepted a copy.
#[cfg(windows)]
pub fn run_drag(hwnd: isize, parent: &str, paths: &[String]) -> Result<bool, FsError> {
    use core::ffi::c_void;
    use windows_sys::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_NONE};
    use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows_sys::Win32::UI::Shell::{
        ILFindLastID, ILFree, SHCreateDataObject, SHDoDragDrop, SHParseDisplayName,
    };
    use windows_sys::core::GUID;

    // IID_IDataObject {0000010e-0000-0000-C000-000000000046}
    const IID_IDATAOBJECT: GUID = GUID {
        data1: 0x0000_010e,
        data2: 0,
        data3: 0,
        data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
    };

    fn fail(what: &str, hr: i32) -> FsError {
        FsError::new(ErrorCode::Io, format!("{what} failed (HRESULT {hr:#010x})"))
    }

    /// Owns absolute PIDLs from `SHParseDisplayName`; frees them on drop (success and error).
    struct Pidls(Vec<*mut ITEMIDLIST>);
    impl Drop for Pidls {
        fn drop(&mut self) {
            for p in self.0.drain(..) {
                // SAFETY: each pointer came from SHParseDisplayName (CoTaskMem allocation) and is
                // freed exactly once here.
                unsafe { ILFree(p) };
            }
        }
    }

    /// Owns one reference to the data object; releases it on drop.
    struct DataObject(*mut c_void);
    impl Drop for DataObject {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live COM object. Slot 2 of every COM vtable is
            // IUnknown::Release; the first pointer-sized field of the object is the vtable.
            unsafe {
                let vtbl = *(self.0 as *const *const usize);
                let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                    core::mem::transmute(*vtbl.add(2));
                release(self.0);
            }
        }
    }

    fn parse(path: &str) -> Result<*mut ITEMIDLIST, FsError> {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut pidl: *mut ITEMIDLIST = core::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `pidl` is a valid out pointer;
        // no bind context and no attribute query.
        let hr = unsafe {
            SHParseDisplayName(
                wide.as_ptr(),
                core::ptr::null_mut(),
                &mut pidl,
                0,
                core::ptr::null_mut(),
            )
        };
        if hr < 0 || pidl.is_null() {
            return Err(fail("SHParseDisplayName", hr).with_path(path));
        }
        Ok(pidl)
    }

    let mut owned = Pidls(Vec::with_capacity(paths.len() + 1));
    let parent_pidl = parse(parent)?;
    owned.0.push(parent_pidl);
    let mut children: Vec<*const ITEMIDLIST> = Vec::with_capacity(paths.len());
    for p in paths {
        let abs = parse(p)?;
        owned.0.push(abs);
        // SAFETY: `abs` is a valid absolute PIDL; the returned pointer points INTO it (no
        // allocation), so it stays valid until `owned` is dropped and is never freed separately.
        children.push(unsafe { ILFindLastID(abs) });
    }

    let mut raw: *mut c_void = core::ptr::null_mut();
    // SAFETY: `parent_pidl` and `children` (len == paths.len() <= MAX_ITEMS) are valid PIDLs kept
    // alive by `owned`; `raw` is a valid out pointer; no aggregation (null inner object).
    let hr = unsafe {
        SHCreateDataObject(
            parent_pidl,
            children.len() as u32,
            children.as_ptr(),
            core::ptr::null_mut(),
            &IID_IDATAOBJECT,
            &mut raw,
        )
    };
    if hr < 0 || raw.is_null() {
        return Err(fail("SHCreateDataObject", hr));
    }
    let data = DataObject(raw);

    let mut effect: u32 = DROPEFFECT_NONE;
    // SAFETY: `data.0` is a live IDataObject, `hwnd` is the Ferrum window handle (main thread),
    // null drop source = the shell's default one, COPY is the only allowed effect, `effect` is a
    // valid out pointer. The call returns when the drag ends.
    let hr = unsafe {
        SHDoDragDrop(
            hwnd as _,
            data.0,
            core::ptr::null_mut(),
            DROPEFFECT_COPY,
            &mut effect,
        )
    };
    drop(data);
    drop(owned);
    // DRAGDROP_S_DROP (0x40100) and DRAGDROP_S_CANCEL (0x40101) are success codes (hr >= 0).
    if hr < 0 {
        return Err(fail("SHDoDragDrop", hr));
    }
    Ok(effect & DROPEFFECT_COPY != 0)
}

#[cfg(not(windows))]
pub fn run_drag(_hwnd: isize, _parent: &str, _paths: &[String]) -> Result<bool, FsError> {
    Err(FsError::new(
        ErrorCode::Unsupported,
        "Dragging files out requires Windows",
    ))
}

/// Shift currently held (used to turn an internal pane-to-pane drop into a move).
#[cfg(windows)]
pub fn shift_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    // SAFETY: GetAsyncKeyState has no preconditions; the high bit of the result means "down" now,
    // whatever thread the command runs on.
    unsafe { GetAsyncKeyState(VK_SHIFT as i32) < 0 }
}

#[cfg(not(windows))]
pub fn shift_down() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn count_limits() {
        assert!(check_count(0).is_err());
        assert!(check_count(1).is_ok());
        assert!(check_count(MAX_ITEMS).is_ok());
        assert!(check_count(MAX_ITEMS + 1).is_err());
    }

    #[test]
    fn empty_list_is_refused() {
        assert!(common_parent(&[]).is_err());
    }

    #[test]
    fn same_parent_is_accepted_case_insensitively() {
        let p =
            common_parent(&v(&["C:\\Data\\a.txt", "c:\\data\\b.txt", "C:\\Data\\sub"])).unwrap();
        assert_eq!(p, "C:\\Data");
    }

    #[test]
    fn different_parents_are_refused() {
        let e = common_parent(&v(&["C:\\Data\\a.txt", "C:\\Other\\b.txt"])).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidPath);
    }

    #[test]
    fn drive_root_is_refused() {
        assert!(common_parent(&v(&["C:\\"])).is_err());
    }

    #[test]
    fn too_many_items_are_refused_before_any_path_work() {
        let many = vec!["C:\\Data\\a.txt".to_string(); MAX_ITEMS + 1];
        assert!(common_parent(&many).is_err());
    }
}
