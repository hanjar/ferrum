/**
 * Shell-launch actions (F2-25): open with OS association, Associate With
 * picker, Open Terminal Here, and the F2-27 print hand-off. The store is
 * the single place that turns structured IPC errors into honest dialogs;
 * components and registry actions never invoke the shell backend directly.
 */
import { dialogs } from './dialogStore.svelte';
import { activePane } from './panesRegistry.svelte';
import { associateWith, openExternal, openTerminal, printFile } from '../ipc/shellOpen';
import { ui } from '../uiState.svelte';
import { activity } from './activityStore.svelte';

class ShellStore {
  busy = $state(false);

  /** Open one file/folder with its OS default association. */
  async openFileExternal(path: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await openExternal(path);
      // Accepted by the OS → Activity receipt (gated by pause/private window).
      activity.recordFileOpen(path);
    } catch (e) {
      this.fail('Open', e);
    } finally {
      this.busy = false;
    }
  }

  /** Open the configured terminal at the ACTIVE pane's folder. */
  async openTerminalHere(): Promise<void> {
    const pane = activePane();
    if (!pane || this.busy) return;
    this.busy = true;
    try {
      await openTerminal(pane.path, ui.terminal);
    } catch (e) {
      this.fail('Open Terminal Here', e);
    } finally {
      this.busy = false;
    }
  }

  /** Open the configured terminal at an explicit folder (context menu). */
  async openTerminalAt(path: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await openTerminal(path, ui.terminal);
    } catch (e) {
      this.fail('Open Terminal Here', e);
    } finally {
      this.busy = false;
    }
  }

  /**
   * Open the OS "Associate With…" picker. The user decides there; on
   * platforms without a picker the error dialog says so honestly.
   */
  async associateWith(path: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await associateWith(path);
    } catch (e) {
      this.fail('Associate With', e);
    } finally {
      this.busy = false;
    }
  }

  /** Print via the associated application (F2-27). */
  async print(path: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await printFile(path);
    } catch (e) {
      this.fail('Print', e);
    } finally {
      this.busy = false;
    }
  }

  /** Shared error surfacing: structured FsErrors become honest dialogs. */
  private fail(title: string, e: unknown): void {
    const message = e instanceof Error ? e.message : String(e);
    dialogs.open({ kind: 'error', title, message });
  }
}

export const shell = new ShellStore();
