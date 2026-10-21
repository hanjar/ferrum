/**
 * F2-27 / FER-583: application quit. Every way of closing the main window
 * (custom X button, Alt+F4, taskbar / Task View close, `file.quit`) ends up in
 * the same flow: with queued/running operations or background jobs a
 * confirmation dialog is shown; otherwise the window closes right away.
 *
 * Mechanism: the Tauri `close-requested` event is intercepted synchronously
 * (`preventDefault`) only while work is active. After the user confirms, the
 * work is cancelled (bounded wait so the backend can clean staging/partial
 * files) and the window is destroyed, which bypasses `close-requested`, so
 * there is no recursion.
 */
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isInTauri } from './ipc/tauri';
import { dialogs } from './stores/dialogStore.svelte';
import { jobs } from './stores/jobsStore.svelte';
import { ops } from './stores/opsStore.svelte';

/** Longest wait for the backend to finish cancelling before the window closes anyway. */
export const QUIT_CANCEL_TIMEOUT_MS = 3000;
const POLL_MS = 50;

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
  const active = activeWorkCount();
  if (active > 0) {
    if (dialogs.current?.kind !== 'quitConfirm') {
      dialogs.open({ kind: 'quitConfirm', operations: active });
    }
    return;
  }
  void quitApp();
}

/** Cancel jobs and queued operations, wait (bounded) until they settle, then close. */
export async function cancelAllAndQuit(timeoutMs: number = QUIT_CANCEL_TIMEOUT_MS): Promise<void> {
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
  const deadline = Date.now() + timeoutMs;
  while (activeWorkCount() > 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
  await quitApp();
}

/**
 * Intercept window close of the main window. The handler must be synchronous:
 * Tauri only honours `preventDefault` before the handler returns. Returns the
 * unlisten function.
 */
export async function installCloseGuard(): Promise<() => void> {
  if (!isInTauri()) return () => {};
  return getCurrentWindow().onCloseRequested((event) => {
    if (activeWorkCount() === 0) return; // Tauri destroys the window itself
    event.preventDefault();
    requestQuit();
  });
}
