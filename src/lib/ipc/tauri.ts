/**
 * Typed invoke wrapper. Components never import @tauri-apps/api directly —
 * everything goes through this module so the command surface stays in one
 * place and browser dev mode can stub it.
 */
import { invoke } from '@tauri-apps/api/core';
import { asFileEntry, FsError, type FileEntry } from './types';

/** Checked per call (not at import) so tests can flip the environment. */
export function isInTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** Listing of one directory via the Rust service. */
export async function listDir(path: string): Promise<FileEntry[]> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  let raw: unknown;
  try {
    raw = await invoke('list_dir', { path });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
  if (!Array.isArray(raw)) throw FsError.fromIPC(raw);
  return raw.map(asFileEntry);
}
