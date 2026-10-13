import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PrefsStore } from './prefsStore.svelte';
import {
  ui,
  TABS_DEFAULTS,
  PREVIEW_DEFAULTS,
  type TabsSettings,
  type PreviewSettings,
} from '../uiState.svelte';
import { DEFAULT_FILE_OPERATION_PREFS, loadPrefs, savePrefs } from '../ipc/prefs';
import { registerPane, type PaneApi } from './panesRegistry.svelte';

// The IPC surface is mocked like FilePane.test.ts mocks its mutations module:
// the store under test must run its real load/validate/persist logic — only
// loadPrefs/savePrefs are stubbed, sanitizeSession stays the real one.
vi.mock('../ipc/prefs', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../ipc/prefs')>();
  return { ...actual, loadPrefs: vi.fn(), savePrefs: vi.fn() };
});

const TABS: TabsSettings = { ...TABS_DEFAULTS };
const PREVIEW: PreviewSettings = { ...PREVIEW_DEFAULTS };

const DEFAULTS = {
  theme: 'dark',
  palette: 'violet',
  showHidden: false,
  terminal: 'auto',
  paneSplit: 0.5,
  tabs: TABS,
  preview: PREVIEW,
  // FER-370 appearance fields (D50): defaults mirror the backend.
  density: 'compact',
  iconStyle: 'colored',
  iconTheme: 'Tile',
  uiFontLevel: 'm',
  showExtensions: true,
  foldersFirst: true,
} as const;

const mockedLoad = vi.mocked(loadPrefs);
const mockedSave = vi.mocked(savePrefs);

/** Minimal pane API with a session provider for the FER-395 hook tests. */
function paneWithSession(session: { tabs: string[]; current: number }): PaneApi {
  return {
    path: 'C:\\t',
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    getSession: () => session,
  };
}

/** Stores created by the running test — stopped after each one so no
 *  persist effect root from a previous test reacts to the ui resets. */
const stores: PrefsStore[] = [];
function makeStore(): PrefsStore {
  const store = new PrefsStore();
  stores.push(store);
  return store;
}

/** The singleton ui carries state between tests — reset the persisted fields. */
function resetUi(): void {
  ui.theme = 'dark';
  ui.palette = 'violet';
  ui.showHidden = false;
  ui.terminal = 'auto';
  ui.paneSplit = 0.5;
  ui.tabs = { ...TABS };
  ui.preview = { ...PREVIEW };
  ui.addressOwnRow = false;
  ui.indexerFolderSizes = false;
  ui.syncJournalSkip = false;
}

beforeEach(() => {
  mockedLoad.mockReset();
  mockedSave.mockReset();
  mockedSave.mockResolvedValue(undefined);
  resetUi();
});

afterEach(() => {
  while (stores.length) stores.pop()?.stop();
  vi.useRealTimers();
});

describe('prefsStore — paneSplit (FER-406)', () => {
  it('init applies a valid saved paneSplit to ui', async () => {
    mockedLoad.mockResolvedValue({ ...DEFAULTS, paneSplit: 0.7 });
    const store = makeStore();
    await store.init();
    expect(ui.paneSplit).toBe(0.7);
    expect(store.ready).toBe(true);
  });

  it('init falls back to 0.5 for out-of-range, non-finite or missing values', async () => {
    for (const saved of [0.95, 0.1, Number.NaN, undefined]) {
      resetUi();
      mockedLoad.mockResolvedValue({
        ...DEFAULTS,
        paneSplit: saved as unknown as number,
      });
      const store = makeStore();
      await store.init();
      expect(ui.paneSplit).toBe(0.5);
    }
  });

  it('a failed load keeps the default paneSplit and still reports ready', async () => {
    mockedLoad.mockRejectedValue(new Error('no desktop app'));
    const store = makeStore();
    await store.init();
    expect(ui.paneSplit).toBe(0.5);
    expect(store.ready).toBe(true);
  });

  it('persists paneSplit changes (debounced) alongside the other fields', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();

    ui.paneSplit = 0.42;
    await vi.advanceTimersByTimeAsync(10); // inside the 300ms debounce window
    expect(mockedSave).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    expect(mockedSave.mock.calls[0][0]).toEqual({
      theme: 'dark',
      palette: 'violet',
      showHidden: false,
      terminal: 'auto',
      paneSplit: 0.42,
      tabs: TABS,
      preview: PREVIEW,
      session: null,
      density: 'compact',
      iconStyle: 'colored',
      iconTheme: 'Tile',
      uiFontLevel: 'm',
      showExtensions: true,
      foldersFirst: true,
      diskUsageFastOffer: true,
      diskUsageCacheWarnHours: 24,
      addressOwnRow: false,
      indexerFolderSizes: false,
      syncJournalSkip: false,
      rarExePath: null,
      ...DEFAULT_FILE_OPERATION_PREFS,
    });
  });

  it('does not re-save unchanged values after init (echo dedupe)', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS, paneSplit: 0.65 });
    const store = makeStore();
    await store.init();
    await vi.advanceTimersByTimeAsync(1000);
    expect(mockedSave).not.toHaveBeenCalled();
  });
});

