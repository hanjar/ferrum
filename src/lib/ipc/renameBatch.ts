/**
 * Multi-Rename Tool IPC (F2-20) — typed wrapper over the Rust
 * `rename_batch` command. Mirrors `rename_batch::RenameBatchReport`.
 */
import { invoke } from '@tauri-apps/api/core';
import { FsError, type ErrorCode } from './types';
import { isInTauri } from './tauri';

export interface RenamePair {
  /** Absolute path of the entry to rename. */
  from: string;
  /** New bare name (single path segment). */
  to: string;
}

export interface FailedRename {
  from: string;
  to: string;
  code: ErrorCode;
  message: string;
}

export interface RenameBatchReport {
  requested: number;
  renamed: number;
  failed: FailedRename[];
  /** Successfully executed (from → to) pairs, in execution order. */
  journal: RenamePair[];
}

export async function renameBatch(pairs: RenamePair[]): Promise<RenameBatchReport> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  try {
    return await invoke<RenameBatchReport>('rename_batch', { pairs });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
