# Ferrum 0.1.0 RC1 — release candidate report (FER-30 / F2-29)

Date: 2026-10-06. Base: `main` @ `388e6da` ("integration: merge fix/e2e-settings-pane-resize"). Worktree `ferrum-rc1`, branch `release/rc1`.
Host: Windows 11 Pro 10.0.26200 x64, 20 logical CPUs (i5-13600K), 63.8 GiB RAM, NVMe, 45 GB free on C: during the run. rustc 1.98.1 (msvc), cargo 1.98.1, Node 22.14.
Scope per decision D44: Windows only. Linux and macOS are untested and no claim is made. Nothing was merged, pushed, installed, signed or published.
The product code is unchanged; this branch adds only `docs/release/rc1/`.

Verdict: **RC1 is reviewable but not releasable as-is.** All gates are green after the fixes in section 1a (working tree, not yet committed). The installers are not install-tested or signed, and RAR evidence is thin (one fixture). Decisions needed are in section 8.

The sections below record the first run at 388e6da. Section 1a records the state after the FER-440/441/442 fixes.

## 1. Gate results at 388e6da

| Gate                              | Command                                                | Result                                                                                                                                                                                                                                                                                                                                                      |
| --------------------------------- | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Install                           | `npm ci`                                               | ok (319 packages; 3 moderate advisories, see section 4)                                                                                                                                                                                                                                                                                                     |
| TypeScript/Svelte                 | `npm run check`                                        | 671 files, **0 errors**, 22 warnings (a11y and unused CSS, pre-existing)                                                                                                                                                                                                                                                                                    |
| Lint                              | `npx eslint src e2e`                                   | clean (exit 0)                                                                                                                                                                                                                                                                                                                                              |
| Frontend tests                    | `npx vitest run`                                       | **165 files, 2098 passed, 0 failed**                                                                                                                                                                                                                                                                                                                        |
| Frontend build                    | `npm run build`                                        | ok                                                                                                                                                                                                                                                                                                                                                          |
| Rust format                       | `cargo fmt --check` (src-tauri)                        | **FAIL (exit 1)**: 3 hunks in `tests/sync_baseline_fs.rs` (KI-01, FER-440). Format only.                                                                                                                                                                                                                                                                    |
| Rust lint                         | `cargo clippy --lib --tests`                           | clean (exit 0, no warnings)                                                                                                                                                                                                                                                                                                                                 |
| Rust unit                         | `cargo test --lib`                                     | **524 passed, 0 failed, 1 ignored** (same result in every run, at least 3)                                                                                                                                                                                                                                                                                  |
| Rust integration                  | `cargo test --tests`                                   | **158 passed, 0 failed, 6 ignored (benchmarks)**: contract 6, create_test 26, extract 16, rar 25, sevenz 41, zip 33, sync_baseline_fs 8, sync_guarantees 3; `disk_usage_bench` 2 and `phase2_bench` 4 ignored                                                                                                                                               |
| E2E                               | `npx playwright test e2e`                              | **45 passed, 0 failed** in 18 s (disk-usage 11, archive-navigation 11, search 8, settings-tabs 6, pane-resize 4, appearance 3, multi-rename 2). Port 5173 was free (no stale dev server). Mock IPC in Chromium, not the Tauri shell.                                                                                                                        |
| Real-exe smoke                    | `node scripts/smoke-activity.mjs <release ferrum.exe>` | **34/34 PASS** in 18 s (first-run disclosure, Recent, palette history, Favorites, pause recording, Activity page 7/30/90 days, hide, forget, F2 rename, private session, real restart persistence, clear history). One check was vacuous: "Frequent unchanged after restart" had no Frequent rows. The script backs up and restores the real `config.json`. |
| Extra (not in the requested list) | `npx eslint .`, `npx prettier --check .`               | eslint: 2 errors in `scripts/ai-desktop-smoke-activity.mjs` (KI-03, FER-442); prettier: 434 files differ (198 with `--end-of-line auto`, KI-04, FER-443)                                                                                                                                                                                                    |

Rust totals: 524 + 158 = 682 passing tests. The 4 RAR fixture tests and `test_rar_with_real_fixture` skipped silently in this checkout because the untracked fixture is absent (KI-02, FER-441). To check, I copied the fixture temporarily into `contract-fixtures/` (gitignored), ran `cargo test --test archive_rar --test archive_create_test` (25 and 26 passed, no "skipping" lines), then removed it. The working tree stayed clean.

