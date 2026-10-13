/** Compare IPC (F2-18) — typed wrappers over the read-only Rust engines. */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

export type DirDiffState =
  | 'leftOnly'
  | 'rightOnly'
  | 'same'
  | 'newerLeft'
  | 'newerRight'
  | 'sizeDifferent'
  | 'contentDifferent';

/** One line of a bounded text diff (FER-106). */
export interface TextDiffLine {
  kind: 'ctx' | 'del' | 'add';
  text: string;
}
export interface TextDiffHunk {
  /** 1-based first line in the left / right file covered by this hunk. */
  leftStart: number;
  rightStart: number;
  lines: TextDiffLine[];
}
/** Limited text diff; absent for binary files. Bounded by lines and bytes. */
export interface TextDiff {
  hunks: TextDiffHunk[];
  /** True when the diff was cut at the engine limit. */
  truncated: boolean;
  /** Encoding actually used to decode both sides: 'utf-8' | 'utf-16le' | 'utf-16be' | 'latin1'. */
  encoding: string;
}
export type CompareEncoding = 'auto' | 'utf-8' | 'utf-16le' | 'utf-16be' | 'latin1';

/** Cooperative progress for long compares, emitted on the `compare://progress` event. */
export interface CompareProgress {
  opId: string;
  phase: 'scan' | 'content';
  done: number;
  total?: number;
  current?: string;
}
export const COMPARE_PROGRESS_EVENT = 'compare://progress';

export interface CompareFilesOptions {
  opId?: string;
  encoding?: CompareEncoding;
  /** Context lines around each hunk (default 3). */
  diffContext?: number;
}
export interface CompareDirsOptions {
  /** Descend into directories present on both sides (default false). */
  recursive?: boolean;
  opId?: string;
  /** Bypass the engine verdict cache (sync by content must re-read bytes). */
  noCache?: boolean;
  /** FER-472: let the engine read local NTFS roots through the elevated MFT helper
   *  (one UAC prompt per volume). Falls back to the ordinary scan, see `report.fastScan.note`. */
  fastScan?: boolean;
  /** FER-477b: recursive compare by content for sync — use the FerrumIndexer change lists since
   *  the stored baseline's journal marks to skip content reads they prove unnecessary, and return
   *  fresh marks in `report.journal`. Any problem = the ordinary compare. */
  incremental?: boolean;
}

/** FER-477b: one side's USN journal position. Decimal strings: u64 does not fit a JS number. */
export interface JournalMark {
  journalId: string;
  usn: string;
  /** File reference of the root at the mark (another one later = rescan). */
  rootFrn: string;
  /** Canonical drive path of the root at the mark (`X:\a\b`, junctions resolved). */
  path: string;
}

/** Both roots' marks, taken by the engine BEFORE the compare walked. */
export interface JournalMarks {
  left: JournalMark;
  right: JournalMark;
  /** When they were taken (ms since epoch); they expire after 7 days. */
  takenAtMs: number;
}

/** FER-477b: whether the change lists were used and how many content reads they saved. */
export interface IncrementalInfo {
  used: boolean;
  /** Code only (`no_baseline_marks`, `service_unavailable`, `left:<reason>`, …). */
  reason?: string;
  contentSkipped: number;
}

/** FER-472: which sides were served from the MFT, and why not when the fast scan was asked. */
export interface FastScanInfo {
  left: boolean;
  right: boolean;
  note?: string;
}

export interface CompareResult {
  equal: boolean;
  /** True when cancelled before a verdict; `equal`/`firstMismatch` are then not meaningful. */
  cancelled?: boolean;
  /** True when the verdict came from the (path,size,mtime)-keyed cache. */
  fromCache?: boolean;
  textDiff?: TextDiff;
  firstMismatch?: number;
  leftSize: number;
  rightSize: number;
  leftLine?: number;
  rightLine?: number;
  bothText: boolean;
}

