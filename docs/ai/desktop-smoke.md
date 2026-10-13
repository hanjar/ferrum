# AI assistant — desktop smoke (opt-in)

`scripts/ai-desktop-smoke.mjs` drives the **real Tauri window** through WebView2 remote debugging (CDP) and
exercises the ZIP workflow end to end with the configured provider. It is not part of `npm test` because it needs
a running app, a reachable provider and the key in Windows Credential Manager. Introduced for FER-165 / AI-08.

## Run

```powershell
# 1. start the app with remote debugging
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'
npx tauri dev

# 2. provider setup: see docs/ai/provider.md (endpoint + model via VITE_FERRUM_AI_ENDPOINT / VITE_FERRUM_AI_MODEL
#    in .env.local, key stored in Windows Credential Manager)

# 3. in another terminal
node scripts/ai-desktop-smoke.mjs        # FERRUM_CDP overrides http://127.0.0.1:9222
```

Exit code 0 means every check passed. The script creates `~/ferrum-ai-smoke-tmp` (left: three files plus
`sub/inner.txt`, right: empty) and removes it at the end. The provider only receives counts and flags, never names
or paths. The message can be changed with `FERRUM_AI_SMOKE_MESSAGE`.

## What it checks

| Check                                                                                                          | Why                                      |
| -------------------------------------------------------------------------------------------------------------- | ---------------------------------------- |
| runs inside the real Tauri webview                                                                             | proves IPC is real, not mocked           |
| `Ctrl+Shift+A` opens the panel                                                                                 | action registry + mount                  |
| preview appears and **no ZIP exists before approval**                                                          | zero mutation before the single approval |
| preview states what the scope includes; subfolder is part of "all files"                                       | spec section 4 (default entries `all`)   |
| `Completed` comes from the engine result; exactly one ZIP; payload matches (names read from the ZIP directory) | success is never the model's claim       |
| a second identical request creates a **new unique name** and keeps the first ZIP                               | collision policy: no silent overwrite    |

## Findings it already produced (all fixed)

1. The preview item list was empty (`listItems` was not wired in the runtime).
2. `name (2).zip` failed contract validation because parentheses were not allowed in output names (TS and Rust).
3. The model read "semua file" as files only and left subfolders out; the prompt now states the default.

## Limits

- It calls a real model, so a run can fail for reasons outside the code (provider down, model answers differently).
  One passing run is evidence, not a guarantee.
- Windows only (Credential Manager, WebView2).
- Not covered by this script: active filter / hidden items (see `-extra`), 100k entries (see `-100k`), output inside the source subtree, unreadable input, full disk,
  cancel (Pack is not a queue job; see `-zip-edge`), sync (see `-sync`).

## Extension: ZIP edge cases (FER-165 / FER-205) — EXECUTED, 28/28 PASS

`scripts/ai-desktop-smoke-zip-edge.mjs` (fixture `~/ferrum-ai-smoke-zipedge-tmp`, removed at the end). Run on
2026-10-05 against the real app built from `feature/ai-wp2-zip-edge` with the live provider: **all 28 checks passed on its
first execution** (first reported as 29/29; the script has 28 checks); the base script above passed 22/22 on the same build.

```powershell
node scripts/ai-desktop-smoke-zip-edge.mjs   # same prerequisites; FERRUM_AI_SMOKE_BIG_MB sets the E3 file size (300)
```

| Scenario                                                                                     | Checks                                                                                                                                                                                                                                                                                             |
| -------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| E1 output inside the source subtree (left `proj`, right `proj\sub`, "simpan di panel kanan") | preview names `proj\sub\proj.zip`, warns that the ZIP lands inside the archived folder, shows a computed size; no ZIP before approval; `Completed`; ZIP members are exactly `a.txt`, `b.txt`, `sub/inner.txt` (not itself); no `.ferrum-tmp` left                                                  |
| E2 unreadable input (`locked.txt` held open with no sharing by another process)              | not `Completed`; no ZIP and no placeholder at the destination; no staging left; no raw engine text in the chat. Today the state is `Outcome unknown`: the adapter returns `execution_failed` with cause `sourceUnreadable`, which becomes `Failed` + cause once the executor maps causes (FER-163) |
| E3 cancel while Pack runs (LIMIT-6)                                                          | "tidak dapat dibatalkan" is shown, the run is never reported as cancelled, the real result (`Completed`, ZIP with `big.bin`) follows                                                                                                                                                               |

A full disk is not exercised here (no safe way to fill a real drive without admin rights); the engine is tested with a
writer that fails like `ERROR_DISK_FULL` (`src-tauri/src/archive_create_test.rs`, `edge_tests`).

## Extension: filter, hidden items and empty selection (FER-224 / AI-13.2) — EXECUTED, 20/20 PASS