## 1a. Status after the gate fixes (FER-440, FER-441, FER-442)

Applied in the `ferrum-rc1` working tree on top of d516741; **not committed, not merged** (the commit and merge are left to the user). All commands exit 0.

| Task    | Change                                                                                                                                                                                                                                                                                                                                         | Result                                                                                                                                                                |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| FER-440 | `cargo fmt` over the whole crate. The first run understated the drift as 3 hunks because its output was truncated; the real drift was **26 files** (`src-tauri/src/activity/*`, `activity_service*.rs`, `ai/*`, `archive_create_test.rs`, `ipc*.rs`, `lib.rs`, `search.rs`, `sync_baseline.rs`, `tests/sync_baseline_fs.rs`). Formatting only. | `cargo fmt --check` clean; clippy, lib and integration tests unchanged                                                                                                |
| FER-441 | RAR tests that need the fixture now **panic** with an explicit message when the RAR fixture is missing. `FERRUM_ALLOW_MISSING_RAR_FIXTURES=1` allows a skip, which is printed as "SKIPPED (FERRUM_ALLOW_MISSING_RAR_FIXTURES=1): ...". Files: `src-tauri/tests/archive_rar.rs`, `src-tauri/tests/archive_create_test.rs`.                      | Without fixture and env: 1 failed (`test_rar_with_real_fixture`) and the rar tests panic; with env: 26 and 25 passed with 5 SKIPPED lines; with the fixture: all pass |
| FER-442 | Removed the unused `rowNames` helper in `scripts/ai-desktop-smoke-activity.mjs` (it held the useless escape). No behaviour change.                                                                                                                                                                                                             | `npx eslint .` clean                                                                                                                                                  |

Fixture: it is a personal RAR5 file, gitignored (`contract-fixtures/*.rar`).

Gate re-run (2026-10-06, RAR fixture present, main still 388e6da): `npm run check` 0 errors (22 warnings); `npx eslint .` clean; `npx vitest run` 165 files, 2098/2098; `npm run build` ok; `cargo fmt --check` clean; `cargo clippy --lib --tests` clean; `cargo test --lib` 524 passed, 1 ignored; `cargo test --tests` all ok (rar tests ran for real); `npx playwright test e2e` 45/45.

## 2. Build artefacts (local only, not committed, not installed, not signed)

