/**
 * TS mirror of `src-tauri/src/domain.rs`. The fixtures in
 * `../../../contract-fixtures/` are the single source of truth both sides
 * test against — change one, change all three.
 */

export interface FileEntry {
  /** Basename for display. */
  name: string;
  /** Absolute path, platform separators — the identity for navigation/ops. */
  path: string;
  isDir: boolean;
  /** Symlink/junction itself; recursion never follows it (decision D07). */
  isSymlink: boolean;
  /** Bytes for files, 0 for directories. */
  size: number;
  /** Unix epoch milliseconds (safe below 2^53). */
  modifiedMs: number;
  hidden: boolean;
}

export type ErrorCode =
  | 'notFound'
  | 'permissionDenied'
  | 'invalidPath'
  | 'notADirectory'
  | 'alreadyExists'
  | 'cancelled'
  | 'unsupported'
  | 'busy'
  | 'io';

/** The only error shape that crosses IPC. */
export interface FsErrorShape {
  code: ErrorCode;
  message: string;
  path?: string;
  /** Structured archive error behind an archive failure (FER-73). Branch on this, never on the
   *  message text. */
  archiveCode?: ArchiveErrorCode;
  /** Offending member path inside an archive when one is known (FER-55). */
  memberPath?: string;
}

export class FsError extends Error {
  readonly code: ErrorCode;
  readonly path?: string;
  readonly archiveCode?: ArchiveErrorCode;
  readonly memberPath?: string;

  constructor(shape: FsErrorShape) {
    super(shape.message);
    this.name = 'FsError';
    this.code = shape.code;
    this.path = shape.path;
    this.archiveCode = shape.archiveCode;
    this.memberPath = shape.memberPath;
  }

  static fromIPC(raw: unknown): FsError {
    if (typeof raw === 'object' && raw !== null && 'code' in raw && 'message' in raw) {
      const r = raw as FsErrorShape;
      if (typeof r.code === 'string' && typeof r.message === 'string') {
        return new FsError({
          code: r.code,
          message: r.message,
          path: r.path,
          archiveCode: typeof r.archiveCode === 'string' ? r.archiveCode : undefined,
          memberPath: typeof r.memberPath === 'string' ? r.memberPath : undefined,
        });
      }
    }
    return new FsError({ code: 'io', message: String(raw) });
  }
}

// ── Operation queue contract (consumed from task 09) ────────────────────

export type OperationId = string;

export type OperationKind = 'copy' | 'move' | 'delete' | 'create' | 'rename';

export type OperationState = 'queued' | 'running' | 'paused' | 'completed' | 'failed' | 'cancelled';

/** One per-file failure attached to a terminal operation (F2-26 BTM). */
export interface OperationFailure {
  path: string;
  message: string;
}

export interface OperationProgress {
  operationId: OperationId;
  kind: OperationKind;
  state: OperationState;
  processedBytes: number;
  totalBytes?: number;
  processedFiles: number;
  totalFiles?: number;
  currentPath?: string;
  rateBytesPerSec?: number;
  /** F2-26 BTM traceability: failure reason, per-file failures, and
   *  enqueue/finish wall-clock stamps (epoch ms) for the history view. */
  error?: string;
  failures?: OperationFailure[];
  queuedAtMs?: number;
  finishedAtMs?: number;
}

// ── Runtime guards for data arriving over IPC ───────────────────────────

function str(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined;
}

