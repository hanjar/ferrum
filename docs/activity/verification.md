# Activity — verification evidence

## ACT-09.3 Query benchmark (2026-10-04)

Run: `cargo test --release --offline -j 2 --test store -- --ignored --nocapture`
in `src-tauri/src/activity/store` (Windows 11 Pro, MSVC release build, rusqlite bundled SQLite,
temporary on-disk DB, warm: 50 iterations, p50/p95 in ms).

Dataset: 100k entities, 1M episodes (10 per entity over 90 days), queried with a 90-day window and
limit 100. Seed ≈ 12 s.

| Query        | Before (6f2f7eb)                                           | After (schema v10) |
| ------------ | ---------------------------------------------------------- | ------------------ |
| Recent       | p95 0 ms                                                   | p95 0 ms           |
| Frequent     | p95 ≈ 2855 ms (565 ms Recent earlier, fixed by v9 rollups) | **p95 7 ms**       |
| Exact search | p95 0–2 ms                                                 | p95 0 ms           |
| Typo search  | p95 0–1 ms                                                 | p95 0 ms           |

Target ≤ 100 ms p95: **met** on this host and dataset.

### What changed

- v10 adds `activity_entity_rank(entity_id, kind, rank_key, active_days, last_utc_ms)`, kept in the
  same transaction as `record`, `purge_before`, `forget_entity`, `exclude_root`, `clear_all`.
  `rank_key = log2(Σ min(count,5)·2^(day/14))` is independent of the query day, so it orders
  entities by the Frequent score; the top `limit + 50` are re-scored exactly from their daily buckets.
- Covering index `activity_daily_frequent(kind, last_utc_ms, …)` for the bucket scan.

### Limitations

- The fast path applies only when the query window excludes no bucket (every bucket is inside
  `[max(from, retention cutoff), now]`). Narrower windows (7/30 day filters), or stale buckets not yet
  purged, fall back to the bucket scan (≈ 300–500 ms at this dataset size). The app queries a
  retention-sized window and purges at the same cutoff, so the default view is fast.
- The benchmark seeds episodes inside the queried window. `purge_before` on 100k entities / 1M episodes
  took > 10 minutes in a trial run (not measured to completion): the purge path is a separate,
  still-open performance gap.
- Ties on the rounded rank key break by recency then id; float noise below 1e-9 in the key is treated
  as a tie.
- Single host, single run; no recall measurement for typo search in this document.

## ACT-09.1 E2E (FER-273) — fresh profile -> visits -> restart -> Recent/Frequent -> search -> forget

Automated at store level (not a GUI run), `src-tauri/src/activity/store/tests/qa.rs`:

| Step                                                                                                                                                                                                                  | Test                                                             |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| Fresh profile (DB dir does not exist), schema = `SCHEMA_VERSION`, empty Recent/Frequent                                                                                                                               | `e2e_fresh_profile_visits_restart_recent_frequent_search_forget` |
| Visits across UTC days (folders + one file open), close store, reopen the file                                                                                                                                        | same                                                             |
| Recent order and per-kind lists after restart; Frequent needs >= 2 active days                                                                                                                                        | same                                                             |
| Search after restart: exact, transposition typo, once-visited old name, unknown name; locator lookup                                                                                                                  | same                                                             |
| Forget one entity: pre-forget epoch queries return `None`, gone from Recent/Frequent/search, a late pre-forget receipt gets `StaleEpoch`, forget survives another restart, a later deliberate visit starts at count 1 | same                                                             |
| Pause persists across restart; resume, clear-all, reopen; cleared receipt ids are reusable                                                                                                                            | `e2e_pause_exclude_and_clear_persist_across_restart`             |

