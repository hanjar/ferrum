/**
 * F2-25 — shell-launch store: open external, Associate With picker and
 * Open Terminal Here, with structured errors surfaced as honest dialogs.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { getAction, isEnabled, labelOf, runAction } from '../actions';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import { dialogs } from './dialogStore.svelte';
import { ui } from '../uiState.svelte';

function fakePane(overrides: Partial<PaneApi> = {}): PaneApi {
  return {
    path: 'C:\\work dir',
    selectedPaths: () => [],
    cursorEntry: () => undefined,
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
  ui.terminal = 'auto';
  dialogs.close();
});

describe('open external (Enter on a file)', () => {
  it('launches the file through the OS association from the registry', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockResolvedValue(undefined);
    registerPane(
      0,
      fakePane({
        cursorEntry: () => ({ name: 'a.txt', path: 'C:\\work dir\\a.txt', isDir: false }),
      }),
    );
    const action = getAction('file.open')!;
    expect(labelOf(action)).toBe('Open with associated application');
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('open_external', { path: 'C:\\work dir\\a.txt' }),
    );
    expect(dialogs.current).toBeNull();
  });

  it('folders still navigate instead of launching', () => {
    const openCursorEntry = vi.fn();
    registerPane(
      0,
      fakePane({
        cursorEntry: () => ({ name: 'sub', path: 'C:\\work dir\\sub', isDir: true }),
        openCursorEntry,
      }),
    );
    const action = getAction('file.open')!;
    expect(labelOf(action)).toBe('Open selected folder');
    expect(runAction(action)).toBe(true);
    expect(openCursorEntry).toHaveBeenCalledOnce();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('surfaces a structured association failure as an error dialog', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockRejectedValue({
      code: 'notFound',
      message: 'No application is associated with this file for the requested action',
    });
    registerPane(
      0,
      fakePane({ cursorEntry: () => ({ name: 'a.xyz', path: 'C:\\w\\a.xyz', isDir: false }) }),
    );
    runAction(getAction('file.open')!);
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
    expect(dialogs.current).toMatchObject({
      title: 'Open',
      message: 'No application is associated with this file for the requested action',
    });
  });
});

describe('Associate With… (OS picker, never a silent change)', () => {
  it('is available for exactly one selected file and invokes the picker verb', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockResolvedValue(undefined);
    registerPane(
      0,
      fakePane({
        selectedPaths: () => ['C:\\work dir\\a.txt'],
        cursorEntry: () => ({ name: 'a.txt', path: 'C:\\work dir\\a.txt', isDir: false }),
      }),
    );
    const action = getAction('file.associate')!;
    expect(action.disabledReason).toBeUndefined();
    expect(action.shortcut).toBeUndefined(); // no keyboard slot claimed
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('associate_with', { path: 'C:\\work dir\\a.txt' }),
    );
  });

  it('stays unavailable for folders and multi-selections', () => {
    registerPane(
      0,
      fakePane({ cursorEntry: () => ({ name: 'sub', path: 'C:\\w\\sub', isDir: true }) }),
    );
    expect(isEnabled(getAction('file.associate')!)).toBe(false);
    expect(runAction(getAction('file.associate')!)).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();

    registerPane(
      0,
      fakePane({
        selectedPaths: () => ['C:\\w\\a.txt', 'C:\\w\\b.txt'],
        cursorEntry: () => ({ name: 'a.txt', path: 'C:\\w\\a.txt', isDir: false }),
      }),
    );
    expect(isEnabled(getAction('file.associate')!)).toBe(false);
  });
});

describe('Open Command Prompt (terminal here)', () => {
  it('launches the configured terminal at the active pane folder', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockResolvedValue(undefined);
    ui.terminal = 'wt';
    const action = getAction('commands.prompt')!;
    expect(action.disabledReason).toBeUndefined();
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith('open_terminal', {
        cwd: 'C:\\work dir',
        program: 'wt',
      }),
    );
  });

  it('requires an active pane', () => {
    registerPane(0, fakePane())();
    registerPane(1, fakePane())();
    expect(isEnabled(getAction('commands.prompt')!)).toBe(false);
    expect(runAction(getAction('commands.prompt')!)).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('prompts via a dialog when the terminal executable is missing', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    invokeMock.mockRejectedValue({
      code: 'notFound',
      message: 'program not found',
      path: 'C:\\work dir',
    });
    runAction(getAction('commands.prompt')!);
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
    expect(dialogs.current).toMatchObject({
      title: 'Open Terminal Here',
      message: 'program not found',
    });
  });
});