Command: `npx tauri build` (2 min 27 s wall, release profile; bundle targets `all` produced MSI and NSIS). Directory: `src-tauri\target\release\`.

| Artefact    | Path (relative to the directory above)   | Size                     | SHA-256                                                            | Authenticode |
| ----------- | ---------------------------------------- | ------------------------ | ------------------------------------------------------------------ | ------------ |
| Release exe | `ferrum.exe`                             | 20,986,880 B (20.01 MiB) | `93A82FC9FA94688EB1629F1C5C5E33337DCE1BAEAB385857ED00633837FDF1B2` | NotSigned    |
| MSI         | `bundle\msi\Ferrum_0.1.0_x64_en-US.msi`  | 7,974,912 B (7.61 MiB)   | `9F45B2D76C1A4C58C4AC35E0FDA219395FAC553502D9718594C79756DEE06C61` | NotSigned    |
| NSIS setup  | `bundle\nsis\Ferrum_0.1.0_x64-setup.exe` | 5,794,596 B (5.53 MiB)   | `BF7F7D80B15CBD6504207CD192766DD7F71C46257FBE2442048E01E0DD19CE71` | NotSigned    |

Compared with the phase-1 record (3.15 MiB MSI, 2.20 MiB setup.exe) the installers are about 2.4x larger, consistent with the archive engines, UnRAR, SQLite and the AI/Activity code. The exe was patched by the bundler with its bundle type; the smoke ran against that exact file.
Not verified: install, uninstall, upgrade, a machine without WebView2 (the config downloads a bootstrapper), SmartScreen behaviour (KI-19, FER-445).

## 3. Benchmarks (release builds, this host, wall clock, +-10% run to run)

`cargo test --release --test phase2_bench -- --ignored --nocapture --test-threads=1`: 4 passed in 81 s.

| Operation                                           | Time                     |
| --------------------------------------------------- | ------------------------ |
| List ZIP, 10,000 / 100,000 members                  | 0.06 s / 0.55 s          |
| Pack ZIP 5,000 x 1 KiB / test / extract             | 0.80 s / 0.06 s / 4.11 s |
| Pack ZIP 200 MiB / test / extract                   | 3.62 s / 0.13 s / 0.38 s |
| Pack 7z 5,000 x 1 KiB / test                        | 6.55 s / 8.84 s          |
| Pack 7z 200 MiB / test                              | 46.02 s / 0.26 s         |
| Search `*.log` over 5,000 files / Branch view 5,000 | 0.15 s / 0.14 s          |

`node scripts/bench-activity.mjs --skip-1m` (Activity store, 100k entities, 1M episodes, DB 541.6 MiB): Frequent query warm p50/p95 3/7 ms; `open()` p50 0.42 ms, p95 0.92 ms; `record()` per navigation p50 4.2 to 5.9 ms, p95 5.7 to 9.1 ms. The 1k-entity baseline in the same run was noisier (record p95 up to 68 ms) and is disk-contention noise from running next to the other benchmarks, not a regression claim. The 1M-entity case was skipped (`--skip-1m`). The runner rewrites a tracked file, `docs/activity/benchmark-latest.txt`; I restored it so the branch has no code or benchmark-file change.
RAR was not benchmarked (no redistributable fixture). **Not instrumented anywhere in the repo:** application cold start, resident memory, idle CPU (KI-20, FER-446).

## 4. Security and audit

- `npm audit`: 3 moderate advisories, all in the dev chain `@vitest/mocker` -> `vitest` -> `@vitest/coverage-v8` (GHSA-82fw-gwwq-j7x9, path traversal in a vitest mock redirect). No runtime dependency is flagged. Not fixed here (`npm audit fix --force` would install vitest 5, breaking).
- `cargo audit`: **not run**, the tool is not installed (`cargo audit` -> "no such command"); not installed by this pass per instructions. Advisories for the Rust dependencies, including the vendored UnRAR C++ code, are unchecked (FER-447).
- Archive hostile-input suite is green in this run (`archive_extract` 16 tests, named in `docs/verification-matrix.md`). The FER-104 review closed 7z thread/memory, solid-read and name-check findings before merge.
- Tauri capabilities: `core:default`, `core:path:default`, create webview window and set-focus only; CSP `default-src 'self'` with `img-src` allowing `data:` and the asset protocol.
- Licence: RAR support uses UnRAR source with a restrictive licence; RAR creation is permanently excluded; the required licence text and legal review for redistribution remain open (see `docs/archive-support.md`).

## 5. What the evidence supports

Classification summary (full table in `FEATURES.md`):

- **Stable**: ZIP and 7z browse, extract, create, test and password read; safe extraction pipeline; copy/move/delete/rename and queue; Multi-Rename; Properties and Occupied Space (core); Hotlist, Branch View, checksums, split/combine, encode/decode, comments; Search (in folders); Compare by content and directories; Quick View of text, images and media metadata (and ZIP members); Disk Usage; menus, palette and shortcuts; Appearance; Settings; Activity (real-exe smoke).
- **Beta**: everything RAR (one fixture, no password/corrupt/multi-volume fixtures); 7z large solid extract; 7z member preview and compare; Synchronize Dirs panel; cross-volume move; symlink and second-drive properties; Open/Associate/Print/Terminal OS hand-off; theme "System" live and restart persistence; separate search window; AI assistant (not re-smoked); the installers.
- **Unsupported**: RAR creation (permanent), multi-volume RAR, archive update in place, copy into archive, encrypted archive creation, search inside archives, RAR member checksum and compare, PDF preview, large-image downsampling, BinHex decode, formats other than ZIP/7z/RAR, Linux and macOS.

## 6. Defect backlog opened by this pass (FER-155)

Searched issue tracker first; none duplicated an existing task. New tasks (project FER):

| Task    | Severity | Title                                                                                         |
| ------- | -------- | --------------------------------------------------------------------------------------------- |
| FER-440 | Medium   | `cargo fmt --check` red on main: `tests/sync_baseline_fs.rs`                                  |
| FER-441 | Medium   | RAR integration tests skip silently without the fixture and count as passed                   |
| FER-445 | Medium   | Install/uninstall/upgrade smoke of MSI and NSIS plus a signing decision (needs user decision) |
| FER-442 | Low      | `npm run lint` red: 2 errors in `scripts/ai-desktop-smoke-activity.mjs`                       |
| FER-443 | Low      | `format:check` (prettier) not green: 198 real drift files                                     |
| FER-444 | Low      | `docs/verification-matrix.md` stale ("still disabled" rows)                                   |
| FER-446 | Low      | Instrument startup time, memory and idle CPU                                                  |
| FER-447 | Low      | Install and run `cargo audit`; track the vitest advisory                                      |

Existing backlog cited in `KNOWN-ISSUES.md` rather than duplicated: FER-434 (RAR checksum), FER-435 (compare flake), FER-437 (7z solid O(n²)), FER-438 (archive read cancellation), FER-439 (document 7z/RAR behaviour), FER-422 (properties symlink/drive skips), FER-416/FER-419 (Occupied Space real-window check), FER-376 (appearance real-app checks), FER-103 and FER-104 (preview gaps and deferred review items).

## 7. Phase 3 plan (evidence-based, short)

Order follows what the evidence in this report shows, not the roadmap wish list.

1. **Make RC1 releasable (first, small).** Fix FER-440/442/443 (format gates), FER-441 (RAR skip becomes a failure in the release gate, and a legal RAR fixture is created), FER-444 (docs), FER-447 (cargo audit), FER-445 (install smoke on a clean machine and the signing decision). Evidence: these are the only reds and unknowns in section 1 and 2.
2. **Archive hardening backlog.** FER-437 (sequential 7z solid extract), FER-438 (cancellation token for preview and read), FER-439 (docs), FER-434 (streaming RAR reader or an explicit decision to keep it Unsupported). Evidence: all four came out of the FER-104 security review; two are DoS-class.
3. **Measurement.** FER-446 (startup, memory, idle CPU). Evidence: the only repeatedly "not instrumented" item in `verification-matrix.md`.
4. **Quick View gaps (FER-103).** PDF and large-image downsampling need a dependency or renderer decision (safe renderer, image crate). Evidence: the Unsupported rows in section 5; user input needed before design.
5. **Real-window evidence.** A small Tauri-driven smoke for archive flows (open ZIP/7z, Alt+F5, Alt+F9, Quick View on a member) in the style of `smoke-activity.mjs`, because Playwright uses mock IPC. Evidence: KI-09, KI-10 and the E2E caveat.
6. **Decide, do not plan yet.**
   - _Archive update in place:_ the ZIP writer already stages and renames atomically, and F5/F6 into an archive is a deliberate read-only error, but there is no usage evidence that people need it. Needs a product decision; not scheduled.
   - _Network providers (FTP/SFTP/cloud):_ no code, tests or tasks exist, and nothing in the RC evidence points to it. Not planned; revisit when a requirement is written.
   - _Plugin API:_ same, no evidence. Not planned.
   - _Linux and macOS:_ deferred by D44 until Windows is stable; the untested rows in `FEATURES.md` stay as they are until then.

## 8. Decisions needed from the user

1. Fix the red `cargo fmt --check` and the other gate reds now (FER-440/442/443), or accept them for RC1?
2. Signing and distribution of the installers (FER-445). Nothing was signed or published. Install-smoke on a clean machine or VM is needed before calling the installers verified.
3. Accept RAR as Beta, or require a legal RAR fixture (and `FERRUM_REQUIRE_RAR`-style failure instead of skip) before RC1 (FER-441)?
4. Install `cargo audit` (tool install on this host) to close the unchecked Rust advisories (FER-447)?
5. Phase 3 scope: confirm the order in section 7, and say whether archive update in place, network providers or a plugin API should get a requirement written.

## 9. Limitations of this report

Windows host only, one machine, one run per benchmark. Playwright runs against mock IPC. The AI assistant desktop smokes need a live provider and were skipped; their last recorded results are in `docs/ai/verification.md`. The one RAR fixture is a single RAR5 file with one PDF. App startup and memory were not measured. The installers were not installed. A developer shell with a real Recycle Bin check, a second drive and symlink privilege was not available.
