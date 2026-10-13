/**
 * F2-27: "Search in Separate Window" — TC's separate Find Files window.
 * The window runs the SAME frontend with a `searchWindow` query flag (the
 * search store lives per window, so the two searches are independent), and
 * asks the main window to navigate via the `search://goto` event. One
 * window at a time: opening again focuses the existing one.
 */
import { FsError } from './ipc/types';

export const SEARCH_WINDOW_LABEL = 'search';
export const GOTO_EVENT = 'search://goto';

/** True when this renderer instance IS the separate search window. */
export function isSearchWindow(): boolean {
  return (
    typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('searchWindow')
  );
}

/** Guarded window-existence check (typed loosely; real API is dynamic). */
async function existingSearchWindow(): Promise<{ setFocus(): Promise<void> } | null> {
  if (!('__TAURI_INTERNALS__' in window)) return null;
  const api = await import('@tauri-apps/api/webviewWindow');
  const found = await api.WebviewWindow.getByLabel(SEARCH_WINDOW_LABEL);
  return found ? { setFocus: () => found.setFocus() } : null;
}

/**
 * Open (or focus) the separate search window rooted at the ACTIVE pane's
 * folder. Browser dev reports honestly that it needs the desktop app.
 */
export async function openSearchSeparate(root?: string): Promise<void> {
  if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) {
    throw new FsError({
      code: 'unsupported',
      message: 'A separate search window requires the desktop app (not browser dev)',
    });
  }
  const existing = await existingSearchWindow();
  if (existing) {
    await existing.setFocus();
    return;
  }
  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
  const params = new URLSearchParams({ searchWindow: '1' });
  if (root) params.set('root', root);
  const win = new WebviewWindow(SEARCH_WINDOW_LABEL, {
    url: `index.html?${params.toString()}`,
    title: 'Ferrum Search',
    width: 720,
    height: 560,
  });
  win.once('tauri://error', () => {
    /* the window simply does not appear; the runtime logs the failure */
  });
}