function num(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

function bool(v: unknown): boolean | undefined {
  return typeof v === 'boolean' ? v : undefined;
}

/** Narrow an unknown payload to FileEntry; throws FsError('io') on garbage. */
export function asFileEntry(raw: unknown): FileEntry {
  if (typeof raw !== 'object' || raw === null) throw badEntry();
  const r = raw as Record<string, unknown>;
  const name = str(r.name);
  const path = str(r.path);
  const isDir = bool(r.isDir);
  const isSymlink = bool(r.isSymlink);
  const size = num(r.size);
  const modifiedMs = num(r.modifiedMs);
  const hidden = bool(r.hidden);
  if (
    name === undefined ||
    path === undefined ||
    isDir === undefined ||
    isSymlink === undefined ||
    size === undefined ||
    modifiedMs === undefined ||
    hidden === undefined
  ) {
    throw badEntry();
  }
  return { name, path, isDir, isSymlink, size, modifiedMs, hidden };
}

function badEntry(): FsError {
  return new FsError({ code: 'io', message: 'Malformed FileEntry from IPC' });
}

// ── Archive contract (F2-02; not yet registered as Tauri commands) ─────
// Command names for integration: archive_list, archive_read, archive_extract,
// archive_create, archive_test. Each receives its corresponding request type;
// archive_list returns ArchiveListResponse; archive_read returns bounded bytes;
// archive_extract/create return an operation ID; archive_test returns report.
// No archive operation is wired in this contract-only phase.
export type Location =
  { kind: 'disk'; path: string } | { kind: 'archive'; archivePath: string; memberPath: string };
export type ArchiveFormat = 'zip' | 'sevenz' | 'rar';
export type ArchiveOperationKind = 'extract' | 'create' | 'test';
export type ArchiveConflictPolicy = 'skip' | 'overwrite' | 'rename' | 'overwriteOlder';
export type ArchiveErrorCode =
  | 'unsupportedFormat'
  | 'unsupportedCodec'
  | 'passwordRequired'
  | 'badPassword'
  | 'corruptArchive'
  | 'unsafeEntry'
  | 'limitExceeded'
  | 'invalidMemberPath'
  | 'conflict'
  | 'cancelled'
  | 'multipartNotFirst'
  | 'multipartMissing'
  | 'io';

export interface ArchiveErrorShape {
  code: ArchiveErrorCode;
  message: string;
  archivePath?: string;
  memberPath?: string;
}
export class ArchiveError extends Error {
  readonly code: ArchiveErrorCode;
  constructor(readonly shape: ArchiveErrorShape) {
    super(shape.message);
    this.name = 'ArchiveError';
    this.code = shape.code;
  }
}
export interface ArchiveEntry {
  memberPath: string;
  name: string;
  isDir: boolean;
  size: number;
  modifiedMs: number | null;
}
export interface ArchiveCapabilities {
  format: ArchiveFormat;
  list: boolean;
  read: boolean;
  extract: boolean;
  create: boolean;
  test: boolean;
  encryptedRead: boolean;
  encryptedCreate: boolean;
  multiVolume: boolean;
}
export interface ArchiveListRequest {
  location: Extract<Location, { kind: 'archive' }>;
  requestToken: number;
}
export interface ArchiveListResponse extends ArchiveListRequest {
  entries: ArchiveEntry[];
}
export interface ArchiveReadRequest {
  location: Extract<Location, { kind: 'archive' }>;
  maxBytes: number;
}
export interface ArchiveReadResponse {
  bytes: number[];
  truncated: boolean;
}
export interface ArchiveOperationResponse {
  operationId: string;
}
export interface ArchiveLimits {
  maxEntries: number;
  maxTotalBytes: number;
  maxEntryBytes: number;
  maxDepth: number;
}
export interface ArchiveExtractRequest {
  archivePath: string;
  memberPaths: string[];
  destinationPath: string;
  conflict: ArchiveConflictPolicy;
  limits: ArchiveLimits;
}
export interface ArchiveCreateRequest {
  format: 'zip' | 'sevenz';
  sourcePaths: string[];
  destinationPath: string;
  conflict: ArchiveConflictPolicy;
}
export interface ArchiveTestRequest {
  archivePath: string;
}
export interface ArchiveTestReport {
  archivePath: string;
  entries: {
    memberPath: string;
    status: 'passed' | 'failed' | 'unsupported' | 'passwordRequired';
    error?: ArchiveErrorShape;
  }[];
}
export interface ArchiveProgress {
  operationId: string;
  kind: ArchiveOperationKind;
  processedBytes: number;
  totalBytes?: number;
  processedEntries: number;
  totalEntries?: number;
  currentMemberPath?: string;
}
export interface ArchiveContractFixture {
  diskLocation: Location;
  location: Extract<Location, { kind: 'archive' }>;
  entry: ArchiveEntry;
  capabilities: ArchiveCapabilities;
  listRequest: ArchiveListRequest;
  listResponse: ArchiveListResponse;
  readRequest: ArchiveReadRequest;
  extractRequest: ArchiveExtractRequest;
  createRequest: ArchiveCreateRequest;
  testRequest: ArchiveTestRequest;
  readResponse: ArchiveReadResponse;
  operationResponse: ArchiveOperationResponse;
  testResponse: ArchiveTestReport;
  progress: ArchiveProgress;
  error: ArchiveErrorShape;
}

function badArchive(message: string): never {
  throw new ArchiveError({ code: 'invalidMemberPath', message });
}
function record(raw: unknown): Record<string, unknown> {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw))
    badArchive('Malformed archive IPC payload');
  return raw as Record<string, unknown>;
}
function diskPath(raw: unknown): string {
  if (typeof raw !== 'string' || !raw.trim() || raw.includes('\0')) badArchive('Invalid disk path');
  return raw;
}
function natural(raw: unknown): number {
  if (typeof raw !== 'number' || !Number.isSafeInteger(raw) || raw < 0)
    badArchive('Invalid archive count');
  return raw;
}
const forbiddenMember = /[\\:<>"|?*]/;
const reservedMember = /^(?:CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³])(?:\.|$)/i;
/** Canonical POSIX-style relative member; empty denotes archive root. Never pass to disk APIs. */
export function validMemberPath(path: unknown): path is string {
  return (
    typeof path === 'string' &&
    (path === '' ||
      (!path.startsWith('/') &&
        !path.endsWith('/') &&
        path
          .split('/')
          .every(
            (part) =>
              part !== '' &&
              part !== '.' &&
              part !== '..' &&
              !forbiddenMember.test(part) &&
              !/\p{Cc}/u.test(part) &&
              !reservedMember.test(part) &&
              !/[. ]$/.test(part),
          )))
  );
}
export function asLocation(raw: unknown): Location {
  const r = record(raw);
  if (r.kind === 'disk') return { kind: 'disk', path: diskPath(r.path) };
  if (r.kind === 'archive') {
    const archivePath = diskPath(r.archivePath);
    if (!validMemberPath(r.memberPath)) badArchive('Invalid archive member path');
    return { kind: 'archive', archivePath, memberPath: r.memberPath };
  }
  return badArchive('Invalid location kind');
}
export function asArchiveEntry(raw: unknown): ArchiveEntry {
  const r = record(raw);
  if (
    !validMemberPath(r.memberPath) ||
    !r.memberPath ||
    r.name !== r.memberPath.split('/').at(-1) ||
    typeof r.isDir !== 'boolean' ||
    (r.modifiedMs !== null &&
      (typeof r.modifiedMs !== 'number' || !Number.isSafeInteger(r.modifiedMs)))
  ) {
    badArchive('Malformed archive entry');
  }
  return {
    memberPath: r.memberPath,
    name: r.name as string,
    isDir: r.isDir,
    size: natural(r.size),
    modifiedMs: r.modifiedMs as number | null,
  };
}
export function asArchiveCapabilities(raw: unknown): ArchiveCapabilities {
  const r = record(raw);
  if (
    !['zip', 'sevenz', 'rar'].includes(r.format as string) ||
    [
      'list',
      'read',
      'extract',
      'create',
      'test',
      'encryptedRead',
      'encryptedCreate',
      'multiVolume',
    ].some((k) => typeof r[k] !== 'boolean') ||
    (r.format === 'rar' && (r.create || r.encryptedCreate))
  )
    badArchive('Malformed archive capabilities');
  return r as unknown as ArchiveCapabilities;
}
export function asArchiveListResponse(raw: unknown): ArchiveListResponse {
  const r = record(raw);
  const location = asLocation(r.location);
  if (location.kind !== 'archive' || !Array.isArray(r.entries))
    badArchive('Malformed archive listing');
  const entries = r.entries.map(asArchiveEntry);
  for (const entry of entries) {
    const prefix = location.memberPath ? `${location.memberPath}/` : '';
    if (!entry.memberPath.startsWith(prefix) || entry.memberPath.slice(prefix.length).includes('/'))
      badArchive('Entry outside requested directory');
  }
  return { location, requestToken: natural(r.requestToken), entries };
}
/** Archive root parent is disk directory containing archive, not OS member path. */
export function archiveParent(location: Extract<Location, { kind: 'archive' }>): Location {
  if (location.memberPath) {
    const lastSlash = location.memberPath.lastIndexOf('/');
    return {
      ...location,
      memberPath: lastSlash < 0 ? '' : location.memberPath.slice(0, lastSlash),
    };
  }
  const slash = Math.max(
    location.archivePath.lastIndexOf('/'),
    location.archivePath.lastIndexOf('\\'),
  );
  if (slash < 0) badArchive('Archive has no disk parent');
  return {
    kind: 'disk',
    path:
      slash === 0 || /^[A-Za-z]:$/.test(location.archivePath.slice(0, slash))
        ? location.archivePath.slice(0, slash + 1)
        : location.archivePath.slice(0, slash),
  };
}
export function childArchiveLocation(
  location: Extract<Location, { kind: 'archive' }>,
  name: string,
): Extract<Location, { kind: 'archive' }> {
  if (!validMemberPath(name) || !name || name.includes('/'))
    badArchive('Child must be one safe member name');
  return { ...location, memberPath: location.memberPath ? `${location.memberPath}/${name}` : name };
}

