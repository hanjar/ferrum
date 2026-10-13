/** FER-452: grid thumbnails get their src from a ferrum-media grant, never a file URL. */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import ThumbImage from './ThumbImage.svelte';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const settle = async () => {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve();
    await tick();
  }
};

describe('ThumbImage', () => {
  it('requests a grant for its path and renders the granted URL', async () => {
    const request = vi.fn(async (p: string) => `http://ferrum-media.localhost/t-${p.length}`);
    const r = render(ThumbImage, { path: 'C:\\d\\p.png', batcher: { scope: 's', request } });
    await settle();
    expect(request).toHaveBeenCalledWith('C:\\d\\p.png');
    const img = r.container.querySelector('img.thumb') as HTMLImageElement;
    expect(img.getAttribute('src')).toMatch(/^http:\/\/ferrum-media\.localhost\//);
    expect(img.getAttribute('src')).not.toContain('asset');
  });

  it('a refused file keeps the img without any src', async () => {
    const request = vi.fn(async () => null);
    const r = render(ThumbImage, { path: 'C:\\d\\x.png', batcher: { scope: 's', request } });
    await settle();
    expect(r.container.querySelector('img.thumb')?.hasAttribute('src')).toBe(false);
  });

  it('re-grants once when the load fails, not forever', async () => {
    const request = vi.fn(async () => 'http://ferrum-media.localhost/tok');
    const r = render(ThumbImage, { path: 'C:\\d\\p.png', batcher: { scope: 's', request } });
    await settle();
    const img = r.container.querySelector('img.thumb') as HTMLImageElement;
    await fireEvent.error(img);
    await settle();
    await fireEvent.error(img);
    await settle();
    expect(request).toHaveBeenCalledTimes(2);
  });

  it('waits for visibility when IntersectionObserver exists', async () => {
    let cb: (e: { isIntersecting: boolean }[]) => void = () => {};
    vi.stubGlobal(
      'IntersectionObserver',
      class {
        constructor(c: typeof cb) {
          cb = c;
        }
        observe() {}
        disconnect() {}
      },
    );
    const request = vi.fn(async () => 'http://ferrum-media.localhost/tok');
    render(ThumbImage, { path: 'C:\\d\\p.png', batcher: { scope: 's', request } });
    await settle();
    expect(request).not.toHaveBeenCalled(); // off screen: no token yet
    cb([{ isIntersecting: true }]);
    await settle();
    expect(request).toHaveBeenCalledTimes(1);
  });
});
