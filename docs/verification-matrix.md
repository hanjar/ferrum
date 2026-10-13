# Verification matrix — MVP (task 15)

Host: Windows 11 (10.0.26200), x64, NVMe. WebView2 153. rustc 1.98.1 (msvc),
Node 22.14. **All numbers and pass/fail below are host-measured on this
machine only.** Linux and macOS rows are UNTESTED — no runner/machine was
available; claiming them would be dishonest. CI (`.github/workflows/ci.yml`)
runs the same gates on `ubuntu-latest` the moment this repo is pushed to
GitHub.

## Gates

| Gate                                       | Windows (this host)                           | Linux                    | macOS    |
| ------------------------------------------ | --------------------------------------------- | ------------------------ | -------- |
| `npm run format:check` / lint / check      | ✅ pass                                       | untested (CI configured) | untested |
| `npm run coverage` (TS logic ≥80%)         | ✅ 100% lines / ≥93% branches on scoped logic | untested                 | untested |
| `cargo fmt --check` / `clippy -D warnings` | ✅ pass                                       | untested (CI configured) | untested |
| `cargo test`                               | ✅ 50 passed                                  | untested                 | untested |
| `cargo llvm-cov` (Rust ≥80%)               | ✅ **82.6% lines** overall                    | untested                 | untested |
| `npm run test` (vitest)                    | ✅ 99 passed                                  | untested                 | untested |
| `vite build`                               | ✅                                            | untested                 | untested |
| `tauri build` (bundle MSI + NSIS)          | ✅ 3.15 MiB MSI / 2.20 MiB setup.exe          | untested                 | untested |
| `tauri dev` window + live listing          | ✅ verified                                   | untested                 | untested |

Coverage note: `src-tauri/src/ipc.rs` measures 0% — it contains only thin
`#[tauri::command]` shims delegating to the services (fs_service 91%,
copy_engine 85%, move_engine 82%, mutations 86%, ops 99%, domain 95%).
Driving those shims requires a live webview; they are exercised manually
below and covered indirectly by the service tests.

## Feature matrix (manual smoke, Windows host)

| Feature                                                                 | Result                                                                                                           |
| ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Listing real directory (unicode, hidden via dot/attr, symlink as entry) | ✅                                                                                                               |
| 10k / 100k directory listing & virtualized rendering                    | ✅ 163 ms / 2.9 s list; 29 DOM rows at 100k                                                                      |
| Dual pane navigation, tabs, back/forward/up, breadcrumbs                | ✅                                                                                                               |
| Selection (click/ctrl/shift/ctrl+A/arrows), sort, filter                | ✅                                                                                                               |
| Create file/folder, rename, delete → Recycle Bin, permanent delete      | ✅ (bin content check was visual-manual; automated test asserts source removal + no error)                       |
| Copy: large file + many small + recursive trees + conflicts             | ✅ 257 MB / 2001 files in 1.22 s (~270 MB/s); byte-identity asserted in tests                                    |
| Move: same-volume rename; cross-volume fallback                         | ✅ same-volume tested; **cross-volume path exercised only via forced-fallback tests — no second volume on host** |
| Queue: serial jobs, cancel queued/running, throttled progress           | ✅                                                                                                               |
| Shortcuts + command palette, keyboard-only flow                         | ✅                                                                                                               |
| Themes dark/light/system                                                | ✅ (screenshots `docs/screenshots/app-final-*.png`)                                                              |
| Trash/Recycle Bin                                                       | ✅ Windows; Linux trash spec / macOS Finder bin untested                                                         |
| Long paths (\\?\), UNC, junctions beyond symlink case                   | ⚠️ partial — UNC accepted by validation, not exhaustively smoked                                                 |
| Permission-denied paths                                                 | ✅ structured error surfaced per pane                                                                            |

## Security review (repo-level, task 15)

- IPC surface: 9 commands, every input validated (`validate_path` /
  `validate_name`); no path sandbox (by design, D00-asumsi), errors are coded
  `FsError`s — no panics cross the boundary (panic catch in the queue;
  command layer contains no `unwrap` on user input).
- No shell/exec APIs used anywhere (`grep -r "Command::new\|std::process"` →
  only test helper comments). No `eval`, no remote content: CSP set to
  `default-src 'self'` (styles inline for Svelte scoped CSS, fonts bundled
  locally via Fontsource).
