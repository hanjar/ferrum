// FER-376 (APP-05) remaining acceptance, automated at the unit/integration
// level: (1) "System" follows OS changes live without rewriting the stored
// preference; (2) every appearance field survives a simulated restart
// (persist -> fresh store init -> document attributes).
// LIMIT: jsdom with a stubbed matchMedia — this is NOT the real WebView2 /
// Windows personalization toggle; see the handoff note for the owner check.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ui } from './uiState.svelte';
import { PrefsStore } from './stores/prefsStore.svelte';
import { loadPrefs, savePrefs, type Prefs } from './ipc/prefs';

vi.mock('./ipc/prefs', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./ipc/prefs')>();
  return { ...actual, loadPrefs: vi.fn(), savePrefs: vi.fn() };
});

type Listener = () => void;
function stubOsScheme(initialLight: boolean) {
  let light = initialLight;
  const listeners = new Set<Listener>();
  const mq = {
    get matches() {
      return light;
    },
    addEventListener: (_: string, l: Listener) => listeners.add(l),
    removeEventListener: (_: string, l: Listener) => listeners.delete(l),
  };
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => mq),
  );
  window.matchMedia = globalThis.matchMedia;
  return {
    setLight(v: boolean) {
      light = v;
      for (const l of [...listeners]) l();
    },
    count: () => listeners.size,
  };
}

function reset() {
  ui.setTheme('dark');
  ui.palette = 'violet';
  ui.density = 'compact';
  ui.iconStyle = 'colored';
  ui.uiFontLevel = 'm';
  ui.showExtensions = true;
  ui.foldersFirst = true;
  ui.applyToDocument();
}

beforeEach(reset);
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  reset();
});

describe('System theme follows the OS live (APP-01/05)', () => {
  it('re-applies data-theme on each OS change and keeps the stored choice "system"', () => {
    const os = stubOsScheme(false);
    ui.setTheme('system');
    expect(document.documentElement.dataset.theme).toBe('dark');
    const stop = ui.watchSystemTheme();
    expect(os.count()).toBe(1);

    os.setLight(true);
    expect(document.documentElement.dataset.theme).toBe('light');
    expect(ui.theme).toBe('system');
    os.setLight(false);
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(ui.theme).toBe('system');

    stop();
    expect(os.count()).toBe(0);
    os.setLight(true);
    expect(document.documentElement.dataset.theme).toBe('dark'); // no listener left
  });

  it('ignores OS changes when the user picked an explicit mode', () => {
    const os = stubOsScheme(false);
    ui.setTheme('dark');
    const stop = ui.watchSystemTheme();
    os.setLight(true);
    expect(document.documentElement.dataset.theme).toBe('dark');
    ui.setTheme('light');
    os.setLight(false);
    expect(document.documentElement.dataset.theme).toBe('light');
    stop();
  });

  it('palette and the other attributes are untouched by an OS flip', () => {
    const os = stubOsScheme(false);
    ui.palette = 'orange';
    ui.density = 'comfortable';
    ui.uiFontLevel = 'l';
    ui.setTheme('system');
    const stop = ui.watchSystemTheme();
    os.setLight(true);
    const d = document.documentElement.dataset;
    expect([d.palette, d.density, d.ui, d.theme]).toEqual(['orange', 'comfortable', 'l', 'light']);
    stop();
  });
});

describe('Appearance persists across a restart (APP-03/05)', () => {
  it('saved payload restores every appearance field and the document attributes', async () => {
    vi.useFakeTimers();
    const mockedLoad = vi.mocked(loadPrefs);
    const mockedSave = vi.mocked(savePrefs);
    mockedSave.mockResolvedValue(undefined);

    // Run 1: boot from defaults, then the user changes everything.
    mockedLoad.mockResolvedValue({
      theme: 'dark',
      palette: 'violet',
      density: 'compact',
      iconStyle: 'colored',
      iconTheme: 'Tile',
      uiFontLevel: 'm',
      showExtensions: true,
      foldersFirst: true,
    } as unknown as Prefs);
    const run1 = new PrefsStore();
    await run1.init();
    stubOsScheme(false);
    ui.setTheme('system');
    ui.palette = 'yellow';
    ui.density = 'comfortable';
    ui.iconStyle = 'monochrome';
    ui.uiFontLevel = 'xl';
    ui.showExtensions = false;
    ui.foldersFirst = false;
    await vi.advanceTimersByTimeAsync(500);
    expect(mockedSave).toHaveBeenCalled();
    const saved = mockedSave.mock.calls.at(-1)![0] as Prefs;
    run1.stop();

    // Restart: wipe ui + document, boot a fresh store from the saved payload.
    stubOsScheme(true);
    reset();
    delete document.documentElement.dataset.palette;
    mockedLoad.mockResolvedValue(saved);
    const run2 = new PrefsStore();
    await run2.init();
    run2.stop();

    expect(ui.theme).toBe('system');
    expect(ui.palette).toBe('yellow');
    expect(ui.density).toBe('comfortable');
    expect(ui.iconStyle).toBe('monochrome');
    expect(ui.uiFontLevel).toBe('xl');
    expect(ui.showExtensions).toBe(false);
    expect(ui.foldersFirst).toBe(false);
    ui.applyToDocument();
    const d = document.documentElement.dataset;
    // system + OS currently light -> resolves to light on the restarted run
    expect([d.theme, d.palette, d.density, d.icon, d.ui]).toEqual([
      'light',
      'yellow',
      'comfortable',
      'monochrome',
      'xl',
    ]);
  });
});
