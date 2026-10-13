/**
 * Opens a PDF with pdf.js using the locked-down options of FER-453. Shared by the
 * sandboxed viewer page and the unit tests, so the tests exercise the real options.
 *
 * - no scripting (enableScripting: false); pdf.js 6.4 has no eval path (checked in the build)
 * - no network: no streaming, no range or auto fetch, no system or CDN fonts
 * - the worker runs in-thread: the viewer sets globalThis.pdfjsWorker before any
 *   document is opened, so pdf.js never creates a Worker (no blob:, no fetch). The
 *   Tauri header CSP blocks blob workers, so this is required, not an option.
 */
import * as pdfjs from 'pdfjs-dist';
import type { PDFDocumentProxy } from 'pdfjs-dist';

export const PDF_OPEN_ERROR = 'This PDF could not be opened.';

/** pdf.js reads this global and uses WorkerMessageHandler on the main thread. */
export function useInThreadWorker(workerModule: { WorkerMessageHandler: unknown }): void {
  (globalThis as { pdfjsWorker?: unknown }).pdfjsWorker = workerModule;
}

/**
 * Parses the document. Any failure (broken or non-PDF bytes) becomes a fixed,
 * safe message; the raw pdf.js error is not shown.
 */
export async function openPdf(bytes: Uint8Array): Promise<PDFDocumentProxy> {
  // enableScripting is read by the build but not typed by pdf.js 6.4; passing it
  // through a variable keeps the typed options object from rejecting it.
  const params = {
    data: bytes,
    enableScripting: false,
    enableXfa: false,
    useSystemFonts: false,
    disableAutoFetch: true,
    disableStream: true,
    disableRange: true,
    verbosity: 0,
  };
  try {
    return await pdfjs.getDocument(params as Parameters<typeof pdfjs.getDocument>[0]).promise;
  } catch {
    throw new Error(PDF_OPEN_ERROR);
  }
}
