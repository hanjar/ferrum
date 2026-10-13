/**
 * PDF preview helpers (FER-103, viewer FER-453).
 *
 * The PDF is drawn by pdf.js inside a sandboxed iframe (sandbox="allow-scripts"
 * only); see PdfViewer.svelte and docs/quick-view-pdf.md. The WebView2 built-in
 * viewer is NOT used: it does not run inside a sandboxed frame.
 */

/** Largest PDF the backend still reports as previewable (mirrors preview.rs). */
export const PDF_PREVIEW_MAX_BYTES = 50 * 1024 * 1024;

export function isPdfName(name: string): boolean {
  return /\.pdf$/i.test(name);
}
