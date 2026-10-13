/**
 * In-memory saved selections per directory (F2-14, Save/Restore Selection).
 * Keyed by the pane's current path (stable directory identity); only
 * basenames are stored so a selection survives re-sort, re-filter, and even
 * a different mount point of the same folder. Session-scoped by design —
 * persistent storage is the separate Save/Load To File flow.
 */
import { SvelteMap } from 'svelte/reactivity';

const saved = new SvelteMap<string, string[]>();

export function saveSelection(dir: string, names: readonly string[]): void {
  if (!dir) return;
  saved.set(dir, [...names]);
}

export function restoreSelection(dir: string): string[] | undefined {
  return saved.get(dir);
}

export function hasSavedSelection(dir: string): boolean {
  return saved.has(dir);
}

/** Test/session reset. */
export function clearSelectionMemory(): void {
  saved.clear();
}
