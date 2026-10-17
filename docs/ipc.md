# IPC contract

One boundary, three synced artifacts: Rust (`src-tauri/src/domain.rs` + `ipc.rs`),
TS (`src/lib/ipc/`), and fixtures (`contract-fixtures/*.json`). Change one, change
all three; the tests on both sides read the same fixtures.

## Serialization rules

- Field names cross as camelCase (`serde(rename_all = "camelCase")`).
- Sizes: `u64` bytes; timestamps: `i64` unix epoch **milliseconds**. Both remain
  below 2^53 for real filesystem objects → plain JS numbers, no precision loss.
- Optional fields are **omitted**, never `null` (`skip_serializing_if`).
- Errors: the only rejection shape is `FsError { code, message, path? }`. No
  panics, no raw OS strings as codes. TS wraps rejections via `FsError.fromIPC`.

## Commands

| Command                 | Payload                   | Result                                                   | Since   |
| ----------------------- | ------------------------- | -------------------------------------------------------- | ------- |
| `list_dir`              | `{ path: string }`        | `FileEntry[]`                                            | task 03 |
| `copy_control_snapshot` | none                      | `CopyControlState[]` (jobs copying now)                  | FER-471 |
| `copy_control_adjust`   | `{ operationId, change }` | `CopyControlState` (rejects when the job is not running) | FER-471 |

FER-471 copy controls: `change` is `{kind:'up'|'down', by?}`, `{kind:'set', threads}`, `{kind:'auto', enabled}` or
`{kind:'priority', priority:'normal'|'background'}`. Live state also arrives as the event
`operations://copy-control` (`CopyControlState`: `operationId, auto, threads, activeWorkers, autoMax, media, priority,
running`). Any explicit thread change turns auto mode off. Performance knobs only; no file is touched.

Planned: `create_entry`, `rename_entry`, `delete_entries` (task 10),
`enqueue_copy` / `enqueue_move`, `cancel_operation` (tasks 09–12).

## FileEntry

```ts
{
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  modifiedMs: number;
  hidden: boolean;
}
```

- `path` is the identity used for navigation and operations.
- `size` is 0 for directories (aggregation is post-MVP).
- `isSymlink` marks the link itself (junctions on Windows included); recursion
  never follows links (decision D07).

## Boundary validation

`validate_path` rejects only structurally unusable input (empty, whitespace-only,
NUL). Ferrum does **not** sandbox users to a root directory — OS permissions are
the access boundary.

## Browser dev mode

`src/lib/ipc/tauri.ts` detects absence of `__TAURI_INTERNALS__` and rejects with
`FsError { code: 'unsupported' }` so the UI fails soft in plain-browser dev.

## Disk usage (DSK-02 / FER-320, DSK-03 / FER-321)

Progressive scan of one local root + snapshot queries. Source of truth:
`src-tauri/src/disk_usage.rs` + `docs/disk-usage-contract.md` (binding).

| Command                       | Payload                                                  | Result                           |
| ----------------------------- | -------------------------------------------------------- | -------------------------------- |
| `disk_usage_start`            | `{ root: string, options: ScanOptions, fast?: boolean }` | `scanId: string`                 |
| `disk_usage_cancel`           | `{ scanId: string }`                                     | `boolean`                        |
| `disk_usage_snapshot`         | `{ scanId: string }`                                     | `DiskUsageSnapshot \| null`      |
| `disk_usage_query_children`   | `{ scanId, nodeId, offset, limit, sort?, filter? }`      | `DiskUsageChildrenPage`          |
| `disk_usage_top_files`        | `{ scanId, nodeId, limit, filter? }`                     | `DiskUsageTopFiles`              |
| `disk_usage_fast_eligibility` | `{ root: string }`                                       | `{ eligible, volume?, reason? }` |

FER-460: `fast: true` tries the elevated MFT helper (`ferrum-scan-helper.exe`,
UAC) first and falls back to the regular walk under the same scanId on any
refusal/failure. Events carry `engine: 'walk' | 'ntfsMft'` and an optional
`notice` (e.g. the fallback reason). While the helper reads, `ntfsMft` events also carry
`mftRecordsDone` / `mftRecordsTotal` (MFT size is known, so a read-phase percent is honest; `done == total` = building
the tree). See `docs/disk-usage-contract.md` §7.

Event `disk_usage://progress` (≤10/s per scan, unique scanId per start):
running totals (`scannedFiles/Dirs/Symlinks`, `knownLogicalBytes`,
`knownAllocatedBytes: number | null`), `coverage`, bounded per-path `errors`

