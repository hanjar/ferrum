/**
 * Archive TS bridge (F2-12): typed wrappers over the archive Tauri commands.
 * Mirrors the contract in `archive_contract.rs` (F2-02/c074300).
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError } from './types';
import { isInTauri } from './tauri';

/** Matches `Location` enum in archive_contract.rs (serde tag = "kind"). */
export type ArchiveLocation =
  { kind: 'disk'; path: string } | { kind: 'archive'; archivePath: string; memberPath: string };

export interface ArchiveEntry {
  memberPath: string;
  name: string;
  isDir: boolean;
  size: number;
  modifiedMs: number | null;
}

export interface ArchiveListResponse {
  location: ArchiveLocation;
  requestToken: number;
  entries: ArchiveEntry[];
}

export interface ArchiveTestReport {
  archivePath: string;
  entries: {
    memberPath: string;
    status: 'passed' | 'failed' | 'unsupported' | 'passwordRequired';
    error?: { code: string; message: string; archivePath?: string; memberPath?: string };
  }[];
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Archive operations require the desktop app',
  });
}

function wrap<T>(promise: Promise<T>): Promise<T> {
  return promise.catch((e) => {
    throw FsError.fromIPC(e);
  });
}

export async function archiveList(
  archivePath: string,
  memberPath: string,
  requestToken: number,
  /** FER-504: the password that unlocked this archive, for header-encrypted
   *  RAR/7z. Memory-only upstream; sent to this command and nowhere else. */
  password?: string,
): Promise<ArchiveListResponse> {
  if (!isInTauri()) throw unsupported();
  const location: ArchiveLocation = { kind: 'archive', archivePath, memberPath };
  return wrap(invoke('archive_list', { location, requestToken, password: password ?? null }));
}

export async function archiveRead(
  archivePath: string,
  memberPath: string,
  maxBytes: number,
  password?: string,
  requestId?: string,
): Promise<{ bytes: number[]; truncated: boolean }> {
  if (!isInTauri()) throw unsupported();
  const location: ArchiveLocation = { kind: 'archive', archivePath, memberPath };
  return wrap(
    invoke('archive_read', { location, maxBytes, password: password ?? null, requestId }),
  );
}

export async function archiveExtract(
  archivePath: string,
  memberPaths: string[],
  destinationPath: string,
  conflict: 'skip' | 'overwrite' | 'rename' | 'overwriteOlder',
  password?: string,
  requestId?: string,
): Promise<void> {
  if (!isInTauri()) throw unsupported();
  await wrap(
    invoke('archive_extract', {
      requestId: requestId ?? null,
      request: {
        archivePath,
        memberPaths,
        destinationPath,
        conflict,
        password: password ?? null,
        limits: {
          maxEntries: 100_000,
          maxTotalBytes: 2 ** 53,
          maxEntryBytes: 512 * 1024 * 1024,
          maxDepth: 32,
        },
      },
    }),
  );
}

export async function archiveCreateZip(
  sourcePaths: string[],
  destinationPath: string,
): Promise<string> {
  return archiveCreate(sourcePaths, destinationPath, 'zip');
}

/** F2-12: create an archive of `format` from `sourcePaths`. RAR is
 *  deliberately unsupported by the engine (licensing). */
/** What `archive_create` does when the destination already exists (FER-411):
 *  `overwrite` replaces it (the manual Pack dialog's explicit choice, and the default), `skip` never
 *  replaces it and rejects with `FsError { code: 'alreadyExists' }`, `rename` takes the first free
 *  `name (n).ext` and resolves with the path actually written. */
export type ArchiveCreateConflict = 'skip' | 'overwrite' | 'rename';

export type ArchiveCompressionLevel = 'store' | 'fast' | 'normal' | 'max';

export type ArchiveCreateFormat = 'zip' | 'sevenz' | 'rar';

export interface RarToolProbeResult {
  ok: boolean;
  version?: string;
  reason?: string;
}

export async function archiveRarToolProbe(): Promise<RarToolProbeResult> {
  if (!isInTauri()) {
    return { ok: false, reason: 'Rar probe requires the desktop app' };
  }
  return wrap(invoke('archive_rar_tool_probe'));
}

export async function archiveRarToolDetect(): Promise<string | null> {
  if (!isInTauri()) return null;
  return wrap(invoke('archive_rar_tool_detect'));
}

/** FER-511: status of the configured Rar.exe. `unset` = nothing stored; `unconfirmed` = a path
 *  is stored without a confirmed fingerprint (legacy config or changed file — it will NOT run);
 *  `changed` = the file no longer matches the confirmation; `ready` = confirmed and usable. */
export type RarToolStatusState = 'unset' | 'unconfirmed' | 'changed' | 'ready';

export interface RarToolStatus {
  path: string | null;
  state: RarToolStatusState;
}

/** FER-511: read-only; hashes the configured exe but never runs it. */
export async function archiveRarToolStatus(): Promise<RarToolStatus> {
  if (!isInTauri()) return { path: null, state: 'unset' };
  return wrap(invoke('archive_rar_tool_status'));
}

