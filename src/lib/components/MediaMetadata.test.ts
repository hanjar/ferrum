/** PRV-06: MediaMetadata component. OFF hides, ON shows, no external links. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import MediaMetadata from './MediaMetadata.svelte';
import { mediaMetaStore } from '../stores/mediaMetaStore.svelte';
import { ui } from '../uiState.svelte';

const readMock = vi.fn();
vi.mock('../ipc/mediaMeta', () => ({
  readMediaMetadata: (...a: unknown[]) => readMock(...a),
}));

const full = {
  kind: 'image',
  width: 800,
  height: 600,
  camera: { make: 'Canon', model: 'EOS' },
  takenAt: '2024-05-17 13:45:09',
  gps: { lat: -6.5, lon: 106.75 },
  other: [['ISO', '400']],
};
const flush = async () => {
  await new Promise((r) => setTimeout(r, 0));
  await tick();
};
const box = (c: HTMLElement) => c.querySelector('[data-testid="media-metadata"]');

beforeEach(() => {
  readMock.mockReset();
  mediaMetaStore.reset();
  ui.preview.readMetadata = true;
});
afterEach(cleanup);

describe('MediaMetadata', () => {
  it('shows compact rows and GPS as plain text only', async () => {
    readMock.mockResolvedValue(full);
    const { container, getByText } = render(MediaMetadata, { path: '/a.jpg' });
    await flush();
    expect(getByText('800 × 600')).toBeTruthy();
    expect(getByText('Canon EOS')).toBeTruthy();
    expect(getByText('6.50000° S, 106.75000° E')).toBeTruthy();
    expect(getByText('400')).toBeTruthy();
    expect(container.querySelector('a, img, iframe, object, embed')).toBeNull();
  });

  it('renders nothing and never invokes when OFF', async () => {
    ui.preview.readMetadata = false;
    const { container } = render(MediaMetadata, { path: '/a.jpg' });
    await flush();
    expect(readMock).not.toHaveBeenCalled();
    expect(box(container)).toBeNull();
  });

  it('hides already-shown metadata when switched OFF', async () => {
    readMock.mockResolvedValue(full);
    const { container } = render(MediaMetadata, { path: '/a.jpg' });
    await flush();
    expect(box(container)).not.toBeNull();
    ui.preview.readMetadata = false;
    await flush();
    expect(box(container)).toBeNull();
    expect(mediaMetaStore.data).toBeNull();
  });

  it('empty on error and on null path', async () => {
    readMock.mockRejectedValue(new Error('boom'));
    const { container, rerender } = render(MediaMetadata, { path: '/a.jpg' });
    await flush();
    expect(box(container)).toBeNull();
    await rerender({ path: null });
    await flush();
    expect(box(container)).toBeNull();
  });
});
