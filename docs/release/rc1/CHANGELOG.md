# Ferrum 0.1.0 RC1 — changelog

Release candidate built from `main` @ `388e6da` (2026-10-06). Windows only. The product version string is still `0.1.0`
(`package.json`, `src-tauri/Cargo.toml`, `tauri.conf.json`); RC1 is the phase-2 state on top of the phase-1 MVP
(`docs/verification-matrix.md`, "MVP"). Entries come from `git log` on `main` between 2026-09-28 and 2026-10-06 and are grouped by area.
Stability labels are the ones in `FEATURES.md`. Nothing here is published, signed or installed. Task numbers are tracking keys (project FER).

## Archives (ZIP, 7z, RAR read-only)

- Open `.zip`, `.7z` (and `.7zip` when the signature matches) and `.rar` as read-only virtual folders; navigate subfolders; tab history stores the full archive location. Stable for ZIP and 7z, Beta for RAR.
- Fail-closed extraction pipeline shared by all formats: preflight on member names (traversal, absolute, drive, ADS, reserved Windows names, case and Unicode aliases), quotas (10k entries, 2 GB total, 512 MB per entry, depth 32), handle-relative no-follow publication, staging with rollback and cancel cleanup, one-ask conflict dialog (FER-368). Stable.
- Pack to ZIP (Alt+F5) and 7z (Ctrl+K palette), unpack to the other pane (Alt+F9), test archive (Alt+Shift+F9: full decode, nothing written). The destination conflict policy is honoured when creating (FER-411).
- Password prompt with retry for encrypted ZIP (AES) and 7z. Wrong password and corrupt archive are reported differently.
- Quick View of an archive member without extracting (FER-104): bounded read, no temp file, cloud placeholders are not hydrated.
- Compare by content with archive members (ZIP/7z, FER-413) and checksum of archive members (FER-133).
- A pane showing an archive refreshes when the archive file changes (FER-369).
- Fixes from the FER-104 security review: 7z solid members read correctly; single-threaded 7z decode (a 156 KB crafted archive peaked at 1235 MB and now peaks at 13 MB); summed codec memory limit; member name cross-check.
- Not included: RAR creation (licence), archive update in place, multi-volume archives, encrypted archive creation, search inside archives.

## Total Commander workflow

- Action registry with 101 actions, six menus (Files, Mark, Commands, View, Sort, Show) with submenus, the Ctrl+K palette and a bottom key bar. 47 actions have shortcuts (`SHORTCUTS.md`).
- Mark: select/unselect group, invert (`*`), same extension (`+`), save/restore selection, selection to/from file, copy names, paths or details, TC Num-key chords (FER-367).
- Show/View: Brief, Full, Tree, Thumbnail view, Custom View Modes, Custom Columns (FER-365), Separate Tree per pane (FER-366), Comments column, vertical arrangement, resizable dual-pane divider (FER-406).
- Search (Alt+F7, Ctrl+F): progressive results, keyboard listbox, feed to pane, copy results, 30 s cap (FER-99/101); search in a separate window.
- Quick View (Ctrl+Q): text, JSON, Markdown, images, audio/video metadata, EXIF; preferences for read limit, highlighting and autoplay (FER-396).
- Compare by content with text diff and encodings, Compare Directories (Shift+F2), mark newer and hide same; verdict cache; cancel and progress (FER-106..109).
- Synchronize Dirs with a required dry-run, a persisted per-pair baseline, no timestamp winner, conflict decisions, delete-extras and content mode (FER-111/112/212/414).
- Multi-Rename Tool (Ctrl+M) with preview (FER-117).
- Properties, Change Attributes, cancellable Calculate Occupied Space (Ctrl+L), Volume Label, System Information.
- Directory Hotlist (Ctrl+D), CD Tree, Source/Target controls, Branch View (Ctrl+B).
- Directory Hotlist is nested (submenus, separators, accelerators, command items), stored in `hotlist.json` at a location you can move, with import from Total Commander. See `docs/hotlist.md` (FER-458).
- Hotlist hardening (FER-467): command items are confirmed in a native Windows dialog, and the approval is tied to the item's place, titles, program, arguments and hotlist file; saves are flushed and checked on a locked handle; custom locations on network drives, behind junctions, with reserved names or streams are refused; network folders are tagged and ask once per host; opening files outside Ferrum only accepts existing drive or share paths.
- Checksum create and verify with progress and cancel, Split and Combine with staging, Encode and Decode (Base64, UUE, XXE).
- Open with the associated application, Associate With, Print, Open Terminal Here, Edit Comment, Quit with confirmation.
- Filesystem watcher auto-refresh and the Background Transfer Manager (Alt+B).
- Delete permanently (Shift+F8) enabled after the dialog initial-state fix (FER-364).
- Deselect all moved to Shift+Minus because Ctrl+D belongs to the hotlist.

## Other features since the MVP

- Disk Usage workspace (Alt+Shift+D): parallel scanner, tree, treemap, top files, dual-panel layout (DSK series, FER-319..409).
- Appearance: six palettes, font scale S/M/L/XL, density, icon style, full Settings dialog with category navigation (FER-370..376).
- Settings: tabs (position, session restore, fixed width, middle-click), File Operations preferences, Preview preferences.
- Activity: local Recent, Frequent and Favorites in the sidebar and palette, Activity page, privacy disclosure and controls, private session, forget and clear (ACT series, FER-236..276). Frequent query p95 on 100k entities / 1M episodes went from about 2855 ms to 7 ms (schema v10).
- AI assistant (opt-in, needs your own provider): ZIP from pane items, two-way sync with preview and approval, follow-up edits, Activity questions. Beta.

## Fixes worth knowing

- Encrypted ZIP members returned raw decoder errors; they now report `PasswordRequired` or `BadPassword` (F2-28).
- Opened archives were not registered, so entering a folder inside an archive listed a virtual path on disk (F2-28).
- Copy progress now reports the final file so completed jobs show real counts (FER-382).
- File-operation dialogs take focus on open so Escape works (FER-385).
- A preview preference with a wrong type resets only that field instead of every preference (FER-103).
- Activity: the disclosure acknowledgement is fail-closed in the renderer; visit writes are queued; a corrupt database is recovered.

## Gate state of this RC

Details and commands in `RC-REPORT.md`: vitest 2098/2098, Rust lib 524 passed, Rust integration 158 passed (6 benchmarks ignored), Playwright 45/45 (mock IPC),
real-exe smoke 34/34, `cargo fmt --check` red (KI-01), MSI 7.61 MiB, NSIS 5.53 MiB, exe 20.0 MiB.
