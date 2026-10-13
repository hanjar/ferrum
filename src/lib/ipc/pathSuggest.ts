/**
 * FER-456 address bar IPC: typed wrappers over `path_suggest` / `path_resolve`
 * (src-tauri/src/path_suggest.rs). In browser dev suggestions are empty and
 * resolution accepts the typed path, so the bar stays usable without Tauri.
 */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

export type SuggestKind = 'drive' | 'dir' | 'archive';

export interface PathSuggestion {
  name: string;
  path: string;
  kind: SuggestKind;
  /** Symlink / junction: listed, never resolved by the backend. */
  link: boolean;
}

export interface PathSuggestResult {
  items: PathSuggestion[];
  truncated: boolean;
  timedOut: boolean;
  /** Unknown UNC host: nothing was contacted; Enter connects. */
  needsConfirm: boolean;
  host: string | null;
}

export type ResolveKind = 'dir' | 'archive' | 'file' | 'missing';

export interface ResolvedPath {
  path: string;
  kind: ResolveKind;
}

export const SUGGEST_LIMIT = 50;

export async function pathSuggest(
  input: string,
  base: string,
  limit = SUGGEST_LIMIT,
  knownHosts: string[] = [],
): Promise<PathSuggestResult> {
  if (!isInTauri()) {
    return { items: [], truncated: false, timedOut: false, needsConfirm: false, host: null };
  }
  try {
    return await invoke<PathSuggestResult>('path_suggest', { input, base, limit, knownHosts });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function pathResolve(input: string, base: string): Promise<ResolvedPath> {
  if (!isInTauri()) return { path: input, kind: 'dir' };
  try {
    return await invoke<ResolvedPath>('path_resolve', { input, base });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
