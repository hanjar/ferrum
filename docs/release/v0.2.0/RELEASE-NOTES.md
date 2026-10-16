# Ferrum 0.2.0

Ferrum is a keyboard-centric dual-pane file manager for Windows, built with Tauri 2, Rust and Svelte 5. Licensed MIT OR Apache-2.0.

> **Pre-release, unsigned.** The installer is not code-signed, so Windows SmartScreen will show a warning. See "Unsigned installer" below before you install.

## Highlights

- **Dual-pane workflow**: tabs, breadcrumbs, history, Directory Hotlist (`Ctrl+D`) with nested menus and drag-and-drop editing, Branch View, command palette (`Ctrl+K`), Total Commander style menus and shortcuts.
- **Safe file operations**: copy and move queue with progress and cancel, Recycle Bin by default, Multi-Rename with preview, checksums, split and combine, resizable columns.
- **Archives**: browse ZIP, 7z and RAR as read-only folders; pack to ZIP and 7z; extract through a fail-closed pipeline (zip-slip, reserved names, junctions, size and entry quotas). RAR is read with UnRAR; creating RAR works only with your own WinRAR (`Rar.exe`), which Ferrum does not ship.
- **Search, compare and sync**: progressive search, compare by content, compare directories, Synchronize Dirs with a required dry-run and a lock so two sync runs cannot touch the same folders.
- **Quick View** (`Ctrl+Q`): text, JSON, Markdown, images (large ones are downscaled), audio and video metadata, EXIF, and PDF (sandboxed pdf.js, Beta).
- **Disk Usage** (`Alt+Shift+D`): tree, nested treemap that follows your selection, largest files. Optional fast NTFS scan (UAC prompt).
- **Optional Ferrum Indexer service** (Beta): an NTFS index that speeds up search and Disk Usage.
- **Optional AI assistant** (Beta): off until you configure your own OpenAI-compatible provider. It builds a plan you approve; nothing runs before that. Optional "Add to chat" and "Ask AI agent" use a CLI you installed, and every agent start asks for confirmation when the folder could run programs.
- **Security**: Windows asks natively before Ferrum opens an executable, script or installer, before it starts an agent CLI, before it changes the `Rar.exe` path, the AI endpoint host or the indexer mode.
- **Personalisation**: six palettes, dark/light/system theme, font scale, density, icon style, private Activity history with full clear.

The full per-feature status (Stable / Beta / Unsupported) is in [FEATURES.md](FEATURES.md).

## System requirements

- Windows 10 or 11, 64-bit (x64). Linux, macOS and ARM64 are not supported.
- Microsoft Edge WebView2 Runtime. If it is missing the setup downloads it, so that machine needs internet access during install.
- Administrator rights to install (the setup installs per machine to `C:\Program Files\Ferrum`).
- Optional: WinRAR 7.00 or newer, only if you want to create RAR archives.

## Install

1. Download `Ferrum_0.2.0_x64-setup.exe` from the assets below.
2. Check its SHA-256 (see "Unsigned installer").
3. Run it and accept the administrator prompt.
4. On a fresh configuration the setup asks how to run the optional indexer service: High performance (your own PC; indexes all files on fixed NTFS volumes, including file names in other accounts) or Shared (results filtered by each user's permissions). Silent installs (`/S`) choose Shared; pass `/INDEXERMODE=high_performance` or `/INDEXERMODE=shared` to choose.

### Unsigned installer

The installer and `ferrum.exe` are not code-signed. SmartScreen shows "Windows protected your PC" on first run.

1. Compare the SHA-256 of your download with the digest GitHub shows next to the asset:
   `Get-FileHash .\Ferrum_0.2.0_x64-setup.exe -Algorithm SHA256`
2. If they match, click **More info**, then **Run anyway**.
3. If they do not match, delete the file and download again. Do not run it.

## Upgrade

Run the new setup over the old install. Settings, session, Hotlist and Activity history are kept. Remove an MSI from an earlier build in Windows Settings > Apps first (this release ships NSIS only). Downgrading is not supported. Details: [UPGRADE-NOTES.md](UPGRADE-NOTES.md).

## Uninstall

Windows Settings > Apps > Ferrum > Uninstall. The setup stops and removes the `FerrumIndexer` service. Tick **Delete the application data** only if you also want your settings and history removed; an upgrade never deletes data. Your own folders (including `.ferrum\comments.json` files) are never touched.

## Behaviour changes

- Operations that change files on administrative shares (`\\server\C$`, `ADMIN$`, also mapped drives) and on UNC paths pointing to this computer are refused. Browsing and reading still work.
- Ferrum's own config, data and Activity folders cannot be a target of file operations.
- Selection files are saved only as `.json` or `.ferrum-selection`.
- `Rar.exe` must be in a folder other accounts cannot write to (for example `C:\Program Files\WinRAR`) and not on a network drive.
- Changing only the port of the AI endpoint asks for a native confirmation.

## Known issues (main ones)

- Not code-signed. Install, upgrade and uninstall are smoke-tested on a CI runner, not on a machine without WebView2.
- RAR: no checksum or Compare By Content of a member, creation needs your own WinRAR 7.00+, and RAR tests are not run in CI.
- 7z: members of solid blocks needing more than 256 MiB of decoder memory are refused.
- PDF preview is Beta: no JPEG 2000 / JBIG2 pages, files over 50 MiB are not previewed.
- Moving a file loses its comment.
- No auto-updater.

Full list with workarounds: [KNOWN-ISSUES.md](KNOWN-ISSUES.md).

## Not in this release

Automation / Sync Profiles (an engine exists in the code, but there is no screen, schedule or background runner), MSI installer, code signing, Linux and macOS.

## More

- [CHANGELOG.md](CHANGELOG.md): what changed since RC1, with pull request numbers.
- [FEATURES.md](FEATURES.md): Stable / Beta / Unsupported matrix with evidence.
- [SHORTCUTS.md](SHORTCUTS.md): keyboard shortcuts and fallbacks.
- [UPGRADE-NOTES.md](UPGRADE-NOTES.md): install, upgrade, uninstall and data locations.
- Security reports: see [SECURITY.md](../../../SECURITY.md) (GitHub private advisories).
- Third-party licences: Settings > "Third-party notices", or `THIRD-PARTY-NOTICES.txt` in the install folder.
