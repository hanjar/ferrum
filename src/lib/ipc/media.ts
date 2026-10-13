/**
 * FER-452: `ferrum-media` protocol client. The webview never builds a file URL:
 * it asks the backend for an opaque token (`media_grant`) and gets back
 * `http://ferrum-media.localhost/<token>` (Windows) — the path stays in Rust.
 * Grants are dropped by scope (`media_revoke`) when the view that asked goes away.
 */
import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';

export const MEDIA_PROTOCOL = 'ferrum-media';

/** Token -> src the protocol handler answers (host form differs per platform). */
export function mediaUrl(token: string): string {
  return convertFileSrc(token, MEDIA_PROTOCOL);
}

let scopeCounter = 0;

/** Unique scope label, so one view's revoke never kills another view's grants. */
export function newMediaScope(prefix: string): string {
  return `${prefix}:${++scopeCounter}`;
}

/** One URL per path, in order; null when the file may not be served (or no backend). */
export async function mediaGrant(paths: string[], scope: string): Promise<(string | null)[]> {
  if (paths.length === 0) return [];
  if (!isInTauri()) return paths.map(() => null);
  const tokens = await invoke<(string | null)[]>('media_grant', { paths, scope });
  return paths.map((_, i) => {
    const t = tokens[i];
    return typeof t === 'string' && t ? mediaUrl(t) : null;
  });
}

export async function mediaGrantOne(path: string, scope: string): Promise<string | null> {
  return (await mediaGrant([path], scope))[0] ?? null;
}

/** Drop every grant of `scope`. Failures are ignored: grants also expire. */
export async function mediaRevoke(scope: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('media_revoke', { scope });
  } catch {
    /* grants also lapse on their own (TTL + LRU) */
  }
}

/**
 * Coalesces the grant requests of many thumbnails that become visible in the
 * same tick into one IPC call (per scope).
 */
export function createGrantBatcher(scope: string) {
  let queue: { path: string; resolve: (url: string | null) => void }[] = [];
  let scheduled = false;
  function flush() {
    scheduled = false;
    const batch = queue;
    queue = [];
    mediaGrant(
      batch.map((b) => b.path),
      scope,
    ).then(
      (urls) => batch.forEach((b, i) => b.resolve(urls[i] ?? null)),
      () => batch.forEach((b) => b.resolve(null)),
    );
  }
  return {
    scope,
    request(path: string): Promise<string | null> {
      return new Promise((resolve) => {
        queue.push({ path, resolve });
        if (!scheduled) {
          scheduled = true;
          queueMicrotask(flush);
        }
      });
    },
  };
}
