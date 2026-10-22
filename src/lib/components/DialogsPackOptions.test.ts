import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { runPack } from '../packService';

const archiveMocks = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  archiveCancelRequest: vi.fn(),
  newArchiveRequestId: () => 'req-pack-opt-1',
  listenArchiveProgress: vi.fn(async () => () => {}) as (cb: unknown) => Promise<() => void>,
}));

vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: archiveMocks.archiveCreate,
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: (id: string) => archiveMocks.archiveCancelRequest(id),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
  newArchiveRequestId: () => archiveMocks.newArchiveRequestId(),
  listenArchiveProgress: (cb: (e: unknown) => void) => archiveMocks.listenArchiveProgress(cb),
}));

const unregisters: (() => void)[] = [];
let refresh = vi.fn();

function register(id: 0 | 1, path: string, selected: string[]): void {
  const api: PaneApi = {
    path,
    selectedPaths: () => selected,
    cursorEntry: () => undefined,
    refresh: () => refresh(),
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  unregisters.push(registerPane(id, api));
}

const DIR = 'C:\\Data\\Reports';
const FILE_A = 'C:\\Data\\Reports\\summary.txt';

beforeEach(() => {
  archiveMocks.archiveCreate.mockReset();
  archiveMocks.archiveCancelRequest.mockReset();
  refresh = vi.fn();
  ui.activePane = 0;
});

afterEach(() => {
  dialogs.close();
  for (const off of unregisters.splice(0)) off();
});

describe('FER-59 / FER-66: Pack compression level options in Dialogs and packService', () => {
  it('dialog renders compression options with Normal as default', async () => {
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');
    const { container } = render(Dialogs);
    await tick();

    const radios = container.querySelectorAll<HTMLInputElement>('input[name="packLevel"]');
    expect(radios.length).toBe(4);
    const checked = container.querySelector<HTMLInputElement>('input[name="packLevel"]:checked');
    expect(checked?.value).toBe('normal');
  });

  it('dialog submits chosen compression level when user selects Simpan (store)', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\Reports.zip');
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');
    const { container, getByRole } = render(Dialogs);
    await tick();

    const storeRadio = container.querySelector<HTMLInputElement>(
      'input[name="packLevel"][value="store"]',
    );
    expect(storeRadio).not.toBeNull();
    if (storeRadio) {
      storeRadio.checked = true;
      await fireEvent.change(storeRadio);
      await tick();
    }

    await fireEvent.click(getByRole('button', { name: 'Pack' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());

    expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1);
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [FILE_A],
      'C:\\Data\\Reports.zip',
      'zip',
      undefined,
      expect.any(String),
      'store',
    );
  });

  it('dialog submits chosen compression level when user selects Maksimal (max)', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\Reports.7z');
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('sevenz');
    const { container, getByRole } = render(Dialogs);
    await tick();

    const maxRadio = container.querySelector<HTMLInputElement>(
      'input[name="packLevel"][value="max"]',
    );
    expect(maxRadio).not.toBeNull();
    if (maxRadio) {
      await fireEvent.click(maxRadio);
      await tick();
    }

    await fireEvent.click(getByRole('button', { name: 'Pack' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());

    expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1);
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [FILE_A],
      'C:\\Data\\Reports.7z',
      'sevenz',
      undefined,
      expect.any(String),
      'max',
    );
  });

  it('packService forwards explicit level (fast)', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\test.zip');
    await runPack({
      paths: [FILE_A],
      target: 'C:\\Data\\test.zip',
      format: 'zip',
      level: 'fast',
    });

    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [FILE_A],
      'C:\\Data\\test.zip',
      'zip',
      undefined,
      expect.any(String),
      'fast',
    );
  });

  it('caller without level (e.g. AI adapter) does not pass level argument to archiveCreate', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\ai.zip');
    await runPack({
      paths: [FILE_A],
      target: 'C:\\Data\\ai.zip',
      format: 'zip',
      conflict: 'skip',
    });

    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [FILE_A],
      'C:\\Data\\ai.zip',
      'zip',
      'skip',
      expect.any(String),
    );
  });
});

describe('FER-579: Ultra (text) 7z level', () => {
  const CSV = 'C:\\Data\\Reports\\sales.csv';
  const LOG = 'C:\\Data\\Reports\\app.log';
  const radio = (c: HTMLElement) =>
    c.querySelector<HTMLInputElement>('input[name="packLevel"][value="ultra-text"]');

  it('is offered for 7z only', async () => {
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');
    const { container } = render(Dialogs);
    await tick();
    expect(radio(container)).toBeNull();
    const sevenz = container.querySelector<HTMLInputElement>(
      'input[name="packFormat"][value="sevenz"]',
    );
    expect(sevenz).not.toBeNull();
    if (sevenz) await fireEvent.click(sevenz);
    await tick();
    expect(radio(container)).not.toBeNull();
    expect(container.querySelectorAll('input[name="packLevel"]').length).toBe(5);
    expect(container.querySelector('[data-testid="pack-ultra-text-hint"]')).not.toBeNull();
  });

  it('mostly-text selection on 7z: preselected with a hint, still changeable', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\Reports.7z');
    register(0, DIR, [CSV, LOG]);
    dialogs.beginPack('sevenz');
    const { container, getByRole } = render(Dialogs);
    await tick();
    expect(radio(container)?.checked).toBe(true);
    expect(container.querySelector('[data-testid="pack-ultra-text-hint"]')?.textContent).toContain(
      'Mostly text files',
    );
    const normal = container.querySelector<HTMLInputElement>(
      'input[name="packLevel"][value="normal"]',
    );
    if (normal) await fireEvent.click(normal);
    await tick();
    await fireEvent.click(getByRole('button', { name: 'Pack' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [CSV, LOG],
      'C:\\Data\\Reports.7z',
      'sevenz',
      undefined,
      expect.any(String),
      'normal',
    );
  });

  it('a non-text selection is not preselected', async () => {
    register(0, DIR, [FILE_A.replace('.txt', '.png')]);
    dialogs.beginPack('sevenz');
    const { container } = render(Dialogs);
    await tick();
    expect(radio(container)?.checked).toBe(false);
    expect(container.querySelector('input[name="packLevel"]:checked')?.getAttribute('value')).toBe(
      'normal',
    );
  });

  it('submits ultra-text for 7z, and switching to ZIP drops it back to normal', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\Reports.7z');
    register(0, DIR, [CSV]);
    dialogs.beginPack('sevenz');
    const { container, getByRole } = render(Dialogs);
    await tick();
    const zip = container.querySelector<HTMLInputElement>('input[name="packFormat"][value="zip"]');
    if (zip) await fireEvent.click(zip);
    await tick();
    expect(radio(container)).toBeNull();
    expect(container.querySelector('input[name="packLevel"]:checked')?.getAttribute('value')).toBe(
      'normal',
    );
    const sevenz = container.querySelector<HTMLInputElement>(
      'input[name="packFormat"][value="sevenz"]',
    );
    if (sevenz) await fireEvent.click(sevenz);
    await tick();
    expect(radio(container)?.checked).toBe(true);
    await fireEvent.click(getByRole('button', { name: 'Pack' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(archiveMocks.archiveCreate.mock.calls[0][5]).toBe('ultra-text');
  });
});
