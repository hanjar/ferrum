/**
 * Sandboxed PDF viewer (FER-453). Runs inside `pdf-viewer.html` with
 * sandbox="allow-scripts" only: opaque origin, no Tauri IPC, no popups, no forms.
 * Draws canvases only: no link layer, no annotation layer, so nothing is clickable.
 *
 * Limits (P3): pages are created as placeholders from the first page's size and
 * fetched only when near the viewport; each canvas is capped at MAX_CANVAS_PIXELS;
 * at most MAX_CONCURRENT_RENDERS pages draw at once. Zoom changes invalidate queued work.
 */
import type { PDFDocumentProxy, RenderTask } from 'pdfjs-dist';
import * as pdfWorker from 'pdfjs-dist/build/pdf.worker.mjs';
import { PDF_MSG, parseLoadMessage, type ViewerToParent } from '../lib/preview/pdfBridge';
import { useInThreadWorker, openPdf } from '../lib/preview/pdfOpen';
import { MAX_CONCURRENT_RENDERS, createLimiter, renderScale } from '../lib/preview/pdfScale';

// In-thread worker: set before any document opens (see pdfOpen.ts for why).
useInThreadWorker(pdfWorker);

const ZOOM_LEVELS = [0.5, 0.75, 1, 1.25, 1.5, 2, 2.5, 3];
const DEFAULT_ZOOM_INDEX = 2;

interface PageView {
  pageNumber: number;
  wrap: HTMLDivElement;
  canvas: HTMLCanvasElement;
  rendered: boolean;
  task: RenderTask | null;
}

const toolbar = document.getElementById('toolbar') as HTMLDivElement;
const statusEl = document.getElementById('status') as HTMLDivElement;
const pagesEl = document.getElementById('pages') as HTMLDivElement;

const runRender = createLimiter(MAX_CONCURRENT_RENDERS);
let doc: PDFDocumentProxy | null = null;
let views: PageView[] = [];
/** Page size in points of page 1: the estimate used for every placeholder. */
let estimate = { w: 612, h: 792 };
let zoomIndex = DEFAULT_ZOOM_INDEX;
/** Bumped on every zoom change or new document; queued renders from older generations are dropped. */
let generation = 0;
let pageLabel: HTMLSpanElement | null = null;
let observer: IntersectionObserver | null = null;

function post(msg: ViewerToParent): void {
  // Target '*': an opaque-origin frame cannot name the parent origin. The payload
  // is a status word, a page count or a short text, never document content.
  window.parent.postMessage(msg, '*');
}

function showStatus(text: string): void {
  statusEl.textContent = text;
  statusEl.hidden = text === '';
}

function zoomScale(): number {
  return ZOOM_LEVELS[zoomIndex];
}

function pixelRatio(): number {
  return Math.min(window.devicePixelRatio || 1, 2);
}

function button(label: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement('button');
  b.type = 'button';
  b.textContent = label;
  b.addEventListener('click', onClick);
  return b;
}

function currentPage(): number {
  const top = pagesEl.scrollTop + pagesEl.clientHeight / 3;
  let page = 1;
  for (const v of views) {
    if (v.wrap.offsetTop <= top) page = v.pageNumber;
    else break;
  }
  return page;
}

function updatePageLabel(): void {
  if (!pageLabel) return;
  pageLabel.textContent = views.length
    ? `${currentPage()} / ${views.length}  ${Math.round(zoomScale() * 100)}%`
    : '';
}

function goToPage(n: number): void {
  const clamped = Math.min(Math.max(n, 1), views.length);
  views[clamped - 1]?.wrap.scrollIntoView({ block: 'start' });
  updatePageLabel();
}

function buildToolbar(): void {
  pageLabel = document.createElement('span');
  toolbar.replaceChildren(
    button('Prev', () => goToPage(currentPage() - 1)),
    pageLabel,
    button('Next', () => goToPage(currentPage() + 1)),
    button('Zoom out', () => setZoom(zoomIndex - 1)),
    button('Zoom in', () => setZoom(zoomIndex + 1)),
  );
}

/** Sets the CSS size of one page: base size (points) times zoom. */
function sizeWrap(view: PageView, w: number, h: number): void {
  view.wrap.style.width = `${Math.floor(w * zoomScale())}px`;
  view.wrap.style.height = `${Math.floor(h * zoomScale())}px`;
}

function markFailed(view: PageView): void {
  view.rendered = false;
  view.wrap.replaceChildren();
  const note = document.createElement('div');
  note.className = 'page-error';
  note.textContent = `Page ${view.pageNumber} could not be drawn.`;
  view.wrap.append(note);
}

