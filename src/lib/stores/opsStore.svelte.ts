/**
 * Frontend mirror of the Rust operation queue. Progress arrives as
 * `operations://progress` events (≤10/s per op); the store keeps one entry
 * per operation id and exposes queue-state for the UI panel.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { SvelteMap } from 'svelte/reactivity';
import type { OperationId, OperationProgress } from '../ipc/types';
import { isInTauri } from '../ipc/tauri';
import { refreshAllPanes } from './panesRegistry.svelte';
import { activity } from './activityStore.svelte';
import {
  COPY_CONTROL_EVENT,
  copyControlAdjust,
  copyControlSnapshot,
  isCopyControlState,
  type CopyAdjust,
  type CopyControlState,
} from '../ipc/copyControl';

class OpsStore {
  // A Map inside $state is NOT reactive (Svelte proxies only plain objects and arrays), so every
  // mutation bumps `rev`, which `active` / `all` read. Without it an open panel never saw progress.
  ops = new Map<OperationId, OperationProgress>();
  private rev = $state(0);
  panelOpen = $state(false);
  /** FER-471: live copy-thread state of the jobs that are copying (backend-owned). */
  copyControls = new SvelteMap<OperationId, CopyControlState>();

  private started = false;
  private unlisten: UnlistenFn | null = null;
  private unlistenControl: UnlistenFn | null = null;

  get active(): OperationProgress[] {
    void this.rev;
    return [...this.ops.values()].filter((o) => o.state === 'queued' || o.state === 'running');
  }

  get all(): OperationProgress[] {
    void this.rev;
    return [...this.ops.values()].sort((a, b) =>
      a.operationId < b.operationId ? -1 : a.operationId > b.operationId ? 1 : 0,
    );
  }

  /** One `operations://progress` event: keep the entry and refresh what a finished job may have changed. */
  applyProgress(p: OperationProgress): void {
    const wasPending =
      this.ops.get(p.operationId)?.state === 'queued' ||
      this.ops.get(p.operationId)?.state === 'running' ||
      !this.ops.has(p.operationId);
    this.ops.set(p.operationId, p);
    this.rev++;
    // A finished filesystem operation may have changed what panes show.
    if (
      wasPending &&
      (p.state === 'completed' || p.state === 'cancelled' || p.state === 'failed')
    ) {
      refreshAllPanes();
      // Move/delete/rename relocate or end activity entities in the backend.
      if (p.kind === 'move' || p.kind === 'delete' || p.kind === 'rename') {
        activity.noteFsChange();
      }
    }
  }

  /** Idempotent: starts the event subscription and pulls a snapshot. */
  async start(): Promise<void> {
    if (this.started || !isInTauri()) return;
    this.started = true;
    this.unlisten = await listen<OperationProgress>('operations://progress', (e) =>
      this.applyProgress(e.payload),
    );
    this.unlistenControl = await listen<unknown>(COPY_CONTROL_EVENT, (e) =>
      this.applyCopyControl(e.payload),
    );
    try {
      const snap = await invoke<OperationProgress[]>('list_operations');
      for (const p of snap) this.ops.set(p.operationId, p);
      this.rev++;
    } catch {
      /* first run: empty queue */
    }
    try {
      for (const state of await copyControlSnapshot())
        this.copyControls.set(state.operationId, state);
    } catch {
      /* no copy running */
    }
  }

  stop() {
    this.unlisten?.();
    this.unlisten = null;
    this.unlistenControl?.();
    this.unlistenControl = null;
    this.started = false;
  }

  /** Event or command result: the backend's word is final. A finished job drops its controls. */
  applyCopyControl(payload: unknown): void {
    if (!isCopyControlState(payload)) return;
    if (payload.running) this.copyControls.set(payload.operationId, payload);
    else this.copyControls.delete(payload.operationId);
  }

  /** Live control of a running copy job, if it has one. */
  copyControlOf(id: OperationId): CopyControlState | undefined {
    const state = this.copyControls.get(id);
    return state?.running ? state : undefined;
  }

  /**
   * [+] / [-] / set / auto / priority on a running copy job. Resolves with the state the backend
   * confirms; a job that already ended just loses its controls.
   */
  async adjustCopy(id: OperationId, change: CopyAdjust): Promise<CopyControlState | null> {
    try {
      const state = await copyControlAdjust(id, change);
      this.applyCopyControl(state);
      return state;
    } catch {
      this.copyControls.delete(id);
      return null;
    }
  }

  /** Enqueue the task-09 harness job; real engines arrive in tasks 10–12. */
  async enqueueDummy(durationMs: number): Promise<OperationId | null> {
    if (!isInTauri()) return null;
    return invoke<OperationId>('enqueue_dummy_op', { durationMs });
  }

  async cancel(id: OperationId): Promise<void> {
    if (!isInTauri()) return;
    await invoke('cancel_operation', { id });
  }

  /**
   * Retry a FAILED operation (F2-26 BTM): the backend re-enqueues a fresh
   * copy under a NEW id (the failed entry stays in the history). Returns the
   * new id, or null when the id is unknown/active/non-retryable or when
   * running outside the desktop app.
   */
  async retry(id: OperationId): Promise<OperationId | null> {
    if (!isInTauri()) return null;
    return invoke<OperationId | null>('retry_operation', { id });
  }

  async clearFinished(): Promise<void> {
    this.ops = new Map(
      [...this.ops].filter(([, o]) => o.state === 'queued' || o.state === 'running'),
    );
    this.rev++;
    if (isInTauri()) await invoke('clear_finished_operations');
  }

  togglePanel() {
    this.panelOpen = !this.panelOpen;
  }
}

export const ops = new OpsStore();

/** Percentage helper for progress bars. */
export function pctOf(p: OperationProgress): number {
  if (p.state === 'completed') return 100;
  if (p.state === 'queued') return 0;
  if (p.totalBytes && p.totalBytes > 0)
    return Math.min(100, (p.processedBytes / p.totalBytes) * 100);
  if (p.totalFiles && p.totalFiles > 0)
    return Math.min(100, (p.processedFiles / p.totalFiles) * 100);
  return 0;
}