GUI E2E (real WebView2) is `scripts/smoke-activity.mjs <path-to-ferrum.exe>`: isolated `APPDATA`/`LOCALAPPDATA` plus
`FERRUM_ACTIVITY_DIR`, CDP on port 9223, counts only (no paths logged). It covers: startup without Recent, breadcrumb
visit -> sidebar Recent, palette history + Enter navigation, Pin to Favorites from the context menu, recording toggle,
clear history, unpin, DB created in the isolated dir. It was **not run in this round** (no GUI runs were requested).

Manual/GUI scenarios still **not covered** by any script (carried from `sisa-pekerjaan.md` section 6):

1. Restart of the real app between the visit and the Recent check (the smoke runs in a single process; the store-level restart test above is the substitute).
2. Search/reveal of a file result in the captured pane, and `forget` of one item from the sidebar.
3. Two windows or two panes visiting the same folder within 30 s (the store dedups, but out-of-order delivery is a known gap, see Gaps).
4. Navigation to an unplugged drive / failed navigation: expected Recent unchanged. Procedure: open a pane on a removable drive, unplug, navigate to a folder on it, confirm no new Recent row and no console error.
5. Session restore: close with tabs on three folders, reopen, expect zero new episodes (store-level proof: `restart_and_session_restore_replay_do_not_inflate_counts`; GUI proof pending).
6. Refresh storm: hold F5 on a folder, expect episode count unchanged (store-level: `one_thousand_refresh_style_receipts_collapse_into_one_episode`, which assumes a misfiring hook; the real hook does not fire on refresh).
7. Keyboard/ARIA accessibility of Recent, Frequent and the palette groups.

## ACT-09.2 Store QA tests (FER-274, 2026-10-04)

File: `src-tauri/src/activity/store/tests/qa.rs`. Gate: `cd src-tauri/src/activity/store && cargo test --release --offline -j 2`
-> store.rs 32 passed / 1 ignored (existing 100k benchmark), identity.rs 26 passed, **qa.rs 21 passed / 4 ignored**
(3 benchmarks + 1 open-bug test, below). All 21 pass; none was weakened to pass.