`scripts/ai-desktop-smoke-extra.mjs` covers three cases the base script lists as not covered. It was written by the
adversarial lane without being run; the integrator then ran it against the real app (merged build, live model): **20/20
checks passed on its first execution**, so its selectors and the empty-selection wording are verified. `scripts/ai-desktop-smoke.mjs`
is unchanged.

```powershell
# same prerequisites as above (app started with remote debugging, provider configured)
node scripts/ai-desktop-smoke-extra.mjs      # FERRUM_CDP overrides http://127.0.0.1:9222
```

Environment overrides: `FERRUM_AI_SMOKE_SELECTED_MESSAGE` (S1 wording), `FERRUM_AI_SMOKE_VISIBLE_MESSAGE` (S2/S3 wording).
Fixture: `~/ferrum-ai-smoke-extra-tmp` (left: `report.pdf`, `invoice.pdf`, `notes.txt`, and `secret.pdf` marked hidden with
`attrib +H`; right: empty), removed at the end; the pane filter and the hidden-files toggle are restored.

| Scenario                                       | Checks                                                                                                                                                                                                 |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| S1 "the selected ones" with an empty selection | the "nothing selected" message appears; no Run button; no ZIP is written (never a silent select-all). If the live model picks another scope the script reports `inconclusive` instead of clicking Run. |
| S2 filter `pdf` active, hidden OFF             | preview states the filter and the hidden exclusion; preview lists exactly the two visible `.pdf`; no ZIP before approval; ZIP members == `invoice.pdf`, `report.pdf`                                   |
| S3 filter `pdf` active, hidden ON (Ctrl+H)     | preview lists the hidden `.pdf`; ZIP members == `invoice.pdf`, `report.pdf`, `secret.pdf` (`notes.txt` still out)                                                                                      |

Known assumptions to verify on the first run: the pane filter input is `input[placeholder="Filter"]` inside the pane
`section`, `Ctrl+H` toggles hidden items when focus is on the page body, the Windows hidden attribute maps to the
app's hidden flag, and the chat wording for an empty selection still contains "Belum ada item yang dipilih".
The same logic is verified deterministically (no live model) by `src/lib/ai/adversarial/matrix.test.ts`
(filter/hidden scope, empty selection) and `src/lib/ai/contextResolver.test.ts`.

## Extension: follow-up refinements (FER-166 / AI-09) — EXECUTED, 18/18 PASS

`scripts/ai-desktop-smoke-followup.mjs` (fixture `~/ferrum-ai-smoke-followup-tmp`, removed at the end):

| Step                             | Checks                                                                                             |
| -------------------------------- | -------------------------------------------------------------------------------------------------- |
| ask for a ZIP                    | preview + Run; revision 1; no ZIP on disk                                                          |
| "namanya laporan.zip"            | revision 2; output name changed; still nothing written                                             |
| "taruh di kanan"                 | revision 3; output moved to the right pane folder                                                  |
| "hanya PDF"                      | revision 4; the preview lists only the two PDFs; still nothing written                             |
| Run                              | exactly one ZIP, `laporan.zip`, in the right folder with only the PDFs; nothing in the left folder |
| "namanya lain.zip" after the run | nothing new is created; the chat says the plan has already finished                                |

The three refinements are handled locally (no provider call); only the first message uses the live model.

## Extension: a real folder with 100,000 entries (FER-160 / AI-03, matrix row 3) — EXECUTED, preview 9/9, run 11/11 PASS

`scripts/ai-desktop-smoke-100k.mjs` writes 100,000 small files into `~/ferrum-ai-smoke-100k-tmp/left` (about 40-50 s),
opens that folder in the left pane and asks for a ZIP of all files.

```powershell
node scripts/ai-desktop-smoke-100k.mjs                                  # preview only, then Cancel
$env:FERRUM_AI_SMOKE_100K_RUN = '1'; node scripts/ai-desktop-smoke-100k.mjs   # also approve and count the ZIP entries
```

Overrides: `FERRUM_AI_SMOKE_100K_COUNT` (default 100000), `FERRUM_AI_SMOKE_KEEP=1` keeps the fixture.

| Check                                                                                    | Why                                                                    |
| ---------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| the pane opens the folder and renders far fewer DOM rows than entries                    | the list is virtual                                                    |
| a preview with Run appears and says `100000 items`; the paged item list says `of 100000` | select-all covers the whole listing, not the viewport; no contract cap |
| no ZIP before approval; Cancel writes nothing                                            | zero mutation before approval                                          |
| (RUN mode) `Completed`, exactly one ZIP, entry count from the ZIP64 end record = 100000  | the engine archived every entry                                        |

Result on `main` @ `4a04a8d` (2026-10-05, Windows 11, live provider): preview mode 9/9 PASS — listing 3.2 s (33 DOM rows),
preview 18.2 s. RUN mode on `main` @ `699d2de`: 11/11 PASS — listing 12.7 s, preview 17.2 s, Run 990.5 s (Pack shows no progress,
LIMIT-6), `left.zip` with 100,000 entries read from the ZIP64 end record.