// ── Disk usage contract (DSK-02 events + snapshot; DSK-03 queries) ──────
// Mirrors `src-tauri/src/disk_usage.rs`. Commands: disk_usage_start(root,
// options) -> scanId, disk_usage_cancel(scanId) -> boolean,
// disk_usage_snapshot(scanId) -> DiskUsageSnapshot | null, and the DSK-03
// queries disk_usage_query_children(scanId, nodeId, offset, limit, sort?,
// filter?) -> DiskUsageChildrenPage and disk_usage_top_files(scanId,
// nodeId, limit, filter?) -> DiskUsageTopFiles. Progress streams as
// `disk_usage://progress` (≤10/s per scan). No UI wiring here (DSK-04).
//
// Number safety (contract §2.6): byte totals are integers ≤ 2^53 − 1
// (Number.MAX_SAFE_INTEGER, ≈ 8 PiB) — internal Rust u64 stays representable
// for any real scan scope; never compute beyond it client-side.

export type DiskUsageCoverage = 'complete' | 'partial' | 'stale';

export interface DiskUsageScanOptions {
  /** Bound recursion depth; null/undefined = unlimited (D07 already
   *  prevents cycles). Hitting the cap reports coverage 'partial'. */
  maxDepth?: number | null;
}

/** One failed entry, aggregated per path (bounded list on the wire; the
 *  true count rides `totalErrors`). Permission denied is never counted as
 *  0 bytes (contract §3.6). */
