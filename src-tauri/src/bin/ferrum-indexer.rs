//! `ferrum-indexer.exe` — FER-474 `FerrumIndexer` Windows service (IDX-S1).
//!
//! Started by the SCM as LocalSystem (no arguments), or run by the
//! installers with `install` / `uninstall` / `stop`, by Ferrum with
//! `set-mode` (elevated, UAC), and by developers with `--console`. All logic
//! lives in `ferrum_indexer` (crates/indexer) so it can be reviewed and
//! tested on its own; this file only adapts it to a binary. Console
//! subsystem on purpose: the installers and the console mode read its
//! output; the SCM and hidden launches show no window.

#[cfg(windows)]
fn main() {
    // Review I2: System32-only DLL search before anything else runs.
    ferrum_indexer::win::harden_dll_search();
    std::process::exit(ferrum_indexer::cli::main(env!("CARGO_PKG_VERSION")));
}

#[cfg(not(windows))]
fn main() {
    // Windows-only feature.
    std::process::exit(2);
}
