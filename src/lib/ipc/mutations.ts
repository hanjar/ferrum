/** Mutation service — typed wrappers over the Rust commands (docs/ipc.md). */
import { invoke } from '@tauri-apps/api/core';
import { FsError, type FileEntry } from './types';
import { isInTauri } from './tauri';

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

export async function createEntry(
  kind: 'file' | 'dir',
  parent: string,
  name: string,
): Promise<FileEntry> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<FileEntry>('create_entry', { kind, parent, name });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function renameEntry(path: string, newName: string): Promise<FileEntry> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<FileEntry>('rename_entry', { path, newName });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export interface FailedItem {
  path: string;
  code: string;
  message: string;
}

export interface DeleteReport {
  requested: number;
  deleted: number;
  failed: FailedItem[];
}

export async function deleteEntries(paths: string[], permanent: boolean): Promise<DeleteReport> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<DeleteReport>('delete_entries', { paths, permanent });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export interface CopyConflict {
  source: string;
  dest: string;
  sourceModifiedMs: number;
  destModifiedMs: number;
}

export interface CopyPlan {
  totalFiles: number;
  totalBytes: number;
  conflicts: CopyConflict[];
}

export async function planCopy(sources: string[], targetDir: string): Promise<CopyPlan> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<CopyPlan>('plan_copy', { sources, targetDir });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function enqueueCopy(
  sources: string[],
  targetDir: string,
  policy: 'ask' | 'skip' | 'overwrite' | 'overwriteOlder' | 'rename',
): Promise<string> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<string>('enqueue_copy', { sources, targetDir, policy });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function enqueueMove(
  sources: string[],
  targetDir: string,
  policy: 'ask' | 'skip' | 'overwrite' | 'overwriteOlder' | 'rename',
): Promise<string> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<string>('enqueue_move', { sources, targetDir, policy });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export interface RecycleCheckRow {
  path: string;
  ok: boolean;
  /** Stable backend code: `network_or_unc`, `reserved_name` or `no_recycle_bin`. */
  reason?: string | null;
}

/** FER-576: can each path go to the Recycle Bin? Read-only. */
export async function aiRecycleCheck(paths: string[]): Promise<RecycleCheckRow[]> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<RecycleCheckRow[]>('ai_recycle_check', { paths });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/**
 * FER-576: the ONLY delete the AI chat can reach. Recycle Bin only: the command has no `permanent` argument and
 * refuses the whole batch when any item cannot go to the Recycle Bin.
 */
export async function aiDeleteToRecycle(paths: string[]): Promise<DeleteReport> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<DeleteReport>('ai_delete_to_recycle', { paths });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
