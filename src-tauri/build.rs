fn main() {
    // unrar_sys's pathfn.o references the Windows Registry API on MSVC
    // (GetRarDataPath); advapi32 is not in unrar_sys link metadata, so we
    // link it here to satisfy the final binary link.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
    // FER-463 I1: the elevated scan helper resolves its static imports from
    // System32 only (LOAD_LIBRARY_SEARCH_SYSTEM32 = 0x800 in the PE load
    // config), so no DLL next to it or in a user-writable PATH entry is
    // loaded before `main` runs SetDefaultDllDirectories.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=ferrum-scan-helper=/DEPENDENTLOADFLAG:0x800");
    }
    tauri_build::build()
}
