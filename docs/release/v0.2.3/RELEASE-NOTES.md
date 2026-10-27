# Ferrum 0.2.3

Ferrum is a keyboard-centric dual-pane file manager for Windows, built with Tauri 2, Rust and Svelte 5. Licensed MIT OR Apache-2.0.

> **Pre-release, unsigned.** The installer is not code-signed, so Windows SmartScreen will show a warning. See "Unsigned installer" below before you install.

0.2.3 builds on [0.2.2](../v0.2.2/RELEASE-NOTES.md). Everything in 0.2.2 and earlier is still there; this page lists what is new.

## What is new

- **7z "Ultra (text)".** A new 7z level in the Pack dialog (Alt+U) uses PPMd, which usually makes noticeably smaller archives of text, logs, source code and CSV than Ultra. The AI panel can pick it too.
- **Safer with hostile 7z archives.** A crafted 7z can no longer crash Ferrum or make it allocate huge amounts of memory: header counts, coder memory (1 GiB per block) and encrypted-archive key rounds are checked first, and such archives are refused with an error.
- **Safer permanent delete.** Permanent delete opens each entry relative to its folder, never follows a link that appears during the delete, and checks it is still the same file before removing it. Cancel now stops in the middle of a large folder.

Details: [CHANGELOG.md](CHANGELOG.md).

## Not active in this release

Code for scheduled and folder-watching automations is part of the build, but it is **not exposed**: the Automation screen is off in release builds (the sidebar entry is greyed out) and there is no way to create an automation, so nothing runs by itself. Sync profiles are manual: they run only when you press Run now.

## System requirements

- Windows 10 or 11, 64-bit (x64). Linux, macOS and ARM64 are not supported.
- Microsoft Edge WebView2 Runtime. If it is missing the setup downloads it, so that machine needs internet access during install.
- Administrator rights to install (the setup installs per machine to `C:\Program Files\Ferrum`).
- Optional: WinRAR 7.00 or newer, only if you want to create RAR archives.

## Install

1. Download `Ferrum_0.2.3_x64-setup.exe` from the assets below.
2. Check its SHA-256 (see "Unsigned installer").
3. Run it and accept the administrator prompt.
4. On a fresh configuration the setup asks how to run the optional indexer service (High performance or Shared). Silent installs (`/S`) choose Shared; pass `/INDEXERMODE=high_performance` or `/INDEXERMODE=shared` to choose.

### Unsigned installer

The installer and `ferrum.exe` are not code-signed. SmartScreen shows "Windows protected your PC" on first run.

1. Compare the SHA-256 of your download with the digest GitHub shows next to the asset:
   `Get-FileHash .\Ferrum_0.2.3_x64-setup.exe -Algorithm SHA256`
2. If they match, click **More info**, then **Run anyway**.
3. If they do not match, delete the file and download again. Do not run it.

## Upgrade

Run the new setup over the 0.2.2 (or older) install. Settings, session, Hotlist and Activity history are kept. Downgrading is not supported. Details: [UPGRADE-NOTES.md](UPGRADE-NOTES.md).

## Uninstall

Windows Settings > Apps > Ferrum > Uninstall. Tick **Delete the application data** only if you also want your settings and history removed; an upgrade never deletes data. Your own folders are never touched.

## Known issues (main ones)

- Not code-signed. Pre-release.
- **Not manually verified before this release:** 7z Ultra (text) on large real-world text sets; and, still from 0.2.2, AI actions against a real provider, Explorer clipboard round trips, Menu key / Shift+F10 / Ctrl+Tab in the packaged window, and the close guard with a hung window.
- Ferrum ships a patched copy of the `sevenz-rust2` library until upstream releases the fix; a valid 7z with more than about one million header entries is refused.
- Still open: a share that reaches this computer through its LAN IP or a hosts-file alias is not recognised as local; files with reserved device names cannot go to the Recycle Bin; RAR limits, 7z solid blocks over 256 MiB for preview, PDF preview limits, a moved file loses its comment, no auto-updater.

Full list: [KNOWN-ISSUES.md](KNOWN-ISSUES.md).

## More

- [CHANGELOG.md](CHANGELOG.md): what changed since 0.2.2, with pull request numbers.
- [UPGRADE-NOTES.md](UPGRADE-NOTES.md): upgrade from 0.2.2 and what happens to your data.
- 0.2.0 documents that still apply: [FEATURES.md](../v0.2.0/FEATURES.md), [SHORTCUTS.md](../v0.2.0/SHORTCUTS.md).
- Security reports: see [SECURITY.md](../../../SECURITY.md) (GitHub private advisories).
- Third-party licences: Settings > "Third-party notices", or `THIRD-PARTY-NOTICES.txt` in the install folder.
