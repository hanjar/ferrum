/**
 * postMessage contract between Ferrum and the sandboxed pdf.js viewer (FER-453).
 *
 * The viewer iframe has `sandbox="allow-scripts"` only: an opaque origin, no
 * IPC, no popups, no forms. It never receives a path. Parent -> viewer carries
 * the PDF bytes (ArrayBuffer, transferred). The pdf.js worker is bundled into the viewer
 * and runs in-thread, so no worker source or blob URL crosses this boundary.
 * Viewer -> parent carries only status words and a short error text. Both
 * sides validate every incoming message and ignore anything unknown.
 */

export const PDF_MSG = {
  load: 'ferrum-pdf:load',
  ready: 'ferrum-pdf:ready',
  loaded: 'ferrum-pdf:loaded',
  error: 'ferrum-pdf:error',
} as const;

/** Longest error text the parent will accept from the viewer. */
export const PDF_ERROR_TEXT_MAX = 300;

export type ViewerToParent =
  | { type: typeof PDF_MSG.ready }
  | { type: typeof PDF_MSG.loaded; pages: number }
  | { type: typeof PDF_MSG.error; message: string };

export interface LoadPayload {
  data: ArrayBuffer;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null;
}

/** Parses a message the viewer sent. Returns null for anything not in the contract. */
export function parseViewerMessage(raw: unknown): ViewerToParent | null {
  if (!isRecord(raw)) return null;
  switch (raw.type) {
    case PDF_MSG.ready:
      return { type: PDF_MSG.ready };
    case PDF_MSG.loaded:
      return typeof raw.pages === 'number' && Number.isInteger(raw.pages) && raw.pages >= 0
        ? { type: PDF_MSG.loaded, pages: raw.pages }
        : null;
    case PDF_MSG.error:
      return typeof raw.message === 'string'
        ? { type: PDF_MSG.error, message: raw.message.slice(0, PDF_ERROR_TEXT_MAX) }
        : null;
    default:
      return null;
  }
}

/** Parses the load message the parent sent. Returns null when the shape is wrong. */
export function parseLoadMessage(raw: unknown): LoadPayload | null {
  if (!isRecord(raw) || raw.type !== PDF_MSG.load) return null;
  if (!(raw.data instanceof ArrayBuffer)) return null;
  return { data: raw.data };
}