export interface DirDiffEntry {
  /** Name, or '/'-separated path relative to both roots when `recursive`. */
  name: string;
  /** True when the entry is a directory (a one-sided directory is one entry, not expanded). */
  isDir?: boolean;
  state: DirDiffState;
  leftSize: number;
  leftModifiedMs: number;
  rightSize: number;
  rightModifiedMs: number;
}

export interface CompareDirsReport {
  leftRoot: string;
  rightRoot: string;
  entries: DirDiffEntry[];
  truncated: boolean;
  recursive?: boolean;
  /** True when cancelled; entries are then a partial result and must not drive a sync. */
  cancelled?: boolean;
  /** Entries that could not be read (permission, vanished); never silently dropped. */
  errors?: { name: string; message: string }[];
  /** Present only when the fast (MFT) scan was requested. */
  fastScan?: FastScanInfo;
  /** FER-477b: present only for an incremental compare whose service answered for both roots. */
  journal?: JournalMarks;
  /** FER-477b: present only when the incremental compare was requested. */
  incremental?: IncrementalInfo;
}

const DECIMAL = /^[0-9]{1,20}$/;

function parseMark(v: unknown): JournalMark | undefined {
  if (typeof v !== 'object' || v === null) return undefined;
  const m = v as Record<string, unknown>;
  const { journalId, usn, rootFrn, path } = m;
  if (typeof journalId !== 'string' || typeof usn !== 'string') return undefined;
  if (typeof rootFrn !== 'string' || typeof path !== 'string') return undefined;
  if (!DECIMAL.test(journalId) || !DECIMAL.test(usn) || !DECIMAL.test(rootFrn)) return undefined;
  if (path.length === 0 || path.length > 1024) return undefined;
  return { journalId, usn, rootFrn, path };
}

/** Strict: both sides or nothing (a malformed mark only costs a full content compare). */
export function parseJournalMarks(v: unknown): JournalMarks | undefined {
  if (typeof v !== 'object' || v === null) return undefined;
  const m = v as Record<string, unknown>;
  const left = parseMark(m.left);
  const right = parseMark(m.right);
  const takenAtMs = m.takenAtMs;
  if (typeof takenAtMs !== 'number' || !Number.isSafeInteger(takenAtMs) || takenAtMs < 0)
    return undefined;
  return left && right ? { left, right, takenAtMs } : undefined;
}

function parseIncremental(v: unknown): IncrementalInfo | undefined {
  if (typeof v !== 'object' || v === null) return undefined;
  const m = v as Record<string, unknown>;
  return {
    used: m.used === true,
    ...(typeof m.reason === 'string' && /^[a-z_:]{1,64}$/.test(m.reason)
      ? { reason: m.reason }
      : {}),
    contentSkipped:
      typeof m.contentSkipped === 'number' && Number.isFinite(m.contentSkipped)
        ? m.contentSkipped
        : 0,
  };
}