export interface DiskUsageScanError {
  path: string;
  code: ErrorCode;
  message: string;
}

/** Aggregates for one directory. Subtree totals = direct entries + child
 *  subtrees; the parent is never re-added at the root (contract §3.7).
 *  `*AllocatedBytes: null` = at least one file had no OS allocation data —
 *  never all-or-nothing (gap G2) and never filled with logical bytes. */
export interface DiskUsageNode {
  /** Stable path-derived identity (the absolute path itself). */
  id: string;
  path: string;
  name: string;
  parentId?: string;
  childrenIds: string[];
  directFiles: number;
  /** Symlink/junction entries counted here (never followed, D07). */
  directSymlinks: number;
  directLogicalBytes: number;
  directAllocatedBytes: number | null;
  subtreeFiles: number;
  subtreeDirs: number;
  subtreeSymlinks: number;
  subtreeLogicalBytes: number;
  subtreeAllocatedBytes: number | null;
  subtreeErrors: number;
  /** Child dirs on other volumes — counted, never crossed (gap G3). */
  skippedVolumeBoundaries: number;
  subtreeSkippedVolumes: number;
}

/** Progressive `disk_usage://progress` payload. Exactly one event per scan
 *  sets `done`; a cancelled scan is done + cancelled + coverage 'partial',
 *  never 'complete'. No percent-done, no ETA (totals unknown mid-walk). */
export interface DiskUsageProgressEvent {
  scanId: string;
  scannedFiles: number;
  scannedDirs: number;
  scannedSymlinks: number;
  knownLogicalBytes: number;
  knownAllocatedBytes: number | null;
  allocatedAvailableFiles: number;
  allocatedUnavailableFiles: number;
  coverage: DiskUsageCoverage;
  errors: DiskUsageScanError[];
  totalErrors: number;
  /** Largest top-level children (bytes desc, id asc tie-break), bounded;
   *  full query/sort/pagination is DSK-03. */
  topChildren: DiskUsageNode[];
  done: boolean;
  cancelled: boolean;
  error?: string;
  durationMs: number;
  /** FER-460: engine behind the event ('ntfsMft' = elevated MFT helper). */
  engine?: DiskUsageScanEngine;
  /** FER-460: short note, e.g. why a requested fast scan fell back. */
  notice?: string;
  /** FER-460: MFT records read / total (ntfsMft only). The MFT size is known
   *  up front, so a percent of the READ phase is honest; done === total
   *  means the tree is being built. Absent on walk events. */
  mftRecordsDone?: number;
  mftRecordsTotal?: number;
}

