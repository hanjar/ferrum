/**
 * Pack service (F2-12, extracted by FER-180). The ONE place that turns "pack
 * these paths into an archive" into an engine call. The manual Pack dialog
 * and the AI facade adapter both go through here, so there is no second ZIP
 * path: this module only forwards to the existing `archive_create` IPC.
 *
 * Note: `archive_create` is awaited directly by the backend; it is NOT an
 * operation-queue job, but runs as a cancellable job with progress (FER-60/67).
 */
import * as archiveIpc from './ipc/archive';
import type {
  ArchiveCompressionLevel,
  ArchiveCreateConflict,
  ArchiveProgressEvent,
} from './ipc/archive';
import { cancelOccupiedSpace, occupiedSpace, volumeInfo } from './ipc/properties';
import { isInTauri } from './ipc/tauri';

export type PackFormat = 'zip' | 'sevenz' | 'rar';

export interface PackRequest {
  paths: readonly string[];
  /** Full destination file path of the archive. */
  target: string;
  format: PackFormat;
  /** Omit for the manual dialog's behavior (replace). Chat uses `skip`: never replace. */
  conflict?: ArchiveCreateConflict;
  requestId?: string;
  level?: ArchiveCompressionLevel;
  onProgress?: (event: ArchiveProgressEvent) => void;
}

export interface PackResult {
  outputPath: string;
  itemCount: number;
}

/** Default Pack destination for a pane: `<pane path>.zip` / `.7z` / `.rar`. */
export function defaultPackTarget(panePath: string, format: PackFormat): string {
  const ext = format === 'zip' ? '.zip' : format === 'sevenz' ? '.7z' : '.rar';
  return panePath.endsWith(ext) ? panePath : panePath + ext;
}

function freshRequestId(): string {
  if ('newArchiveRequestId' in archiveIpc && typeof archiveIpc.newArchiveRequestId === 'function') {
    return archiveIpc.newArchiveRequestId();
  }
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return `arc-${c.randomUUID()}`;
  return `arc-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** Create the archive. Resolves only after the engine finished; rejects with
 *  the engine's error. Never swallows a failure. */
export async function runPack(request: PackRequest): Promise<PackResult> {
  if (request.paths.length === 0) throw new Error('Nothing to pack: no source paths.');
  const paths = [...request.paths];
  const requestId = request.requestId ?? freshRequestId();
  let unlisten: (() => void) | undefined;
  if (
    request.onProgress &&
    'listenArchiveProgress' in archiveIpc &&
    typeof archiveIpc.listenArchiveProgress === 'function'
  ) {
    unlisten = await archiveIpc.listenArchiveProgress((ev) => {
      if (ev.requestId === requestId) {
        request.onProgress?.(ev);
      }
    });
  }
  try {
    const outputPath = request.conflict
      ? request.level !== undefined
        ? await archiveIpc.archiveCreate(
            paths,
            request.target,
            request.format,
            request.conflict,
            requestId,
            request.level,
          )
        : await archiveIpc.archiveCreate(
            paths,
            request.target,
            request.format,
            request.conflict,
            requestId,
          )
      : request.level !== undefined
        ? await archiveIpc.archiveCreate(
            paths,
            request.target,
            request.format,
            undefined,
            requestId,
            request.level,
          )
        : await archiveIpc.archiveCreate(
            paths,
            request.target,
            request.format,
            undefined,
            requestId,
          );
    return { outputPath, itemCount: paths.length };
  } finally {
    unlisten?.();
  }
}

/** Cancel a running pack job by its request id (FER-60/67). */
export async function cancelPack(requestId: string): Promise<void> {
  if (
    'archiveCancelRequest' in archiveIpc &&
    typeof archiveIpc.archiveCancelRequest === 'function'
  ) {
    await archiveIpc.archiveCancelRequest(requestId);
  }
}

/** Pack is a desktop service (`archive_create` over Tauri IPC); outside the app there is no engine. */
export function packServiceAvailable(): boolean {
  return isInTauri();
}

/**
 * Engine failure messages that are a contract with `src-tauri/src/archive_create_test.rs`
 * (`SOURCE_UNREADABLE`, `NO_SPACE`). Change both sides together.
 */
export const PACK_SOURCE_UNREADABLE = 'Source could not be read';
export const PACK_NO_SPACE = 'Not enough disk space on the destination drive';

export type PackFailureCause = 'sourceUnreadable' | 'noSpace' | 'permissionDenied' | 'other';

/** Classify a Pack failure without exposing the engine's raw message. */
export function packFailureCause(error: { code?: string; message?: string }): PackFailureCause {
  const message = error.message ?? '';
  if (message.startsWith(PACK_SOURCE_UNREADABLE)) return 'sourceUnreadable';
  if (message.startsWith(PACK_NO_SPACE)) return 'noSpace';
  if (error.code === 'permissionDenied') return 'permissionDenied';
  return 'other';
}

export interface PackMeasure {
  /** Uncompressed payload in bytes (files only; links are never followed, like the engine). */
  bytes: number;
  files: number;
  /** False when part of the tree could not be read or the walk ran out of time: `bytes` is partial. */
  complete: boolean;
  /** Items the walk could not read (paths stay local). */
  unreadable: string[];
  /** Free space on the destination volume, when known. */
  freeBytes?: number;
}

export interface MeasureOptions {
  /** Stop the walk after this long and report a partial size (default 3 s). */
  timeoutMs?: number;
}

let measureSeq = 0;

/**
 * Read-only size estimate for a Pack preview: the existing occupied-space walk over the same paths
 * plus the free space of the destination volume. Resolves `undefined` when the services are not
 * available (never throws): an estimate must not block a preview.
 */
export async function measurePack(
  paths: readonly string[],
  targetDirectory: string,
  options: MeasureOptions = {},
): Promise<PackMeasure | undefined> {
  if (!packServiceAvailable() || paths.length === 0) return undefined;
  const scanId = `pack-measure-${Date.now().toString(36)}-${(++measureSeq).toString(36)}`;
  const timer = setTimeout(() => void cancelOccupiedSpace(scanId), options.timeoutMs ?? 3000);
  try {
    const [report, volume] = await Promise.all([
      occupiedSpace(scanId, [...paths]),
      volumeInfo(targetDirectory).catch(() => undefined),
    ]);
    const measure: PackMeasure = {
      bytes: report.logicalBytes,
      files: report.files,
      complete: !report.cancelled && report.failed.length === 0,
      unreadable: report.failed.map((f) => f.path),
    };
    if (volume) measure.freeBytes = volume.freeBytes;
    return measure;
  } catch {
    return undefined;
  } finally {
    clearTimeout(timer);
  }
}
