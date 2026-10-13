/**
 * Compare run lifecycle (FER-109 UI): unique opId per run, progress from the
 * `compare://progress` event, cooperative cancel, and stale-run detection so a
 * late result of an older run never overwrites a newer one.
 */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import * as compareIpc from '../ipc/compare';
import type { CompareProgress } from '../ipc/compare';

let counter = 0;

class CompareRun {
  opId = $state<string | null>(null);
  progress = $state<CompareProgress | null>(null);
  private unlisten: UnlistenFn | null = null;
  private subscribing = 0;

  /** Start a run: returns a fresh opId; any previous run becomes stale. */
  async begin(): Promise<string> {
    this.detach();
    counter += 1;
    const id = `cmp-${Date.now().toString(36)}-${counter}`;
    this.opId = id;
    this.progress = null;
    const ticket = ++this.subscribing;
    try {
      const un = await listen<CompareProgress>(compareIpc.COMPARE_PROGRESS_EVENT, (e) => {
        if (e.payload?.opId === this.opId) this.progress = e.payload;
      });
      if (ticket === this.subscribing) this.unlisten = un;
      else un();
    } catch {
      // Progress is best-effort (browser dev / no event bridge); the run still works.
    }
    return id;
  }

  /** True while `id` is still the latest run. */
  isCurrent(id: string): boolean {
    return this.opId === id;
  }

  /** Finish `id` (no-op when a newer run replaced it). */
  end(id: string): void {
    if (this.opId === id) this.detach();
  }

  /** Ask the engine to stop the current run. */
  async cancel(): Promise<void> {
    const id = this.opId;
    if (id) await compareIpc.cancelCompare(id);
  }

  private detach(): void {
    this.subscribing++;
    this.unlisten?.();
    this.unlisten = null;
    this.opId = null;
    this.progress = null;
  }

  reset(): void {
    this.detach();
  }
}

export const compareRun = new CompareRun();