| Scenario                                    | Tests                                                                                                                                                                                                                                                                                                                                                                   | Result                           |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------- |
| Exclusion aliases                           | case + `/` + trailing separator on Windows paths; subtree boundary (`PrivateX` is not excluded); UNC case-insensitive vs POSIX case-sensitive; excluding purges alias-spelled history and survives restart; `..` and relative roots rejected without bumping the epoch                                                                                                  | pass                             |
| Junction / 8.3 short name / `\\?\` verbatim | `junction_short_name_and_verbatim_aliases_are_not_resolved_by_store_documented_gap` pins the behaviour at the HEAD store (`a0073f6`): these spellings are **not** matched by exclusion. The working tree's ACT-06 work (`identity.md`: locator folding, `link_alias`) may change this; if so the test fails and this row must be updated rather than the test loosened. | pass (documents a gap)           |
| Offline devices                             | Offline/Missing/Inaccessible/Unknown receipts are kept and listed with their availability; availability change updates the entity without a new episode; same entity id on another volume is rejected, rolled back, and does not consume the event id                                                                                                                   | pass                             |
| Failed navigation                           | invalid locator, wrong entity kind, source/kind mismatch, blank id are rejected with zero rows and reusable ids; late receipts after clear/pause get `StaleEpoch`; paused write is `Ignored`; preview and operation receipts are `Ignored`                                                                                                                              | pass                             |
| Restore inflation                           | 500 receipts, 10 restarts, then the full receipt log replayed after restart: all `Duplicate`, episode counts unchanged                                                                                                                                                                                                                                                  | pass                             |
| Duplicate hooks                             | same event id or same action id with any other field changed (including a different entity) is `Duplicate`; two panes inside the cooldown form one episode; exactly 30 s after the last timestamp starts a new one; two store handles on one DB file behave like two windows, and a privacy change in one makes the other's queued write `StaleEpoch`                   | pass                             |
| Out-of-order cross-pane hooks               | a receipt older than the open episode's last timestamp opens a second episode inside the 30 s window                                                                                                                                                                                                                                                                    | pass (pins a **gap**, see below) |
| 1000 refreshes                              | 1000 receipts 100 ms apart: 1 new + 999 continued episodes, count 1, last access advanced, not in Frequent                                                                                                                                                                                                                                                              | pass                             |
| Bulk sync of 10 000 files                   | 10 000 `OperationCompleted` receipts: all `Ignored`, no entity/vocabulary/receipt rows created, only the one explicit open counted (whole qa suite runs in 1.8 s)                                                                                                                                                                                                       | pass                             |

Not testable at store level (the store never sees these inputs): accessibility, real refresh/restore/failed-load hooks in the
renderer, drive unplug. See the GUI list in ACT-09.1.

## ACT-09.4 Startup and navigation-overhead benchmark (FER-275 / FER-276, 2026-10-04)

Runner: `node scripts/bench-activity.mjs [--skip-1m] [--only-qa]` -> runs the ignored benchmarks in release mode and writes
`docs/activity/benchmark-latest.txt` (host spec, git SHA, dirty-tree flag, full output including histograms). Latest committed
output: `benchmark-latest.txt` (HEAD `a0073f6`, dirty tree: ACT-06/07 work in progress; schema v11 in the working tree).

Host: Windows 11 Pro 10.0.26200 x64, Intel Core i5-13600K (20 logical CPUs), 63.8 GiB RAM, rustc/cargo 1.98.1, MSVC release,
bundled SQLite, temp directory on the system volume, `-j 2`. Other agents were compiling on the same host during the run.
Single run per size, warm OS page cache (data just written). Not a CI-quality number.

Datasets (synthetic, seeded through raw SQL like ACT-09.3; rollups and rank table rebuilt): **1k** entities x 2 episodes (baseline),
**100k** entities x 10 episodes = 1M episodes (541.6 MiB), **1M** entities x 2 episodes = 2M episodes (3140.7 MiB).
"DB 100k/1M" in the task was read as 100k entities / 1M episodes and 1M entities / 2M episodes.

Startup (`ActivityStore::open` on an existing, current-schema DB, 25 open+drop cycles) and first Recent query:

| Dataset | open() p50 / p95 | first `recent(limit 20)` p50 / p95 |
| ------- | ---------------- | ---------------------------------- |
| 1k      | 0.350 / 0.611 ms | 0.137 / 0.243 ms                   |
| 100k    | 0.258 / 0.382 ms | 0.128 / 0.172 ms                   |
| 1M      | 0.360 / 0.797 ms | 0.188 / 0.386 ms                   |

Startup is independent of DB size (no scan at open). Not measured: cold-cache first open after boot, and the one-time migration
cost of an upgrade (a v9 -> v10 simulation on the 100k DB, run before the working tree moved to schema v11, took 1.12 s; the
current runner skips it because the simulation is written for v10).

`record()` per call, i.e. the navigation hook's store cost (on-disk, default journal, fsync per commit; 400 calls per scenario). Disk
floor = a raw one-row SQLite commit in the same directory:

| Scenario                      | 1k p50 / p95     | 100k p50 / p95       | 1M p50 / p95         |
| ----------------------------- | ---------------- | -------------------- | -------------------- |
| disk floor (raw commit)       | 2.7 / 3.9 ms     | 2.6 / 3.6 ms         | 3.1 / 4.5 ms         |
| new entity + new episode      | 4.0 / 6.1 ms     | **135.1 / 233.7 ms** | **200.0 / 247.9 ms** |
| existing entity + new episode | 5.7 / 8.4 ms     | **134.7 / 173.5 ms** | **174.7 / 215.6 ms** |
| continued episode (<30 s)     | 5.1 / 7.2 ms     | **132.9 / 175.8 ms** | **174.9 / 215.6 ms** |
| duplicate event id            | 0.041 / 0.067 ms | 0.036 / 0.109 ms     | 0.039 / 0.066 ms     |
| ignored (operation receipt)   | < 0.001 ms       | < 0.001 ms           | < 0.001 ms           |

Histogram, 100k "continued episode" (all 400 samples are >= 50 ms; 1k DB for comparison):

```
100k  >=50 ms | ######################################## 400
1k      <5 ms | ################## 174
       <10 ms | ####################### 222
       <50 ms | # 3
      >=50 ms | # 1
