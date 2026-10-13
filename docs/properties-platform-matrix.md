# Properties platform matrix (F2-21)

Scope: Alt+Enter Properties, Change Attributes, Ctrl+L Calculate Occupied
Space, Volume Label, System Information. Source of truth:
`src-tauri/src/properties.rs` (services), `src-tauri/src/ipc.rs` (commands),
`src/lib/components/PropertiesDialog.svelte` (UI).

Evidence legend: **verified (host)** = automated test run on the Windows 11
dev host; **by code** = `cfg`-gated code with a `cfg`-gated test that has NOT
been run on that OS (no Linux/macOS runner available, see D44); **n/a** =
not offered, the UI says so.

| Operation                            | Windows                                                                                                                 | Linux / macOS                                                                                        |
| ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Type, path, logical size, times      | supported, verified (host)                                                                                              | supported, by code (std `symlink_metadata`)                                                          |
| Allocated ("on disk") size           | real size via `GetCompressedFileSizeW`, verified (host)                                                                 | not reported: UI shows the logical size and says "allocated size not reported by this OS"            |
| Symlink / junction target            | shown, never followed; verified (host) only when the process may create symlinks (see below)                            | shown, never followed; by code                                                                       |
| Read-only / Hidden / System flags    | read from file attributes, verified (host)                                                                              | read-only from the write bits, hidden by dot-prefix convention, no System flag; by code              |
| POSIX permission mode                | not reported ("Not reported on this platform")                                                                          | reported as `rwx` + octal, by code (`cfg(unix)` test)                                                |
| Change Attributes (read-only/hidden) | supported, per-item preview + per-item errors, verified (host)                                                          | **n/a**: toggles disabled, UI states "Not supported on this platform"; backend returns `Unsupported` |
| Change permissions / timestamps      | n/a (not implemented on any platform)                                                                                   | n/a (not implemented; no cross-platform toggle is faked)                                             |
| Calculate Occupied Space             | supported, cancellable, progress events, verified (host)                                                                | supported, by code; allocated size = logical                                                         |
| Volume Label / info                  | local drive-letter volumes, verified (host; second drive only if present)                                               | `Unsupported` ("not supported for this path on this platform")                                       |
| UNC / POSIX-root paths for volume    | `Unsupported` with explanation (not a raw InvalidPath), verified (host)                                                 | `Unsupported`                                                                                        |
| System Information                   | Rust std: OS, arch, family, computer name (env); navigator: platform, language, CPU threads, memory, screen, user agent | same fields (host name only if `HOSTNAME` is exported)                                               |
| OS version / build number            | **not reported** (needs a platform API we do not link)                                                                  | **not reported**                                                                                     |

## Occupied Space behaviour

- Symlinks and reparse points are counted, never descended (D07), so loops
  cannot occur.
- Heavy work runs on the Rust blocking pool. `occupied_space(scanId, paths)`
  emits `properties://occupied-progress` (at most one event per 100 ms) and
  `occupied_space_cancel(scanId)` stops the walk; a cancelled walk still
  resolves with the partial totals and `cancelled = true`.
- Closing the dialog (Close, Escape, scrim click) or opening another dialog
  cancels a running walk; its late result is ignored.
- Per-entry failures (permission denied, vanished entries) are reported in
  `failed` and the walk continues.

## Test evidence (Rust `properties::tests`, vitest `PropertiesDialog`/`propertiesStore`)

Verified on the Windows host: large tree (6000 files in nested dirs, exact
totals, time-bounded), progress monotonicity, cancel mid-walk, pre-cancelled
token, scan registry, ACL-denied directory (icacls deny-read, skipped
explicitly if icacls cannot be applied), hidden+system flags, Unsupported
rendering in the dialog, Cancel / close-cancels in the dialog.

Environment-dependent tests skip **explicitly** (`SKIPPED:` on stderr) and are
never silent passes:

- Symlink tests need the symlink privilege (Windows developer mode or an
  elevated shell). Without it they print `SKIPPED` and pass vacuously; set
  `FERRUM_REQUIRE_SYMLINKS=1` to make that a failure (CI should do this on
  runners that can create links).
- The second-volume test needs a second drive letter; otherwise it prints
  `SKIPPED` and the UNC/`Unsupported` case is asserted instead.
- Unix-only tests (`posix_mode_bits_are_reported_on_unix`,
  `dot_files_report_hidden_on_unix`, `unreadable_directory_is_reported_*`) are
  compiled out on Windows and have not been executed on Linux/macOS.
