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
