/**
 * Encode/Decode actions (F2-27). Format support is deliberate and narrow
 * (MIME base64 encode; base64/UUE/XXE decode; BinHex refused) — see
 * src-tauri/src/codecs.rs. The work enqueues onto the operation queue, so
 * progress and cancellation live in the existing OpsPanel.
 */
import { activePane } from './panesRegistry.svelte';
import { enqueueDecodeFiles, enqueueEncodeFiles } from '../ipc/fileTools';
import { FsError } from '../ipc/types';
import { dialogs } from './dialogStore.svelte';
import { ops } from './opsStore.svelte';

class CodecsStore {
  busy = $state(false);

  /** Encode the selection to `<name>.b64` (MIME base64). */
  async encodeSelection(): Promise<void> {
    await this.run('Encode File(s)', (paths) => enqueueEncodeFiles(paths));
  }

  /** Decode the selection (base64 / UUE / XXE auto-detected). */
  async decodeSelection(): Promise<void> {
    await this.run('Decode File(s)', (paths) => enqueueDecodeFiles(paths));
  }

  private async run(title: string, enqueue: (paths: string[]) => Promise<string>): Promise<void> {
    const pane = activePane();
    const paths = pane?.selectedPaths() ?? [];
    if (paths.length === 0 || this.busy) return;
    this.busy = true;
    try {
      await enqueue(paths);
      ops.panelOpen = true; // progress/cancel live in the queue panel
    } catch (e) {
      dialogs.open({
        kind: 'error',
        title,
        message: e instanceof FsError ? e.message : String(e),
      });
    } finally {
      this.busy = false;
    }
  }
}

export const codecs = new CodecsStore();
