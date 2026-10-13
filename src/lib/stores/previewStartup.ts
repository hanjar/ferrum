/**
 * PRV-02 — "Open preview on launch". Applied at most once per process,
 * AFTER prefs have loaded, so a persisted OFF never flashes the panel open
 * and a user who closes it (or changes the setting at runtime) is never
 * overridden. A failed prefs load leaves `ui.preview` at its OFF default.
 */
import { ui } from '../uiState.svelte';

let applied = false;

export function applyStartupPreview(): void {
  if (applied) return;
  applied = true;
  if (ui.preview.openOnLaunch) ui.quickViewOpen = true;
}

/** Test hook: forget that the startup rule ran. */
export function resetStartupPreviewForTest(): void {
  applied = false;
}
