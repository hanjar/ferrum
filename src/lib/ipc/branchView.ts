/**
 * Branch View IPC (F2-23 / FER-128) — typed wrappers over the streaming
 * Rust commands `branch_start` / `branch_cancel` and the `branch://progress`
 * event stream. Mirrors `branch_view::BranchEvent`. The listing never travels
 * as one response (FER-479): batches of entries arrive as events, followed by
 * exactly one `done` event with the summary (or the error).
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { asFileEntry, FsError, type FileEntry } from './types';
import { isInTauri } from './tauri';

export interface BranchFailedDir {
  path: string;
  code: string;
  message: string;
}

export interface BranchDone {
  total: number;
  truncated: boolean;
  cancelled: boolean;
  failedCount: number;
  durationMs: number;
}

export interface BranchEvent {
  id: string;
  entries: FileEntry[];
  failed: BranchFailedDir[];
  done: boolean;
  summary?: BranchDone;
  error?: FsError;
}

/** Default cap kept in sync with `branch_view::DEFAULT_MAX_ENTRIES`. */
export const DEFAULT_MAX_ENTRIES = 100_000;

/** Validate one `branch://progress` payload; a malformed one is dropped by the caller. */
export function asBranchEvent(raw: unknown): BranchEvent {
  if (typeof raw !== 'object' || raw === null) {
    throw new FsError({ code: 'io', message: 'Malformed BranchEvent from IPC' });
  }
  const r = raw as Record<string, unknown>;
  if (typeof r.id !== 'string' || typeof r.done !== 'boolean' || !Array.isArray(r.entries)) {
    throw new FsError({ code: 'io', message: 'Malformed BranchEvent from IPC' });
  }
  const failed = Array.isArray(r.failed) ? r.failed : [];
  const summary = r.summary as Record<string, unknown> | undefined;
  return {
    id: r.id,
    done: r.done,
    entries: r.entries.map(asFileEntry),
    failed: failed.flatMap((item) => {
      if (typeof item !== 'object' || item === null) return [];
      const f = item as Record<string, unknown>;
      if (typeof f.path !== 'string' || typeof f.message !== 'string') return [];
      return [{ path: f.path, code: String(f.code), message: f.message }];
    }),
    summary: summary
      ? {
          total: Number(summary.total ?? 0),
          truncated: summary.truncated === true,
          cancelled: summary.cancelled === true,
          failedCount: Number(summary.failedCount ?? 0),
          durationMs: Number(summary.durationMs ?? 0),
        }
      : undefined,
    error: r.error ? FsError.fromIPC(r.error) : undefined,
  };
}

/** Start a streaming walk of `root`; events for `id` follow. */
export async function branchStart(id: string, root: string, maxEntries?: number): Promise<void> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  try {
    await invoke('branch_start', { id, root, maxEntries: maxEntries ?? null });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Stop a running walk. False when nothing runs under the id (already done). */
export async function branchCancel(id: string): Promise<boolean> {
  if (!isInTauri()) return false;
  try {
    return await invoke<boolean>('branch_cancel', { id });
  } catch {
    return false;
  }
}

export async function onBranchEvent(handler: (e: BranchEvent) => void): Promise<UnlistenFn> {
  return listen<unknown>('branch://progress', (e) => {
    try {
      handler(asBranchEvent(e.payload));
    } catch {
      // A malformed payload must not kill the listener.
    }
  });
}

/**
 * Path of an entry relative to the branch root (`data\2026\q1.csv` under
 * `C:\data`), keeping duplicates unambiguous. Falls back to the full path
 * when the entry does not live under the root.
 */
export function relativePath(entryPath: string, root: string): string {
  const normRoot = root.replace(/[\\/]+$/, '');
  if (entryPath.startsWith(normRoot)) {
    const rest = entryPath.slice(normRoot.length).replace(/^[\\/]+/, '');
    if (rest !== '') return rest;
  }
  return entryPath;
}
