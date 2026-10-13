/** Selection save/load files (F2-14) — typed wrappers over the Rust commands. */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

export interface SelectionFileDoc {
  version: number;
  path: string;
  names: string[];
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

export async function saveSelectionFile(
  filePath: string,
  dir: string,
  names: string[],
): Promise<void> {
  if (!isInTauri()) throw unsupported();
  try {
    await invoke('save_selection', { filePath, dir, names });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function loadSelectionFile(filePath: string): Promise<SelectionFileDoc> {
  if (!isInTauri()) throw unsupported();
  try {
    const raw = await invoke<unknown>('load_selection', { filePath });
    if (typeof raw !== 'object' || raw === null) throw FsError.fromIPC(raw);
    const r = raw as Record<string, unknown>;
    if (typeof r.version !== 'number' || typeof r.path !== 'string' || !Array.isArray(r.names)) {
      throw new FsError({ code: 'io', message: 'Malformed SelectionFile from IPC' });
    }
    return { version: r.version, path: r.path, names: r.names.map(String) };
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
