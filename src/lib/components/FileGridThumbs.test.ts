/**
 * FER-593: Thumbnail View SVG cells (inline data URL in an <img>, icon fallback), size levels as
 * CSS variables, and Ctrl+wheel stepping.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FileGrid from './FileGrid.svelte';
import { toRow } from '../stores/paneStore.svelte';
import type { FileEntry } from '../ipc/types';

const readImagePreview = vi.fn();
vi.mock('../ipc/previewImage', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  readImagePreview: (...a: unknown[]) => readImagePreview(...a),
}));
vi.mock('../ipc/tauri', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  isInTauri: () => true,
}));
vi.mock('../ipc/media', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  mediaGrant: vi.fn(async (paths: string[]) => paths.map(() => null)),
  mediaRevoke: vi.fn(async () => {}),
}));

function row(name: string) {
  const e: FileEntry = {
    name,
    path: `C:\\d\\${name}`,
    isDir: false,
    isSymlink: false,
    size: 1,
    modifiedMs: 1,
    hidden: false,
  };
  return toRow(e);
}

const settle = async () => {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
    await tick();
  }
};

beforeEach(() => {
  readImagePreview.mockReset();
});
afterEach(cleanup);

describe('Thumbnail View SVG cells', () => {
  it('shows an SVG through readImagePreview as an <img> with a data URL only', async () => {
    readImagePreview.mockResolvedValue({
      downsampled: false,
      inline: true,
      mime: 'image/svg+xml',
      data: 'PHN2Zy8+',
      width: 0,
      height: 0,
      originalWidth: 0,
      originalHeight: 0,
    });
    const { container } = render(FileGrid, { rows: [row('logo.svg')], thumbnails: true });
    await settle();
    expect(readImagePreview).toHaveBeenCalledWith('C:\\d\\logo.svg');
    const img = container.querySelector('img.thumb') as HTMLImageElement;
    expect(img.getAttribute('src')).toBe('data:image/svg+xml;base64,PHN2Zy8+');
    expect(container.querySelector('object,embed,iframe')).toBeNull();
  });

  it('falls back to the file icon for an oversize or unreadable SVG, without a toast', async () => {
    readImagePreview.mockRejectedValue(new Error('SVG file too large to preview'));
    const { container } = render(FileGrid, { rows: [row('big.svg')], thumbnails: true });
    await settle();
    expect(readImagePreview).toHaveBeenCalledTimes(1);
    expect(container.querySelector('img.thumb')).toBeNull();
    expect(container.querySelector('.thumbBox svg')).not.toBeNull(); // FileIcon
  });

  it('plain (non-thumbnail) grid never reads SVGs', async () => {
    render(FileGrid, { rows: [row('logo.svg')] });
    await settle();
    expect(readImagePreview).not.toHaveBeenCalled();
  });
});

describe('Thumbnail size levels in the grid', () => {
  it('sets the box and column size from the level (Medium is the old look)', async () => {
    const { container, rerender } = render(FileGrid, { rows: [], thumbnails: true });
    const grid = container.querySelector('.grid') as HTMLElement;
    expect(grid.style.getPropertyValue('--thumb-box')).toBe('72px');
    expect(grid.style.getPropertyValue('--thumb-col')).toBe('124px');
    await rerender({ rows: [], thumbnails: true, thumbSize: 'xlarge' });
    await tick();
    expect(grid.style.getPropertyValue('--thumb-box')).toBe('224px');
    expect(grid.style.getPropertyValue('--thumb-col')).toBe('272px');
  });

  it('Ctrl+wheel steps one level (up = larger) and is cancelled so the webview does not zoom', () => {
    const onthumbstep = vi.fn();
    const { container } = render(FileGrid, { rows: [], thumbnails: true, onthumbstep });
    const body = container.querySelector('.body') as HTMLElement;
    const up = new WheelEvent('wheel', { deltaY: -100, ctrlKey: true, cancelable: true });
    body.dispatchEvent(up);
    expect(onthumbstep).toHaveBeenLastCalledWith(1);
    expect(up.defaultPrevented).toBe(true);
    body.dispatchEvent(new WheelEvent('wheel', { deltaY: 100, ctrlKey: true, cancelable: true }));
    expect(onthumbstep).toHaveBeenLastCalledWith(-1);
    expect(onthumbstep).toHaveBeenCalledTimes(2);
    // a plain wheel scrolls as usual
    const plain = new WheelEvent('wheel', { deltaY: 100, cancelable: true });
    body.dispatchEvent(plain);
    expect(plain.defaultPrevented).toBe(false);
    expect(onthumbstep).toHaveBeenCalledTimes(2);
  });

  it('ignores Ctrl+wheel outside Thumbnail View', () => {
    const onthumbstep = vi.fn();
    const { container } = render(FileGrid, { rows: [], onthumbstep });
    (container.querySelector('.body') as HTMLElement).dispatchEvent(
      new WheelEvent('wheel', { deltaY: -100, ctrlKey: true, cancelable: true }),
    );
    expect(onthumbstep).not.toHaveBeenCalled();
  });
});
