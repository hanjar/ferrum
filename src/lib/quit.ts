/**
 * F2-27 / FER-583: application quit. Every way of closing the main window
 * (custom X button, Alt+F4, taskbar / Task View close, `file.quit`) ends up in
 * the same flow: with queued/running operations or background jobs a
 * confirmation dialog is shown; otherwise the window closes right away.
 *
 * Mechanism: Rust (close_guard.rs) holds every native close of the main window
 * and emits CLOSE_EVENT. The frontend acks at once (`close_ack`), then either
 * destroys the window (idle) or shows the confirmation. No ack within ~2 s (frozen
 * webview) or a second close without ack lets Rust close the window itself.
 * After the user confirms, the work is cancelled (bounded wait so the backend can
 * clean staging/partial files) and the window is destroyed.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isInTauri } from './ipc/tauri';
import { dialogs } from './stores/dialogStore.svelte';
import { jobs } from './stores/jobsStore.svelte';
import { ops } from './stores/opsStore.svelte';

/** Longest wait for the backend to finish cancelling before the window closes anyway. */
export const QUIT_CANCEL_TIMEOUT_MS = 3000;
const POLL_MS = 50;
export const CLOSE_EVENT = 'ferrum-close-requested';

/** Set while the window is closing: further close / quit requests are ignored. */
let quitting = false;

/** Test seam. */
export function resetQuitState(): void {
  quitting = false;
}

/** Queued/running operations plus background jobs (FER-578). */
export function activeWorkCount(): number {
  return ops.active.length + jobs.runningCount;
}

/** Close the main window for good (no `close-requested`); the runtime exits when the last window closes. */
export async function quitApp(): Promise<void> {
  if (!isInTauri()) return;
  try {
    await getCurrentWindow().destroy();
  } catch (e) {
    console.warn('[quit] window destroy failed', e);
  }
}

/** Single quit entry: confirm when work is active, quit directly otherwise. */
export function requestQuit(): void {
  if (quitting) return;
  const active = activeWorkCount();
  if (active > 0) {
    if (dialogs.current?.kind !== 'quitConfirm') {
      dialogs.open({ kind: 'quitConfirm', operations: active });
    }
    return;
  }
  void quitApp();
}

/**
 * Cancel jobs and operations, wait (bounded) until the ones that really ran have settled, then
 * close. A queued operation is cancelled in the backend without any event, so it is not awaited.
 */
export async function cancelAllAndQuit(timeoutMs: number = QUIT_CANCEL_TIMEOUT_MS): Promise<void> {
  if (quitting) return;
  quitting = true;
  const runningOps = ops.active.filter((o) => o.state === 'running').map((o) => o.operationId);
  const jobsToWait = jobs.running.map((j) => j.id);
  try {
    for (const job of jobs.running) {
      try {
        jobs.cancel(job);
      } catch (e) {
        console.warn('[quit] job cancel failed', e);
      }
    }
    await Promise.all(
      ops.active.map((o) =>
        Promise.resolve(ops.cancel(o.operationId)).catch((e) =>
          console.warn('[quit] operation cancel failed', e),
        ),
      ),
    );
    const settled = () => {
      const activeOps = new Set(ops.active.map((o) => o.operationId));
      const activeJobs = new Set(jobs.running.map((j) => j.id));
      return (
        !runningOps.some((id) => activeOps.has(id)) && !jobsToWait.some((id) => activeJobs.has(id))
      );
    };
    const deadline = Date.now() + timeoutMs;
    while (!settled() && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, POLL_MS));
    }
  } catch (e) {
    console.warn('[quit] cancel phase failed', e);
  }
  await quitApp();
}

/** Handle one close request forwarded by Rust. Fail-open: any error closes the window. */
export async function handleCloseRequest(): Promise<void> {
  try {
    await invoke('close_ack'); // the frontend is alive: stop Rust's escape-hatch timer
  } catch (e) {
    console.warn('[quit] close_ack failed', e);
  }
  try {
    if (quitting) return;
    requestQuit();
  } catch (e) {
    console.warn('[quit] close handling failed, closing', e);
    await quitApp();
  }
}

/** Listen for the close requests of the main window. Returns the unlisten function. */
export async function installCloseGuard(): Promise<() => void> {
  if (!isInTauri()) return () => {};
  return listen(CLOSE_EVENT, () => void handleCloseRequest());
}
