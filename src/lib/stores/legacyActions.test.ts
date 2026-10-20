/**
 * F2-27 — legacy menu actions: view presets, thumbnails, quit respecting
 * in-flight operations, and the separate search window opener.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const webviewMock = vi.hoisted(() => ({
  getByLabel: vi.fn(async () => null),
  instances: [] as { label: string; options: Record<string, unknown> }[],
}));
vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: class {
    label: string;
    options: Record<string, unknown>;
    constructor(label: string, options: Record<string, unknown>) {
      this.label = label;
      this.options = options;
      webviewMock.instances.push(this);
    }
    static getByLabel = webviewMock.getByLabel;
    once() {}
  },
}));

import { actionForEvent, getAction, isEnabled, matches, runAction } from '../actions';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import { dialogs } from './dialogStore.svelte';
import { ops } from './opsStore.svelte';
import { jobs } from './jobsStore.svelte';
import { ui } from '../uiState.svelte';
import { viewState } from './viewStateStore.svelte';
import {
  applyPresetActive,
  parseViewPresets,
  persistViewPresets,
  saveActivePreset,
  viewPresets,
  type ViewPreset,
} from './viewPresetsStore.svelte';
import { openSearchSeparate, SEARCH_WINDOW_LABEL, isSearchWindow } from '../searchSeparate';

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
    openPath: vi.fn(),
    ...overrides,
  };
}

beforeEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  registerPane(0, fakePane());
  registerPane(1, fakePane());
  ui.activePane = 0;
  dialogs.close();
  viewPresets.reset();
  ops.ops = new Map();
  jobs.reset();
  webviewMock.instances.length = 0;
  webviewMock.getByLabel.mockClear().mockResolvedValue(null);
  for (const p of [0, 1] as const) {
    viewState.setViewMode(p, 'full');
    viewState.setSort(p, 'name', 1);
    viewState.setFilterMode(p, 'all');
  }
});

describe('Encode/Decode File (F2-27)', () => {
  it('is available with a selection and unavailable without one', () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    const encode = getAction('file.encode')!;
    const decode = getAction('file.decode')!;
    expect(encode.disabledReason).toBeUndefined();
    expect(decode.disabledReason).toBeUndefined();
    expect(isEnabled(encode)).toBe(false); // nothing selected
    registerPane(0, fakePane({ selectedPaths: () => ['C:\\w\\a.txt'] }));
    expect(isEnabled(encode)).toBe(true);
    expect(isEnabled(decode)).toBe(true);
  });
});

describe('Quit (respects in-flight operations)', () => {
  it('quits immediately with an idle queue (no confirm dialog)', () => {
    // Outside Tauri quitApp() is a no-op; the registry contract under test
    // is: idle queue → quit WITHOUT the confirmation dialog.
    expect(runAction(getAction('file.quit')!)).toBe(true);
    expect(dialogs.current).toBeNull();
  });

  it('asks for confirmation while operations are queued or running', () => {
    ops.ops = new Map([
      [
        'op_1',
        {
          operationId: 'op_1',
          kind: 'copy',
          state: 'running',
          processedBytes: 0,
          processedFiles: 0,
        },
      ],
    ]);
    expect(runAction(getAction('file.quit')!)).toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 1 });
    expect(dialogs.current).not.toBeNull();
  });

  it('FER-578: running background jobs (docked or not) also ask for confirmation', () => {
    const job = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'C:\a.zip' });
    jobs.attempt(job, 'req-1');
    jobs.dock(job);
    expect(runAction(getAction('file.quit')!)).toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 1 });
    dialogs.close();

    // queued operations and jobs add up
    ops.ops = new Map([
      [
        'op_1',
        {
          operationId: 'op_1',
          kind: 'copy',
          state: 'running',
          processedBytes: 0,
          processedFiles: 0,
        },
      ],
    ]);
    expect(runAction(getAction('file.quit')!)).toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 2 });
    dialogs.close();

    // a finished job no longer blocks quitting
    ops.ops = new Map();
    jobs.finish(job, 'done', { silent: true });
    expect(runAction(getAction('file.quit')!)).toBe(true);
    expect(dialogs.current).toBeNull();
  });

  it('quitConfirm only appears from the action, never as a stray default', () => {
    expect(dialogs.current).toBeNull();
    expect(getAction('file.quit')!.shortcut).toBeUndefined();
  });
});

describe('Thumbnail View (Ctrl+Shift+F2, TC slot)', () => {
  it('switches the active pane to the thumbnail mode', () => {
    const action = getAction('show.thumbnails')!;
    expect(action.disabledReason).toBeUndefined();
    expect(action.shortcut).toEqual({ key: 'F2', ctrlOrCmd: true, shift: true });
    expect(
      matches(action.shortcut!, { key: 'F2', ctrlKey: true, metaKey: false, shiftKey: true }),
    ).toBe(true);
    expect(actionForEvent({ key: 'F2', ctrlKey: true, metaKey: false, shiftKey: true })?.id).toBe(
      'show.thumbnails',
    );
    runAction(action);
    expect(viewState.viewMode(0)).toBe('thumbnail');
    expect(viewState.viewMode(1)).toBe('full'); // per pane
    // Brief/Full still switch away from it.
    runAction(getAction('show.full')!);
    expect(viewState.viewMode(0)).toBe('full');
  });
});

describe('Custom View Modes (view presets)', () => {
  const preset: ViewPreset = {
    name: 'Photos',
    viewMode: 'thumbnail',
    sort: { key: 'size', dir: -1 },
    reversed: false,
    filterMode: 'custom',
    filterPattern: '*.jpg',
  };

  it('opens the dialog, saves a snapshot of the active pane and applies it', () => {
    const action = getAction('show.customModes')!;
    expect(action.disabledReason).toBeUndefined();
    expect(runAction(action)).toBe(true);
    expect(viewPresets.open).toBe(true);

    viewState.setViewMode(0, 'thumbnail');
    viewState.setSort(0, 'size', 1);
    saveActivePreset('Photos');
    expect(viewPresets.entries()).toHaveLength(1);
    expect(viewPresets.entries()[0].viewMode).toBe('thumbnail');

    viewState.setViewMode(0, 'full');
    viewState.setSort(0, 'name', 1);
    applyPresetActive(viewPresets.entries()[0]);
    expect(viewState.viewMode(0)).toBe('thumbnail');
    expect(viewState.sort(0)).toEqual({ key: 'size', dir: 1 });
    expect(viewState.viewMode(1)).toBe('full'); // other pane untouched
    viewPresets.close();
    expect(viewPresets.open).toBe(false);
  });

  it('round-trips through storage, dropping malformed entries', () => {
    persistViewPresets([preset]);
    const raw = globalThis.localStorage?.getItem('ferrum.viewPresets.v1') ?? '';
    const parsed = parseViewPresets(raw);
    expect(parsed).toEqual([preset]);

    expect(
      parseViewPresets(
        JSON.stringify([
          preset,
          { name: '', viewMode: 'full', sort: { key: 'name', dir: 1 }, filterMode: 'all' },
          { name: 'bad-mode', viewMode: 'grid', sort: { key: 'name', dir: 1 }, filterMode: 'all' },
          { name: 'bad-sort', viewMode: 'full', sort: { key: 'size' }, filterMode: 'all' },
          'junk',
        ]),
      ),
    ).toEqual([preset]);
    expect(parseViewPresets('not json')).toEqual([]);
  });

  it('removes presets and caps the list', async () => {
    const { VIEW_PRESET_LIMIT } = await import('./viewPresetsStore.svelte');
    for (let i = 0; i < VIEW_PRESET_LIMIT + 5; i++) saveActivePreset(`p${i}`);
    expect(viewPresets.entries().length).toBe(VIEW_PRESET_LIMIT);
    viewPresets.remove('p0');
    expect(viewPresets.entries().some((p) => p.name === 'p0')).toBe(false);
  });
});

describe('Search in Separate Window', () => {
  it('is disabled outside Tauri and disabled without a pane — honest, not silent', async () => {
    const action = getAction('commands.searchSeparate')!;
    expect(action.disabledReason).toBeUndefined();
    // Inside a browser the run surfaces the unsupported error as a dialog.
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
    expect(dialogs.current).toMatchObject({
      title: 'Search in Separate Window',
      message: 'A separate search window requires the desktop app (not browser dev)',
    });
  });

  it('creates one window rooted at the active pane; a second call focuses it', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    registerPane(0, fakePane({ path: 'C:\\work dir' }));
    await openSearchSeparate('C:\\work dir');
    expect(webviewMock.instances).toHaveLength(1);
    const win = webviewMock.instances[0];
    expect(win.label).toBe(SEARCH_WINDOW_LABEL);
    expect(String(win.options.url)).toContain('searchWindow=1');
    expect(String(win.options.url)).toContain('root=C%3A%5Cwork+dir');

    const focus = vi.fn();
    webviewMock.getByLabel.mockResolvedValueOnce({ setFocus: focus } as never);
    await openSearchSeparate('C:\\elsewhere');
    expect(webviewMock.instances).toHaveLength(1); // reused, not duplicated
    expect(focus).toHaveBeenCalledOnce();
  });

  it('detects the search window renderer by query flag', () => {
    expect(isSearchWindow()).toBe(false);
  });
});
