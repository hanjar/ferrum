/**
 * FER-103 — Quick View PDF viewer iframe and downsampled images. IPC is
 * mocked at the module boundary; no Tauri/webview involved.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import QuickView from './QuickView.svelte';
import PdfPreview from './PdfPreview.svelte';
import { previewStore } from '../stores/previewStore.svelte';
import { ui } from '../uiState.svelte';
import type { PreviewData } from '../ipc/preview';
import type { ImagePreview } from '../ipc/previewImage';

const readPreviewMock = vi.fn();
const readArchiveMock = vi.fn();
const readImageMock = vi.fn();
const openExternalMock = vi.fn();

vi.mock('../ipc/preview', () => ({
  readPreview: (...a: unknown[]) => readPreviewMock(...a),
  readArchiveMemberPreview: (...a: unknown[]) => readArchiveMock(...a),
}));
vi.mock('../ipc/previewImage', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  readImagePreview: (...a: unknown[]) => readImageMock(...a),
}));
vi.mock('../ipc/shellOpen', () => ({
  openExternal: (...a: unknown[]) => openExternalMock(...a),
}));
// FER-453: the viewer gets its bytes from read_pdf_bytes; these tests never load them.
const readPdfBytesMock = vi.fn<(path: string) => Promise<ArrayBuffer>>(() => new Promise(() => {}));
vi.mock('../ipc/pdf', () => ({
  readPdfBytes: (p: string) => readPdfBytesMock(p),
}));
vi.mock('../ipc/media', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  mediaGrantOne: async (path: string) =>
    `http://ferrum-media.localhost/tok-${encodeURIComponent(path)}`,
  mediaRevoke: vi.fn(async () => {}),
}));

function data(kind: string, over: Partial<PreviewData> = {}): PreviewData {
  return {
    kind: kind as PreviewData['kind'],
    data: '',
    truncated: false,
    tooLarge: false,
    size: 9,
    modifiedMs: 1,
    ...over,
  };
}

function img(over: Partial<ImagePreview> = {}): ImagePreview {
  return {
    downsampled: false,
    mime: 'image/png',
    data: '',
    width: 10,
    height: 10,
    originalWidth: 10,
    originalHeight: 10,
    ...over,
  };
}

async function show(name: string, path: string, d: PreviewData, size = 9) {
  readPreviewMock.mockResolvedValue(d);
  ui.quickViewOpen = true;
  previewStore.setTarget({ name, path, isDir: false, size, modifiedMs: 1 });
  await previewStore.flush();
  const r = render(QuickView);
  await tick();
  return r;
}

async function settle() {
  for (let i = 0; i < 10; i++) {
    await Promise.resolve();
    await tick();
  }
}

beforeEach(() => {
  ui.preview.maxTextBytes = 2_000_000;
  readPreviewMock.mockReset();
  readArchiveMock.mockReset();
  readImageMock.mockReset();
  openExternalMock.mockReset();
  previewStore.reset();
  ui.quickViewOpen = false;
});

afterEach(() => {
  cleanup();
  previewStore.reset();
  ui.quickViewOpen = false;
});

describe('PDF preview', () => {
  it('a PDF under the cap opens in the sandboxed pdf.js viewer, plus Open in default app', async () => {
    const r = await show(
      'doc.pdf',
      'C:\\d\\doc.pdf',
      data('pdf', { size: 20_000_000 }),
      20_000_000,
    );
    const frame = r.container.querySelector('iframe') as HTMLIFrameElement | null;
    expect(frame).not.toBeNull();
    expect(frame!.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame!.getAttribute('src')).toBe('./pdf-viewer.html');
    expect(r.container.querySelector('embed, object')).toBeNull();
    expect(readPdfBytesMock).toHaveBeenCalledWith('C:\\d\\doc.pdf');
    expect(readPreviewMock).toHaveBeenCalledOnce();
    const btn = r.getByText('Open in default app');
    await fireEvent.click(btn);
    expect(openExternalMock).toHaveBeenCalledWith('C:\\d\\doc.pdf');
  });

  it('over the cap: message plus Open in default app, no iframe', async () => {
    const r = await show(
      'big.pdf',
      'C:\\d\\big.pdf',
      data('pdf', { tooLarge: true, size: 80_000_000 }),
      80_000_000,
    );
    expect(r.container.querySelector('iframe')).toBeNull();
    expect(r.container.textContent).toContain('larger than the preview limit');
    const btn = [...r.container.querySelectorAll('button')].find((b) =>
      b.textContent?.includes('Open in default app'),
    );
    expect(btn).toBeTruthy();
    await fireEvent.click(btn!);
    expect(openExternalMock).toHaveBeenCalledWith('C:\\d\\big.pdf');
  });

  it('a renamed non-PDF comes back as binary and never reaches the viewer', async () => {
    const r = await show('fake.pdf', 'C:\\d\\fake.pdf', data('binary'));
    expect(r.container.querySelector('iframe')).toBeNull();
    expect(r.container.textContent).toContain('Binary file');
  });

  it('PDF inside an archive is a message only', async () => {
    readArchiveMock.mockResolvedValue(data('binary'));
    ui.quickViewOpen = true;
    previewStore.setTarget({
      name: 'a.pdf',
      path: 'C:\\d\\pack.zip\\inner\\a.pdf',
      isDir: false,
      size: 5,
      modifiedMs: 1,
    });
    await previewStore.flush();
    const r = render(QuickView);
    await tick();
    expect(r.container.querySelector('iframe')).toBeNull();
    expect(r.container.textContent).toContain('PDF inside an archive');
  });

  it('open failure is reported, not swallowed', async () => {
    openExternalMock.mockRejectedValue(new Error('no association'));
    const r = render(PdfPreview, { props: { path: 'C:\\d\\x.pdf' } });
    await fireEvent.click(r.getByText('Open in default app'));
    await settle();
    expect(r.container.querySelector('[role="alert"]')?.textContent).toContain('no association');
  });
});

describe('downsampled image preview', () => {
  it('small image gets a ferrum-media src', async () => {
    readImageMock.mockResolvedValue(img());
    const r = await show('p.png', 'C:\\d\\p.png', data('image'));
    await settle();
    const el = r.container.querySelector('img.image') as HTMLImageElement;
    expect(el.src).toBe('http://ferrum-media.localhost/tok-C%3A%5Cd%5Cp.png');
    expect(readImageMock).toHaveBeenCalledWith('C:\\d\\p.png');
    expect(r.container.textContent).not.toContain('Downsampled');
  });

  it('big image shows the shrunk data URL and says so', async () => {
    readImageMock.mockResolvedValue(
      img({
        downsampled: true,
        mime: 'image/jpeg',
        data: 'QUJD',
        width: 2048,
        height: 1229,
        originalWidth: 5000,
        originalHeight: 3000,
      }),
    );
    const r = await show('big.png', 'C:\\d\\big.png', data('image'), 30_000_000);
    await settle();
    const el = r.container.querySelector('img.image') as HTMLImageElement;
    expect(el.src).toBe('data:image/jpeg;base64,QUJD');
    expect(r.container.textContent).toContain('2048×1229');
    expect(r.container.textContent).toContain('5000×3000');
  });

  it('backend refusal (decompression bomb) is shown as an error, no <img>', async () => {
    readImageMock.mockRejectedValue(new Error('Image dimensions are too large to preview safely'));
    const r = await show('bomb.png', 'C:\\d\\bomb.png', data('image'));
    await settle();
    expect(r.container.querySelector('img.image')).toBeNull();
    expect(r.container.querySelector('[role="alert"]')?.textContent).toContain('too large');
  });
});