/** FER-476: 'index' = FerrumIndexer (no elevation). */
export type DiskUsageScanEngine = 'walk' | 'ntfsMft' | 'index';

/** FER-464: a cached scan of a root outside the index, registered as a normal
 *  stored snapshot (queries work unchanged). */
export interface DiskUsageCacheLoad {
  summary: DiskUsageSnapshot;
  engine: DiskUsageScanEngine;
  /** An in-app delete/rename/move touched the root after the scan. */
  mutated: boolean;
}

/** FER-464: header data of one cached root (Settings). */
export interface DiskUsageCacheEntry {
  rootPath: string;
  scannedAtMs: number;
  engine: DiskUsageScanEngine;
  sizeBytes: number;
  mutated: boolean;
}

/** FER-460: may this root be offered the elevated fast NTFS scan? */
export interface DiskUsageFastEligibility {
  eligible: boolean;
  /** Drive letter when the root is on a local NTFS volume. */
  volume?: string;
  reason?: string;
}

export interface DiskUsageStats {
  scannedFiles: number;
  scannedDirs: number;
  scannedSymlinks: number;
  skippedVolumeBoundaries: number;
  logicalBytes: number;
  allocatedAvailableFiles: number;
  allocatedUnavailableFiles: number;
  knownAllocatedBytes: number | null;
  totalErrors: number;
  coverage: DiskUsageCoverage;
  cancelled: boolean;
  depthCapped: boolean;
  durationMs: number;
}

/** Summary of a finished scan (`disk_usage_snapshot(scanId)`), kept
 *  for a bounded number of recent scans. FER-479: the response is BOUNDED —
 *  only the scan root's aggregates travel (its `childrenIds` is always
 *  empty); a whole-drive scan has millions of nodes, which stay in Rust and
 *  are read level by level through disk_usage_query_children. `version` + `scannedAtMs` identify
 *  the generation (DSK-03): keep selections while `version` is unchanged;
 *  an evicted `scanId` makes every query fail with a structured `NotFound`
 *  (UI: rescan). Direct file listings are NOT serialized here — query them
 *  via disk_usage_query_children / disk_usage_top_files. */
export interface DiskUsageSnapshot {
  scanId: string;
  rootPath: string;
  rootId: string;
  /** Monotonic snapshot generation (gap G8) — never reused in a process. */
  version: number;
  /** Wall-clock ms (UNIX epoch) when the scan finished. */
  scannedAtMs: number;
  /** Aggregates of the scan root (`childrenIds` empty on the wire). */
  rootNode: DiskUsageNode;
  stats: DiskUsageStats;
}

// ── DSK-03 query commands (FER-321): largest-first + top files ──────────
// Both queries read PURELY from the stored snapshot — the filesystem is
// never re-walked, so tree and Largest files always come from the SAME scan
// and totals can never inflate. Unknown/evicted scanId or a nodeId outside
// the scanned tree rejects with FsError { code: 'notFound' }.

/** Byte metric for sort/threshold/percent. Selecting `allocated` NEVER
 *  substitutes logical bytes for unknown values (contract §2.2). */
export type DiskUsageSizeMetric = 'logical' | 'allocated';

/** Extension-based classification (DSK-03.2); unknown/missing ext → 'other'. */
export type DiskUsageFileCategory = 'video' | 'image' | 'audio' | 'document' | 'archive' | 'other';

/** Children sort. Metric sorts tie-break by stable id ascending so pages
 *  are reproducible across calls (contract §3.8). */
export type DiskUsageSort = 'largestFirst' | 'smallestFirst' | 'nameAsc';

export type DiskUsageEntryKind = 'dir' | 'file' | 'symlink';

/** Optional query filters — all present fields are ANDed. */
export interface DiskUsageFilter {
  /** Default 'logical'. */
  metric?: DiskUsageSizeMetric;
  /** Minimum size under the selected metric. Entries with UNKNOWN allocated
   *  bytes fail the threshold (unknown is never guessed). */
  minBytes?: number | null;
  /** Case-insensitive substring match on the entry name. */
  nameSubstring?: string | null;
  /** File extensions, with/without dot, case-insensitive; files only. */
  extensions?: string[] | null;
  /** Categories; files only (dirs/symlinks are never classified). */
  categories?: DiskUsageFileCategory[] | null;
}

