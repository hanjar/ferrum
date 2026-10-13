import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';

const archiveMocks = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  archiveCancelRequest: vi.fn(),
  archiveRarToolProbe: vi.fn(),
  newArchiveRequestId: () => 'req-pack-dialog-rar',
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
  archiveRarToolProbe: () => archiveMocks.archiveRarToolProbe(),
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

const DIR = 'C:\\Data\\ArchiveTests';
const FILE_A = 'C:\\Data\\ArchiveTests\\doc.txt';

beforeEach(() => {
  archiveMocks.archiveCreate.mockReset();
  archiveMocks.archiveCancelRequest.mockReset();
  archiveMocks.archiveRarToolProbe.mockReset();
  refresh = vi.fn();
  ui.activePane = 0;
});

afterEach(() => {
  dialogs.close();
  for (const off of unregisters.splice(0)) off();
});

describe('FER-505: Pack dialog RAR format conditional display and execution', () => {
  it('hides RAR option when probe returns ok: false', async () => {
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: false,
      reason: 'Rar.exe not configured',
    });
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');

    const { container } = render(Dialogs);
    await tick();
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolProbe).toHaveBeenCalled());

    const rarRadio = container.querySelector<HTMLInputElement>(
      'input[name="packFormat"][value="rar"]',
    );
    expect(rarRadio).toBeNull();
  });

  it('shows RAR option when probe returns ok: true', async () => {
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: true,
      version: 'RAR 7.00 x64',
    });
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');

    const { container } = render(Dialogs);
    await tick();
    await vi.waitFor(() => {
      const rarRadio = container.querySelector<HTMLInputElement>(
        'input[name="packFormat"][value="rar"]',
      );
      expect(rarRadio).not.toBeNull();
    });
  });

  it('selecting RAR updates output target extension to .rar and submits format rar', async () => {
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: true,
      version: 'RAR 7.00 x64',
    });
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\ArchiveTests.rar');
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');

    const { container, getByText } = render(Dialogs);
    await tick();

    await vi.waitFor(() => {
      const rarRadio = container.querySelector<HTMLInputElement>(
        'input[name="packFormat"][value="rar"]',
      );
      expect(rarRadio).not.toBeNull();
    });

    const rarRadio = container.querySelector<HTMLInputElement>(
      'input[name="packFormat"][value="rar"]',
    )!;
    await fireEvent.click(rarRadio);
    await tick();

    expect(dialogs.current).toMatchObject({
      format: 'rar',
      target: 'C:\\Data\\ArchiveTests.rar',
    });

    // Output span displays .rar
    const mono = container.querySelector('span.mono');
    expect(mono?.textContent).toBe('C:\\Data\\ArchiveTests.rar');

    // Submit pack
    await fireEvent.click(getByText('Pack', { selector: 'button' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());

    expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1);
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [FILE_A],
      'C:\\Data\\ArchiveTests.rar',
      'rar',
      undefined,
      expect.any(String),
      'normal',
    );
  });

  it('switching from RAR back to ZIP updates target extension back to .zip', async () => {
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: true,
      version: 'RAR 7.00 x64',
    });
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');

    const { container } = render(Dialogs);
    await tick();

    await vi.waitFor(() => {
      const rarRadio = container.querySelector<HTMLInputElement>(
        'input[name="packFormat"][value="rar"]',
      );
      expect(rarRadio).not.toBeNull();
    });

    const rarRadio = container.querySelector<HTMLInputElement>(
      'input[name="packFormat"][value="rar"]',
    )!;
    await fireEvent.click(rarRadio);
    await tick();
    expect(dialogs.current).toMatchObject({
      format: 'rar',
      target: 'C:\\Data\\ArchiveTests.rar',
    });

    const zipRadio = container.querySelector<HTMLInputElement>(
      'input[name="packFormat"][value="zip"]',
    )!;
    await fireEvent.click(zipRadio);
    await tick();
    expect(dialogs.current).toMatchObject({
      format: 'zip',
      target: 'C:\\Data\\ArchiveTests.zip',
    });
  });
});