```

Result against the (proposed) budget: **navigation overhead is not acceptable on large databases.** The 1k baseline stays close to
the commit floor (4-6 ms), but at 100k entities a write costs 130-170 ms and at 1M entities 175-200 ms (p50), growing with the
number of daily buckets. The renderer fires the hook without awaiting (`void invoke(...)`), so the visible UI is not blocked by
this, but every navigation keeps the store busy for >130 ms and bursts queue behind the single connection.

Root cause (found by timing per-segment inside a copy of `record()`, then confirmed with `EXPLAIN QUERY PLAN`): `refresh_rank`
reads `activity_daily WHERE entity_id = ?1 AND kind = ?2`. Since schema v10 added the covering index
`activity_daily_frequent(kind, last_utc_ms, entity_id, utc_day, episodes)`, SQLite plans that read as
`SEARCH activity_daily USING COVERING INDEX activity_daily_frequent (kind=?)`, a scan of every bucket of the kind
(95-105 ms at 1M rows) instead of the primary key (35 us). With the read forced onto the primary key
(`FROM activity_daily INDEXED BY sqlite_autoindex_activity_daily_1`) in a throw-away copy of `lib.rs` (not applied to the real
store, which another agent owns): 60 `record()` calls on the 100k DB took 498 ms (8.3 ms each) against 7.2 s (120 ms each) before;
`refresh_rank` itself dropped from 92-97 ms to 0.08 ms. A regression test that fails today is committed as
`refresh_rank_bucket_read_uses_primary_key_not_kind_wide_scan` (`#[ignore]` with the reason; run with `-- --ignored` to see it fail on a
tiny DB). The ACT-09.3 Frequent p95 gain (2855 -> 7 ms) came from this index, so it must be kept; only the query hint is wrong.

### Limitations

- One host, one run per size, noisy host (parallel builds), warm page cache; fsync cost depends on the disk and antivirus. The 1k baseline varied by up to 10x between earlier runs when the machine was busy, so only the order of magnitude matters.
- The p95 of 400 samples is the 380th value; no confidence intervals.
- `record()` was measured on-disk through the store API, not through Tauri IPC, so the IPC hop, JSON and renderer cost are excluded. Not measured: memory (working set), cold start, UI frame cost, GUI navigation latency on a commit-attributed build.
- Seeds use raw SQL with one daily bucket per episode on 90 days; real histories have different bucket shapes and vocabulary.
- The recorded entity names/locators use a single volume `C:`; NTFS cost of a large DB file on other file systems or network drives is not covered.
- `purge_before` is deliberately **not** benchmarked on 100k+ (known > 10 min; see ACT-09.3) and remains an open gap.

### Gaps found in this round

1. **record() O(buckets) cost** (above): fix in `store/src/lib.rs` `refresh_rank` (one-line index hint); not applied because the file belongs to the ACT-06 agent.
2. (CLOSED by FER-252, 2026-10-05) **Out-of-order cross-pane receipts** inflated episode counts (pinned by test); fix would fold a receipt older than the open episode's last timestamp into that episode when it is inside the 30 s window.
3. **Junction/8.3/verbatim exclusion aliases** at HEAD store (may be closed by ACT-06 working-tree changes; re-run the pinned test after it lands).
4. **`purge_before` performance** on 100k+ entities still unmeasured/unfixed.
5. GUI scenarios listed in ACT-09.1 are unrun.