/** One row of disk_usage_query_children: a direct child directory (subtree
 *  aggregates), direct file, or direct symlink (0 bytes, never followed). */
export interface DiskUsageChildItem {
  id: string;
  name: string;
  kind: DiskUsageEntryKind;
  logicalBytes: number;
  /** null = unavailable (per-node null semantics, gap G2) — never logical
   *  bytes in disguise. */
  allocatedBytes: number | null;
  /** False exactly when allocatedBytes is null. */
  allocatedSupported: boolean;
  category: DiskUsageFileCategory;
  /** null = unknown (partial coverage, zero denominator, unknown allocated)
   *  — render "—", NEVER 0% (contract §2.7). */
  percentOfParent: number | null;
  /** Independent of percentOfParent: denominator is the SCAN ROOT total. */
  percentOfScanRoot: number | null;
  /** FER-479: subtree counts of a child DIRECTORY (absent for files and
   *  symlinks) — the tree renders counts/error badges from the page itself,
   *  never from a client-side copy of the whole node map. */
  subtreeFiles?: number;
  subtreeDirs?: number;
  subtreeErrors?: number;
  /** Subtree items loaded for multi-level nested treemap views. */
  children?: DiskUsageChildItem[];
}

/** Page of disk_usage_query_children (largest-first by default). */
export interface DiskUsageChildrenPage {
  scanId: string;
  nodeId: string;
  /** Snapshot generation — keep selections while unchanged. */
  version: number;
  scannedAtMs: number;
  metric: DiskUsageSizeMetric;
  coverage: DiskUsageCoverage;
  offset: number;
  /** Applied page size (clamped to 1000). */
  limit: number;
  /** Children matching the filter — the paging denominator. */
  total: number;
  items: DiskUsageChildItem[];
}

/** One row of disk_usage_top_files — files only, whole subtree. */
export interface DiskUsageFileItem {
  id: string;
  parentId: string;
  name: string;
  logicalBytes: number;
  allocatedBytes: number | null;
  allocatedSupported: boolean;
  category: DiskUsageFileCategory;
  /** Denominator: the file's DIRECT parent directory total. */
  percentOfParent: number | null;
  /** Denominator: the SCAN ROOT total (independent, §2.7). */
  percentOfScanRoot: number | null;
}

/** Result of disk_usage_top_files: the N largest files of the whole
 *  subtree, recursive, files only (symlinks excluded — never followed). */
export interface DiskUsageTopFiles {
  scanId: string;
  nodeId: string;
  version: number;
  scannedAtMs: number;
  metric: DiskUsageSizeMetric;
  coverage: DiskUsageCoverage;
  /** Applied limit (clamped to 1000). */
  limit: number;
  /** Matches in the whole subtree before the cap. */
  totalMatching: number;
  truncated: boolean;
  /** "Top N" — render verbatim; NOT a claim of all results (§3.8). */
  label: string;
  items: DiskUsageFileItem[];
}

/** One active tab per pane/controller; tokens monotonic, never reused on path/tab change. */
export class ArchiveListingGate {
  private nextToken = 0;
  private activeTab: string | undefined;
  private state = new Map<string, { location: Location; token: number }>();

  activate(tab: string, location: Location): void {
    this.activeTab = tab;
    this.state.set(tab, { location: asLocation(location), token: ++this.nextToken });
  }
  request(tab: string): number {
    if (this.activeTab !== tab || !this.state.has(tab)) badArchive('Inactive listing tab');
    const token = ++this.nextToken;
    this.state.get(tab)!.token = token;
    return token;
  }
  accept(tab: string, raw: unknown): boolean {
    const current = this.state.get(tab);
    if (tab !== this.activeTab || !current || !raw || typeof raw !== 'object') return false;
    const reply = raw as Record<string, unknown>;
    if (reply.requestToken !== current.token) return false;
    const response = asArchiveListResponse(raw);
    return (
      current.location.kind === 'archive' &&
      current.location.archivePath === response.location.archivePath &&
      current.location.memberPath === response.location.memberPath
    );
  }
  close(tab: string): void {
    this.state.delete(tab);
    if (tab === this.activeTab) this.activeTab = undefined;
  }
}
