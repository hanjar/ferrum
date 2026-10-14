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

| Command                 | Payload                             | Result                                                                          | Notes                                                                                                |
| ----------------------- | ----------------------------------- | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `sync_profile_list`     | none                                | `ProfileSummary[]`                                                              | readiness per root with a 2 s budget; slower roots report `checking`                                 |
| `sync_profile_get`      | `{ id }`                            | `ProfileDto`                                                                    | policy + identities + per-revision changes list                                                      |
| `sync_profile_validate` | `{ draft }`                         | `{ errors: FieldError[], identities, kinds }`                                   | no side effects, no dialog                                                                           |
| `sync_profile_save`     | `{ id?, expectedRevision?, draft }` | `{ status: 'saved', profile } \| { status: 'canceled' } \| { status: 'stale' }` | native confirm only for `permanent` (new/changed) or guard off; revision bumps only on policy fields |
| `sync_profile_delete`   | `{ id, expectedRevision }`          | `{ status: 'deleted' }`                                                         | `in_use` once automations reference the profile (P1.1)                                               |
| `sync_lease_acquire`    | `{ left, right }`                   | `{ leaseId } \| { status: 'busy', holder: { kind, runId? } }`                   | cross-process lock on a folder pair (P0.4); `busy` is a normal answer, not an error                  |
| `sync_lease_release`    | `{ leaseId }`                       | `{}`                                                                            | idempotent; only the token handed out by `sync_lease_acquire` releases anything                      |

`draft` is `{ name, left, right, mode: 'oneWay'|'mirror'|'twoWay', from?, propagateDeletes?,
deletePolicy: 'recycleBin'|'versioning'|'permanent', versioningRoot?, compare:
'sizeAndDate'|'content', include: string[], exclude: string[], guard: { enabled, maxCount,
maxPercent } }`; unknown fields are refused (SE-1: no input may behave like a `confirmed` flag).

Errors cross as `{ code, message }` with a closed-set code (`invalid_input`, `not_found`,
`stale`, `in_use`, `busy`, `store_unavailable`, `store_untrusted`, `store_read_only`,
`needs_review`, `lease_busy`, `lease_required`); `busy` means another native confirmation dialog is open, so nothing was shown or saved
and never contain user paths. Ids are lowercase UUID v4 text, validated before any lookup.
`readiness` is `{ status: 'ready' } | { status: 'checking' } | { status: 'notReady', reason }`
with reasons `rootMissing | rootOffline | identityUnknown | identityMismatch |
recycleBinUnavailable | versioningRootInvalid | profileInvalid`. A network (UNC) root that cannot be
opened is saved with an unverified identity (no `volumeSerial`/`fileId`) and reports `rootOffline`;
one saved that way stays `identityUnknown` until it is saved again while reachable. The revision changes list shows, per revision, `{ field, from, to }` display
values (SP-6.3); the newest 50 revisions are kept.

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
  check is made in the backend. `sync_baseline_load` and `sync_baseline_clear` are unchanged.
- Manual Synchronize Dirs (and the chat sync) acquire the lease before the re-validation compare,
  hold it through the run and the baseline settle, and release it after the settle, on cancel, on
  error and on close. If the folders are busy nothing is compared or enqueued and the panel shows
  that they are in use.
