# Ferrum 0.2.4

Ferrum is a keyboard-centric dual-pane file manager for Windows, built with Tauri 2, Rust and Svelte 5. Licensed MIT OR Apache-2.0.

> **Pre-release, unsigned.** The installer is not code-signed, so Windows SmartScreen will show a warning. See "Unsigned installer" below before you install.

0.2.4 builds on [0.2.3](../v0.2.3/RELEASE-NOTES.md). Everything in 0.2.3 and earlier is still there; this page lists what is new.

## What is new

- **Drag files out of Ferrum.** Drag one or many files from a pane into a browser upload box (an e-mail attachment, a form), Explorer, the desktop or any other app. The drop is always a copy: the originals stay where they are.
- **Drop files onto Ferrum.** Files dragged from Explorer or another app are copied into the pane under the cursor. Between Ferrum's own panes, drag copies and Shift+drag moves, as before.
- **Shift+F4: new file.** As in Total Commander, Shift+F4 asks for a name, creates an empty file in the active pane, selects it and opens it in its default app. Also in the context menu and Ctrl+K.

Details: [CHANGELOG.md](CHANGELOG.md).

## Not active in this release

Code for scheduled and folder-watching automations is part of the build, but it is **not exposed**: the Automation screen is off in release builds (the sidebar entry is greyed out) and there is no way to create an automation, so nothing runs by itself. Sync profiles are manual: they run only when you press Run now.

## System requirements

- Windows 10 or 11, 64-bit (x64). Linux, macOS and ARM64 are not supported.
- Microsoft Edge WebView2 Runtime. If it is missing the setup downloads it, so that machine needs internet access during install.
- Administrator rights to install (the setup installs per machine to `C:\Program Files\Ferrum`).
- Optional: WinRAR 7.00 or newer, only if you want to create RAR archives.

## Install

1. Download `Ferrum_0.2.4_x64-setup.exe` from the assets below.
2. Check its SHA-256 (see "Unsigned installer").
3. Run it and accept the administrator prompt.
4. On a fresh configuration the setup asks how to run the optional indexer service (High performance or Shared). Silent installs (`/S`) choose Shared; pass `/INDEXERMODE=high_performance` or `/INDEXERMODE=shared` to choose.

### Unsigned installer

The installer and `ferrum.exe` are not code-signed. SmartScreen shows "Windows protected your PC" on first run.

1. Compare the SHA-256 of your download with the digest GitHub shows next to the asset:
   `Get-FileHash .\Ferrum_0.2.4_x64-setup.exe -Algorithm SHA256`
2. If they match, click **More info**, then **Run anyway**.
3. If they do not match, delete the file and download again. Do not run it.

## Upgrade

Run the new setup over the 0.2.3 (or older) install. Settings, session, Hotlist and Activity history are kept. Downgrading is not supported. Details: [UPGRADE-NOTES.md](UPGRADE-NOTES.md).

## Uninstall

Windows Settings > Apps > Ferrum > Uninstall. Tick **Delete the application data** only if you also want your settings and history removed; an upgrade never deletes data. Your own folders are never touched.

## Known issues (main ones)

- Not code-signed. Pre-release.
- Dragging out of Ferrum takes items from one folder at a time (up to 10,000) and is always a copy; a whole drive or share root cannot be dragged out.
- **Not manually verified before this release:** the cursor landing on the new file after Shift+F4 in the packaged window; and, from earlier releases, 7z Ultra (text) on large real-world text sets, AI actions against a real provider, Explorer clipboard round trips, Menu key / Shift+F10 / Ctrl+Tab in the packaged window, and the close guard with a hung window.
- Ferrum ships a patched copy of the `sevenz-rust2` library until upstream releases the fix; a valid 7z with more than about one million header entries is refused.
- Still open: a share that reaches this computer through its LAN IP or a hosts-file alias is not recognised as local; files with reserved device names cannot go to the Recycle Bin; RAR limits, 7z solid blocks over 256 MiB for preview, PDF preview limits, a moved file loses its comment, no auto-updater.

Full list: [KNOWN-ISSUES.md](KNOWN-ISSUES.md).

## More

- [CHANGELOG.md](CHANGELOG.md): what changed since 0.2.3, with pull request numbers.
- [UPGRADE-NOTES.md](UPGRADE-NOTES.md): upgrade from 0.2.3 and what happens to your data.
- 0.2.0 documents that still apply: [FEATURES.md](../v0.2.0/FEATURES.md), [SHORTCUTS.md](../v0.2.0/SHORTCUTS.md).
- Security reports: see [SECURITY.md](../../../SECURITY.md) (GitHub private advisories).
- Third-party licences: Settings > "Third-party notices", or `THIRD-PARTY-NOTICES.txt` in the install folder.
