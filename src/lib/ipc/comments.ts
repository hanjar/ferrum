/**
 * Comments IPC (F2-27). Mirrors `src-tauri/src/comments.rs`: a per-
 * directory sidecar (`.ferrum/comments.json`) keyed by file name. The map
 * crossing IPC is a plain name → text record.
 */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Comments require the desktop app (not browser dev)',
  });
}

/** The comment of one file, or null when it has none. */
export async function commentGet(path: string): Promise<string | null> {
  if (!isInTauri()) throw unsupported();
  try {
    const raw = await invoke<unknown>('comment_get', { path });
    return raw === null || raw === undefined ? null : String(raw);
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** One directory's comment map (file name → text). */
export async function commentsForDir(dir: string): Promise<Record<string, string>> {
  if (!isInTauri()) throw unsupported();
  try {
    const raw = await invoke<unknown>('comments_for_dir', { dir });
    if (typeof raw !== 'object' || raw === null) {
      throw new FsError({ code: 'io', message: 'Malformed comment map from IPC' });
    }
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
      if (typeof v === 'string') out[k] = v;
    }
    return out;
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Set (non-empty text) or clear (null/empty) one file's comment. */
export async function commentSet(path: string, comment: string | null): Promise<void> {
  if (!isInTauri()) throw unsupported();
  try {
    await invoke('comment_set', { path, comment });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
