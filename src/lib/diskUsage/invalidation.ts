/**
 * DSK-06.3 — snapshot invalidation for completed filesystem mutations.
 *
 * After a successful delete/rename/move that touches the scanned scope, the
 * current scan is marked STALE (`diskUsage.markStale()`): the chip and the
 * amber note with a Rescan CTA stay until the user rescans. The store NEVER
 * claims freed space — logical/allocated numbers keep their old values
 * labelled by the stale marker, and free space remains a separate volume
 * metric (contract §2.3/§2.5).
 *
 * Wiring: `Dialogs.svelte` calls `noteScopeMutation` when the synchronous
 * delete/rename IPC reports success, and `recordQueuedMove` right after a
 * move is accepted onto the EXISTING background queue (enqueueMove). Queued
 * moves reach a terminal state later; the `operations://progress` listener
 * below converts that terminal state into the same stale marker. There is
 * deliberately no auto-clean and no new delete/move path anywhere — only
 * observation of the existing flows.
 */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { OperationId, OperationProgress } from '../ipc/types';
import { isInTauri } from '../ipc/tauri';
import { diskUsageCacheNoteMutation } from '../ipc/diskUsage';
import { duWorkspace } from './duWorkspace.svelte';

/** Sources of one enqueued move + its destination directory (BTM op). */
interface QueuedMove {
  sources: string[];
  target: string;
}

const pendingMoves = new Map<OperationId, QueuedMove>();
let unlisten: UnlistenFn | null = null;
let starting = false;

/** Separator/case-insensitive normalization so `C:/data/x` and `c:\DATA\x`
 *  compare equal (Windows-first, mirrors the pane path style). */
function normPath(p: string): string {
  return p
    .replace(/[\\/]+/g, '\\')
    .replace(/\\+$/, '')
    .toLowerCase();
}

/** True when `path` IS the scan root or lives inside it. */
export function withinScope(root: string, path: string): boolean {
  const r = normPath(root);
  const p = normPath(path);
  if (r === '' || p === '') return false;
  return p === r || p.startsWith(`${r}\\`);
}

/** A finished (non-queued) mutation — delete/rename — may have touched the
 *  scanned scope. Marking stale is conservative and honest: it never claims
 *  HOW the numbers changed, only that they may have. DSK-11: the workspace
 *  runs ONE scan session — only its store can go stale here (per-instance
 *  stores built by tests are never wired to invalidation). */
export function noteScopeMutation(paths: string[]): void {
  // FER-464: cached scans of OTHER roots (not the one on screen) are marked
  // too; the backend only touches roots that contain / lie inside a path.
  void diskUsageCacheNoteMutation(paths);
  const store = duWorkspace.store;
  const root = store.root;
  if (!root || !store.hasData) return;
  if (paths.some((p) => withinScope(root, p))) store.markStale();
}

/** Record a move accepted onto the existing operation queue (BTM). Staleness
 *  is applied only when the op reaches a terminal state — enqueue alone is
 *  not success. Moving INTO the scope also invalidates (files appear). */
export function recordQueuedMove(
  operationId: OperationId,
  sources: string[],
  target: string,
): void {
  pendingMoves.set(operationId, { sources: [...sources], target });
  ensureProgressListener();
}

/** Terminal-state handler for recorded queued moves. Exposed for tests; the
 *  `operations://progress` listener feeds it verbatim. A completed op always
 *  invalidates; a failed/cancelled one only when it had already processed
 *  files (those may have been moved — still no claim about how much). */
export function handleOperationProgress(p: OperationProgress): void {
  const rec = pendingMoves.get(p.operationId);
  if (!rec) return;
  if (p.state === 'queued' || p.state === 'running' || p.state === 'paused') return;
  pendingMoves.delete(p.operationId);
  if (p.state === 'completed' || p.processedFiles > 0) {
    noteScopeMutation([...rec.sources, rec.target]);
  }
}

/** Idempotent listener attach; inert outside the desktop app (tests). */
function ensureProgressListener(): void {
  if (unlisten || starting || !isInTauri()) return;
  starting = true;
  void listen<OperationProgress>('operations://progress', (e) => handleOperationProgress(e.payload))
    .then((fn) => {
      unlisten = fn;
      starting = false;
    })
    .catch(() => {
      starting = false;
    });
}

/** Test hook: drop recorded ops (listener lifetime is app-wide by design). */
export function resetInvalidationForTests(): void {
  pendingMoves.clear();
}
