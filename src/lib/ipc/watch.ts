/** Filesystem watcher (backend: watch_start / watch_stop, event fs://changed). */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError } from './types';
import { isInTauri } from './tauri';

/**
 * One coalesced change batch (F2-26). `roots` are the registered watch roots
 * that saw activity; `paths` are the deduped changed paths (capped backend
 * side). A root appearing inside `paths` means the watched directory itself
 * appeared/vanished/changed — ambiguous for incremental UI updates.
 */
export interface FsChangeBatch {
  roots: string[];
  paths: string[];
}

export async function watchStart(path: string): Promise<void> {
  if (!isInTauri()) {
    throw new FsError({ code: 'unsupported', message: 'Watching requires the desktop app' });
  }
  try {
    await invoke('watch_start', { path });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function watchStop(path: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('watch_stop', { path });
  } catch {
    /* stop is best-effort */
  }
}

export function onFsChanged(cb: (batch: FsChangeBatch) => void): Promise<UnlistenFn> {
  return listen<FsChangeBatch>('fs://changed', (e) => cb(e.payload));
}
