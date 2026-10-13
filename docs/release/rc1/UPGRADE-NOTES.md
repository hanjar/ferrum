# Ferrum 0.1.0 RC1 — upgrade notes

For people moving from the phase-1 MVP build (also versioned 0.1.0) to this RC. Windows only. There is no auto-updater. RC1 is a local build:
nothing has been published, signed or installed by the release pass.

## Installing

- Two installers exist: MSI `Ferrum_0.1.0_x64_en-US.msi` and NSIS `Ferrum_0.1.0_x64-setup.exe`. Both use the identifier `dev.ferrum.app` and the same version, so install only one. Neither was installed or upgrade-tested in the RC pass (KI-19).
- The installers are **not signed**; Windows SmartScreen will warn. Signing is tracked in FER-445.
- **NSIS now installs per machine (FER-460).** `Ferrum_0.1.0_x64-setup.exe` asks for admin once and installs to
  `C:\Program Files\Ferrum`, like the MSI. Earlier NSIS builds installed per user under `%LOCALAPPDATA%`. Uninstall
  such an old per-user copy first. If you keep it, it still runs, but the Disk usage fast scan is never offered,
  because the elevated `ferrum-scan-helper.exe` is used only from Program Files: a user-writable folder would let any
  program swap the helper before you approve the UAC prompt.
- WebView2 comes from a download bootstrapper (`webviewInstallMode: downloadBootstrapper`). A machine without WebView2 needs network access during install.
- The portable `ferrum.exe` (`src-tauri\target\release`) runs without installing and was smoke-tested (34/34).

## Your data

| Item                                                                                | Location                                              | On upgrade                                                                                                                                                                                                                                                                              |
| ----------------------------------------------------------------------------------- | ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Preferences and session (tabs, panes, appearance, preview, file-operation settings) | `%APPDATA%\dev.ferrum.app\config.json`                | Read and migrated on load. A wrong-typed `preview.*` field resets only that field. If the file cannot be read the app starts with in-memory defaults and logs a warning. Keep a copy first if you want to return to the MVP build.                                                      |
| Activity history (Recent, Frequent, Favorites)                                      | `activity.sqlite3` in the app-local data directory    | Created on first run; the schema is at v13 and migrates forward. There is no downgrade: an older build cannot read a newer database. Clear history from the Activity page to remove it. The first run shows a privacy disclosure that must be acknowledged before anything is recorded. |
| Per-folder comments                                                                 | `.ferrum\comments.json` inside each folder            | Unchanged. Renaming a file moves its comment; moving a file loses it.                                                                                                                                                                                                                   |
| AI provider setup                                                                   | kept by the backend, not in browser storage (FER-430) | The assistant stays off until you configure a provider.                                                                                                                                                                                                                                 |

## Behaviour that differs from the MVP

- **Shortcuts.** Ctrl+D is the Directory Hotlist. Deselect all is Shift+Minus. Full list in `SHORTCUTS.md`.
- **Archives open as folders.** Enter on a `.zip`, `.7z` or `.rar` opens it read-only. F5/F6 out of an archive extracts; F5/F6 into an archive is an error, so use Alt+F5 to pack.
- **Delete permanently** (Shift+F8) is enabled; the confirm dialog opens with the permanent option pre-checked.
- **7z and RAR limits (KI-14).** Compare and archive reads of a 7z are refused when a solid block or a codec dictionary needs more than 256 MiB ("solid block/codec memory too large"). Reading a RAR member whose declared size is above the caller's limit is now an error instead of a truncated read. Quick View and compare of a 7z decode on a single thread by design. Quick View and `archive_read` of a member can be cancelled (`archive_cancel_request`), and extracting a solid 7z is now sequential (FER-437/438/439). Details: `docs/archive-support.md`, section "P2 - hardening arsip".
- **Menus** were regrouped: View, Sort and Show are separate menus; Files has Archive, Checksums, Split & encode and Attributes & properties submenus (FER-429, FER-431). Pack to 7z is only in the Ctrl+K palette.
- **Search** results open in a panel with keyboard navigation; archives are matched by name only.
- **Pack, Unpack and Test** need a selection; Unpack also needs the second pane.

## Not available in RC1

RAR creation (permanent, licence), archive update in place, encrypted archive creation, multi-volume archives, search inside archives, PDF preview, Linux and macOS builds.
Fallbacks are in `SHORTCUTS.md`. Open problems with repro steps are in `KNOWN-ISSUES.md`.

## Docs drift

`docs/verification-matrix.md` still shows some phase-2 features as "disabled" (KI-06). Treat `FEATURES.md` and `RC-REPORT.md` as current.