export interface RarToolSetPathResult {
  saved: boolean;
}

/** FER-511: the ONLY way to change the configured Rar.exe path. A non-null path pops a NATIVE
 *  confirmation dialog (never a webview surface) before anything is stored; resolving the
 *  promise means path + fingerprint are stored. Rejects with FsError code 'cancelled' when the
 *  user answered No — nothing is stored then. `null` clears the setting without asking. */
export async function archiveRarToolSetPath(path: string | null): Promise<RarToolSetPathResult> {
  if (!isInTauri()) throw unsupported();
  return wrap(invoke('archive_rar_tool_set_path', { path }));
}

export async function archiveCreate(
  sourcePaths: string[],
  destinationPath: string,
  format: ArchiveCreateFormat,
  conflict: ArchiveCreateConflict = 'overwrite',
  requestId?: string,
  level?: ArchiveCompressionLevel,
): Promise<string> {
  if (!isInTauri()) throw unsupported();
  return wrap(
    invoke('archive_create', {
      ...(requestId ? { requestId } : {}),
      request: {
        format,
        sourcePaths,
        destinationPath,
        conflict,
        ...(level ? { level } : {}),
      },
    }),
  );
}

export async function archiveTest(
  archivePath: string,
  password?: string,
  requestId?: string,
): Promise<ArchiveTestReport> {
  if (!isInTauri()) throw unsupported();
  return wrap(
    invoke('archive_test', {
      requestId: requestId ?? null,
      request: { archivePath, password: password ?? null },
    }),
  );
}

// ── FER-79: cancel + progress for extract / Test Archive ────────────────

/** A fresh id for one cancellable archive job. */
export function newArchiveRequestId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return `arc-${c.randomUUID()}`;
  return `arc-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** Ask the backend to stop the job registered under `requestId` (idempotent; unknown ids are
 *  harmless). Cooperative: the job stops at its next chunk and removes its staging file. */
export async function archiveCancelRequest(requestId: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('archive_cancel_request', { requestId });
  } catch {
    // Best effort: a failed cancel must never mask the job's own result.
  }
}

/** One throttled `archive://progress` event. Carries no password and no archive content. */
export interface ArchiveProgressEvent {
  requestId: string;
  kind: 'extract' | 'create' | 'test';
  archivePath: string;
  processedBytes: number;
  totalBytes?: number;
  processedEntries: number;
  totalEntries?: number;
  currentMember?: string;
  /** FER-55: Number of files written */
  writtenEntries?: number;
  /** FER-55: Number of files skipped due to conflict policy */
  skippedEntries?: number;
  /** FER-55: Number of files renamed */
  renamedEntries?: number;
}

export const ARCHIVE_PROGRESS_EVENT = 'archive://progress';

/** Subscribe to progress events. Returns the unsubscribe function. */
export async function listenArchiveProgress(
  handler: (e: ArchiveProgressEvent) => void,
): Promise<UnlistenFn> {
  if (!isInTauri()) return () => {};
  return listen<ArchiveProgressEvent>(ARCHIVE_PROGRESS_EVENT, (ev) => handler(ev.payload));
}

// ── FER-75: multipart archives ──────────────────────────────────────────

export type ArchiveMultipartInfo =
  | { kind: 'single' }
  | {
      kind: 'notFirst';
      part: number;
      firstName: string;
      firstPath: string;
      firstExists: boolean;
    }
  | { kind: 'missing'; missing: string[] }
  | { kind: 'complete'; volumes: number };

/** Classify an archive file name against the files next to it. Reads a directory listing only
 *  for names that can be volumes. */
export async function archiveMultipartCheck(path: string): Promise<ArchiveMultipartInfo> {
  if (!isInTauri()) return { kind: 'single' };
  return wrap(invoke('archive_multipart_check', { path }));
}

// ── FER-368: one-ask extraction preflight (D21) ─────────────────────────

export interface ArchivePreflightConflict {
  memberPath: string;
  destinationPath: string;
}

export interface ArchivePreflightReport {
  totalFiles: number;
  totalBytes: number;
  conflicts: ArchivePreflightConflict[];
}

/** Read-only advisory pass before an extract: which selected members already
 *  exist at the destination? Nothing is written. Fails with the archive
 *  error surface (e.g. password required) so the caller can run its retry
 *  flow before showing the one-ask conflict dialog. `password` (FER-504) is
 *  the one that already unlocked this archive in the UI, so header-encrypted
 *  RAR/7z enumerate too. */
export async function archiveExtractPreflight(
  archivePath: string,
  memberPaths: string[],
  destinationPath: string,
  password?: string,
): Promise<ArchivePreflightReport> {
  if (!isInTauri()) throw unsupported();
  return wrap(
    invoke('archive_extract_preflight', {
      request: { archivePath, memberPaths, destinationPath, password: password ?? null },
    }),
  );
}
