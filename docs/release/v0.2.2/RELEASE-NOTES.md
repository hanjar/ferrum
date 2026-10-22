# Ferrum 0.2.2

Ferrum is a keyboard-centric dual-pane file manager for Windows, built with Tauri 2, Rust and Svelte 5. Licensed MIT OR Apache-2.0.

> **Pre-release, unsigned.** The installer is not code-signed, so Windows SmartScreen will show a warning. See "Unsigned installer" below before you install.

0.2.2 builds on [0.2.1](../v0.2.1/RELEASE-NOTES.md). Everything in 0.2.1 and 0.2.0 is still there; this page lists what is new.

## What is new

- **Copy and paste files with Windows Explorer.** Ctrl+C / Ctrl+X in a pane puts the selected files on the Windows clipboard and Explorer can paste them; Ctrl+V pastes what you copied in Explorer into the active folder, with the usual conflict dialog, progress and safety checks.
- **Open a file inside an archive.** Press Enter on a member of an archive and Ferrum unpacks just that file to a temporary folder and opens it. Your edits are not written back into the archive.
- **More from the AI panel.** It can create 7z archives with a compression level, go to a path you type, and delete to the **Recycle Bin only** (never permanently, and always after a confirmation card). A request can be a short chain, for example "archive this as 7z, then delete the original": the delete runs only after the archive was created and tested. When the provider refuses a request you now see its own message, and size questions point you to Disk Usage.
- **Keyboard-first dialogs and menus.** Every dialog traps Tab, closes with Esc and gives focus back; buttons and fields have underlined Alt access letters; the menu bar opens with Alt+F, Alt+M, Alt+C, Alt+V, Alt+S and Alt+W; the Menu key / Shift+F10 opens the context menu on the cursor row; Ctrl+Tab and Ctrl+Shift+Tab switch pane tabs. Enter on a focused button now presses that button, so Enter on Cancel can no longer delete or start a pack.
- **Recent commands.** The empty Ctrl+K palette lists the last three commands you ran.
- **Background jobs.** Alt+B opens the operations panel with a dock of background jobs and a badge in the title bar. Progress dialogs have a **Background** button (or press Esc) so you can keep working. Checksum verify and hashing an archive member can run in the background too.
- **Large deletes no longer freeze Ferrum.** Delete runs as a job with progress and a Cancel button. Cancelling a permanent delete stops between top-level items.
- **Close guard.** The X button and Alt+F4 ask first while jobs are running.
- **Extract never replaces files silently.** A file that appears at the destination while extracting is skipped or renamed, and replaced only when you chose Overwrite.
- **Disk Usage starts from the active pane.**

The per-feature status table of 0.2.0 ([FEATURES.md](../v0.2.0/FEATURES.md)) still applies. The new features above have automated tests but were not all checked by hand before release; see "Known issues".

## Not active in this release

Code for scheduled and folder-watching automations is part of the build, but it is **not exposed**: the Automation screen is off in release builds (the sidebar entry is greyed out) and there is no way to create an automation, so nothing runs by itself. Sync profiles are manual: they run only when you press Run now.

## System requirements

- Windows 10 or 11, 64-bit (x64). Linux, macOS and ARM64 are not supported.
- Microsoft Edge WebView2 Runtime. If it is missing the setup downloads it, so that machine needs internet access during install.
- Administrator rights to install (the setup installs per machine to `C:\Program Files\Ferrum`).
- Optional: WinRAR 7.00 or newer, only if you want to create RAR archives.

## Install

1. Download `Ferrum_0.2.2_x64-setup.exe` from the assets below.
2. Check its SHA-256 (see "Unsigned installer").
3. Run it and accept the administrator prompt.
4. On a fresh configuration the setup asks how to run the optional indexer service (High performance or Shared). Silent installs (`/S`) choose Shared; pass `/INDEXERMODE=high_performance` or `/INDEXERMODE=shared` to choose.

### Unsigned installer

The installer and `ferrum.exe` are not code-signed. SmartScreen shows "Windows protected your PC" on first run.

1. Compare the SHA-256 of your download with the digest GitHub shows next to the asset:
   `Get-FileHash .\Ferrum_0.2.2_x64-setup.exe -Algorithm SHA256`
2. If they match, click **More info**, then **Run anyway**.
3. If they do not match, delete the file and download again. Do not run it.

## Upgrade

Run the new setup over the 0.2.1 (or 0.2.0) install. Settings, session, Hotlist and Activity history are kept. Downgrading is not supported. Details: [UPGRADE-NOTES.md](UPGRADE-NOTES.md).

## Uninstall

Windows Settings > Apps > Ferrum > Uninstall. Tick **Delete the application data** only if you also want your settings and history removed; an upgrade never deletes data. Your own folders are never touched.

## Known issues (main ones)

- Not code-signed. Pre-release.
- **Not manually verified before this release:** AI actions against a real provider; AI delete on volumes with a tiny Recycle Bin or "no Recycle Bin"; Explorer clipboard round trips; opening a file inside a password-protected RAR; Menu key, Shift+F10 and Ctrl+Tab in the packaged window; the close guard with a hung window. The Windows "too big for the Recycle Bin" prompt may appear behind Ferrum, and cancelling a permanent delete takes effect only between top-level items.
- Still open from 0.2.1: a share that reaches this computer through its LAN IP or a hosts-file alias is not recognised as local; files with reserved device names cannot go to the Recycle Bin; RAR limits, 7z solid blocks over 256 MiB, PDF preview limits, a moved file loses its comment, no auto-updater.

Full list: [KNOWN-ISSUES.md](KNOWN-ISSUES.md).

## More

- [CHANGELOG.md](CHANGELOG.md): what changed since 0.2.1, with pull request numbers.
- [UPGRADE-NOTES.md](UPGRADE-NOTES.md): upgrade from 0.2.1 and what happens to your data.
- 0.2.0 documents that still apply: [FEATURES.md](../v0.2.0/FEATURES.md), [SHORTCUTS.md](../v0.2.0/SHORTCUTS.md).
- Security reports: see [SECURITY.md](../../../SECURITY.md) (GitHub private advisories).
- Third-party licences: Settings > "Third-party notices", or `THIRD-PARTY-NOTICES.txt` in the install folder.
