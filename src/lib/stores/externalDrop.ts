/**
 * External (Windows Explorer) inbound drag-drop (backlog polish): files
 * dropped from the OS shell are copied INTO the active pane's directory.
 * Conflicts flow through the D21 dialog like any other copy.
 */
import { dialogs } from './dialogStore.svelte';
import { isInTauri } from '../ipc/tauri';

export function handleExternalDrop(paths: string[], target: string | null): boolean {
  if (paths.length === 0 || !target) return false;
  dialogs.beginTransferTo('copy', paths, target);
  return true;
}

type DragDropPayload = {
  type: 'enter' | 'over' | 'drop' | 'leave';
  paths?: string[];
};

/** Subscribes to Tauri webview drag-drop events. Returns the unlisten. */
export async function setupExternalDrop(getTarget: () => string | null): Promise<() => void> {
  if (!isInTauri()) return () => {};
  try {
    const { getCurrentWebview } = await import('@tauri-apps/api/webview');
    return await getCurrentWebview().onDragDropEvent((event) => {
      const payload = (event as { payload: DragDropPayload }).payload;
      if (payload.type === 'drop') {
        handleExternalDrop(payload.paths ?? [], getTarget());
      }
    });
  } catch {
    // jsdom or a webview without drag-drop support: soft no-op.
    return () => {};
  }
}
