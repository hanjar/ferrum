/** Search IPC (F2-16) — typed wrappers over search_start/search_cancel and
 *  the `search://progress` event stream. */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError, type FileEntry } from './types';
import { isInTauri } from './tauri';

export interface SearchOptions {
  pattern: string;
  /** 'glob' (default) or 'regex'. */
  mode: 'glob' | 'regex';
  caseSensitive: boolean;
  recursive: boolean;
  minSize?: number;
  maxSize?: number;
  modifiedAfterMs?: number;
  modifiedBeforeMs?: number;
  /** Literal substring inside file contents (bounded read in Rust). */
  content?: string;
  maxResults?: number;
  contentMaxFileBytes?: number;
  maxDepth?: number;
}

export interface SearchStats {
  matched: number;
  visited: number;
  errors: number;
  truncated: boolean;
  cancelled: boolean;
  /** True when the 30s wall-clock cap stopped the walk; results may be incomplete. */
  timedOut: boolean;
  /** Wall-clock duration of the search in milliseconds. */
  durationMs: number;
  /** FER-476: what answered — the FerrumIndexer, the directory walk, or the
   *  index plus a walk of the user's own profile (shared mode). */
  source?: 'walker' | 'index' | 'indexAndWalker';
  /** FER-476: the index answered from its previous state while rebuilding. */
  stale?: boolean;
}

export interface SearchEvent {
  id: string;
  entries: FileEntry[];
  done: boolean;
  stats?: SearchStats;
  error?: string;
}

export function defaultSearchOptions(): SearchOptions {
  return { pattern: '', mode: 'glob', caseSensitive: false, recursive: true };
}

/** Kick off a search worker; results arrive as events for this id. */
export async function searchStart(id: string, root: string, options: SearchOptions): Promise<void> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  try {
    await invoke('search_start', { id, root, options });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Stop a running search. Returns false when nothing is running under the id. */
export async function searchCancel(id: string): Promise<boolean> {
  if (!isInTauri()) return false;
  try {
    return await invoke<boolean>('search_cancel', { id });
  } catch {
    return false;
  }
}

export async function onSearchEvent(handler: (e: SearchEvent) => void): Promise<UnlistenFn> {
  return listen<SearchEvent>('search://progress', (e) => handler(e.payload));
}
