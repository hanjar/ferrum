/** FER-591: drag files out of Ferrum into other apps (native OLE drag) — typed wrappers. */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

/**
 * Starts a native shell drag of these files (COPY only). Resolves when the drag ends:
 * `'copy'` = a target accepted the files, `'none'` = cancelled / refused / not in the desktop app.
 */
export async function startFileDrag(paths: string[]): Promise<'copy' | 'none'> {
  if (!isInTauri()) return 'none';
  try {
    const r = await invoke<string>('start_file_drag', { paths });
    return r === 'copy' ? 'copy' : 'none';
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Is Shift held right now (the drop event itself carries no modifier keys). */
export async function dragShiftDown(): Promise<boolean> {
  if (!isInTauri()) return false;
  try {
    return await invoke<boolean>('drag_shift_down');
  } catch {
    return false;
  }
}
