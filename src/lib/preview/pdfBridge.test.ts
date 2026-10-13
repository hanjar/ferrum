/**
 * FER-453: the viewer <-> parent message contract. Unknown types, wrong shapes and
 * oversized error text are rejected or cut, never passed on.
 */
import { describe, expect, it } from 'vitest';
import { PDF_ERROR_TEXT_MAX, PDF_MSG, parseLoadMessage, parseViewerMessage } from './pdfBridge';

describe('parseViewerMessage', () => {
  it('accepts ready, loaded with a page count, and error text', () => {
    expect(parseViewerMessage({ type: PDF_MSG.ready })).toEqual({ type: PDF_MSG.ready });
    expect(parseViewerMessage({ type: PDF_MSG.loaded, pages: 3 })).toEqual({
      type: PDF_MSG.loaded,
      pages: 3,
    });
    expect(parseViewerMessage({ type: PDF_MSG.error, message: 'bad' })).toEqual({
      type: PDF_MSG.error,
      message: 'bad',
    });
  });

  it('ignores unknown types and non-objects', () => {
    expect(parseViewerMessage({ type: 'ferrum-pdf:exec', code: 'x' })).toBeNull();
    expect(parseViewerMessage('ferrum-pdf:ready')).toBeNull();
    expect(parseViewerMessage(null)).toBeNull();
    expect(parseViewerMessage(undefined)).toBeNull();
  });

  it('rejects a loaded page count that is not a non-negative integer', () => {
    expect(parseViewerMessage({ type: PDF_MSG.loaded, pages: '3' })).toBeNull();
    expect(parseViewerMessage({ type: PDF_MSG.loaded, pages: -1 })).toBeNull();
    expect(parseViewerMessage({ type: PDF_MSG.loaded, pages: 1.5 })).toBeNull();
  });

  it('rejects non-string error text and cuts long text', () => {
    expect(parseViewerMessage({ type: PDF_MSG.error, message: { html: 1 } })).toBeNull();
    const long = 'x'.repeat(PDF_ERROR_TEXT_MAX + 50);
    const got = parseViewerMessage({ type: PDF_MSG.error, message: long });
    expect(got).toEqual({ type: PDF_MSG.error, message: 'x'.repeat(PDF_ERROR_TEXT_MAX) });
  });
});

describe('parseLoadMessage', () => {
  it('accepts an ArrayBuffer and nothing else', () => {
    const data = new ArrayBuffer(8);
    expect(parseLoadMessage({ type: PDF_MSG.load, data })).toEqual({ data });
  });

  it('rejects a path, a typed array, a worker source, or the wrong type', () => {
    const data = new ArrayBuffer(8);
    expect(parseLoadMessage({ type: PDF_MSG.load, path: 'C:\\x.pdf' })).toBeNull();
    expect(parseLoadMessage({ type: PDF_MSG.load, data: new Uint8Array(8) })).toBeNull();
    // A worker source is never accepted: the worker is bundled in-thread.
    expect(parseLoadMessage({ type: PDF_MSG.load, data, workerSource: 'x' })).toEqual({ data });
    expect(parseLoadMessage({ type: PDF_MSG.ready, data })).toBeNull();
  });
});
