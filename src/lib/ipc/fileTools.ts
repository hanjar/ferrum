/**
 * File tools IPC (F2-24): checksum verify (read-only, direct) and the
 * queued producers (checksum create, split, combine). Mirrors
 * `src-tauri/src/file_tools.rs`; every payload is runtime-guarded.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError } from './types';
import type { OperationId } from './types';
import { isInTauri } from './tauri';

export type Algorithm = 'sha256' | 'sha1' | 'md5' | 'crc32';

/**
 * What to do when an output (sidecar, split parts, combined file) already
 * exists. `refuse` is the safe default; `rename` only applies to the combined
 * file (sidecar and part names are fixed, the backend refuses it for them).
 */
export type OutputPolicy = 'refuse' | 'skip' | 'overwrite' | 'overwriteOlder' | 'rename';

export interface ToolOutcome {
  path: string;
  /** ok | mismatch | missing | noManifest | failed | created | skipped */
  status: string;
  expected?: string;
  actual?: string;
  message?: string;
}

export interface VerifyReport {
  requested: number;
  ok: number;
  mismatched: number;
  outcomes: ToolOutcome[];
  /** A cancel stopped the run; `outcomes` only holds the items finished before it. */
  cancelled?: boolean;
}

export interface ArchiveMemberChecksum {
  archivePath: string;
  memberPath: string;
  algorithm: Algorithm;
  hash: string;
}

