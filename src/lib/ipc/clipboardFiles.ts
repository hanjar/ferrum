/** FER-572: file clipboard (Windows shell CF_HDROP) — typed wrappers over the Rust commands. */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'The file clipboard requires the desktop app (not browser dev)',
  });
}

/** What a paste found: sources already validated by the backend, and the cut/copy intent. */
export interface ClipboardPaste {
  paths: string[];
  cut: boolean;
  sequence: number;
}

/** Ctrl+C / Ctrl+X: put files on the Windows clipboard so Explorer can paste them. */
export async function clipboardCopyFiles(paths: string[], cut: boolean): Promise<number> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<number>('clipboard_copy_files', { paths, cut });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Ctrl+V: the files on the Windows clipboard, or null when it holds none. */
export async function clipboardReadFiles(): Promise<ClipboardPaste | null> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<ClipboardPaste | null>('clipboard_read_files');
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** After a pasted cut completed: empty the clipboard if it still holds that content. */
export async function clipboardFinishCut(sequence: number): Promise<boolean> {
  if (!isInTauri()) throw unsupported();
  try {
    return await invoke<boolean>('clipboard_finish_cut', { sequence });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