- Secrets: `git log --all -p | grep -iE "api[_-]?key|token|password"` → none;
  repo contains no credentials.
- Tauri capability: `core:default` + `core:path:default` only — no shell,
  fs-plugin, or http permissions granted.

## Phase 2 legacy features (F2-25 / F2-27) — honest status

Host: Windows 11 (same machine). Everything below is covered by
unit/integration tests on this host unless marked otherwise; Linux/macOS
rows are UNTESTED (D01).

| Feature (registry id)                                           | Status                                        | Evidence / boundary                                                                                                                                                                                                                                    |
| --------------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Open external (`file.open` Enter on file)                       | implemented                                   | Windows ShellExecuteW `open`; argv-structured; unit tests for paths with spaces/Unicode/quotes/UNC; browser dev = error dialog                                                                                                                         |
| Associate With (`file.associate`)                               | implemented                                   | OS `openas` picker; Ferrum never writes associations; non-Windows = structured `unsupported`                                                                                                                                                           |
| Open Command Prompt / terminal here (`commands.prompt`)         | implemented                                   | structured argv (wt → cmd fallback); preference `auto/wt/cmd/powershell` in config.json; missing executable = `notFound` dialog; Linux/macOS list UNTESTED                                                                                             |
| Edit Comment (`file.comment`) + Comments view (`show.comments`) | implemented                                   | per-directory sidecar `.ferrum/comments.json` (D34); rename migrates (Rust-hooked); move loses the comment (documented)                                                                                                                                |
| Print (`file.print`)                                            | implemented                                   | OS print verb via ShellExecuteW; no custom renderer; non-Windows = `unsupported`                                                                                                                                                                       |
| Encode File (`file.encode`)                                     | implemented                                   | MIME Base64 only (RFC 4648 vectors, 76-col CRLF, `<name>.b64`); queued with progress/cancel                                                                                                                                                            |
| Decode File (`file.decode`)                                     | implemented                                   | base64 raw/MIME-header, UUE (uuencode(5) vector), XXE; BinHex refused with `unsupported`; outputs refuse to clobber                                                                                                                                    |
| Quit (`file.quit`)                                              | implemented                                   | closes the main window; confirmation dialog while queue operations are queued/running                                                                                                                                                                  |
| Thumbnail View (`show.thumbnails`)                              | implemented (media source changed in FER-452) | per-pane mode; images load through the token-based `ferrum-media` protocol (the Tauri asset protocol is off, see "Media protocol" below); lazy images, no dedicated Rust cache (webview cache only); image extensions only                             |
| Custom View Modes (`show.customModes`)                          | implemented                                   | named presets of existing per-pane view mode/sort/filter, localStorage, validated on load                                                                                                                                                              |
| Custom Columns Mode (`show.customColumns`)                      | implemented (FER-365)                         | opened D33: configures the existing SortKey-able columns (name/extension/size/modified) for Full mode; `columns.ts` engine + persisted preference + dialog; sorting by a hidden column falls back to Name — see the Windows-completeness section below |
| Separate Tree (`show.separateTree`)                             | implemented (FER-366, `085a91e`)              | per-pane drive tree panel above each pane's list (height-capped 40%), per-pane `TreeStore`, two-way pane navigation; shortcut-free by spec; merged on main (D49 follow-up fix `73ef511`)                                                               |
| Search in Separate Window (`commands.searchSeparate`)           | implemented                                   | one `search` WebviewWindow, `search://goto` bridge to the active pane; minimal capability additions; window lifecycle NOT host-verified (no E2E runner in gates)                                                                                       |
| Pack/Unpack/Test Archive (`file.pack/unpack/testArchive`)       | implemented                                   | Alt+F5 / Alt+F9 / Alt+Shift+F9 registered with `run`; real-window evidence in `scripts/smoke-archive.mjs` (FER-448), engine evidence in the Phase-2 sections below                                                                                     |
| Delete permanently (`file.delete.permanent`)                    | implemented (FER-364, `dcc52e7`)              | Shift+F8; the confirm dialog opens with the permanent checkbox pre-checked. No action in the registry uses `disabledReason` any more                                                                                                                   |

> **Status note (FER-444, main `161180f`, 2026-10-07):** the two "still
> disabled" rows that stood here are fixed above. Gate counts in the sections
> below (50 / 99 / 189 / 245 / 454 / 539 tests, "4 passed" and "11 passed" E2E)
> are **superseded historical snapshots**. Current gate numbers (2098 vitest,
> 524 Rust lib + 158 integration, 45 E2E at `388e6da`) are in
> [release/rc1/RC-REPORT.md](release/rc1/RC-REPORT.md).