function parseFastScan(v: unknown): FastScanInfo | undefined {
  if (typeof v !== 'object' || v === null) return undefined;
  const m = v as Record<string, unknown>;
  return {
    left: m.left === true,
    right: m.right === true,
    ...(typeof m.note === 'string' ? { note: m.note } : {}),
  };
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

function asGuarded<T>(raw: unknown, shape: (r: Record<string, unknown>) => T): T {
  if (typeof raw !== 'object' || raw === null) throw FsError.fromIPC(raw);
  try {
    return shape(raw as Record<string, unknown>);
  } catch {
    throw new FsError({ code: 'io', message: 'Malformed compare payload from IPC' });
  }
}

export async function compareFiles(
  pathA: string,
  pathB: string,
  opts: CompareFilesOptions = {},
): Promise<CompareResult> {
  if (!isInTauri()) throw unsupported();
  try {
    const raw = await invoke<unknown>('compare_files', {
      pathA,
      pathB,
      opId: opts.opId ?? null,
      encoding: opts.encoding ?? null,
      diffContext: opts.diffContext ?? null,
    });
    return asGuarded(raw, (r) => {
      if (typeof r.equal !== 'boolean' || typeof r.leftSize !== 'number') throw new Error();
      return {
        equal: r.equal,
        firstMismatch: typeof r.firstMismatch === 'number' ? r.firstMismatch : undefined,
        leftSize: r.leftSize,
        rightSize: typeof r.rightSize === 'number' ? r.rightSize : 0,
        leftLine: typeof r.leftLine === 'number' ? r.leftLine : undefined,
        rightLine: typeof r.rightLine === 'number' ? r.rightLine : undefined,
        bothText: r.bothText === true,
        cancelled: r.cancelled === true ? true : undefined,
        fromCache: r.fromCache === true ? true : undefined,
        textDiff: (r.textDiff as TextDiff | undefined) ?? undefined,
      };
    });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

const DIFF_STATES: readonly DirDiffState[] = [
  'leftOnly',
  'rightOnly',
  'same',
  'newerLeft',
  'newerRight',
  'sizeDifferent',
  'contentDifferent',
];

export async function compareDirs(
  left: string,
  right: string,
  byContent: boolean,
  opts: CompareDirsOptions = {},
): Promise<CompareDirsReport> {
  if (!isInTauri()) throw unsupported();
  try {
    const raw = await invoke<unknown>('compare_dirs', {
      left,
      right,
      byContent,
      recursive: opts.recursive ?? false,
      opId: opts.opId ?? null,
      ...(opts.noCache ? { noCache: true } : {}),
      ...(opts.fastScan ? { fastScan: true } : {}),
      ...(opts.incremental ? { incremental: true } : {}),
    });
    return asGuarded(raw, (r) => {
      if (typeof r.leftRoot !== 'string' || typeof r.rightRoot !== 'string') throw new Error();
      if (!Array.isArray(r.entries) || typeof r.truncated !== 'boolean') throw new Error();
      const entries = r.entries.map((e: unknown): DirDiffEntry => {
        if (typeof e !== 'object' || e === null) throw new Error();
        const m = e as Record<string, unknown>;
        if (typeof m.name !== 'string' || !DIFF_STATES.includes(m.state as DirDiffState))
          throw new Error();
        return {
          name: m.name,
          isDir: m.isDir === true ? true : undefined,
          state: m.state as DirDiffState,
          leftSize: typeof m.leftSize === 'number' ? m.leftSize : 0,
          leftModifiedMs: typeof m.leftModifiedMs === 'number' ? m.leftModifiedMs : 0,
          rightSize: typeof m.rightSize === 'number' ? m.rightSize : 0,
          rightModifiedMs: typeof m.rightModifiedMs === 'number' ? m.rightModifiedMs : 0,
        };
      });
      return {
        leftRoot: r.leftRoot,
        rightRoot: r.rightRoot,
        entries,
        truncated: r.truncated,
        recursive: r.recursive === true ? true : undefined,
        cancelled: r.cancelled === true ? true : undefined,
        errors: Array.isArray(r.errors)
          ? (r.errors as { name: string; message: string }[])
          : undefined,
        fastScan: parseFastScan(r.fastScan),
        ...(parseJournalMarks(r.journal) ? { journal: parseJournalMarks(r.journal) } : {}),
        ...(parseIncremental(r.incremental)
          ? { incremental: parseIncremental(r.incremental) }
          : {}),
      };
    });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Ask the engine to stop an in-flight compare started with the same `opId`. */
export async function cancelCompare(opId: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('compare_cancel', { opId });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Drop cached content verdicts (they are also invalidated by size/mtime change). */
export async function clearCompareCache(): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('compare_cache_clear');
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Reject same / nested / aliased roots before a sync is planned or run. */
export async function validateSyncRoots(left: string, right: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('validate_sync_roots', { left, right });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
