# Ferrum 0.2.1

Ferrum is a keyboard-centric dual-pane file manager for Windows, built with Tauri 2, Rust and Svelte 5. Licensed MIT OR Apache-2.0.

> **Pre-release, unsigned.** The installer is not code-signed, so Windows SmartScreen will show a warning. See "Unsigned installer" below before you install.

0.2.1 builds on [0.2.0](../v0.2.0/RELEASE-NOTES.md). Everything in 0.2.0 is still there; this page lists what is new.

## What is new

- **Sync profiles** (new screen): the sidebar has a "Sync & Backup" section with a **Sync profiles** page. A profile is a saved pair of folders with a direction (Update, Mirror or Two-way), a delete policy, compare options and include/exclude filters. The page shows whether each profile is ready to run, lets you preview a sync (a read-only dry run) and run it on demand. A run that would overwrite files, or delete them to the Recycle Bin or to versions, asks you first. Permanent deletes ask through a native Windows dialog.
- **Sync never writes into Ferrum's own folders or the Windows Startup folder.** A profile whose folder is (or lies inside) Ferrum's config, data or Activity folders, or the per-user or all-users Startup folder, is shown as not ready and is refused at run time. Folders on a share that points back to this computer are refused too.
- **Safer overwrite in sync.** If you edit the target file while a long sync copy is running, sync now notices and leaves your edit alone instead of replacing it. When a run keeps old versions and the copy then fails or is cancelled, the old file is put back instead of being left missing.
- **More security hardening** (same family as 0.2.0):
  - The file manager no longer writes into the Windows Startup folder (copy, move, extract, archive create, split, combine, encode, decode, checksum, selection save). Reading, opening and deleting there still work. Renaming a folder to, into or above Startup is refused.
  - Copying or moving a file whose name is a Windows device name (for example `nul` or `aux.c`) now keeps its content. Such an entry cannot go to the Recycle Bin: Ferrum says so and you can delete it permanently.
  - `Rar.exe` behind a junction or symbolic link in any parent folder is refused (choose the real folder). A folder that gives other accounts write rights through an inherit-only entry is no longer trusted.
- **Smaller fix**: `Esc` no longer closes the delete dialog while a delete that holds the sync lock is still running, and the sync lock is released more reliably when the page reloads.

The per-feature status table of 0.2.0 ([FEATURES.md](../v0.2.0/FEATURES.md)) still applies. Sync profiles are new and have no entry there yet; treat them as Beta.

## Not active in this release

Code for scheduled and folder-watching automations is part of the build, but it is **not exposed**: the Automation screen is off in release builds (the sidebar entry is greyed out) and there is no way to create an automation, so nothing runs by itself. Sync profiles are manual: they run only when you press Run now.

## System requirements

- Windows 10 or 11, 64-bit (x64). Linux, macOS and ARM64 are not supported.
- Microsoft Edge WebView2 Runtime. If it is missing the setup downloads it, so that machine needs internet access during install.
- Administrator rights to install (the setup installs per machine to `C:\Program Files\Ferrum`).
- Optional: WinRAR 7.00 or newer, only if you want to create RAR archives.

## Install

1. Download `Ferrum_0.2.1_x64-setup.exe` from the assets below.
2. Check its SHA-256 (see "Unsigned installer").
3. Run it and accept the administrator prompt.
4. On a fresh configuration the setup asks how to run the optional indexer service (High performance or Shared). Silent installs (`/S`) choose Shared; pass `/INDEXERMODE=high_performance` or `/INDEXERMODE=shared` to choose.

### Unsigned installer

The installer and `ferrum.exe` are not code-signed. SmartScreen shows "Windows protected your PC" on first run.

1. Compare the SHA-256 of your download with the digest GitHub shows next to the asset:
   `Get-FileHash .\Ferrum_0.2.1_x64-setup.exe -Algorithm SHA256`
2. If they match, click **More info**, then **Run anyway**.
3. If they do not match, delete the file and download again. Do not run it.

## Upgrade

Run the new setup over the 0.2.0 install. Settings, session, Hotlist and Activity history are kept. Downgrading is not supported. Details: [UPGRADE-NOTES.md](UPGRADE-NOTES.md).

## Uninstall

Windows Settings > Apps > Ferrum > Uninstall. Tick **Delete the application data** only if you also want your settings and history removed; an upgrade never deletes data. Your own folders are never touched.

## Known issues (main ones)

- Not code-signed. Pre-release.
- Sync profiles: a share that reaches this computer through its LAN IP address or a hosts-file alias is not recognised as local, so it is not refused. Files with reserved device names cannot go to the Recycle Bin.
- Everything listed for 0.2.0 that is not fixed (RAR limits, 7z solid blocks over 256 MiB, PDF preview limits, a moved file loses its comment, no auto-updater).

Full list: [KNOWN-ISSUES.md](KNOWN-ISSUES.md).

## More

- [CHANGELOG.md](CHANGELOG.md): what changed since 0.2.0, with pull request numbers.
- [UPGRADE-NOTES.md](UPGRADE-NOTES.md): upgrade from 0.2.0 and what happens to your data.
- 0.2.0 documents that still apply: [FEATURES.md](../v0.2.0/FEATURES.md), [SHORTCUTS.md](../v0.2.0/SHORTCUTS.md).
- Security reports: see [SECURITY.md](../../../SECURITY.md) (GitHub private advisories).
- Third-party licences: Settings > "Third-party notices", or `THIRD-PARTY-NOTICES.txt` in the install folder.
