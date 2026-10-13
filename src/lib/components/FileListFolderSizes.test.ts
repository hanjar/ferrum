import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FileList from './FileList.svelte';
import { toRow } from '../stores/paneStore.svelte';
import type { FileEntry } from '../ipc/types';
import { ui } from '../uiState.svelte';
import { folderSizes } from '../stores/folderSizesStore.svelte';

const indexerFolderSizes = vi.hoisted(() => vi.fn());
vi.mock('../ipc/indexer', async (orig) => ({
  ...(await orig<typeof import('../ipc/indexer')>()),
  indexerFolderSizes: (...a: unknown[]) => indexerFolderSizes(...a),
}));

const SEP = String.fromCharCode(92);
const dir = (name: string): FileEntry => ({
  name,
  path: ['C:', 'Data', name].join(SEP),
  isDir: true,
  isSymlink: false,
  size: 0,
  modifiedMs: 1_790_512_000_000,
  hidden: false,
});

afterEach(() => {
  cleanup();
  ui.indexerFolderSizes = false;
  folderSizes.clear();
  indexerFolderSizes.mockReset();
});

function sizeOf(container: HTMLElement, name: string): string | null | undefined {
  const row = [...container.querySelectorAll('.row[data-path]')].find(
    (r) => r.querySelector('.nameText')?.textContent === name,
  );
  return row?.querySelector('.size')?.textContent;
}

describe('FileList folder sizes (FER-477a)', () => {
  it('fills folder rows from the indexer when the setting is on', async () => {
    vi.useFakeTimers();
    ui.indexerFolderSizes = true;
    indexerFolderSizes.mockResolvedValue({
      available: true,
      reason: null,
      sizes: [2048, null],
      stale: false,
    });
    const { container } = render(FileList, {
      rows: [toRow(dir('alpha')), toRow(dir('beta'))],
      viewportOverride: 400,
    });
    await vi.advanceTimersByTimeAsync(200);
    vi.useRealTimers();
    await tick();
    expect(indexerFolderSizes).toHaveBeenCalledWith(['C:', 'Data'].join(SEP), ['alpha', 'beta']);
    expect(sizeOf(container, 'alpha')).toBe('2.00 KB');
    expect(sizeOf(container, 'beta')).toBe('—');
  });

  it('never asks and keeps "—" while the setting is off', async () => {
    vi.useFakeTimers();
    const { container } = render(FileList, {
      rows: [toRow(dir('alpha'))],
      viewportOverride: 400,
    });
    await vi.advanceTimersByTimeAsync(200);
    vi.useRealTimers();
    expect(indexerFolderSizes).not.toHaveBeenCalled();
    expect(sizeOf(container, 'alpha')).toBe('—');
  });
});
