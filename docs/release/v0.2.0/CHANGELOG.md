# Ferrum 0.2.0 — changelog

Changes since the 0.1.0 RC1 build (`docs/release/rc1/`). Windows 10/11 x64 only. Entries refer to public pull requests
(`#n` on `hanjar/ferrum`). The repository's first public commit (`864d7d8`, "Initial public release of Ferrum (v0.2.0)")
is a snapshot of the development tree and has no pull request. Items that arrived with it are listed under
"Included in the initial public snapshot", each with the tests or documents that back it.

Everything in the RC1 [CHANGELOG](../rc1/CHANGELOG.md) is still part of the product. This file lists only what is new or changed.

## Features

- **Resizable file-list columns** (#9): Ext, Size, Modified and Comment can be dragged (pointer drag, Left/Right arrow keys on the handle, double-click resets). Widths are stored with the custom-columns setting.
- **Disk Usage** (#5, #14): resizable columns and a single row actions menu in the largest-files table. The focused folder follows the tree selection and the treemap shows its contents up to 3 levels deep (tiny items merged into "Other"). Clicking a block focuses it and selects it in the tree; double-click reveals it in the file pane.
- **Directory Hotlist editor** (#11): pointer-based drag and drop in the Configure tree (HTML5 drag and drop never fired on Windows), Into/Out of submenu buttons with `Ctrl+Right` / `Ctrl+Left`, `Ctrl+Up` / `Ctrl+Down` to move an item, foldable submenus.
- **Build information** (#7): the title bar and Settings show version, short commit and build date generated at build time. Settings has a "Copy build info" button. The version string reads 0.2.0 everywhere.
- **Third-party notices** (#18): a generated `THIRD-PARTY-NOTICES.txt` (components, SPDX licenses, license texts) is installed next to the app together with `LICENSE-MIT`, `LICENSE-APACHE` and `licenses/`. Settings has a "Third-party notices" button. CI fails when the notice is out of date.
- **Synchronize Dirs takes a cross-process lease** (#10): manual and chat-driven sync acquire a root lease before they re-validate and execute, and baseline saves require it. A second sync on the same roots is refused with a message instead of racing.
- **Installer**: NSIS only, per-machine, with the optional FerrumIndexer service and a "Delete the application data" uninstall checkbox (see `UPGRADE-NOTES.md`). The release workflow builds `--bundles nsis`.

### Included in the initial public snapshot (no pull request)

- **Quick View of PDF files** (`docs/quick-view-pdf.md`): pdf.js in a sandboxed iframe that gets no file path and no IPC access, 50 MiB cap, lazy page rendering. Tests: `PdfViewer.test.ts`, `pdfScale.test.ts`, `pdfBridge.test.ts`, `pdfOpen.test.ts`, `e2e/pdf-viewer.e2e.ts`. Real-window script: `scripts/smoke-pdf-preview.mjs`.
- **Large images in Quick View**: header-only limit checks (100 MP, 256 MiB file). Images above 4096 px or 16 MiB are downscaled in the backend. Media is served through the token-based `ferrum-media` protocol and the Tauri asset protocol is disabled.
- **Ferrum Indexer service** (`docs/indexer-service.md`): optional Windows service with an NTFS MFT/USN index, used by search and Disk Usage while it runs. Settings > Indexer shows status and mode.
- **Disk Usage fast scan** through an elevated helper (`ferrum-scan-helper.exe`, UAC prompt), offered only for a per-machine install.
- **AI assistant additions**: "Add to chat" (`Ctrl+Shift+L`) and "Ask AI agent about selection" (a read-only question through an installed agent CLI), plain-text extraction from DOCX/XLSX/PPTX/ODT/ODS/ODP for those two actions (`src-tauri/src/office_text.rs`), and an optional agent CLI terminal at the current folder (`docs/ai/agent-cli.md`). The assistant stays off until you configure a provider.
- **RAR**: creating RAR archives through your own WinRAR 7.00 or newer (`Rar.exe`, configured in Settings; Ferrum does not ship WinRAR), and listing of header-encrypted archives with a masked password prompt. See `docs/archive-support.md`.

## Security

- **Native confirmation before opening risky targets** (#4): Open, Open externally and Print resolve the target once through the shell (`SHParseDisplayName`) and launch the parsed item. Executables, scripts, shortcuts and installers need a native Windows confirmation that shows name, type and full path. Print of such a target is refused.
- **`Rar.exe` pinning, archive consistency and IPC hardening** (#3): the RAR tool path can be set only through a native confirmation, and its size, mtime and SHA-256 are re-checked before each run. ZIP, 7z and RAR listings share one linear-time name-conflict rule. The AI request always uses the stored endpoint, and changing its host needs a native confirmation. Indexer mode changes need a native confirmation. Mutating commands run the path guard.
- **Native confirmation before starting an agent CLI** (#15): the webview can no longer assert that a folder is trusted. Agents without their own trust prompt always ask. Claude Code and Codex ask when the folder has hooks or MCP configuration.
- **Archive extraction publish hardening** (#19): the destination root is opened without following links and its identity (volume serial and file id) is checked on the handle. Rollback of a failed extraction now really deletes what it created and never follows a planted junction.
- **Dependency audit** (#17): vitest 5 clears the dev-tool advisories that `npm audit` reported (0 vulnerabilities after the upgrade, according to the PR).
- **Workflows** (#16): explicit `contents: read` permissions and every action pinned to a commit SHA.

<!-- PENDING: FER-514 / FER-515 — hardening P3 (IPC name/path validation, protection of the config folder from file operations, Rar.exe bidi/TOCTOU/network-drive checks, AI endpoint origin with port). List the merged PR numbers here before the release. -->

## Fixes

- **Archives**: a 7z with explicit folder entries lists once instead of leaving the pane on "Loading"; a ZIP with explicit folder entries unpacks instead of failing with "Invalid archive member path"; `Enter` on a `.zip`, `.7z` or `.rar` enters it as a folder, like a double-click. These came with the initial public snapshot. Tests: `src-tauri/tests/archive_member_keys.rs`, and "Enter on a supported archive enters it ..." in `src/lib/actions.test.ts`.
- **Hotlist** (#11): moving items in the Configure tree, and putting an item into an empty submenu, now works.
- **Disk Usage** (#14): a block was labelled 1.0% when it was 99.8% of its parent. Labels are now relative to the focused folder.

## CI and tooling

- Rust toolchain pinned to 1.98.1 through `rust-toolchain.toml` (#1).
- Shell-open and root-canonicalisation tests no longer depend on the runner's 8.3 temp path or on `C:\temp` casing (#12).
- The Disk Usage end-to-end mock is deterministic: no `setTimeout` race, fail-fast wait (#20).
- vitest 5 (#17), generated third-party notices with a `--check` step in CI (#18), pinned workflow actions (#16).
- Docs: Automation spec (#2), public docs tidy-up (#16).

<!-- PENDING: FER-445 — NSIS install/upgrade/uninstall smoke in CI. -->
<!-- PENDING: FER-558 — stabilisation of the Playwright e2e suite. -->

## Not included in this release

- **Automation / Sync Profiles.** The engine pieces merged in #6, #8, #10 and #13 (profile plan, store, leases, run engine with delete policies) are backend code with unit and contract tests. There is no screen, no schedule, no folder watcher and no background runner in the 0.2.0 UI: nothing under `src/` outside tests imports the profile commands. Treat it as in development. Design: `docs/specs/automation/`.
- MSI installer, code signing, auto-update, Linux and macOS.