describe('prefsStore — addressOwnRow (FER-491)', () => {
  it('defaults to off, applies a saved true, and treats garbage as off', async () => {
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    await makeStore().init();
    expect(ui.addressOwnRow).toBe(false);
    mockedLoad.mockResolvedValue({ ...DEFAULTS, addressOwnRow: true });
    await makeStore().init();
    expect(ui.addressOwnRow).toBe(true);
    ui.addressOwnRow = false;
    mockedLoad.mockResolvedValue({ ...DEFAULTS, addressOwnRow: 'yes' as unknown as boolean });
    await makeStore().init();
    expect(ui.addressOwnRow).toBe(false);
  });

  it('a change reaches the debounced save payload', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();
    ui.addressOwnRow = true;
    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    expect(mockedSave.mock.calls[0][0].addressOwnRow).toBe(true);
  });
});

describe('prefsStore — tabs section (FER-395)', () => {
  it('init applies a saved tabs object to ui', async () => {
    mockedLoad.mockResolvedValue({
      ...DEFAULTS,
      tabs: {
        openNextToCurrent: false,
        restoreTabs: true,
        tabWidth: 'fixed',
        closeOnMiddleClick: false,
      },
    });
    const store = makeStore();
    await store.init();
    expect(ui.tabs).toEqual({
      openNextToCurrent: false,
      restoreTabs: true,
      tabWidth: 'fixed',
      closeOnMiddleClick: false,
    });
  });

  it('invalid or partial tabs shapes fall back field-wise to the defaults', async () => {
    for (const bad of [undefined, 'nope', 7, { tabWidth: 'wide' }]) {
      resetUi();
      mockedLoad.mockResolvedValue({ ...DEFAULTS, tabs: bad as unknown as TabsSettings });
      const store = makeStore();
      await store.init();
      expect(ui.tabs).toEqual(TABS);
    }
  });

  it('a tabs change reaches the debounced save payload', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();

    ui.tabs.tabWidth = 'fixed';
    ui.tabs.openNextToCurrent = false;
    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    const payload = mockedSave.mock.calls[0][0];
    expect(payload.tabs).toEqual({ ...TABS, tabWidth: 'fixed', openNextToCurrent: false });
  });
});

describe('prefsStore — preview section (FER-396)', () => {
  it('init applies a saved preview object to ui', async () => {
    const saved = {
      openOnLaunch: true,
      maxTextBytes: 500_000,
      syntaxHighlight: false,
      autoplayVideo: true,
      readMetadata: false,
    };
    mockedLoad.mockResolvedValue({ ...DEFAULTS, preview: saved });
    await makeStore().init();
    expect(ui.preview).toEqual(saved);
  });

  it('missing, invalid or partial preview falls back field-wise to defaults', async () => {
    for (const bad of [undefined, null, 'x', { maxTextBytes: 123, openOnLaunch: 'yes' }]) {
      mockedLoad.mockResolvedValue({ ...DEFAULTS, preview: bad as unknown as PreviewSettings });
      await makeStore().init();
      expect(ui.preview).toEqual(PREVIEW);
    }
    mockedLoad.mockResolvedValue({
      ...DEFAULTS,
      preview: { ...PREVIEW, maxTextBytes: 123, autoplayVideo: true },
    });
    await makeStore().init();
    expect(ui.preview).toEqual({ ...PREVIEW, autoplayVideo: true });
  });

  it('a preview change reaches the debounced save payload', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();

    ui.preview.maxTextBytes = 4_000_000;
    ui.preview.syntaxHighlight = false;
    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    expect(mockedSave.mock.calls[0][0].preview).toEqual({
      ...PREVIEW,
      maxTextBytes: 4_000_000,
      syntaxHighlight: false,
    });
  });
});