## Extension: follow-ups through the provider, revision limit, follow-up while running (FER-166 / AI-09) — EXECUTED, 19/19 PASS

`scripts/ai-desktop-smoke-followup-live.mjs` (fixture `~/ferrum-ai-smoke-followup-live-tmp`, removed at the end). Run on `main` @
`699d2de` with the live provider: **19/19 on its first execution**.

| Scenario                                                                                                                                                                            | Checks                                                                                                                                                   |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| L1 free-form follow-up (`FERRUM_AI_SMOKE_FREE_FOLLOWUP`, default "sebenarnya cukup file PDF dan file txt saja yang masuk"; no local pattern matches it, so it goes to the provider) | revision 2; the preview lists the PDFs and TXTs and leaves `notes.md` out; nothing written; "batal" ends the plan                                        |
| L2 revision limit                                                                                                                                                                   | 19 local renames reach revision 20; the next one is refused with "Batas 20 revisi tercapai"; the plan stays at 20; nothing written                       |
| L3 follow-up while Pack runs (`FERRUM_AI_SMOKE_BIG_MB`, default 300)                                                                                                                | "namanya lain.zip" is refused with "Perubahan tidak mengubah job yang aktif"; the job finishes `Completed`; the ZIP has the approved name; no `lain.zip` |

A follow-up during a _queued_ job uses the same refusal branch but is not shown here (Pack never queues).

## Manual Pack regression (matrix row 22) — EXECUTED, 7/7 PASS

`scripts/ai-desktop-smoke-manual.mjs` drives the manual path without the AI: select all in the left pane, Ctrl+K "Pack", confirm
the "Pack to archive" dialog. Checks: dialog says "Pack 3 items", nothing written before confirm, one ZIP at the default target
`<folder>.zip` next to the folder with `a.txt`, `b.txt`, `sub/inner.txt`, no staging file. No provider is needed.

## Sync result card counts (WP3 addition to `ai-desktop-smoke-sync.mjs`)

Run 1 now also checks that the result card reads "2 completed · 0 failed" and that every job reads N/N (copy-engine fix `61e9425`).
On `main` @ `699d2de` and `189ca14`: 28/28 PASS (jobs 1/1, 1/1). WP3 re-ran every script in this file on both builds;
results are in `docs/ai/verification.md` section 7.

## Archive flows in the real window (FER-448): `scripts/smoke-archive.mjs`

Not an AI smoke and needs no provider. It drives the release `ferrum.exe` (real Tauri IPC, WebView2 CDP on port 9224)
through the archive workflows and complements Playwright, which only runs against mock IPC. Style and isolation are the
same as `scripts/smoke-activity.mjs`: isolated through `FERRUM_PROFILE_DIR` (see `docs/test-profile.md`); the real profile is hashed before and after and the
run fails if it changed.

```powershell
npx tauri build --no-bundle
node scripts/smoke-archive.mjs "<abs path>\src-tauri	arget
elease\ferrum.exe"
```

Exit code 0 means every check passed. Fixtures are generated at run time (tiny ZIP, 7z (Copy method) and PNG encoders in
the script; no 7-Zip, no downloads) in `~/ferrum-arcsmoke-fx-*` and removed at the end. Screenshots go to
`FERRUM_SMOKE_ARTIFACTS` (default `%TEMP%\ferrum-archive-smoke-shots`) and are kept; they are never committed.
`FERRUM_SMOKE_DEBUG=1` traces every CDP call. Keys are real CDP key events (`Input.dispatchKeyEvent`).

| Area                | Checks                                                                                                                             |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Open ZIP / 7z       | Enter enters the archive, root and nested member lists, Backspace returns (double-click is the fallback so later checks still run) |
| Alt+F9 unpack       | extracted files compared byte for byte, for ZIP, 7z, and both with explicit folder entries                                         |
| Alt+F5 pack         | Pack dialog, ZIP created from three items, parsed in Node and compared byte for byte                                               |
| Alt+Shift+F9 test   | reports OK for good archives and a problem for a corrupt ZIP                                                                       |
| Quick View (Ctrl+Q) | text member and 8x8 PNG member (decoded `<img>`) inside ZIP and 7z                                                                 |
| Corrupt archives    | safe one-line error in the pane, no panic text or full path, app alive, pane usable                                                |

Caveat: Enter on an archive currently hands the file to the OS association instead of entering it (FER-450), so the
Enter checks may make Windows try to open the fixture ZIP/7z with its default program once per run.

Result on 2026-10-06 against `feature/p3-archive-smoke` @ main 7092bea: 4 FAIL, all product defects with issue tracker tasks and no
fixture or script cause: Enter on ZIP and on 7z (FER-450), 7z with explicit folder entries hangs the pane (FER-449), ZIP with
explicit folder entries cannot be unpacked (FER-451). Everything else passed. See `docs/release/rc1/KNOWN-ISSUES.md` KI-23 to KI-25.
