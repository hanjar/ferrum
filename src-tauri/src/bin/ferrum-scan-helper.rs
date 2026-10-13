//! `ferrum-scan-helper.exe` — FER-460 elevated, read-only NTFS MFT reader.
//!
//! Launched by Ferrum via ShellExecuteEx `runas` (UAC) with exactly
//! `--volume X --pipe ferrum-scan-<hex> --nonce <hex>`; streams metadata
//! (names, sizes, parent links) over Ferrum's named pipe and exits. All
//! logic lives in `ferrum_ntfs_scan::helper` (crates/ntfs-scan) so it can be
//! reviewed and tested on its own; this file only adapts it to a binary.
//! No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    // FER-474 review I2: System32-only DLL search for this elevated binary.
    unsafe {
        windows_sys::Win32::System::LibraryLoader::SetDefaultDllDirectories(
            windows_sys::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    std::process::exit(ferrum_ntfs_scan::helper::run());
}

#[cfg(not(windows))]
fn main() {
    // Windows-only feature (D44); the helper refuses to do anything else.
    std::process::exit(2);
}
