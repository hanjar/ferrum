/**
 * FER-572: Copy / Cut / Paste of FILES through the Windows clipboard (CF_HDROP +
 * "Preferred DropEffect"), so Explorer and Ferrum can exchange files in both directions.
 *
 * Paste adds no operation path of its own: the validated source list goes into the same
 * confirm / conflict dialog and operation queue as F5 / F6 / external drag-drop.
 */
import { activePane } from './panesRegistry.svelte';
import { dialogs } from './dialogStore.svelte';
import { toast } from './toastStore.svelte';
import { FsError } from '../ipc/types';
import { clipboardCopyFiles, clipboardReadFiles } from '../ipc/clipboardFiles';

function fail(title: string, e: unknown): void {
  dialogs.open({ kind: 'error', title, message: e instanceof FsError ? e.message : String(e) });
}

/** Ctrl+C (cut = false) / Ctrl+X (cut = true) on the active pane's selection. */
export async function copySelectionToClipboard(cut: boolean): Promise<void> {
  const title = cut ? 'Cut' : 'Copy';
  const paths = activePane()?.selectedPaths() ?? [];
  if (paths.length === 0) return;
  try {
    await clipboardCopyFiles(paths, cut);
    toast.show(`${cut ? 'Dipotong' : 'Disalin'}: ${paths.length} item (clipboard file)`);
  } catch (e) {
    fail(title, e);
  }
}

/** Ctrl+V: paste the clipboard's files into the active pane's folder. */
export async function pasteFromClipboard(): Promise<void> {
  const pane = activePane();
  if (!pane) return;
  try {
    const found = await clipboardReadFiles();
    if (!found || found.paths.length === 0) {
      toast.show('Clipboard tidak berisi file');
      return;
    }
    await dialogs.beginTransferTo(
      found.cut ? 'move' : 'copy',
      found.paths,
      pane.path,
      found.cut ? { clipboardSequence: found.sequence } : undefined,
    );
  } catch (e) {
    fail('Paste', e);
  }
}