describe('prefsStore — session snapshot (FER-395)', () => {
  const SESSION = { panes: [{ tabs: ['C:\\a', 'C:\\b'], current: 1 }] };

  it('a saved session is kept when restore is on and dropped when off', async () => {
    mockedLoad.mockResolvedValue({ ...DEFAULTS, session: SESSION });
    let store = makeStore();
    await store.init();
    expect(store.session).toEqual(SESSION);

    resetUi();
    mockedLoad.mockResolvedValue({
      ...DEFAULTS,
      tabs: { ...TABS, restoreTabs: false },
      session: SESSION,
    });
    store = makeStore();
    await store.init();
    expect(store.session).toBeNull(); // ignored — the next persist clears it
  });

  it('malformed sessions are treated as absent', async () => {
    for (const bad of [
      null,
      'nope',
      {},
      { panes: 'x' },
      { panes: [{ tabs: [], current: 0 }] },
      { panes: [{ tabs: ['', '  '], current: 0 }] },
      { panes: [{ tabs: 'C:\\a' }] },
    ]) {
      mockedLoad.mockResolvedValue({ ...DEFAULTS, session: bad as never });
      const store = makeStore();
      await store.init();
      expect(store.session, JSON.stringify(bad)).toBeNull();
    }
  });

  it('clamps an out-of-range current while keeping valid paths', async () => {
    mockedLoad.mockResolvedValue({
      ...DEFAULTS,
      session: { panes: [{ tabs: ['C:\\a', 'C:\\b'], current: 9 }] },
    });
    const store = makeStore();
    await store.init();
    expect(store.session).toEqual({ panes: [{ tabs: ['C:\\a', 'C:\\b'], current: 0 }] });
  });

  it('tab mutations ride the debounced save as a validated session', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();
    const unregister = registerPane(0, paneWithSession({ tabs: ['C:\\x', 'C:\\y'], current: 1 }));

    store.noteSessionChanged();
    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    const payload = mockedSave.mock.calls[0][0];
    expect(payload.session).toEqual({ panes: [{ tabs: ['C:\\x', 'C:\\y'], current: 1 }] });
    expect(payload.tabs).toEqual(TABS);
    unregister();
  });

  it('restore off keeps the session field empty on disk', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS, tabs: { ...TABS, restoreTabs: false } });
    const store = makeStore();
    await store.init();
    const unregister = registerPane(0, paneWithSession({ tabs: ['C:\\x'], current: 0 }));

    store.noteSessionChanged(); // must not collect while restore is off
    ui.paneSplit = 0.6; // any pref change triggers the persist
    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    expect(mockedSave.mock.calls[0][0].session).toBeNull();
    unregister();
  });

  it('an unchanged session snapshot does not re-save (echo dedupe)', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS, session: SESSION });
    const store = makeStore();
    await store.init();
    const unregister = registerPane(0, paneWithSession(SESSION.panes[0]));

    store.noteSessionChanged(); // same content as loaded
    await vi.advanceTimersByTimeAsync(1000);
    expect(mockedSave).not.toHaveBeenCalled();
    unregister();
  });

  it('drops panes with no valid paths and caps tabs per pane', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();
    const paths = Array.from({ length: 20 }, (_, i) => `C:\\t${i}`);
    const unregister = registerPane(0, paneWithSession({ tabs: paths, current: 19 }));

    store.noteSessionChanged();
    await vi.advanceTimersByTimeAsync(400);
    const session = mockedSave.mock.calls[0][0].session;
    expect(session?.panes[0].tabs.length).toBe(12);
    expect(session?.panes[0].tabs[11]).toBe('C:\\t11'); // capped at MAX_SESSION_TABS
    unregister();
  });

  it('the persisted payload never carries a password (FER-504)', async () => {
    // The session may contain an open archive's path; its password must stay
    // in the pane's memory only. A schema-level guard: if a password field
    // ever reaches Prefs (or the session snapshot), this fails.
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();
    const unregister = registerPane(
      0,
      paneWithSession({ tabs: ['C:\\a\\enc-header.rar', 'C:\\b'], current: 0 }),
    );

    store.noteSessionChanged();
    await vi.advanceTimersByTimeAsync(400);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    const json = JSON.stringify(mockedSave.mock.calls[0][0]).toLowerCase();
    expect(json).toContain('enc-header.rar'); // the session really carried the archive
    expect(json).not.toContain('password');
    unregister();
  });
});

describe('prefsStore — flush (FER-505)', () => {
  it('saves the edited value immediately, without waiting for the debounce', async () => {
    vi.useFakeTimers();
    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    const store = makeStore();
    await store.init();
    ui.rarExePath = 'D:\\Tools\\WinRAR\\Rar.exe';
    await expect(store.flush()).resolves.toBe(true);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    expect(mockedSave.mock.calls[0][0].rarExePath).toBe('D:\\Tools\\WinRAR\\Rar.exe');
    // The pending debounced save was dropped: no second write afterwards.
    await vi.advanceTimersByTimeAsync(1000);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    ui.rarExePath = null;
  });

  it('reports false when the save fails, and is a no-op before init', async () => {
    const idle = makeStore();
    await expect(idle.flush()).resolves.toBe(false);
    expect(mockedSave).not.toHaveBeenCalled();

    mockedLoad.mockResolvedValue({ ...DEFAULTS });
    mockedSave.mockRejectedValue(new Error('disk full'));
    const store = makeStore();
    await store.init();
    await expect(store.flush()).resolves.toBe(false);
  });
});
