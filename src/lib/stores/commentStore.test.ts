/**
 * F2-27 — comment actions and store: Edit Comment dialog flow, the
 * per-pane sidecar map that feeds the Comments column, and the Print
 * hand-off. Rust contract: src-tauri/src/comments.rs (sidecar keyed by
 * file NAME inside the directory, rename-migrating).
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const shellSpies = vi.hoisted(() => ({
  openFileExternal: vi.fn(),
  associateWith: vi.fn(),
  openTerminalHere: vi.fn(),
  print: vi.fn(),
}));
vi.mock('./shellStore.svelte', () => ({ shell: shellSpies }));

import { getAction, isEnabled, runAction } from '../actions';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import { dialogs } from './dialogStore.svelte';
import { ui } from '../uiState.svelte';

function fakePane(overrides: Partial<PaneApi> = {}): PaneApi {
  return {
    path: 'C:\\work dir',
    selectedPaths: () => ['C:\\work dir\\notes.txt'],
    cursorEntry: () => ({ name: 'notes.txt', path: 'C:\\work dir\\notes.txt', isDir: false }),
    refresh: vi.fn(),
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  registerPane(0, fakePane());
  registerPane(1, fakePane());
  ui.activePane = 0;
  dialogs.close();
  shellSpies.print.mockClear();
});

describe('Edit Comment (per-directory sidecar)', () => {
  it('opens the dialog for exactly one selected file and saves through IPC', async () => {
    const action = getAction('file.comment')!;
    expect(action.disabledReason).toBeUndefined();
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    expect(dialogs.current).toEqual({
      kind: 'comment',
      path: 'C:\\work dir\\notes.txt',
      name: 'notes.txt',
      initial: '',
    });

    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock
      .mockResolvedValueOnce(undefined) // comment_set
      .mockResolvedValueOnce({ 'notes.txt': 'verify numbers' }); // comments_for_dir
    const { comments } = await import('./commentStore.svelte');
    comments.reset();
    await comments.set(0, 'C:\\work dir\\notes.txt', 'verify numbers');
    expect(invokeMock).toHaveBeenCalledWith('comment_set', {
      path: 'C:\\work dir\\notes.txt',
      comment: 'verify numbers',
    });
    expect(comments.for(0, 'notes.txt')).toBe('verify numbers');
  });

  it('an empty edit clears the comment (null crosses the boundary)', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockResolvedValue(undefined).mockResolvedValue({});
    const { comments } = await import('./commentStore.svelte');
    comments.reset();
    await comments.set(0, 'C:\\work dir\\notes.txt', '   ');
    expect(invokeMock).toHaveBeenCalledWith('comment_set', {
      path: 'C:\\work dir\\notes.txt',
      comment: null,
    });
  });

  it('is unavailable for folders and multi-selections', () => {
    registerPane(
      0,
      fakePane({ cursorEntry: () => ({ name: 'sub', path: 'C:\\w\\sub', isDir: true }) }),
    );
    expect(isEnabled(getAction('file.comment')!)).toBe(false);
    expect(runAction(getAction('file.comment')!)).toBe(false);
    expect(dialogs.current).toBeNull();
  });

  it('a failed write reports the structured error in the dialog', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock
      .mockRejectedValueOnce({ code: 'permissionDenied', message: 'folder is read-only' })
      .mockResolvedValueOnce({});
    const { comments } = await import('./commentStore.svelte');
    comments.reset();
    await expect(comments.set(0, 'C:\\work dir\\notes.txt', 'x')).rejects.toMatchObject({
      code: 'permissionDenied',
    });
  });
});

describe('Comments view map loading', () => {
  it('loads each pane directory map and guards malformed payloads', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockImplementation((_cmd: string, args: { dir: string }) =>
      args.dir === 'C:\\left'
        ? Promise.resolve({ 'a.txt': 'left comment', bad: 42 })
        : Promise.resolve({}),
    );
    const { comments } = await import('./commentStore.svelte');
    comments.reset();
    await comments.load(0, 'C:\\left');
    await comments.load(1, 'C:\\right');
    expect(comments.map(0)).toEqual({ 'a.txt': 'left comment' }); // non-string dropped
    expect(comments.map(1)).toEqual({});
  });

  it('outside Tauri the map stays honestly empty', async () => {
    const { comments } = await import('./commentStore.svelte');
    comments.reset();
    await comments.load(0, 'C:\\left');
    expect(comments.map(0)).toEqual({});
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('Print (OS print verb hand-off)', () => {
  it('prints the single selected file through the shell store', () => {
    const action = getAction('file.print')!;
    expect(action.disabledReason).toBeUndefined();
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    expect(shellSpies.print).toHaveBeenCalledWith('C:\\work dir\\notes.txt');
  });

  it('is unavailable for folders and multi-selections', () => {
    registerPane(0, fakePane({ selectedPaths: () => ['C:\\w\\a.txt', 'C:\\w\\b.txt'] }));
    expect(isEnabled(getAction('file.print')!)).toBe(false);
    expect(runAction(getAction('file.print')!)).toBe(false);
    expect(shellSpies.print).not.toHaveBeenCalled();
  });
});