## purge_before

Problem: `ActivityStore::purge_before` on 100k entities x 10 episodes (1M episodes) ran > 10 minutes (gap 4 above).

Root cause (measured, not guessed): `activity_receipts.episode_id` is `REFERENCES activity_episodes(id) ON DELETE CASCADE` but had
no index, so every deleted episode made SQLite scan the whole receipts table for the cascade (O(episodes x receipts)). With one
receipt per episode (what `record()` produces) the purge was quadratic: 2k entities 10 s, 4k entities 45-52 s (x4 entities = x4.5-5
time), extrapolated to 100k entities well beyond 10 minutes. Without receipt rows (the bare benchmark seed) the same purge is
linear and only took ~4.8 s, which is why it only shows with realistic data. The other candidates (`NOT IN` subquery, summary
`NOT EXISTS`, rank refresh via the `+kind` primary-key read, other cascades) all have usable indexes and were linear.

Changes (`store/src/lib.rs`):

- Schema v12 (`SCHEMA_VERSION` 11 -> 12): `CREATE INDEX IF NOT EXISTS activity_receipt_episode ON activity_receipts(episode_id)`.
  `tests/identity.rs` pinned `SCHEMA_VERSION == 11` and was bumped to 12.
- `purge_before` made set-based with identical semantics: per-pair deleted counts go to `temp.purge_pairs` in one INSERT..SELECT,
  summary counts are fixed with one `UPDATE .. FROM`, rank rows for the affected pairs are recomputed from one ordered read of
  their remaining daily buckets (still `+kind` so the PK is used) and one insert pass, and the orphan entity delete uses
  `NOT EXISTS` on `activity_episode_lookup`. Privacy epoch is not touched.

Measurements (release, one noisy shared Windows host, `FERRUM_PURGE_ENTITIES` scales the dataset; purge cuts exactly half of the episodes):

| dataset                                              | before                                         | after     |
| ---------------------------------------------------- | ---------------------------------------------- | --------- |
| 2k entities / 20k episodes, receipts                 | 10.0 s                                         | 0.1 s     |
| 4k entities / 40k episodes, receipts                 | 45.5 s                                         | 0.23 s    |
| 100k / 1M, no receipts                               | 4.8 s (index irrelevant)                       | 4.1-4.4 s |
| 100k / 1M, receipts per episode                      | > 10 min (extrapolated; not run to completion) | 8-13 s    |
| 100k / 1M, receipts, half the entities removed whole | > 10 min (extrapolated)                        | 8-14 s    |

Tests (`store/tests/purge_perf.rs`): two correctness tests on small data (summary equals GROUP BY of episodes, rank equals
`rebuild_rank` of daily, no orphans in vocabulary/grams/summary/daily/rank/receipts, hidden flag kept for survivors, idempotent
purge, purge-all empties every table) and three `#[ignore]` perf tests. Run:
`cargo test --release --offline -j 2 --test purge_perf -- --ignored --nocapture --test-threads 1`.

Limitations: the target of < 10 s is met for the benchmark-style seed (no receipts); with a receipt per episode the compute part is
about 7 s and the final commit (fsync, rollback journal, `secure_delete = ON`) varies from 1 s to 28 s on this host, so those two
perf tests assert < 30 s. Existing databases get the index only when opened (the v12 migration builds it once, proportional to the
receipt count). The purge is still one transaction, so it holds the store busy for its whole duration; chunking was not done.
Orphan `activity_hidden` rows of purged entities are left as before (unchanged semantics).

## Audit gap closure (FER-246 / 248 / 250 / 252, 2026-10-05)

Commands (all green): `cd src-tauri/src/activity/store && cargo test --release --offline -j 2` (gaps 9, identity 26, purge_perf 2,
qa 22, store 32; perf tests stay `#[ignore]`) and `cd src-tauri && cargo test --offline -j 2 --lib` (110 passed, including 9 new in
`activity_service_queue_tests.rs`).