## Known limitations (honest list)

1. Cross-OS claims: none made — Linux/macOS untested (see matrix).
2. Cross-volume move verified via forced-fallback tests, not a real second
   volume.
3. Headless scroll benchmark cannot measure input latency; worst handler
   time (0.72 ms) is the proxy metric.
4. Recycle Bin CONTENT verification was manual/visual.
5. Preview/search/compare/assistant UI from the prototype is intentionally
   post-MVP (decisions D09, backlog).

---

# Phase-2 verification (F2-28, 29 September 2026) — SUPERSEDED numbers

**Base:** merge of `main` (tc-workflow F2-14..24 + tech debt) with
`feature/archive-engine` (archive backend F2-03..11 + F2-12 UI substance) on
branch `feature/f2-28-verify-win`, worktree `ferrum-verify`. Same host as
above. **Linux and macOS remain UNTESTED** — same honesty rule as phase 1.

## Gates (merged state)

| Gate              | Command                                     | Result                                 |
| ----------------- | ------------------------------------------- | -------------------------------------- |
| TypeScript/Svelte | `npm run check`                             | 0 errors (20 pre-existing warnings)    |
| Frontend tests    | `npm run test` (vitest)                     | 454 passed / 0 failed                  |
| Frontend build    | `npm run build`                             | ✓                                      |
| Rust tests        | `cargo test`                                | 9/9 binaries ok, 189 passed / 0 failed |
| Rust lint         | `cargo clippy --all-targets -- -D warnings` | clean                                  |
| Rust format       | `cargo fmt --check`                         | clean                                  |
| E2E browser flow  | `npx playwright test`                       | 4 passed / 0 failed                    |

## Merge conflict survey (input for the integrator)

`git merge main` into `feature/archive-engine`: 10 files, 17 conflict
regions, resolved on this branch:

- `src-tauri/src/{lib,ipc,domain}.rs` — union of archive + tc-workflow
  commands/modules; domain fixture tests took main's `normalize_newlines`.