/** Live hashing progress (`filetools://checksum-progress`), keyed by request id. */
export interface ChecksumProgress {
  requestId: string;
  processedBytes: number;
  filesDone: number;
  filesTotal: number;
  /** Path (verify) or member path (archive) being hashed right now. */
  current: string;
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

function bad(shape: string): FsError {
  return new FsError({ code: 'io', message: `Malformed ${shape} from IPC` });
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined;
}

function num(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

function asOutcome(raw: unknown): ToolOutcome {
  if (typeof raw !== 'object' || raw === null) throw bad('ToolOutcome');
  const r = raw as Record<string, unknown>;
  const path = str(r.path);
  const status = str(r.status);
  if (!path || !status) throw bad('ToolOutcome');
  const out: ToolOutcome = { path, status };
  const expected = str(r.expected);
  const actual = str(r.actual);
  const message = str(r.message);
  if (expected !== undefined) out.expected = expected;
  if (actual !== undefined) out.actual = actual;
  if (message !== undefined) out.message = message;
  return out;
}

export function asVerifyReport(raw: unknown): VerifyReport {
  if (typeof raw !== 'object' || raw === null) throw bad('VerifyReport');
  const r = raw as Record<string, unknown>;
  const requested = num(r.requested);
  const ok = num(r.ok);
  const mismatched = num(r.mismatched);
  if (requested === undefined || ok === undefined || mismatched === undefined) {
    throw bad('VerifyReport');
  }
  const outcomes = Array.isArray(r.outcomes) ? r.outcomes.map(asOutcome) : [];
  const report: VerifyReport = { requested, ok, mismatched, outcomes };
  if (r.cancelled === true) report.cancelled = true;
  return report;
}

export function asArchiveMemberChecksum(raw: unknown): ArchiveMemberChecksum {
  if (typeof raw !== 'object' || raw === null) throw bad('ArchiveMemberChecksum');
  const r = raw as Record<string, unknown>;
  const archivePath = str(r.archivePath);
  const memberPath = str(r.memberPath);
  const algorithm = str(r.algorithm);
  const hash = str(r.hash);
  if (!archivePath || !memberPath || !hash) throw bad('ArchiveMemberChecksum');
  if (
    algorithm !== 'sha256' &&
    algorithm !== 'sha1' &&
    algorithm !== 'md5' &&
    algorithm !== 'crc32'
  ) {
    throw bad('ArchiveMemberChecksum');
  }
  return { archivePath, memberPath, algorithm, hash };
}

async function call<T>(
  command: string,
  args: Record<string, unknown>,
  guard: (raw: unknown) => T,
): Promise<T> {
  if (!isInTauri()) throw unsupported();
  let raw: unknown;
  try {
    raw = await invoke(command, args);
  } catch (e) {
    throw FsError.fromIPC(e);
  }
  return guard(raw);
}

/**
 * Verify the selection against sidecar manifests or manifests themselves.
 * With a `requestId`, `cancelChecksumRequest(requestId)` can stop the run;
 * the report then has `cancelled: true`.
 */
export function verifyChecksums(paths: string[], requestId?: string): Promise<VerifyReport> {
  return call('verify_checksums', { paths, requestId }, asVerifyReport);
}

/**
 * Subscribe to hashing progress. Events carry the `requestId` passed to
 * `verifyChecksums` / `checksumArchiveMember`; the handler only sees well-formed
 * payloads. Outside Tauri this is a no-op.
 */
export async function onChecksumProgress(
  handler: (p: ChecksumProgress) => void,
): Promise<UnlistenFn> {
  if (!isInTauri()) return () => {};
  return listen<unknown>('filetools://checksum-progress', (e) => {
    const r = e.payload;
    if (typeof r !== 'object' || r === null) return;
    const o = r as Record<string, unknown>;
    const requestId = str(o.requestId);
    const processedBytes = num(o.processedBytes);
    const filesDone = num(o.filesDone);
    const filesTotal = num(o.filesTotal);
    if (
      requestId === undefined ||
      processedBytes === undefined ||
      filesDone === undefined ||
      filesTotal === undefined
    ) {
      return;
    }
    handler({ requestId, processedBytes, filesDone, filesTotal, current: str(o.current) ?? '' });
  });
}

/** Stop a running verify / archive-member hash; false when nothing runs under that id. */
export function cancelChecksumRequest(requestId: string): Promise<boolean> {
  return call('cancel_checksum_request', { requestId }, (raw) => raw === true);
}

/**
 * Hash one archive member in place (streamed, nothing extracted). ZIP and 7z
 * only: the backend refuses RAR because its reader buffers the whole member.
 */
export function checksumArchiveMember(
  archivePath: string,
  memberPath: string,
  algorithm: Algorithm,
  options: { password?: string; requestId?: string } = {},
): Promise<ArchiveMemberChecksum> {
  return call(
    'checksum_archive_member',
    {
      archivePath,
      memberPath,
      algorithm,
      password: options.password,
      requestId: options.requestId,
    },
    asArchiveMemberChecksum,
  );
}

/** Queue sidecar creation (progress/cancel ride the operation queue). */
export function enqueueChecksumCreate(
  paths: string[],
  algorithm: Algorithm,
  policy: OutputPolicy = 'refuse',
): Promise<OperationId> {
  return call('enqueue_checksum_create', { paths, algorithm, policy }, (raw) => {
    if (typeof raw !== 'string') throw bad('OperationId');
    return raw;
  });
}

/** Queue a split into `partBytes` chunks inside `outDir`. */
export function enqueueSplitFile(
  path: string,
  partBytes: number,
  outDir: string,
  policy: OutputPolicy = 'refuse',
): Promise<OperationId> {
  return call('enqueue_split_file', { path, partBytes, outDir, policy }, (raw) => {
    if (typeof raw !== 'string') throw bad('OperationId');
    return raw;
  });
}

/** Queue combining `firstPart` (a `.001` file) into `outFile`. */
export function enqueueCombineParts(
  firstPart: string,
  outFile: string,
  policy: OutputPolicy = 'refuse',
): Promise<OperationId> {
  return call('enqueue_combine_parts', { firstPart, outFile, policy }, (raw) => {
    if (typeof raw !== 'string') throw bad('OperationId');
    return raw;
  });
}

// ── F2-27: Encode/Decode (MIME base64 / UUE / XXE — codecs.rs) ──────────

/** Queue encoding the selection to `<name>.b64` (MIME base64). */
export function enqueueEncodeFiles(paths: string[]): Promise<OperationId> {
  return call('enqueue_encode_files', { paths }, (raw) => {
    if (typeof raw !== 'string') throw bad('OperationId');
    return raw;
  });
}

/** Queue decoding the selection (format auto-detected: b64/UUE/XXE). */
export function enqueueDecodeFiles(paths: string[]): Promise<OperationId> {
  return call('enqueue_decode_files', { paths }, (raw) => {
    if (typeof raw !== 'string') throw bad('OperationId');
    return raw;
  });
}
