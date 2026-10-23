/**
 * FER-585: typed wrappers for the async delete job (`delete_start`, `delete_cancel`, events
 * `delete://progress` and `delete://done`). The confirmation happens before `runDelete`, as ever;
 * this only runs the delete off the UI thread with progress and cancel.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError } from './types';
import { isInTauri } from './tauri';
import type { DeleteReport } from './mutations';

export const DELETE_PROGRESS_EVENT = 'delete://progress';
export const DELETE_DONE_EVENT = 'delete://done';

export interface DeleteProgress {
  /** Top-level items finished. */
  done: number;
  total: number;
  /** The top-level item being deleted now. */
  currentName?: string;
  /** Files and folders removed so far, inside folders included. */
  entries: number;
  /** FER-586: bytes of the files removed so far (optional: older backends do not send it). */
  bytes?: number;
}

export interface DeleteJobResult {
  report: DeleteReport;
  /** Stopped by a cancel before every item was processed. */
  cancelled: boolean;
  /** Items a cancel never reached: still where they were. */
  notProcessed: string[];
  /** Folders a cancel stopped inside: some entries are gone, the folder remains. */
  partial: string[];
}

type ProgressEvent = DeleteProgress & { requestId: string };
type DoneEvent = {
  requestId: string;
  result?: DeleteJobResult;
  error?: { code?: string; message?: string };
};

export function newDeleteRequestId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return `del-${c.randomUUID()}`;
  return `del-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** Ask the backend to stop the delete job (idempotent; unknown ids are harmless). */
export async function deleteCancel(requestId: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('delete_cancel', { requestId });
  } catch {
    // Best effort: a failed cancel must never mask the job's own result.
  }
}

/**
 * Start the delete and resolve when it ended (with what it did, cancelled or not). Rejects with
 * the backend's refusal when a path is not allowed (nothing was deleted then). The listeners are
 * in place before the job starts, so no event is missed.
 */
export async function runDelete(
  paths: string[],
  permanent: boolean,
  requestId: string,
  onProgress: (p: DeleteProgress) => void,
): Promise<DeleteJobResult> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  const unlisten: UnlistenFn[] = [];
  const stop = () => {
    for (const u of unlisten.splice(0)) u();
  };
  try {
    let settle!: { ok: (r: DeleteJobResult) => void; fail: (e: unknown) => void };
    const done = new Promise<DeleteJobResult>((ok, fail) => {
      settle = { ok, fail };
    });
    unlisten.push(
      await listen<DoneEvent>(DELETE_DONE_EVENT, (ev) => {
        if (ev.payload.requestId !== requestId) return;
        if (ev.payload.result) settle.ok(ev.payload.result);
        else settle.fail(FsError.fromIPC(ev.payload.error ?? { message: 'Delete failed' }));
      }),
    );
    unlisten.push(
      await listen<ProgressEvent>(DELETE_PROGRESS_EVENT, (ev) => {
        if (ev.payload.requestId === requestId) onProgress(ev.payload);
      }),
    );
    try {
      await invoke('delete_start', { paths, permanent, requestId });
    } catch (e) {
      throw FsError.fromIPC(e);
    }
    return await done;
  } finally {
    stop();
  }
}