| FER | Evidence                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 246 | `record_visit` returns `queued` without touching the DB (store lock held by the test, returns < 250 ms); backlog of 150 applied exactly once; overflow of 768 sends is `dropped`, counted, never an error, and every accepted receipt lands once; clear/pause/bumped generation cancel queued receipts; poisoned store does not fail `record_visit`. Latency measurement: `cargo test --release --offline -j 2 --test gaps -- --ignored --nocapture` (about 3.1 ms p50 single, 13 ms for a batch of 64). |
| 248 | corrupt file -> quarantine + working fresh DB + `state.recovered`; second corruption does not overwrite the first quarantine; future schema rejected, bytes unchanged, no quarantine file.                                                                                                                                                                                                                                                                                                               |
| 250 | chat receipt -> `Ignored`, no episode/recent/frequent, event id not burned; `watch.rs` static check.                                                                                                                                                                                                                                                                                                                                                                                                     |
| 252 | out-of-order within window continues (also before start), outside window opens a new episode; summary/daily/rank stay consistent, no phantom bucket across UTC midnight.                                                                                                                                                                                                                                                                                                                                 |

Gaps list update: item 2 of "Gaps found in this round" (out-of-order receipts) is closed. Limitations: visits are now eventually
consistent (a `recent`/`search` issued within a few ms of a visit may not include it; `flush()` is the barrier in tests); the visit
queue is shared with identity jobs (256 slots); no startup integrity check beyond open-time corruption.

## GUI smoke 2026-10-05

HEAD `bb70391` (branch `feature/activity-local`, merged with main), binary `src-tauri\target\release\ferrum.exe` (built 2026-10-05 11:43, not rebuilt).
Command: `node scripts/smoke-activity.mjs "<abs path>\ferrum.exe"`. Result: **34 PASS / 0 FAIL**, about 20 s per run (two consecutive full runs, both 34/34).

Covered (each is a PASS/FAIL line in the script log): startup Recent empty with the empty message; first-run disclosure shown, acknowledged,
and NOT shown again after a real restart (process killed and relaunched with the same `FERRUM_ACTIVITY_DIR`); navigation to a fixture folder
through the UI; visits -> Recent; palette history result navigates; Pin to Favorites via context menu; pause recording does not change Recent;
Activity page (titlebar clock) with the 7/30/90 filters, items visible and coverage label present; hide (X) removes an item from Recent but it stays
on the Activity page marked `disembunyikan`; forget (preview + confirm) removes it from page and sidebar; F2 Rename dialog on a visited folder and
revisit gives exactly one Recent entry (identity kept, total count unchanged); private session hides Recent and a visit made in it is recorded
nowhere; Recent, Favorites, hidden and forgotten state survive the real restart; clear history shows a preview with counts, Batal keeps data,
Konfirmasi empties Recent, Frequent and the page, Favorites survive, unpin works; `activity.sqlite3` sits in the isolated dir.

Not covered / weak:

- Frequent was empty in the run (visits happen on one day with cooldown), so "Frequent persists after restart" is vacuous (the log says so). Frequent ranking itself is covered only by store tests.
- Ctrl+Shift+D pin shortcut, Enter/double-click on a file (opens an external app) and shellOpen receipts: deliberately not exercised.
- Rename is the F2 dialog only (not the inline "Rename inline" context entry), and only a folder rename; file rename and move are not covered.
- Multi-window private session isolation (only the single window is checked), two panes visiting the same folder, unplugged drive, session-restore-does-not-inflate in the GUI (Recent equality after restart is a partial proof).
- Exclusion and retention-shortening scopes in the GUI.

Findings from the run (application, not fixed here):

