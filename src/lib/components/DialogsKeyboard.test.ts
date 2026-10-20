/**
 * FER-577 (B1): keyboard-only operation of Dialogs.svelte: Alt+letter mnemonics, roving radios,
 * Tab trap, focus return, accessible names, and the modalDepth gate on global shortcuts.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import App from '../../App.svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { dialog, duplicateMnemonics, modalDepth, resetModalDepth } from '../dialogKit';

const archiveMocks = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
}));
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: archiveMocks.archiveCreate,
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: true })),
  archiveRarToolDetect: vi.fn(async () => null),
  newArchiveRequestId: () => 'req-kbd-1',
  listenArchiveProgress: vi.fn(async () => () => {}),
}));
const mutMocks = vi.hoisted(() => ({
  planCopy: vi.fn(),
  createEntry: vi.fn(),
  deleteEntries: vi.fn().mockResolvedValue({ requested: 1, failed: [] }),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  renameEntry: vi.fn(),
}));
vi.mock('../ipc/mutations', () => mutMocks);
vi.mock('../ipc/clipboardFiles', () => ({ clipboardFinishCut: vi.fn() }));

const unregisters: (() => void)[] = [];
function register(selected: string[]): void {
  const api: PaneApi = {
    path: 'C:/Data/Invoice',
    selectedPaths: () => selected,
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  unregisters.push(registerPane(0, api));
}

beforeEach(() => {
  archiveMocks.archiveCreate.mockReset();
  mutMocks.deleteEntries.mockClear();
  ui.activePane = 0;
});
afterEach(() => {
  dialogs.close();
  for (const off of unregisters.splice(0)) off();
  resetModalDepth();
});

const modal = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
function alt(k: string): KeyboardEvent {
  const ev = new KeyboardEvent('keydown', {
    key: k,
    altKey: true,
    bubbles: true,
    cancelable: true,
  });
  (document.activeElement ?? modal()).dispatchEvent(ev);
  return ev;
}

describe('Pack dialog, keyboard only', () => {
  async function openPack(format: 'zip' | 'sevenz' = 'zip') {
    register(['C:/Data/Invoice/a.txt']);
    dialogs.beginPack(format);
    render(Dialogs);
    await tick();
    await vi.waitFor(() =>
      expect(modal().querySelector('input[name="packFormat"][value="rar"]')).not.toBeNull(),
    );
  }

  it('Alt+7, Alt+M, Alt+P pack 7z with maximum compression without the mouse', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('x');
    await openPack();
    expect(alt('7').defaultPrevented).toBe(true);
    await tick();
    expect(modal().textContent).toContain('Invoice.7z');
    alt('m');
    await tick();
    expect(modal().querySelector<HTMLInputElement>('input[name="packLevel"]:checked')?.value).toBe(
      'max',
    );
    alt('p');
    await vi.waitFor(() => expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1));
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      ['C:/Data/Invoice/a.txt'],
      'C:/Data/Invoice.7z',
      'sevenz',
      undefined,
      expect.any(String),
      'max',
    );
  });

  it('Alt+Z / Alt+R switch format, Alt+S/F/N/M switch level', async () => {
    await openPack();
    alt('r');
    await tick();
    expect(modal().querySelector<HTMLInputElement>('input[name="packFormat"]:checked')?.value).toBe(
      'rar',
    );
    alt('z');
    await tick();
    expect(modal().querySelector<HTMLInputElement>('input[name="packFormat"]:checked')?.value).toBe(
      'zip',
    );
    for (const [k, v] of [
      ['s', 'store'],
      ['f', 'fast'],
      ['n', 'normal'],
      ['m', 'max'],
    ]) {
      alt(k);
      await tick();
      expect(
        modal().querySelector<HTMLInputElement>('input[name="packLevel"]:checked')?.value,
      ).toBe(v);
    }
  });

  it('shows "ZIP/7Z/RAR" once (no raw "SEVENZ" line) and no mixed-in language', async () => {
    await openPack('sevenz');
    expect(modal().textContent).not.toContain('SEVENZ');
    expect(modal().textContent).not.toMatch(/Format:/);
    expect(modal().textContent).toContain('7Z');
    expect(modal().textContent).not.toMatch(/Kompresi|Simpan|Cepat|Maksimal|Memeriksa/);
  });

  it('arrow keys move through the compression radios, Home/End jump', async () => {
    await openPack();
    const normal = modal().querySelector<HTMLInputElement>(
      'input[name="packLevel"][value="normal"]',
    )!;
    normal.focus();
    normal.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }),
    );
    await tick();
    expect(document.activeElement).toBe(
      modal().querySelector('input[name="packLevel"][value="max"]'),
    );
    document.activeElement!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Home', bubbles: true, cancelable: true }),
    );
    await tick();
    expect(modal().querySelector<HTMLInputElement>('input[name="packLevel"]:checked')?.value).toBe(
      'store',
    );
  });

  it('has a human accessible name, unique mnemonics, and a trapped Tab', async () => {
    await openPack();
    const label = modal().getAttribute('aria-labelledby')!;
    expect(document.getElementById(label)?.textContent).toBe('Pack to archive');
    expect(modal().getAttribute('aria-label')).toBeNull();
    expect(duplicateMnemonics(modal())).toEqual([]);
    const pack = Array.from(modal().querySelectorAll('button')).find((b) =>
      b.textContent?.includes('Pack'),
    )!;
    pack.focus();
    const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    pack.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true); // last control wraps to the first
    expect(modal().contains(document.activeElement)).toBe(true);
  });
});

describe('Delete dialog, keyboard only', () => {
  it('Alt+P toggles "permanently"; the unlabeled Delete button has no accelerator', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'delete', paths: ['C:/x/a.txt'], permanent: false });
    await tick();
    const box = modal().querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    expect(box.checked).toBe(false);
    alt('p');
    await tick();
    expect(box.checked).toBe(true);
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
    expect(modal().querySelectorAll('button[data-mnemonic]').length).toBe(0);
  });

  it('Esc cancels without deleting; focus returns to the opener', async () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    render(Dialogs);
    dialogs.open({ kind: 'delete', paths: ['C:/x/a.txt'], permanent: false });
    await tick();
    expect(document.activeElement).toBe(modal());
    fireEvent.keyDown(modal(), { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});

describe('every dialog kind: unique access letters', () => {
  const kinds = [
    { kind: 'mkdir', parent: 'C:/x' },
    { kind: 'rename', path: 'C:/x/a', original: 'a' },
    { kind: 'password', archivePath: 'C:/x/a.zip', retry: () => Promise.resolve() },
    { kind: 'selectGroup', parent: 'C:/x', unselect: false },
    { kind: 'saveSelection', parent: 'C:/x', names: ['a'] },
    { kind: 'loadSelection', parent: 'C:/x' },
    { kind: 'showFilter', parent: 'C:/x' },
    { kind: 'comment', path: 'C:/x/a', name: 'a', initial: '' },
    { kind: 'quitConfirm', operations: 1 },
    {
      kind: 'extractConflicts',
      target: 'C:/x',
      conflicts: [{ memberPath: 'a', destinationPath: 'C:/x/a' }],
      run: () => {},
    },
    {
      kind: 'copy',
      paths: ['C:/x/a'],
      target: 'D:/t',
      totalFiles: 1,
      totalBytes: 1,
      conflicts: [{ dest: 'D:/t/a' }],
    },
    { kind: 'delete', paths: ['C:/x/a'], permanent: false },
    {
      kind: 'fastScanOffer',
      purpose: 'diskUsage',
      volume: 'C',
      root: 'C:/',
      fastAvailable: true,
      indexAvailable: true,
      choose: () => {},
    },
  ];
  for (const k of kinds) {
    it(`${k.kind}: no letter used twice, accessible name present`, async () => {
      render(Dialogs);
      dialogs.open(k as never);
      await tick();
      expect(duplicateMnemonics(modal())).toEqual([]);
      const title = document.getElementById(modal().getAttribute('aria-labelledby')!);
      expect(title?.textContent?.trim().length).toBeGreaterThan(2);
      expect(title?.textContent).not.toBe(k.kind);
    });
  }
});

describe('X1: shortcuts behind a modal', () => {
  it('F7 and F8 do nothing while any modal holds the keyboard; Ctrl+K still opens the palette', async () => {
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    const host = document.createElement('div');
    host.tabIndex = -1;
    document.body.append(host);
    const act = dialog(host);
    expect(modalDepth()).toBe(1);
    fireEvent.keyDown(window, { key: 'F7' });
    fireEvent.keyDown(window, { key: 'F8' });
    fireEvent.keyDown(window, { key: 'Delete' });
    await tick();
    expect(dialogs.current).toBeNull();
    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    await tick();
    expect(ui.paletteOpen).toBe(true);
    ui.paletteOpen = false;
    act.destroy();
    host.remove();
    fireEvent.keyDown(window, { key: 'F7' });
    await tick();
    expect(dialogs.current?.kind).toBe('mkdir');
  });
});
