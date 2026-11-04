/** FER-593: the SVG thumbnail queue bounds concurrent reads and only yields inline SVG data URLs. */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSvgThumbQueue, SVG_THUMB_CONCURRENCY } from './svgThumbs';
import type { ImagePreview } from './previewImage';

const readImagePreview = vi.fn();
vi.mock('./previewImage', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  readImagePreview: (...a: unknown[]) => readImagePreview(...a),
}));

function svgPreview(): ImagePreview {
  return {
    downsampled: false,
    inline: true,
    mime: 'image/svg+xml',
    data: 'PHN2Zy8+',
    width: 0,
    height: 0,
    originalWidth: 0,
    originalHeight: 0,
  };
}

const flush = async () => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};

beforeEach(() => {
  readImagePreview.mockReset();
});

describe('createSvgThumbQueue', () => {
  it('resolves an inline SVG to a data URL', async () => {
    readImagePreview.mockResolvedValue(svgPreview());
    const url = await createSvgThumbQueue('s').request('C:\\a.svg').promise;
    expect(url).toBe('data:image/svg+xml;base64,PHN2Zy8+');
  });

  it('never has more than the limit of reads in flight', async () => {
    let inFlight = 0;
    let peak = 0;
    const gates: (() => void)[] = [];
    readImagePreview.mockImplementation(async () => {
      inFlight++;
      peak = Math.max(peak, inFlight);
      await new Promise<void>((r) => gates.push(r));
      inFlight--;
      return svgPreview();
    });
    const q = createSvgThumbQueue('s');
    const handles = Array.from({ length: 12 }, (_, i) => q.request(`C:\\${i}.svg`));
    await flush();
    expect(readImagePreview).toHaveBeenCalledTimes(SVG_THUMB_CONCURRENCY);
    for (let i = 0; i < 40; i++) {
      await flush();
      gates.shift()?.();
    }
    await Promise.all(handles.map((h) => h.promise));
    expect(readImagePreview).toHaveBeenCalledTimes(12);
    expect(peak).toBe(SVG_THUMB_CONCURRENCY);
  });

  it('a request cancelled before its turn is never read', async () => {
    const gates: (() => void)[] = [];
    readImagePreview.mockImplementation(async () => {
      await new Promise<void>((r) => gates.push(r));
      return svgPreview();
    });
    const q = createSvgThumbQueue('s', 1);
    const first = q.request('C:\\1.svg');
    const second = q.request('C:\\2.svg');
    second.cancel();
    expect(await second.promise).toBeNull();
    gates.shift()!();
    await first.promise;
    await flush();
    expect(readImagePreview).toHaveBeenCalledTimes(1);
  });

  it('a failed or oversize read gives null (no throw), as does non-inline data', async () => {
    const q = createSvgThumbQueue('s');
    readImagePreview.mockRejectedValueOnce(new Error('SVG file too large to preview'));
    expect(await q.request('C:\\big.svg').promise).toBeNull();
    readImagePreview.mockResolvedValueOnce({ ...svgPreview(), inline: false, mime: 'image/png' });
    expect(await q.request('C:\\odd.svg').promise).toBeNull();
  });
});
