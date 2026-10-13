/**
 * Drag & drop payload for file rows (cross-pane drag, prototype `dropOn`
 * overlay). The paths ride on a custom MIME inside the standard
 * DataTransfer; the drag ORIGIN pane lives in this module instead, because
 * DataTransfer data is unreadable during dragover (protected mode) — only
 * the MIME type list stays visible there.
 *
 * Pure module: no runes, no component state, unit-tested directly.
 */
import type { PaneId } from '../uiState.svelte';

/** Custom MIME type that marks a drag as an internal Ferrum row drag. */
export const FERRUM_MIME = 'application/x-ferrum-entries';

interface DragOrigin {
  pane: PaneId;
  count: number;
}

let origin: DragOrigin | null = null;

/** SOURCE pane, on dragstart: remember where the drag began and how many
 *  entries it carries (the count feeds the other pane's overlay text). */
export function beginInternalDrag(pane: PaneId, count: number): void {
  origin = { pane, count };
}

/** dragend/drop: forget the origin so stale state can't fake a drag. */
export function endInternalDrag(): void {
  origin = null;
}

/** Pane the current internal drag started from; null for foreign drags. */
export function dragOriginPane(): PaneId | null {
  return origin?.pane ?? null;
}

/** How many entries the current internal drag carries (0 when none). */
export function dragItemCount(): number {
  return origin?.count ?? 0;
}

/** Encode the dragged paths onto the event (source side, on dragstart). */
export function setDragPaths(e: DragEvent, paths: string[]): void {
  const dt = e.dataTransfer;
  if (!dt) return;
  dt.effectAllowed = 'copyMove';
  dt.setData(FERRUM_MIME, JSON.stringify({ paths }));
}

/** Decode the dragged paths; null when absent or malformed (foreign drag,
 *  corrupted payload). Safe to call on drop, where DataTransfer data is
 *  fully readable. */
export function readDragPaths(e: DragEvent): string[] | null {
  const raw = e.dataTransfer?.getData(FERRUM_MIME);
  if (!raw) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    const paths = isPathsShape(parsed) ? parsed.paths : undefined;
    if (paths && paths.every((p) => typeof p === 'string')) return paths as string[];
  } catch {
    // malformed JSON → treat as a foreign drag
  }
  return null;
}

/** True when the event carries the Ferrum payload. Works during dragover:
 *  `dataTransfer.types` stays readable while getData() is protected. */
export function isInternalDrag(e: DragEvent): boolean {
  return e.dataTransfer?.types.includes(FERRUM_MIME) ?? false;
}

function isPathsShape(v: unknown): v is { paths: unknown[] } {
  return typeof v === 'object' && v !== null && Array.isArray((v as { paths?: unknown }).paths);
}
