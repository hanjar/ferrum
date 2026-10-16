# Ferrum 0.2.0 — upgrade notes

For people moving from 0.1.0 (the phase-1 MVP or the RC1 build, both versioned 0.1.0) to 0.2.0. Windows 10/11 x64 only.
There is no auto-updater: you upgrade by running the new setup over the old install.

## Installing

- The only installer is the NSIS setup `Ferrum_0.2.0_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum` and asks for administrator rights once.
- **One installer family only.** The MSI of earlier builds uses the same identifier (`dev.ferrum.app`). If you installed an MSI, remove it in Windows Settings > Apps first, then run the NSIS setup. Builds before RC1 installed per user under `%LOCALAPPDATA%`: uninstall such a copy first. If you keep it, it still runs, but Disk Usage fast scan and the indexer service are never offered from a user-writable folder.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page against the file you downloaded (`Get-FileHash .\Ferrum_0.2.0_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- **WebView2** is installed by a download bootstrapper when it is missing, so that machine needs internet access during install. Windows 11 and current Windows 10 builds normally have it already.
- The installer copies the licence files and `THIRD-PARTY-NOTICES.txt` next to the app. Settings > "Third-party notices" opens the notice.
- Install, run, upgrade and uninstall of the setup are smoke-tested automatically on a CI runner (including the indexer service). The installer is still unsigned.

### Ferrum Indexer service (optional)

- The setup registers a Windows service named `FerrumIndexer` (automatic, delayed start) **only when the install folder is under Program Files**. A failing service step never aborts the Ferrum install, and Ferrum works without the service.
- On a fresh configuration the setup asks which mode to use: **High performance** (all files on fixed NTFS volumes are indexed, including file names in other accounts; for a PC only you use) or **Shared** (results are filtered by each user's permissions). Silent and passive installs choose Shared. You can force the mode with `/INDEXERMODE=high_performance` or `/INDEXERMODE=shared` on the command line. An existing valid configuration is never overwritten.
- Change the mode later in Settings > Indexer. The change asks for a native Windows confirmation.
- During an upgrade the setup stops the running service before replacing files and registers it again afterwards.

## Upgrading over an existing install

- Run the new setup. Your settings, session and Activity history stay in place (table below).
- If a file operation or archive job is running, finish or cancel it first: the installer replaces `ferrum.exe`.
- Downgrading is not supported: the Activity database is at schema v13 and an older build cannot read a newer one.

## Uninstalling

- Uninstall from Windows Settings > Apps, or run the uninstaller in the install folder. The setup stops and removes the `FerrumIndexer` service.
- The uninstaller has a **"Delete the application data"** checkbox. Leave it unchecked to keep your settings and history for a later reinstall (an upgrade never deletes data). Checked, a real uninstall also runs `ferrum-indexer.exe uninstall --purge-data`, which removes the indexer's files in `C:\ProgramData\Ferrum`, and also removes Ferrum's application data folders (the standard Tauri uninstaller behaviour for that checkbox).
- Per-folder comment files (`.ferrum\comments.json`) inside your own folders are never touched by the uninstaller.

## Your data

| Item                                                                        | Location                                                                | On upgrade                                                                                                                                                                                                |
| --------------------------------------------------------------------------- | ----------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Preferences and session (tabs, panes, appearance, preview, file operations) | `%APPDATA%\dev.ferrum.app\config.json`                                  | Read and migrated on load. A wrong-typed `preview.*` field resets only that field. If the file cannot be read, the app starts with in-memory defaults and logs a warning. Keep a copy if you may go back. |
| Directory Hotlist                                                           | `hotlist.json` in the same folder by default; the location can be moved | Kept. Command items stay approved only while their place, titles, program, arguments and hotlist file are unchanged; edited command items ask again. See `docs/hotlist.md`.                               |
| Activity history (Recent, Frequent, Favorites)                              | `activity.sqlite3` in the app-local data directory                      | Migrated forward (schema v13), no downgrade. Clear it from the Activity page. The first run shows a privacy disclosure that must be acknowledged before anything is recorded.                             |
| Per-folder comments                                                         | `.ferrum\comments.json` inside each folder                              | Unchanged. Renaming a file moves its comment; moving a file loses it.                                                                                                                                     |
| AI provider setup                                                           | Backend storage; the API key is in Windows Credential Manager           | The assistant stays off until you configure a provider. The endpoint is locked to your saved setup; changing its host asks for a native confirmation.                                                     |
| RAR tool (`Rar.exe`) path                                                   | Settings > Archive                                                      | Setting or changing it asks for a native confirmation, and its fingerprint is checked before every run. After upgrading from a build without this check, set the path again if RAR is not offered.        |

## Behaviour that differs from 0.1.0

- **Operations on administrative shares are refused.** Anything that changes files on `\\server\C$`, `ADMIN$` and similar shares (also through a drive mapped to one), or on a UNC path that points to this computer, is refused. Browsing and reading still work.
- **Ferrum's own folders are protected.** Its config, data and Activity folders cannot be the target of create, rename, delete, copy, move, extract or archive create.
- **Selection files** are saved only with the extension `.json` or `.ferrum-selection`.
- **`Rar.exe` location.** It must be in a folder other accounts cannot write to (for example `C:\Program Files\WinRAR`) and not on a network drive. If RAR creation disappears after upgrading, check the path in Settings > Archive.
- **AI endpoint.** Changing only the port of the saved host now asks for a native confirmation.

- **Shortcuts.** `Ctrl+D` is the Directory Hotlist, `Alt+D` edits the path, Deselect all is `Shift+-`, Disk Usage is `Alt+Shift+D`, and `Ctrl+Shift+L` is "Add to chat". Full list: `SHORTCUTS.md`.
- **Archives open as folders.** `Enter` or double-click on a `.zip`, `.7z` or `.rar` opens it read-only. `F5`/`F6` out of an archive extracts. `F5`/`F6` into an archive is an error: use `Alt+F5` to pack.
- **Opening risky files asks first.** Opening or printing an executable, script, shortcut or installer shows a native Windows confirmation with name, type and full path. Ordinary documents and folders open as before.
- **Starting an agent CLI asks first** for agents without their own trust prompt, and for Claude Code and Codex when the folder has hooks or MCP configuration.
- **Synchronize Dirs** takes a lock on both roots. If another Ferrum process is synchronising the same folders you get a message and nothing runs.
- **Quick View** previews PDF files and downsamples very large images. It still never hydrates cloud placeholders.
- **7z and RAR limits.** Compare and archive reads of a 7z are refused when a solid block or dictionary needs more than 256 MiB. Reading a RAR member above the caller's limit is an error, not a truncated read. Details: `docs/archive-support.md`.
- **Menus** are regrouped (View, Sort and Show are separate menus; Files has Archive, Checksums, Split & encode and Attributes & properties submenus). Pack to 7z is in the `Ctrl+K` palette.
- **Version label.** The title bar shows the version and short commit; Settings has a "Copy build info" button for bug reports.

## Not available in 0.2.0

RAR creation without your own WinRAR (permanent), archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Automation / Sync Profiles, Linux and macOS builds.
Fallbacks are in `SHORTCUTS.md`. Open problems are in `KNOWN-ISSUES.md`.
