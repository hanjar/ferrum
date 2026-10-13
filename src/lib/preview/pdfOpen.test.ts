/**
 * FER-453: the locked-down pdf.js open path. A minimal generated PDF opens and has
 * its page; broken or non-PDF bytes give the fixed safe message, never a raw
 * pdf.js error or a crash.
 */
import { describe, expect, it, vi } from 'vitest';
import * as pdfjs from 'pdfjs-dist';

// pdf.js's main build needs a browser worker; under Node the legacy build of the SAME
// version runs the same code paths. The app itself always uses 'pdfjs-dist' (see pdfOpen.ts).
vi.mock('pdfjs-dist', () => import('pdfjs-dist/legacy/build/pdf.mjs'));
import { PDF_OPEN_ERROR, openPdf, useInThreadWorker } from './pdfOpen';

// pdf.js 6 calls Promise.try, which Node 22 (this test runner) lacks. WebView2 has
// it. Test-only shim; the app code does not depend on it.
const promiseWithTry = Promise as PromiseConstructor & {
  try?: (fn: () => unknown) => Promise<unknown>;
};
if (typeof promiseWithTry.try !== 'function') {
  promiseWithTry.try = (fn) => new Promise((resolve) => resolve(fn()));
}

/** Builds a one-page PDF with a correct xref table. */
function makeSmallPdf(): Uint8Array {
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>',
    (() => {
      const stream = 'BT /F1 12 Tf 10 50 Td (Hi) Tj ET';
      return `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`;
    })(),
  ];
  let out = '%PDF-1.4\n';
  const offsets: number[] = [];
  objects.forEach((body, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const off of offsets) out += `${String(off).padStart(10, '0')} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return new TextEncoder().encode(out);
}

// The viewer runs pdf.js in-thread (see pdfOpen.ts). Same here: the legacy worker module
// of the same version is the main-thread handler pdf.js uses.
useInThreadWorker(await import('pdfjs-dist/legacy/build/pdf.worker.mjs'));

describe('openPdf', () => {
  it('opens a small valid PDF and reads its page', async () => {
    const doc = await openPdf(makeSmallPdf());
    expect(doc.numPages).toBe(1);
    const page = await doc.getPage(1);
    const vp = page.getViewport({ scale: 1 });
    expect(Math.round(vp.width)).toBe(200);
    expect(Math.round(vp.height)).toBe(100);
  });

  it('a broken file gives the fixed safe message, not a raw pdf.js error', async () => {
    const junk = new TextEncoder().encode('%PDF-1.4\nthis is not really a pdf \x00\x01');
    await expect(openPdf(junk)).rejects.toThrow(PDF_OPEN_ERROR);
  });

  it('non-PDF bytes give the same fixed message', async () => {
    await expect(openPdf(new TextEncoder().encode('hello world'))).rejects.toThrow(PDF_OPEN_ERROR);
  });

  it('exposes the pdf.js version used for this viewer', () => {
    expect(pdfjs.version).toBe('6.4.299');
  });
});
