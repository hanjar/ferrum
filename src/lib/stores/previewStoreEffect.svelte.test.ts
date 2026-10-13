/**
 * FER-454 — previewStore.setTarget is called from a component $effect
 * (FilePane drives Quick View from the active cursor). The call must not make
 * that effect depend on the store's own state: setTarget reads `target` and
 * `state` and then writes them, so a tracked read turned every call into a
 * self-retriggering effect (effect_update_depth_exceeded in the real
 * webview whenever Quick View showed a target).
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import { PreviewStore } from './previewStore.svelte';
import type { PaneEntryInfo } from './panesRegistry.svelte';

vi.mock('../ipc/preview', () => ({
  readPreview: vi.fn(() => new Promise(() => {})),
  readArchiveMemberPreview: vi.fn(() => new Promise(() => {})),
  cancelArchiveRequest: vi.fn(),
}));

function entry(path: string): PaneEntryInfo {
  return { name: path.split(/[\\/]/).pop() ?? path, path, isDir: false, size: 10, modifiedMs: 0 };
}

describe('PreviewStore.setTarget inside an effect (FER-454)', () => {
  let cleanup: (() => void) | null = null;
  afterEach(() => {
    cleanup?.();
    cleanup = null;
  });

  it.each([
    ['disk file', 'C:\\data\\a.txt'],
    ['zip member', 'C:\\data\\src.zip\\docs\\a.txt'],
    ['7z member', 'C:\\data\\src.7z\\img\\b.png'],
  ])('runs the driving effect once per selection (%s)', (_label, path) => {
    const store = new PreviewStore();
    const sel = $state({ path });
    let runs = 0;
    cleanup = $effect.root(() => {
      $effect(() => {
        runs++;
        if (runs > 50) throw new Error('effect loop');
        store.setTarget(entry(sel.path));
      });
    });
    flushSync();
    expect(runs).toBe(1);
    expect(store.target?.path).toBe(path);
    expect(store.state).toBe('loading');

    sel.path = path + '.other';
    flushSync();
    expect(runs).toBe(2);
    expect(store.target?.path).toBe(path + '.other');
    store.reset();
  });
});