async function drawPage(view: PageView, gen: number): Promise<void> {
  if (!doc || gen !== generation) return;
  const page = await doc.getPage(view.pageNumber);
  if (gen !== generation) return;
  const base = page.getViewport({ scale: 1 });
  // The real size replaces the estimate once known, so placeholders settle on the page.
  sizeWrap(view, base.width, base.height);
  const scale = renderScale(base.width, base.height, zoomScale(), pixelRatio());
  const viewport = page.getViewport({ scale });
  const ctx = view.canvas.getContext('2d');
  if (!ctx) throw new Error('no 2d context');
  view.canvas.width = Math.floor(viewport.width);
  view.canvas.height = Math.floor(viewport.height);
  view.canvas.style.width = `${Math.floor(base.width * zoomScale())}px`;
  view.canvas.style.height = `${Math.floor(base.height * zoomScale())}px`;
  view.task = page.render({ canvasContext: ctx, viewport, canvas: view.canvas });
  await view.task.promise;
}

function renderPage(view: PageView): void {
  if (!doc || view.rendered) return;
  view.rendered = true;
  const gen = generation;
  void runRender(async () => {
    try {
      await drawPage(view, gen);
    } catch (e) {
      // Cancelled by a zoom change, or the document was replaced: nothing to show.
      if (gen !== generation) return;
      if (e instanceof Error && e.name === 'RenderingCancelledException') return;
      markFailed(view);
    } finally {
      view.task = null;
    }
  });
}

function observe(): void {
  observer?.disconnect();
  observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const view = views.find((v) => v.wrap === entry.target);
        if (view) renderPage(view);
      }
      updatePageLabel();
    },
    { root: pagesEl, rootMargin: '300px 0px' },
  );
  for (const v of views) observer.observe(v.wrap);
}

/**
 * Creates one placeholder per page. Only page 1 is fetched (for the estimate);
 * no other page is touched until it nears the viewport.
 */
async function buildPages(pdf: PDFDocumentProxy): Promise<void> {
  pagesEl.replaceChildren();
  views = [];
  if (pdf.numPages > 0) {
    const first = (await pdf.getPage(1)).getViewport({ scale: 1 });
    estimate = { w: first.width, h: first.height };
  }
  for (let i = 1; i <= pdf.numPages; i++) {
    const wrap = document.createElement('div');
    wrap.className = 'page';
    const canvas = document.createElement('canvas');
    wrap.append(canvas);
    pagesEl.append(wrap);
    const view: PageView = { pageNumber: i, wrap, canvas, rendered: false, task: null };
    sizeWrap(view, estimate.w, estimate.h);
    views.push(view);
  }
  observe();
}

function setZoom(index: number): void {
  const next = Math.min(Math.max(index, 0), ZOOM_LEVELS.length - 1);
  if (next === zoomIndex || views.length === 0) return;
  zoomIndex = next;
  generation++;
  for (const v of views) {
    v.task?.cancel();
    v.rendered = false;
    v.canvas.width = 0;
    v.canvas.height = 0;
    v.canvas.style.width = '';
    v.canvas.style.height = '';
    sizeWrap(v, estimate.w, estimate.h);
  }
  observe();
  updatePageLabel();
}

async function load(data: ArrayBuffer): Promise<void> {
  showStatus('Loading…');
  try {
    generation++;
    doc = await openPdf(new Uint8Array(data));
    await buildPages(doc);
    showStatus(doc.numPages === 0 ? 'This PDF has no pages.' : '');
    post({ type: PDF_MSG.loaded, pages: doc.numPages });
  } catch (e) {
    const message = e instanceof Error ? e.message : 'This PDF could not be opened.';
    showStatus(message);
    post({ type: PDF_MSG.error, message });
  }
}

// `ready` is re-sent until the parent's load arrives: if the parent's listener is not yet
// registered when the first ready is posted, the handshake would otherwise never complete.
const readyTimer = window.setInterval(() => post({ type: PDF_MSG.ready }), 250);

window.addEventListener('message', (ev: MessageEvent) => {
  if (ev.source !== window.parent) return;
  const payload = parseLoadMessage(ev.data);
  if (!payload) return;
  window.clearInterval(readyTimer);
  void load(payload.data);
});

pagesEl.addEventListener('scroll', updatePageLabel, { passive: true });
buildToolbar();
showStatus('Waiting for the document…');
