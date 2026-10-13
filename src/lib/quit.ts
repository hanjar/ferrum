/**
 * F2-27: application quit. Closing the (last) window ends a Tauri app.
 * Registry actions decide WHEN quitting is offered (respecting in-flight
 * operations); this module is only the mechanism.
 */
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isInTauri } from './ipc/tauri';

/** Close the main window; the runtime exits when the last window closes. */
export async function quitApp(): Promise<void> {
  if (!isInTauri()) return;
  try {
    await getCurrentWindow().close();
  } catch (e) {
    console.warn('[quit] window close failed', e);
  }
}
