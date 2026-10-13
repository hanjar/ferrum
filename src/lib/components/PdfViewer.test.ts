/**
 * FER-453: the sandboxed viewer's frame attributes, its CSP, what the parent sends,
 * and which messages it accepts. The bytes come through a mocked read_pdf_bytes.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import PdfViewer from './PdfViewer.svelte';
import { PDF_MSG } from '../preview/pdfBridge';
import viewerHtml from '../../../public/pdf-viewer.html?raw';
import viewerMain from '../../pdf-viewer/main.ts?raw';

const readPdfBytesMock = vi.fn();
vi.mock('../ipc/pdf', () => ({
  readPdfBytes: (...a: unknown[]) => readPdfBytesMock(...a),
}));

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await Promise.resolve();
    await tick();
  }
}

afterEach(() => {
  cleanup();
  readPdfBytesMock.mockReset();
});

describe('viewer iframe', () => {
  it('is sandboxed to allow-scripts only: no same-origin, forms, popups or top navigation', async () => {
    readPdfBytesMock.mockReturnValue(new Promise(() => {}));
    const r = render(PdfViewer, { props: { path: 'C:\\d\\a.pdf' } });
    const frame = r.container.querySelector('iframe')!;
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame.getAttribute('allow')).toBeNull();
    expect(frame.getAttribute('src')).toBe('./pdf-viewer.html');
    expect(frame.getAttribute('referrerpolicy')).toBe('no-referrer');
    await settle();
  });

  it('the viewer page carries the strict CSP: no network, scripts and workers from self/blob only', () => {
    const m = viewerHtml.match(/http-equiv="Content-Security-Policy"\s+content="([^"]+)"/);
    expect(m).not.toBeNull();
    expect(m![1]).toBe(
      "default-src 'none'; script-src 'self'; img-src blob: data:; style-src 'self' 'unsafe-inline'",
    );
    expect(m![1]).not.toMatch(/unsafe-eval|blob:\s*;|worker|connect-src|frame-src|ipc|\*/);
  });

  it('the viewer code has no Tauri IPC reference and no eval', () => {
    expect(viewerMain).not.toMatch(/__TAURI|@tauri-apps|__TAURI_INTERNALS__/);
    expect(viewerMain).not.toMatch(/\beval\(|new Function\(/);
  });

  it('sends the bytes and worker to the frame only after it reports ready', async () => {
    const bytes = new ArrayBuffer(16);
    readPdfBytesMock.mockResolvedValue(bytes);
    const r = render(PdfViewer, { props: { path: 'C:\\d\\a.pdf' } });
    await vi.waitFor(() => expect(readPdfBytesMock).toHaveBeenCalledWith('C:\\d\\a.pdf'));
    const frame = r.container.querySelector('iframe') as HTMLIFrameElement;
    const post = vi.spyOn(frame.contentWindow!, 'postMessage').mockImplementation(() => {});
    expect(post).not.toHaveBeenCalled();

    window.dispatchEvent(
      new MessageEvent('message', { data: { type: PDF_MSG.ready }, source: frame.contentWindow }),
    );
    // Bytes and the worker source resolve asynchronously; the send happens once both are in.
    await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(1), { timeout: 15000 });
    const [msg, , transfer] = post.mock.calls[0] as unknown as [
      { type: string; data: ArrayBuffer },
      string,
      ArrayBuffer[],
    ];
    expect(msg.type).toBe(PDF_MSG.load);
    // A copy in the page's realm, transferred (see PdfViewer.svelte): same content, same length.
    expect(msg.data).not.toBe(bytes);
    expect(msg.data.byteLength).toBe(bytes.byteLength);
    // No worker source crosses the boundary: the worker is bundled in-thread.
    expect(Object.keys(msg).sort()).toEqual(['data', 'type']);
    expect(transfer).toEqual([msg.data]);
  });

  it('ignores messages whose source is not the frame', async () => {
    readPdfBytesMock.mockReturnValue(new Promise(() => {}));
    const r = render(PdfViewer, { props: { path: 'C:\\d\\a.pdf' } });
    await settle();
    window.dispatchEvent(
      new MessageEvent('message', {
        data: { type: PDF_MSG.error, message: 'forged' },
        source: null,
      }),
    );
    await settle();
    expect(r.container.textContent).not.toContain('forged');
    expect(r.container.querySelector('[role="alert"]')).toBeNull();
  });

  it('ignores unknown message types from the frame', async () => {
    readPdfBytesMock.mockResolvedValue(new ArrayBuffer(4));
    const r = render(PdfViewer, { props: { path: 'C:\\d\\a.pdf' } });
    await settle();
    const frame = r.container.querySelector('iframe') as HTMLIFrameElement;
    const post = vi.spyOn(frame.contentWindow!, 'postMessage').mockImplementation(() => {});
    window.dispatchEvent(
      new MessageEvent('message', {
        data: { type: 'ferrum-pdf:navigate', url: 'x' },
        source: frame.contentWindow,
      }),
    );
    await settle();
    expect(post).not.toHaveBeenCalled();
  });

  it('shows a viewer error as plain text, not as markup', async () => {
    readPdfBytesMock.mockReturnValue(new Promise(() => {}));
    const r = render(PdfViewer, { props: { path: 'C:\\d\\a.pdf' } });
    await settle();
    const frame = r.container.querySelector('iframe') as HTMLIFrameElement;
    window.dispatchEvent(
      new MessageEvent('message', {
        data: { type: PDF_MSG.error, message: '<img src=x onerror=alert(1)>' },
        source: frame.contentWindow,
      }),
    );
    await settle();
    const alert = r.container.querySelector('[role="alert"]')!;
    expect(alert.textContent).toBe('<img src=x onerror=alert(1)>');
    expect(alert.querySelector('img')).toBeNull();
  });

  it('a backend error (for example a broken or oversized PDF) is shown as text', async () => {
    readPdfBytesMock.mockRejectedValue({ code: 'unsupported', message: 'Not a PDF file' });
    const r = render(PdfViewer, { props: { path: 'C:\\d\\a.pdf' } });
    await settle();
    expect(r.container.querySelector('[role="alert"]')?.textContent).toBe('Not a PDF file');
  });
});