1. Isolation gap: CLOSED by FER-457. Every store (config.json incl. the tab session, AI stores, sync baselines, Activity DB, WebView2 data) is rooted at `FERRUM_PROFILE_DIR` when set (`docs/test-profile.md`), so the script no longer backs up or restores anything; it hashes the real profile before/after and fails if it changed. Entries left in the real Recent by runs made BEFORE this fix are forgotten by the user from the Activity UI.
2. Breadcrumb for the drive root: with a path like `C:\Users\...`, clicking the first crumb (`C:`) navigates to the drive-relative path `C:`, the pane shows "Can't open this folder" (os error 123) and the persisted session tab becomes `/C:`. The drive root crumb should navigate to `C:\`.
3. After renaming a visited folder through F2, the sidebar Recent keeps listing the OLD name until the next visit (stale label pointing at a path that no longer exists); after the revisit there is exactly one entry with the new name, so identity is preserved.

## ACT-08 desktop smoke with the real provider (2026-10-05)

Build: release `ferrum.exe` from `feature/activity-local` = `main` `06205f8` (ACT-08 R1-R7 v1 included), real profile, configured provider
(key in Credential Manager), WebView2 CDP on 9222. Run with `NODE_PATH` pointing at a `playwright-core` install.

| Script                                                                    | Result          | What it shows                                                                                                                                                                                                                                                                                                               |
| ------------------------------------------------------------------------- | --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `scripts/ai-desktop-smoke.mjs` (ZIP, `archive.create`)                    | 22/22 PASS      | Prompt with the new `activity.query` section did not shift the mutating ZIP workflow (preview, no write before approval, subfolder included, unique names, stale guard).                                                                                                                                                    |
| `scripts/ai-desktop-smoke-sync.mjs` (`directories.sync`)                  | all checks PASS | Sync workflow unchanged (baseline, conflicts skipped, no delete propagation, backend stale guard).                                                                                                                                                                                                                          |
| `scripts/ai-desktop-smoke-activity.mjs` (new, read-only `activity.query`) | 13/13 PASS      | The model picks the read intent for a frequent-folders question and a find-by-name question; answer card appears with no Run button, says "Answered from local history", shows the local-only disclosure and items; the sidebar Recent counters are unchanged by the query; "Buka di panel" navigates a pane to the folder. |

Limits (honest): one run per script against a live model, so a pass is evidence, not a guarantee. The webview IPC is non-writable and absent from the
CDP network log, so provider payloads and invoked commands CANNOT be observed from the GUI: "no history in any provider payload" and "no mutating
command" rest on the unit tests (`executorActivity.test.ts`, `ipc_activity_chat_tests.rs`), not on this smoke. Not covered: withheld/stale/ambiguous
cards against a real model, Missing/offline items, consent copy beyond the existing card disclosure line, an independent privacy review.
The card shows the parent path locally (never sent to the provider).

## ACT-08 re-smoke after privacy fixes + consent copy (2026-10-05, main 2830436 + copy change)

`scripts/ai-desktop-smoke-activity.mjs` now seeds its own history (two fixture folders navigated through the pane), runs against an ISOLATED activity
profile (`FERRUM_ACTIVITY_DIR`, so the first-run "Mengerti" click is test data, never the user's real consent) and the real configured provider:
**18/18 PASS**. New in this run: with the first-run disclosure NOT acknowledged the model produced no history answer card (finding #5: `activity.query`
not offered), and after "Mengerti" it did; the disclosure text states that the typed question goes to the AI provider while the history result stays
local; the answer card says "(pertanyaan Anda tetap dikirim)"; the query leaves the sidebar counters unchanged; find-by-name and "Buka di panel" work.
Consent copy (checked and tightened): the disclosure previously said only that history is not sent anywhere; it now also says asking the AI assistant
about history sends the typed question to the provider while results are read and shown on this device. Limits as before: payloads and invoked commands
are not observable from the GUI (unit tests carry that claim); one run against a live model; withheld/stale/ambiguous cards and Missing items not
exercised against a real model.