- `totalErrors`, largest `topChildren`, `done`, `cancelled`, `error?`.
  Exactly one final event sets `done`; cancel is done + coverage `partial`,
  never `complete`. No percent-done, no ETA. Byte numbers stay ≤ 2^53 − 1
  (contract §2.6). Allocated bytes use per-node null semantics (gap G2);
  symlinks are never followed (D07); cross-volume dirs are never crossed
  (gap G3, `X:\` prefixes only — UNC/mounted folders documented limitation).
  Hardlinks count per pathname (footprint); unique dedup deferred (gap G6).

### Query semantics (DSK-03)

- Both queries read PURELY from the stored snapshot of the finished scan —
  the filesystem is never re-walked, so tree and Largest files always come
  from the SAME scan and totals can never inflate.
- Unknown or evicted `scanId` (bounded store) and a `nodeId` outside the
  scanned tree reject with a structured `FsError { code: 'notFound' }` —
  the UI answers with a rescan (gap G8). Responses carry `version` +
  `scannedAtMs` so the UI preserves selection across progress.
- `query_children` lists the DIRECT children of one node: child directories
  (subtree aggregates), direct files, and direct symlinks (0 bytes, counted,
  never followed). Default sort `largestFirst`; `smallestFirst`/`nameAsc`
  available. All metric sorts tie-break by stable id ascending; `limit` is
  clamped to 1000; `total` counts filter matches for the pager.
- `top_files` returns the largest FILES of the whole subtree, recursive,
  with `totalMatching`, `truncated` and the `label` ("Top N") — a capped
  list is never a claim of all results (§3.8).
- `filter` fields are ANDed: `metric` (logical|allocated) selects the metric
  for sort/threshold/percent; `minBytes` threshold fails entries with
  UNKNOWN allocated bytes (never guessed); `nameSubstring` is a
  case-insensitive name match; `extensions`/`categories` match files only.
  Unknown allocated stays `null` + `allocatedSupported: false` and only
  affects ordering (smallest side) — never substituted with logical bytes.
- `percentOfParent` / `percentOfScanRoot` are computed independently with
  explicit denominators; partial coverage, zero denominator, or unknown
  value → `null` (render "—", never 0%, contract §2.7).

## Sync profiles (automation P0.3)

Source of truth: `src-tauri/src/automation/` + `src/lib/ipc/syncProfiles.ts`; fixtures in
`contract-fixtures/automation/` (both test suites read them). The request payloads are flat
top-level arguments (Tauri passes each one to the matching camelCase parameter, so `sync_profile_save`
receives `id`, `expectedRevision` and `draft` as three parameters, never one nested object); the
`*.request.json` fixtures are asserted by the TypeScript wrappers and against the Rust command
signatures. The store lives only under the
local (non-roaming) app data dir (`…\dev.ferrum.app\automation\automation.db`); commands refuse
reparse points (`store_untrusted`) and an older binary never writes a newer store
(`store_read_only`).

| Command                 | Payload                               | Result                                                                          | Notes                                                                                                |
| ----------------------- | ------------------------------------- | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `sync_profile_list`     | none                                  | `ProfileSummary[]`                                                              | readiness per root with a 2 s budget; slower roots report `checking`                                 |
| `sync_profile_get`      | `{ id }`                              | `ProfileDto`                                                                    | policy + identities + per-revision changes list                                                      |
| `sync_profile_validate` | `{ draft }`                           | `{ errors: FieldError[], identities, kinds }`                                   | no side effects, no dialog                                                                           |
| `sync_profile_save`     | `{ id?, expectedRevision?, draft }`   | `{ status: 'saved', profile } \| { status: 'canceled' } \| { status: 'stale' }` | native confirm only for `permanent` (new/changed) or guard off; revision bumps only on policy fields |
| `sync_profile_delete`   | `{ id, expectedRevision }`            | `{ status: 'deleted' }`                                                         | `in_use` once automations reference the profile (P1.1)                                               |
| `sync_profile_preview`  | `{ id }`                              | `{ previewId, summary, conflicts, guard, blockers? }`                           | read-only engine dry run; needs no lease; the `previewId` is required by `sync_profile_run`          |
| `sync_profile_run`      | `{ id, previewId, confirmOverwrite }` | `{ runId }`                                                                     | manual run under its own lease; reports through the `sync-profile://run` event; see below            |
| `sync_lease_acquire`    | `{ left, right }`                     | `{ leaseId } \| { status: 'busy', holder: { kind, runId? } }`                   | cross-process lock on a folder pair (P0.4); `busy` is a normal answer, not an error                  |
| `sync_lease_release`    | `{ leaseId }`                         | `{}`                                                                            | idempotent; only the token handed out by `sync_lease_acquire` releases anything                      |

`draft` is `{ name, left, right, mode: 'oneWay'|'mirror'|'twoWay', from?, propagateDeletes?,
deletePolicy: 'recycleBin'|'versioning'|'permanent', versioningRoot?, compare:
'sizeAndDate'|'content', include: string[], exclude: string[], guard: { enabled, maxCount,
maxPercent } }`; unknown fields are refused (SE-1: no input may behave like a `confirmed` flag).

Errors cross as `{ code, message }` with a closed-set code (`invalid_input`, `not_found`,
`stale`, `in_use`, `busy`, `store_unavailable`, `store_untrusted`, `store_read_only`,
`needs_review`, `not_ready`, `canceled`, `lease_busy`, `lease_required`); `busy` means another native confirmation dialog is open, so nothing was shown or saved
and never contain user paths. Ids are lowercase UUID v4 text, validated before any lookup.
`readiness` is `{ status: 'ready' } | { status: 'checking' } | { status: 'notReady', reason }`
with reasons `rootMissing | rootOffline | identityUnknown | identityMismatch |
recycleBinUnavailable | versioningRootInvalid | profileInvalid`. A network (UNC) root that cannot be
opened is saved with an unverified identity (no `volumeSerial`/`fileId`) and reports `rootOffline`;
one saved that way stays `identityUnknown` until it is saved again while reachable. The revision changes list shows, per revision, `{ field, from, to }` display
values (SP-6.3); the newest 50 revisions are kept.

### Manual preview and run (automation P0.2)

Source: `src-tauri/src/automation/manual_run.rs` + `src-tauri/src/sync_engine/execute.rs`; wrappers in
`src/lib/ipc/syncProfiles.ts`; fixtures `sync-profile-preview*.json`, `sync-profile-run*.json`.

- `sync_profile_preview { id }` runs the engine read-only (gate, fresh compare with the profile's
  filters and the 200 000 entry cap, plan, delete guard) and returns
  `{ previewId, summary, conflicts, guard, blockers? }`. `summary` is the plan summary
  (`total, toRight, toLeft, overwrites, deletes, conflicts, skipped, userSkipped`). `guard.verdict`
  is `off | ok | tripped | emptySource` (`tripped` adds `side, deletes, entries, percent`;
  `emptySource` adds `empty`). `blockers` (closed codes `truncated | cancelled | errors`) appears
  only when the comparison cannot be trusted; the `previewId` is then empty and no run is possible.
  A profile that is not ready to run (a folder missing or another folder now, Recycle Bin on a
  non-fixed drive, unusable versioning folder) is the error `not_ready`; the message names a code,
  never a path. A previewId is valid for 15 minutes, for that profile revision, for one run.
- `sync_profile_run { id, previewId, confirmOverwrite }` returns `{ runId }` once the checks passed
  and the run holds its folder lease (both roots and, for versioning, the versioning folder); the
  run continues in the backend. Refusals: `stale` (preview missing, expired, another profile or
  revision), `invalid_input` (the preview has overwrites and `confirmOverwrite` is `false`),
  `not_ready`, `lease_busy` (the folders are in use), `canceled` (the user declined the native
  permanent-delete dialog), `busy` (another native dialog is open), the store errors. A profile
  whose delete policy is `permanent` and whose previewed plan deletes files is confirmed by a
  native dialog per run; no argument can say "already confirmed" (SE-1). The engine refuses the run
  as `plan_changed` if the folders now produce a different plan than the one previewed (names and
  actions; changed sizes or times alone do not count).
- Progress: the event `sync-profile://run` carries `{ runId, phase }` (`checking | comparing |
syncing | stopping | settling`), `{ runId, counts }` (at most 10 per second: `total, done, failed,
skipped, changed, held, notStarted`) and finally `{ runId, result }`. `result.kind` is
  `completed | partial | conflictHeld | failed | cancelled | interrupted | nothingToDo`, with
  `counts`, `errorCode` and `errorText` for `partial`/`failed`, `interrupted` (`rootOffline |
leaseLost`), `pendingRerun`, `settle` (`notAttempted | saved | unchanged | skipped | saveFailed`)
  and `problems` (at most 200 operations that did not end `done`/`skipped`: `{ index, name, action,
status, reason? }`; `reason` is a closed code such as `sharing_violation`, `access_denied`,
  `disk_full`, `invalid_name`, `link_in_path`). `errorText` is the documented run error text (it
  may name the profile's own folder, e.g. "Would delete 812 items (41 % of E:\Backups)"); `problems[].name` is a path RELATIVE to the profile's roots (what the user sees in the operation list); no other
  field and no error message of the commands ever carries an absolute path or an operating-system message.
- Delete guard (Q4): when the preview says `guard.verdict: 'tripped'`, `sync_profile_run` still starts, but
  only after a NATIVE dialog that states the number and percent of deletes (one dialog together with the
  permanent-delete prompt); `canceled` / `busy` as above. `emptySource` is refused with the error `held` before any
  dialog. A profile that is already running, or a third concurrent preview, is `in_use`. Run error codes
  (`errorCode`) added: `guard_tripped`, `empty_source`, `plan_changed`, `identity_mismatch`, `recycle_bypassed` (the
  shell removed a file without moving it to the Recycle Bin; the run stops, at most that file is affected),
  `root_protected` (a root, or the versioning root, is inside Ferrum's own state folders or a Windows Startup
  folder, or contains one while the profile writes to it; nothing is changed). Profile validation reports the
  same as field errors `root_protected_folder`, `root_contains_protected_folder`, `root_startup_folder`,
  `root_contains_startup_folder`; a root (read-only too) on a share that points at this computer is
  `root_local_share` (run error `root_local_share`, readiness `rootLocalShare`); readiness reports `rootProtected`. Per item, an operation whose final path is
  in such a folder ends `failed` with reason `protected_folder`.
  Links (symlink/junction entries) end `skipped` with reason `link_unsupported`.
- Safety properties tested in the engine (design §5.4-§5.7): folders only get a non-recursive
  `mkdir` or an empty-folder `remove_dir`, parents are created and walked without following
  reparse points, every file is re-checked right before its operation, deletes wait when a copy
  failed, the lease is checked before every operation, and the baseline is saved only while the
  lease is alive.

### Root leases and baseline saves (automation P0.4)

A lease is the cross-process lock of design §3.4 (RQ-2): at most one run - an automation, a manual
profile run, Synchronize Dirs or the AI chat sync - may be active on overlapping folders across all
Ferrum processes of the user. Both folders are canonicalized by the backend (real path, mapped
drive resolved to its UNC form, case-insensitive, component-wise: `data` and `data-backup` do not
overlap) before the overlap test, and the overlap test, reclaiming of dead leases and the insert are
one `BEGIN IMMEDIATE` transaction.

- `sync_lease_acquire` returns `{ leaseId }` (32 hex characters, random, kept secret by the
  webview) or `{ status: 'busy', holder }` where `holder.kind` is `manual | app | runner |
unknown` and `holder.runId` is present for runs. The backend heartbeats the lease every 5 s for as
  long as it is held (at most 12 h); a lease whose holder process is gone (pid plus creation time)
  or whose heartbeat is older than 30 s is reclaimed by the next acquirer. `lease_busy` is returned
  as an error only when this process already holds 8 manual leases. Input errors are `invalid_input`;
  an unusable or newer-schema store reports `store_unavailable` / `store_untrusted` /
  `store_read_only`, and the sync does not run.
- `sync_lease_release` is idempotent. An unknown or foreign token does nothing and answers the same
  `{}` (no existence oracle); a malformed token is `invalid_input`.
- `sync_baseline_save` now requires a fresh lease held by the calling process that covers both
  roots of the pair; otherwise it fails with `{ code: 'lease_required' }` and writes nothing. The
  check is made in the backend. `sync_baseline_clear` follows the same rule; `sync_baseline_load`
  is unchanged.
- Manual Synchronize Dirs (and the chat sync) acquire the lease before the re-validation compare,
  hold it through the run and the baseline settle, and release it after the settle, on cancel, on
  error and on close. If the folders are busy nothing is compared or enqueued and the panel shows
  that they are in use.
- When the webview (re)loads its page (F5, DevTools reload, full navigation) the backend releases
  every manual lease of the process, because the JavaScript that held the tokens is gone. The
  hook only drains the registry on the UI thread and drops the guards on a blocking worker; an
  acquire that was in flight during the reload is refused and released (epoch check). There is no
  handler for a renderer crash (WebView2 `ProcessFailed`): nothing reloads by itself, so after a
  crash the leases stay until Ferrum exits, when the process probe lets the next acquire reclaim
  them; the backend stops refreshing a manual lease after the 12 h hold limit
  (`MAX_MANUAL_HOLD_MS`), so it also goes stale by itself then.
  Known edge: the epoch only covers an acquire that began BEFORE the reload event. A
  `sync_lease_acquire` sent by the old page but started by the backend after the event reads the
  new epoch and registers a lease for a page that no longer exists; it stays until the next
  reload or the 12 h limit. The
  Synchronize Dirs lease also covers the delete phase (confirmation dialog and deletes) and is
  released only when both the baseline settle and that phase have ended.

## Automations: approval, test run, pause and delete (automation P1.1)

Source of truth: `src-tauri/src/automation/{approval,policy,dpapi,test_run,dto,profile_baseline}.rs`
(Tauri-free cores) + `automation/commands.rs` (wrappers) + `src/lib/ipc/automation.ts`; fixtures
`contract-fixtures/automation/automation-*.json`, read by both test suites. Requests are flat
top-level arguments like the sync profile commands. **No command takes a "confirmed", "approved" or
dialog-text argument (SE-1):** approving and resuming show a native Windows confirmation from the
backend, built from stored data (design §10.3), and its outcome comes back as the `status` of the
result.

| Command                  | Payload                                      | Result                                                                                     | Notes                                                                                                                                |
| ------------------------ | -------------------------------------------- | ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| `automation_validate`    | `{ draft }`                                  | `{ errors: FieldError[], checks: Step[], next: {atMs,label}[] }`                          | no side effects; `next` = next 3 runs (SC-1.2); Once errors `date_out_of_range` / `time_passed`, unknown zone `tz_invalid`          |
| `automation_test`        | `{ id?, draft }`                             | `{ testId, automationId, policyHash, steps: Step[], passed }`                              | read-only (engine dry run); records `(policyHash, passed, at)` in the in-memory test ledger; `id` = automation being edited          |
| `automation_approve`     | `{ id?, expectedConfigRev?, draft, testId }` | `{ status: 'approved', automation } \| { status: 'canceled' } \| { status: 'stale' }`      | needs a passed test of the same hash from this app session within 15 min; native confirmation; one transaction                       |
| `automation_set_enabled` | `{ id, enabled }`                            | `{ status: 'enabled' \| 'paused' \| 'canceled' \| 'needsChoice' }` or error `needs_review` | enable = resume: approval verified before AND after the native confirmation; disable = no dialog, `needsChoice` while a run is going |
| `automation_pause`       | `{ id, mode: 'finish' \| 'stop' }`           | `{ status: 'paused' }`                                                                     | configuration part only (the running run joins in with the host, P1.3); safe direction, no dialog                                    |
| `automation_delete`      | `{ id }`                                     | `{ status: 'deleted' }`                                                                    | removes approval, runtime row, queued entries and events; the webview confirms in its own dialog; frees the profile (`in_use` ends)  |

`draft` = `{ name, profileId, policy }` with `policy` = `{ trigger, runtime: 'app' | 'background',
retries: 0..3, conditions: { skipOnBattery, skipOnMetered } }`. `trigger` is `{ kind: 'schedule',
freq: { kind: 'once', date } | { kind: 'daily' } | { kind: 'weekly', days }, time: 'HH:MM', tz, missed:
'skip' | 'runOnce' }` or `{ kind: 'watch', watchLeft, watchRight, onChanges, onAvailable, idleSecs,
maxWaitSecs }`. The wire type is the stored type: unknown fields anywhere are refused. Only the
profile **id** crosses; the backend loads the profile and its policy.

- **Policy hash and MAC** (`HASH_VERSION` 1, design §8.3/§8.4): `H = SHA-256(lp("ferrum.automation.policy.v1")
‖ lp(automationId) ‖ lp(profileId) ‖ u64le(profileRevision) ‖ lp(json({profile, automation})))`;
  the automation name is not part of `H`. `mac = HMAC-SHA256(key, lp("ferrum.automation.approval.v1") ‖
u64le(hashVersion) ‖ lp(H) ‖ lp(automationId) ‖ lp(profileId) ‖ u64le(profileRevision) ‖
u64le(configRev) ‖ u64le(approvedMs))`, compared in constant time. The key is 32 random bytes
  protected with Windows DPAPI (current user, entropy `ferrum.automation.approval.v1`,
  `CRYPTPROTECT_UI_FORBIDDEN`) in `automation\approval.key`. A missing or undecryptable key makes
  every approval `keyUnavailable` (fail closed); the next approval creates a new key, which does not
  revive old approvals.
- **Needs review** (`AutomationDto.review`, closed reasons): `profileChanged { from, to }`,
  `policyChanged`, `notApproved` (no approval, or a MAC that does not verify), `keyUnavailable`,
  `unsupportedVersion`. A policy-changing profile save marks every dependant in the SAME
  transaction (queued entries removed, watch state reset to `watching`, one `warn` event each).
- **Test ledger** (SE-3.2): a `testId` is single-use, lives in this process only, and expires after
  15 minutes (re-checked after the dialog). A new automation gets its id in the test answer
  (`automationId`; the hash covers the id) and `automation_approve` sends no id for it.
- **Errors:** `invalid_input` (shape, ranges, unknown fields), `not_found`, `needs_review`
  (resume of an automation whose approval no longer verifies, no dialog shown), `stale` (edit
  raced), `busy` (another native dialog is open: nothing shown, nothing saved), `in_use` (a
  profile still referenced), `store_unavailable` / `store_untrusted` / `store_read_only`.
- **Profile and automation baselines** are a separate store: `<store dir>\baselines\`, one file per
  folder pair, each with a MAC (HMAC under a subkey of the approval key). The manual
  `sync_baseline_*` commands cannot reach it; a file without a valid MAC loads as a first run
  (`untrusted`), so a forged baseline can never propagate a deletion. `sync_profile_preview`,
  `sync_profile_run` and `automation_test` use this store, so manual profile runs start from a
  first run once after this change.
  Each pair also has a save counter (`baseline_gen`) inside the signed file, so a replayed older
  file is `untrusted` too. Signing may create the approval key only when no key file exists at all
  (a first manual profile run before any approval); it never replaces an existing, unusable or
  unreadable key.
- **Delete guard:** `maxPercent` is 1..99 (100 could never stop a run; `guard_percent_range`). A
  guard that cannot trip counts as off for the native confirmation of a profile save, and every
  approval dialog states the guard thresholds (or "off").
- **Approval key:** replaced only by an approval (and only when DPAPI refused a
  blob that was read in full; never after an I/O error), under a cross-process file lock, with the
  old blob kept as `approval.key.bak`; every automation then logs a `warn` event.
- **Schema v2** adds `automations`, `approvals`, `automation_runtime`, `queue`, `runs`, `run_ops`,
  `events`, `controls`, `baseline_gen`; the v1 -> v2 migration backs the store up first (`automation.db.bak-v1`,
  removed on success). `usedBy` in `sync_profile_list` now counts automations.
- A lease id never appears in any automation DTO (only `sync_lease_acquire` hands one to its
  requester); a source test and a fixture scan hold that.

## Automations: queue, status and history (automation P1.2)

Source of truth: `src-tauri/src/automation/{status,read,dto}.rs` + `automation/store/{queue,runtime,runs,events,controls}.rs`
(Tauri-free cores) + `automation/commands.rs` (wrappers) + `src/lib/ipc/automation.ts`; fixtures
`contract-fixtures/automation/automation-{list,get,history,run-ops,events}*.json`, read by both test suites.
All five commands are **read-only**: they take no flag that could stand for a confirmation or an
authorization, write nothing (not even `change_seq`), and never create or replace the approval key. Requests
are flat top-level arguments like the P1.1 commands; Tauri ignores extra keys, so there is no unknown-field
refusal on the wire (the internal request structs are `deny_unknown_fields` for callers that deserialize a whole
request). What protects the store is the validation of every value: the backend validates ids (lowercase UUID v4), cursors (>= 1), offsets and page sizes before it touches the store
(`invalid_input`); an unknown automation or run is `not_found`.

| Command              | Payload                                                           | Result                            | Notes                                                                                    |
| -------------------- | ----------------------------------------------------------------- | --------------------------------- | ---------------------------------------------------------------------------------------- |
| `automation_list`    | -                                                                 | `AutomationRow[]`                 | by name; status from `status::compute` (design 9.1); one consistent snapshot             |
| `automation_get`     | `{ id }`                                                          | `AutomationDetail`                | pipeline, quiet countdown, current run, waiting entries, 4 recent runs, 14 events        |
| `automation_history` | `{ filter: 'all' \| 'attention', automationId?, before?, limit }` | `{ runs: Run[], next? }`          | newest first; `before` = run id cursor; `limit` 1..=100; `attention` = runs a hold names |
| `automation_run_ops` | `{ runId, offset, limit }`                                        | `{ ops: RunOp[], total, stored }` | `limit` 1..=200, `offset` 0..=5000; `total` = true count, `stored` = rows (first 5 000)  |
| `automation_events`  | `{ id, before?, limit }`                                          | `{ events: Event[], next? }`      | newest first; `before` = event id cursor; `limit` 1..=50                                 |

- **Status chip** (`{ kind, label, sub? }`): `kind` is one of `watching quiet checking device recheck unsupported
scheduled queued pending running paused completed partial cancelled skipped attention review warn`. Precedence
  (first match wins): Needs review, Runner not running (background only), Running, Queued / Waiting for device,
  Needs attention, Paused, trigger state. The verdict for "Needs review" is `policy::verify` of the stored
  approval - the `enabled` flag only decides "Paused" and is never an authorization; `canRunNow` is a hint (the
  gate checks the approval again on every run). Labels and sub-lines are built by the backend
  (`Running 3/6`, `Waiting for quiet · 7 s`, `Waits for "<run>" — same folders`, `<text> · new runs held`).
- **Run result chips:** Completed -> `completed`; Partial -> `partial`; Conflict held -> `attention`
  "Conflict held"; Failed -> `attention` "Failed"; Cancelled -> `cancelled`; Interrupted -> `cancelled`
  "Interrupted"; Skipped -> `skipped`. Counts are file operations, never filesystem events.
- **Pagination:** a page has `next` only when more rows exist; pass it back as `before`.
- **Queue (store, RQ-3):** one entry per profile (`UNIQUE(profile_id)`); a second trigger merges into it (count +
  distinct reasons, at most 10); entries start FIFO per host class and a blocked entry does not hold up a later one;
  the queue is stored and survives a restart; pause, edit, profile change and delete remove it.
- **Runs (store):** `runs.id` is `AUTOINCREMENT` (never reused); `run_ops` keeps the first 5 000 operations per run
  while `ops_total` and `counts` stay exact; Partial, Conflict held and Failed set the attention hold in the
  statement that ends the run, Interrupted and Cancelled never do (RQ-9.1/9.2); a run that is running or named by
  an attention hold is never deleted (OP-2; a hold that no longer parses protects every run of its automation).
- **Events (store):** the newest 200 per automation are kept (pruned on insert, ordered by insertion), text <= 512
  characters.
- **Controls (store, design 3.5):** a request older than 10 minutes (or stamped more than a minute in the future)
  is ignored and swept; a control is a request, never an authorization - the host runs the same gate.
- **Rules for the read side:** no lease id, MAC or key in any DTO; the approval key is loaded at most once per read
  and never created; the store closure never calls the engine or `KeyedAuth` (the store mutex is not re-entrant).
- Not yet built (later packages): the scheduler's next occurrence (`next` stays "No upcoming run" until P1.4, then
  the label gets the zone name). The host loop, the `automation://changed` / `automation://progress` events and the control
  commands (`automation_run_now`, `automation_stop_run`, ...) are P1.3, below.

## Automations: host loop and controls (automation P1.3)

Source of truth: `src-tauri/src/automation/host.rs` + `host/{gate,runner,request,engine,requeue,prod}.rs` (Tauri-free
cores) + `automation/commands.rs` (wrappers, events) + `src/lib/ipc/automation.ts`; fixtures
`contract-fixtures/automation/automation-{run-now*,stop-run,recheck,retry,event-*}.json`, read by both test suites.

The in-app host is started by `lib.rs` `setup` and stopped on `RunEvent::Exit`. Exactly one Ferrum instance per user
hosts (a `hosts` row, claimed in one immediate transaction; live = beat <= 15 s AND process alive); the others show the
same data and send controls. **Every command below is a request.** It queues an entry or writes a `controls` row; the
host passes it through the gate (design §10.4: enabled, runtime, `policy::verify`, attention, conditions, roots and
identity, delete-policy prerequisites, lease) before a file is touched. No parameter names a profile, a root, a
policy, a hash or a lease, none says "confirmed", and ids are validated (lowercase UUID v4 / run id >= 1) before any
lookup. Errors are `{ code, message }` without paths; no command can panic.

| Command               | Payload        | Result                                                                                | Notes                                                                                                                                                               |
| --------------------- | -------------- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `automation_run_now`  | `{ id }`       | `{ status: 'queued' \| 'waiting' \| 'merged' \| 'device' \| 'requested', waitsFor? }` | the toasts of RQ-4. App automations are queued here; a background one writes a control (`requested`, error `runner_down` if no runner is alive)                     |
| `automation_stop_run` | `{ runId }`    | `{ status: 'requested' }`                                                             | RQ-8.1; error `not_ready` when the run has ended                                                                                                                    |
| `automation_recheck`  | `{ runId }`    | `{ status: 'requested' }`                                                             | RQ-10.1; only for the run an attention hold names (`not_ready` otherwise); the answer is stored on the run (`recheck`) and announced by `automation://changed`      |
| `automation_retry`    | `{ runId }`    | `{ status: 'queued' \| 'waiting' \| 'merged' \| 'device' \| 'requested' }`            | RQ-10.2; needs a recheck that found work (`not_ready`), never for a conflict; the run executes a fresh plan with `retryOf`                                          |
| `automation_pause`    | `{ id, mode }` | `{ status: 'paused' }`                                                                | now also the run part (RQ-8.2): `stop` asks the host to stop the run that is going, `finish` leaves it; queued entries and the pending rerun are dropped either way |

Errors added: `held` (the automation needs attention; Run now is refused with the hold's text), `paused`, `runner_down`,
`not_ready` (`recheck_first`, `not_running`, `not_held`), `busy` (a retry cannot merge into another automation's
entry), `rate_limited` (more than 10 controls per second for one target), plus the existing `needs_review`, `not_found`,
`invalid_input`, `store_*`.

**Events** (both carry ids, a phase and counts only - never a path, MAC, key or lease id):

- `automation://changed` `{ seq }` - `meta.change_seq` moved. Emitted by EVERY instance (host or not) within 2 s of a
  change made by any process (MH-10). The webview re-reads what it shows.
- `automation://progress` `{ runId, automationId, phase, opsTotal, counts }` - `phase` is the run's phase column
  (`gate | compare | plan | sync | settle | done`); at most 5 per second per run; the run row itself is updated at most
  once a second. The per-operation rows of a run are written when it ends (the live view reads counts).

Behaviour the contract relies on:

- **One run at a time per host.** Queue entries of the host's class start in FIFO order; an entry the gate keeps waiting
  (folders busy, a folder offline, an unmet condition) does not hold up a later one and is asked again every 5 s.
- **Outcomes** (RQ-9): Completed, Partial, Conflict held, Failed, Cancelled, Interrupted, Skipped. Partial, Conflict
  held and Failed set the attention hold in the transaction that ends the run; new triggers are then removed with one
  event ("Trigger held") until a recheck that finds nothing left, or a successful retry ("Resolved by run #n"), releases
  the hold. A failed delete-policy prerequisite (no Recycle Bin, versioning folder unusable, protected folder) is a
  Failed run that never started. A guard that trips in an unattended run fails the run (SP-5.1); only the manual run can
  carry the native "yes".
- **Interrupted** never sets attention. A folder that disappears sends the automation to Waiting for device with a
  fresh-comparison entry queued; a run left `Running` by a dead host becomes Interrupted when the next host takes over,
  and its lease expires.
- **Pending rerun** (a file changed during the run) queues one run 10 s later, at most 5 in a row per automation - only when automatic runs are on (see below).
- **Merged triggers** (design §9.5): when the owner of a merged queue entry is paused, edited, deleted, held or in review,
  the merged triggers move to the first other healthy automation of the profile.

**Automatic runs are off.** While `UNATTENDED_RUNS_ENABLED` is false (the shipped state) the host starts the engine
only for a Run now or Retry the user asked for. A queue entry of any other kind (schedule, watch, catch-up, pending
rerun, "folder is back") is not started: it stays in the queue, `automation_list` shows `Queued` with the sub-line
"Waiting - run it with Run now", and one event says so. An entry the gate kept waiting (device, conditions, busy
folders) is not retried by itself; pressing Run now again asks again. A file that changed during a run, or a folder that
disappeared, is reported in the event log instead of queueing a rerun. Open conditions fail closed: unknown power status
counts as battery, and `skipOnMetered` waits until metered detection exists.
