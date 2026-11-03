/**
 * OS-level drag-drop onto the window (Tauri webview drag-drop events). Two sources arrive here:
 *
 *  - files dragged in from another app (Windows Explorer, ...): copied INTO the pane under the
 *    cursor; conflicts flow through the D21 dialog like any other copy;
 *  - FER-591: files dragged out of a Ferrum pane with the native shell drag (see `startFileDrag`).
 *    Dropped back on the SAME pane = ignored, on the OTHER pane = copy (Shift = move), the same
 *    rule the old HTML5 pane-to-pane drag had.
 */
import { dialogs } from './dialogStore.svelte';
import { isInTauri } from '../ipc/tauri';
import { dragShiftDown } from '../ipc/dragOut';
import { dragOriginPane } from './dragPayload';
import { paneById } from './panesRegistry.svelte';
import type { PaneId } from '../uiState.svelte';

export function handleExternalDrop(paths: string[], target: string | null): boolean {
  if (paths.length === 0 || !target) return false;
  dialogs.beginTransferTo('copy', paths, target);
  return true;
}

type DragDropPayload = {
  type: 'enter' | 'over' | 'drop' | 'leave';
  paths?: string[];
  /** Physical pixels, relative to the webview. */
  position?: { x: number; y: number };
};

/** The pane under a physical-pixel position, or null when the point is not over a pane. */
export function paneAt(position: { x: number; y: number } | undefined): PaneId | null {
  if (!position || typeof document === 'undefined') return null;
  const dpr = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(position.x / dpr, position.y / dpr);
  const raw = el?.closest('[data-pane]')?.getAttribute('data-pane');
  return raw === '0' ? 0 : raw === '1' ? 1 : null;
}

/**
 * Routes one drop. `origin` is the Ferrum pane the drag started from (read synchronously when the
 * event arrived), null for a drag from another app. Returns true when a transfer was started.
 */
export async function routeDrop(
  paths: string[],
  position: { x: number; y: number } | undefined,
  origin: PaneId | null,
): Promise<boolean> {
  if (paths.length === 0) return false;
  const pane = paneAt(position);
  if (pane === null) return false;
  if (origin !== null) {
    if (origin === pane) return false;
    const folder = paneById(pane)?.path;
    if (!folder) return false;
    const shift = await dragShiftDown();
    dialogs.beginTransferTo(shift ? 'move' : 'copy', paths, folder);
    return true;
  }
  return handleExternalDrop(paths, paneById(pane)?.path ?? null);
}

/** Subscribes to Tauri webview drag-drop events. Returns the unlisten. */
export async function setupExternalDrop(): Promise<() => void> {
  if (!isInTauri()) return () => {};
  try {
    const { getCurrentWebview } = await import('@tauri-apps/api/webview');
    return await getCurrentWebview().onDragDropEvent((event) => {
      const payload = (event as { payload: DragDropPayload }).payload;
      if (payload.type === 'drop') {
        void routeDrop(payload.paths ?? [], payload.position, dragOriginPane());
      }
    });
  } catch {
    // jsdom or a webview without drag-drop support: soft no-op.
    return () => {};
  }
}