- `src-tauri/Cargo.toml` — superset union (archive crates + checksum/watch
  crates; windows-sys keeps the archive lane's larger feature set).
- `src/lib/actions.ts` — **shortcut collision caught by tests**: Ctrl+D was
  claimed by both `commands.hotlist` (main, F2-22, the TC convention) and
  `select.none` (engine branch). Deselect rebound to Shift+Minus; the
  shortcut-uniqueness test enforces this class of bug.
- `src/lib/components/{Dialogs,FilePane,PaneStatusBar}.svelte` and
  `src/lib/stores/{dialogStore,panesRegistry}.svelte.ts` — unions of dialog
  kinds, pane-API methods, status-bar props.

## Archive engine benchmarks

Harness: `src-tauri/tests/phase2_bench.rs` (re-runnable:
`cargo test --test phase2_bench --release -- --ignored --nocapture`).
Release build, NVMe host, wall clock; ±10% run-to-run:

| Operation                             | Time        | Notes                                          |
| ------------------------------------- | ----------- | ---------------------------------------------- |
| List ZIP, 10k members                 | 0.07 s      | central-directory read                         |
| List ZIP, 100k members                | 0.97 s      | central-directory read                         |
| Pack ZIP, 5,000 × 1 KiB (50 dirs)     | 0.95 s      | deflate, staging + atomic rename               |
| Extract ZIP, 5,000 × 1 KiB            | 5.53 s      | F2-03 no-follow publish pipeline (~1 ms/entry) |
| Pack ZIP, 200 MiB incompressible      | 4.30 s      | deflate on random data                         |
| Extract ZIP, 200 MiB                  | 1.28 s      | publish pipeline                               |
| Test ZIP (full decode + CRC), 200 MiB | 0.37 s      |                                                |
| Pack 7z, 5,000 × 1 KiB                | 8.14 s      | LZMA2                                          |
| Test 7z, 5,000 × 1 KiB                | 10.99 s     | per-entry decode overhead dominates            |
| Pack 7z, 200 MiB                      | 47.26 s     | single-threaded LZMA2                          |
| Test 7z, 200 MiB                      | 0.53 s      |                                                |
| Search `*.log`, 5,000-file tree       | 0.16–0.22 s | 5,000 hits; 30 s cap armed                     |
| Branch view, 5,000 entries            | 0.13–0.16 s | flattened subtree                              |

Follow-up observations (not blockers): many-small extraction pays ~1 ms of
handle-relative publish per entry; 7z pack dominates large-archive time.
**RAR: not benchmarked — no legally redistributable RAR fixture exists on
this host.** Startup time, resident memory, and idle CPU are now measured by
`scripts/measure-app.mjs` (baseline in `docs/perf/baseline-2026-10-06.md`, FER-446);
DOM rows are statically bounded by the FileList virtualizer
(`src/lib/virtual.test.ts`).

## Security audit — archive surface

Each threat maps to named regression tests (all green above):

| Threat                      | Mitigation                                                       | Evidence                                                                                              |
| --------------------------- | ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Zip-slip / traversal names  | preflight rejects `..`, absolute, drive, ADS; MemberPath grammar | `rejects_malicious_names_and_links_before_disk_write`, `create_rejects_traversal_and_missing_sources` |
| Symlink/junction escape     | handle-relative NtCreateFile publication, reparse-point refusal  | `junction_swapped_member_dir_is_refused_without_outside_writes`                                       |
| Decompression bomb / quota  | declared + runtime ceilings (QuotaWriter)                        | `enforces_metadata_and_runtime_quotas`, `trusted_ceilings_ignore_untrusted_maximums`                  |
| Partial files on failure    | staging commit + per-entry handle rollback                       | `cancel_midstream_removes_partial_file_via_handle`, `create_failure_leaves_no_partial_archive`        |
| Corrupt archive accepted    | full-decode CRC test, signature sniff                            | `test_zip_detects_passes_and_corruption`, `non_zip_signature_is_unsupported_format`                   |
| Unicode alias names         | decomposition rejection                                          | `rejects_unicode_decomposition_aliases`                                                               |
| Password handling (AES ZIP) | structured error codes                                           | **fixed this pass** (below); `test_zip_encrypted_fixture_password_matrix`                             |
| Command injection           | no shell execution on this branch                                | code audit of `archive_*`, `search`, `branch_view`                                                    |

`npm audit`: 3 moderate advisories, dev-tooling chain only (no runtime
dependency flagged). `cargo audit`: **not run — tool not installed on this
host**; left for the integrator/release step.

## Findings fixed during this verification

1. **Encrypted ZIP members produced raw decoder errors** — `ZipProvider`
   ignored `request.password` (RAR handled it; ZIP did not). Now:
   `read`/`extract`/`test` decrypt via `by_index_decrypt` and surface
   `PasswordRequired`/`BadPassword`; listing/extract metadata degrade to
   central-directory names for encrypted members instead of failing.
2. **Archive subdirectory navigation was broken in the merged UI** —
   `FilePane` never registered opened archives into `knownArchives`, so
   entering a folder inside an archive issued `list_dir` on a virtual path.
   Fixed; locked in by the Playwright flow (enter ZIP → navigate → nested
   members).
3. **Ctrl+D double-booking** between hotlist and deselect (see merge notes).
4. **E2E environment trap:** Playwright `reuseExistingServer: true` silently
   connected to a stale Vite dev server from another checkout
   on port 5173, serving outdated code. If E2E results look
   impossible, check what owns port 5173 before debugging the app.

## Remaining limitations (honest gaps)

- Linux/macOS: untested (no runner on this host).
- RAR: no legal fixture → list/extract/test/password untested on host.
- Startup/memory/idle CPU: baseline only (`docs/perf/baseline-2026-10-06.md`, one machine, no CI threshold).
- `cargo audit` not run (tool absent).
- 3 moderate npm dev-chain advisories open.
- Up-navigation out of an archive via the `..` row can issue `list_dir` on a
  virtual path (error toast); enter/activate flows are covered by E2E.

---

# Windows-completeness verification (FER-29, 29 September 2026) — SUPERSEDED numbers

Re-runs the Windows release gate at the tip of
`feature/win-view-completeness` (`affb9ca`, FER-365 custom columns +
FER-369 archive-parent watcher on top of main `8a26280`). Same Windows 11
host as above (10.0.26200, x64, NVMe, WebView2 153, rustc 1.98.1 msvc,
Node 22.14). **Linux and macOS remain UNTESTED — deferred per D44
(Windows-first); no cross-OS claims are made.**

## Gates (this branch, measured 29 Sep 2026)

| Gate              | Command                                     | Result                                                                                                                                                                                                                                                                       |
| ----------------- | ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript/Svelte | `npm run check`                             | 0 errors (19 pre-existing warnings)                                                                                                                                                                                                                                          |
| Frontend lint     | `npm run lint`                              | clean (dead E2E mock helpers removed in 677bc08 to keep this green)                                                                                                                                                                                                          |
| Frontend tests    | `npm test` (vitest)                         | 64 files / 539 passed / 0 failed                                                                                                                                                                                                                                             |
| Frontend build    | `npm run build`                             | ✓                                                                                                                                                                                                                                                                            |
| Rust format       | `cargo fmt --check`                         | clean                                                                                                                                                                                                                                                                        |
| Rust lint         | `cargo clippy --all-targets -- -D warnings` | clean                                                                                                                                                                                                                                                                        |
| Rust tests        | `cargo test`                                | 245 passed / 0 failed / 4 ignored (bench) across 9 binaries                                                                                                                                                                                                                  |
| E2E browser flow  | `npx playwright test`                       | 11 passed / 0 failed (twice consecutively; the very first invocation had one cold-start timeout in `shows sample.zip` that passed on immediate re-run — dev-server warm-up, not app behaviour). Suite is unchanged in substance from the 11/11 recorded at commit `3149c3a`. |

## Windows-completeness deltas verified here

- **Custom Columns (FER-365, opens D33):** `show.customColumns` is a real
  action; the Full mode listing renders the persisted column config
  (name always visible/first; extension/size/modified hideable+orderable;
  default = the built-in grid); malformed storage recovers to the default;
  sorting by a hidden column falls back to Name. Locked by
  `src/lib/columns.test.ts`, `src/lib/stores/columnsStore.test.ts`,
  `FileList.test.ts` custom-columns cases, and the registry tests.
- **Archive-parent watcher (FER-369, opens D43):** a pane inside an archive
  keeps an OS watch on the parent archive FILE (`watch_start` now accepts
  files; missing paths are refused). A change to the archive file
  state-preservingly refreshes the pane's virtual listing; sibling/disk
  events never do; subscriptions stop on navigation. Locked by the Rust
  file-watch tests (`watch_start_follows_a_single_file`,
  `watched_archive_file_ignores_sibling_changes`,
  `watch_start_rejects_missing_path`) and the
  `watcher archive-parent routing (FER-369)` vitest suite; the
  storm/device-disconnect/coalesce contracts are unchanged and green.

## Archive security posture (unchanged by this round, re-asserted)

The fail-closed extraction pipeline (F2-03) and its regression suite are
untouched by FER-365/FER-369 and stay green at this tip: preflight member
validation (traversal/absolute/drive/ADS/Unicode-alias rejection), symlink
and junction escape refusal, entry/size/depth/ratio quota ceilings, staging
commit with per-entry rollback and cancel cleanup, full-decode CRC test,
structured password errors. Named evidence (all passing in the cargo run
above): `rejects_malicious_names_and_links_before_disk_write`,
`junction_swapped_member_dir_is_refused_without_outside_writes`,
`enforces_metadata_and_runtime_quotas`,
`cancel_midstream_removes_partial_file_via_handle`,
`test_zip_detects_passes_and_corruption`,
`rejects_unicode_decomposition_aliases`. `cargo audit` still not run
(tool absent on this host).

## Benchmarks

Recorded in `docs/benchmarks.md` ("Phase-2 engine benches — FER-29,
29 September 2026"): all four `phase2_bench` tests pass; headline numbers
include list ZIP 100k members 0.85 s, extract ZIP 5k small files 4.65 s
(publish pipeline), pack 7z 200 MiB 58.48 s (single-threaded LZMA2),
search 5k-file tree 0.16 s, branch view 0.17 s. RAR remains unbenchmarked
(no legal fixture).

## Still open (honest gaps, unchanged)

- Linux/macOS rows: untested (deferred per D44).
- RAR on-host behaviour: untested (no legal fixture).
- ~~`file.delete.permanent`: still deferred~~ — enabled by FER-364 (`dcc52e7`).
  `show.separateTree` graduated (FER-366, `085a91e`).
- `cargo audit` not run; 3 moderate npm dev-chain advisories open.

## F2-16 Search: results panel, limits (FER-99 / FER-101)

- Result list is a keyboard listbox: Up/Down/Home/End move the cursor,
  Enter reveals the item in the active pane (`revealPath`), Escape stops a
  running search or closes the panel. **Feed to pane** sends the results to
  the active pane as a virtual listing (the Branch View row source, so
  sort/filter/selection/copy behave as in any pane; Ctrl+B or navigating
  returns to the folder). **Copy results** puts one full path per line on the
  clipboard.
- Unreadable directories/files are counted in `SearchStats.errors` (shown as
  "N unreadable") and skipped; the search never fails because of them. Rust
  tests: `unreadable_directory_is_skipped_and_search_continues` (real
  Windows ACL deny via `icacls`; the test is a no-op if the volume refuses
  the ACL), `locked_file_during_content_search_is_counted_not_fatal`.
- **Not supported: search inside archives.** The walker only traverses real
  directories; archive files (zip/7z/rar) are matched by name but never
  opened, and archive members are never reported. Needs a contract that
  streams `archive_*` listings into the same progress events; deferred.
- E2E: `e2e/search.e2e.ts` (Playwright, mock IPC). See the FER-101 note in
  the issue tracker for whether it ran on the host that produced a given commit.

## F2-21 Properties / attributes / occupied space / volume info

Per-platform supported/unsupported table, Occupied Space cancel/progress
contract and test evidence (with explicit skips) live in
[properties-platform-matrix.md](properties-platform-matrix.md).

---

# Changes since RC1 evidence (main `161180f`, 7 October 2026)

Source-of-truth for current gate numbers remains
[release/rc1/RC-REPORT.md](release/rc1/RC-REPORT.md). This section records what
landed on main afterwards. Same Windows host; Linux/macOS still untested.

## Media protocol (FER-452)

The Tauri asset protocol is **off** (`assetProtocol.enable=false`, the
`protocol-asset` feature dropped). Images, audio and video for thumbnails and
Quick View are served by the custom `ferrum-media` protocol
(`src-tauri/src/media_protocol.rs`): only image/audio/video by extension, via
random 128-bit tokens held in a backend allowlist (LRU 4096, 10 min idle TTL,
scope revoke), `nosniff`, `no-store`, HTTP Range (206/416). SVG travels inline
as a data URL. CSP opens only `img-src` and `media-src`. Hardening from review:
`74069e4` (L1-L3) and `495015a` (downloaded cloud files with the reparse
attribute are not blocked; `\??\` paths refused).

## Quick View effect loop (FER-454)

`FilePane`'s Quick View `$effect` re-ran itself (`previewStore.setTarget`
read and wrote the same state) until `effect_update_depth_exceeded`, seen
13-15x in `smoke-archive`. Fixed by running `setTarget` untracked (`664aabf`).

## Archive fixes (FER-449 / FER-450 / FER-451)

- FER-449 (`d6966e1`): 7z listing returns each folder once when explicit and
  implicit entries coexist; FER-451 (`1989d98`): ZIP extract accepts explicit
  folder entries ending in `/`; both locked by cross-provider tests (`317195e`).
- FER-450 (`6067009`, `f54c24a`): Enter on a supported archive enters it like
  a double-click.

## Perf baselines (FER-446 / FER-455)

- `scripts/measure-app.mjs` and `docs/perf/baseline-2026-10-06.md` (FER-446):
  startup median 1239.5 ms (p95 7022 ms, first run builds the WebView2
  profile), idle private 239.4 MB, +225 MB private after a 10k-file folder.
  One machine, other sessions active: order-of-magnitude baseline, not a
  threshold.
- FER-455 (`e0139ee`, `docs/perf/memory-large-folder-2026-10-07.md`): listing
  rows kept out of the `$state` deep proxy. 10k folder: renderer JS heap
  27.5 to 6.7 MB, private growth +62.7 to +12.9 MB (same-session
  comparison); startup and listing time unchanged within noise.

## Real-window smokes (Windows only, built `ferrum.exe`, real IPC)

- `scripts/smoke-archive.mjs` (FER-448): Enter on ZIP/7z, Alt+F9 unpack
  compared byte for byte, Alt+F5 pack parsed in Node, Alt+Shift+F9 test,
  Quick View of text/image members, corrupt archives. Quick View of image
  members exercises the `ferrum-media` path.
- `scripts/smoke-activity.mjs` and `scripts/ai-desktop-smoke*.mjs` cover the
  activity and AI surfaces. Smokes share the real `config.json`; never run
  two at once.
