# Quick View: PDF and large images (FER-103)

## PDF: decision

Goal was the built-in WebView2 PDF viewer in a sandboxed `<iframe>` fed by the asset protocol (no PDF library, no network). The asset protocol no longer exists: FER-452 replaced it with the `ferrum-media` protocol (see the last section), which never serves documents.

Measured (while the asset protocol still existed) on a real Ferrum window (debug build, WebView2 over CDP, asset URL `http://asset.localhost/...pdf#navpanes=0`, CSP with `frame-src asset: http://asset.localhost`):

| iframe `sandbox`                                                                                                                        | Result                   |
| --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------ |
| (attribute absent)                                                                                                                      | viewer renders the PDF   |
| `""` (everything denied)                                                                                                                | blocked glyph, no viewer |
| `allow-scripts`                                                                                                                         | blocked glyph            |
| `allow-same-origin`                                                                                                                     | blocked glyph            |
| `allow-scripts allow-same-origin`                                                                                                       | blocked glyph            |
| every allow-* token (scripts, same-origin, forms, modals, popups, downloads, pointer-lock, presentation, orientation, popups-to-escape) | blocked glyph            |

Chromium refuses to start its PDF viewer in any sandboxed frame. The only working configuration has no sandbox, and the rule for this task forbids loosening security silently.

**Shipped behaviour (FER-453):** the PDF is drawn by pdf.js inside a sandboxed iframe (section below). Over 50 MiB the message says so (backend `too_large`, no read). A PDF inside an archive shows "PDF inside an archive is not previewed". "Open in default app" (`open_external`) stays available in every case as an explicit user action.

Backend (`preview.rs`): `.pdf` is classified `PreviewKind::Pdf` without reading it. The first 1 KiB is checked for `%PDF-` (a renamed non-PDF becomes `binary`). Files over `PDF_PREVIEW_MAX_BYTES` (50 MiB) return `too_large`.

### Unsandboxed iframe viewer: REJECTED (security review, FER-103)

An iframe without `sandbox` is not an option. The asset protocol scope was `**` (whole filesystem), so a hostile document loaded from `asset.localhost` could read other local files through the same origin and send them out via local HTML, and the frame can open popups. Do not re-add `frame-src` for this. (Since FER-452 the asset protocol is off and `ferrum-media` serves only image/audio/video, so there is no origin left that could host a PDF either.)

The sandboxed pdf.js viewer below (FER-453) is the chosen alternative. Backend rendering to images stays as a fallback if the viewer is ever dropped.

## PDF viewer: pdf.js in a sandboxed iframe (FER-453)

Design decision: pdf.js inside `<iframe sandbox="allow-scripts">`. No native Rust PDF renderer, no WebView2 built-in PDF viewer.

**Dependency:** `pdfjs-dist` **6.4.299** (exact pin, published 2026-10-03), license **Apache-2.0**, https://github.com/mozilla/pdf.js. Bundled into `public/pdf-viewer.js` (classic IIFE, built by `npm run build:viewer`, which runs before `npm run build` and `npm run dev`) and into a lazy chunk for the worker source.

**Frame:** `sandbox="allow-scripts"` only: no `allow-same-origin`, `allow-forms`, `allow-popups` or `allow-top-navigation`. The frame has an opaque origin. It never receives a path; the parent reads the bytes with `read_pdf_bytes` (`path_guard`, 50 MiB cap read in full before the magic check, `%PDF-` within 1 KiB) and posts them as an `ArrayBuffer` (transferred).

**Build:** `src/pdf-viewer/main.ts` (with pdf.js and its worker) is bundled by a Vite plugin (`pdfViewerBundle` in `vite.config.ts`, esbuild, IIFE) into `public/pdf-viewer.js`. It runs on every Vite start (dev server, `npx vite` under Playwright, `vite build`), so the bundle is never older than its sources; a failed bundle stops the start. The file is git-ignored and is not committed.

**Why a classic script:** a sandboxed frame has an opaque origin, and ES module scripts are fetched in CORS mode, which an opaque origin cannot satisfy for same-origin requests. The viewer is therefore an IIFE loaded by a plain `<script src>`.

**Worker, in-thread (P1):** the Tauri header CSP has no `worker-src`, so it blocks every `blob:` Worker and blob fallback. The app CSP is NOT loosened for this. The pdf.js worker module is bundled into the viewer and registered as `globalThis.pdfjsWorker` before any document opens (`useInThreadWorker`); pdf.js then runs its handler on the viewer's main thread and never creates a Worker, a blob or a fetch. Verified in a real Tauri WebView2 window with `scripts/smoke-pdf-preview.mjs` (`node scripts/smoke-pdf-preview.mjs <ferrum.exe>` against a `tauri build --debug --no-bundle` output).

**Viewer CSP** (meta tag in `public/pdf-viewer.html`): `default-src 'none'; script-src 'self'; img-src blob: data:; style-src 'self' 'unsafe-inline'`. No `worker-src` and no `blob:` in `script-src`, no `connect-src`, so no network. The effective policy in the real app is this meta intersected with Tauri's header CSP. NEVER add `connect-src` (or any fetch source) for `ipc:`, `ipc.localhost` or the app origin: that would open the frame to the Tauri IPC endpoint.

**Handshake and transfer (found by the real-window smoke):** the viewer re-sends `ferrum-pdf:ready` every 250 ms until the parent's `load` arrives, so a `ready` posted before the parent's listener exists is not lost. The parent copies the bytes into a fresh buffer before transferring them: the buffer returned by invoke is not always transferable (WebView2 throws `DataCloneError`, which the Chromium e2e does not reproduce).

**Render limits (P3, `src/lib/preview/pdfScale.ts`):** each canvas is capped at 16 megapixels (`MAX_CANVAS_PIXELS`), by scaling the render down, so a huge page cannot allocate gigabytes. Pages are lazy: placeholders use page 1's size, and a page is fetched only when it nears the viewport. At most 3 pages draw at once (`MAX_CONCURRENT_RENDERS`); zoom changes invalidate queued work by generation number.

**IPC inside the frame (P2):** Tauri 2.12 injects its core IPC script (`window.__TAURI_INTERNALS__`, `window.ipc`) into every frame, including this sandboxed one. The main-frame-only option (`initialization_script` vs `initialization_script_for_all_frames`) applies only to app-level user scripts; the core IPC script is injected by Tauri itself and cannot be restricted to the main frame in this version, so this is recorded as a limit, not closed at injection. The frame cannot use IPC anyway: the header and meta CSP block `fetch` to `ipc.localhost`, the call hangs, and the host does not execute the message. Layered defences: (1) CSP above, (2) no `connect-src`, (3) the regression in `scripts/smoke-pdf-preview.mjs`, which calls `invoke('save_prefs')` and `window.ipc.postMessage` from inside the frame and checks that `config.json` does not change.

**Protocol** (`src/lib/preview/pdfBridge.ts`): parent to viewer carries only the bytes (no worker source). Viewer to parent `ferrum-pdf:ready | loaded | error` (error text cut to 300 characters). Parent to viewer `ferrum-pdf:load`. Both sides check the message shape; the viewer accepts `load` only from `window.parent`, the parent accepts messages only from the frame's `contentWindow`. Unknown types are ignored. Error text is shown as text, never as markup. The parent target origin is `'*'` because the frame's origin is opaque, so the posted payload is limited to the PDF bytes for that frame only.

**IPC:** the frame has no path and no Tauri API. In Chromium, a sandboxed frame is opaque, has no `__TAURI__` or `__TAURI_INTERNALS__`, cannot reach `window.parent.document`, and cannot `fetch()` (see e2e `e2e/pdf-viewer.e2e.ts`). Verification on the real WebView2 window is still open (see Limits).

**Limits:**

- The CSP has no `wasm-unsafe-eval`, so pdf.js WASM decoders (JPEG 2000, JBIG2) cannot run. Pages that need them may not draw; the error path shows a text message for that page. Native images and standard streams draw normally.
- Non-embedded standard fonts are not fetched (no network); pdf.js draws them with its built-in fallback glyphs, so text in those fonts can look different.
- Lazy rendering: pages draw when near the viewport. Zoom 50% to 300%, previous and next page, keyboard focus on the page list.
- Tauri's header CSP (`default-src 'self'`, no `worker-src`) also applies to `pdf-viewer.html`. The viewer needs no worker, so this is harmless; the real-window smoke (`scripts/smoke-pdf-preview.mjs`) is the check.

**Tests:** `src/lib/preview/pdfScale.test.ts` (16 MP cap, render limiter), `src/lib/preview/pdfBridge.test.ts` (contract), `src/lib/preview/pdfOpen.test.ts` (generated one-page PDF opens; broken and non-PDF bytes give the fixed safe message), `src/lib/components/PdfViewer.test.ts` (sandbox attribute, CSP text, no IPC reference, message filtering, backend errors as text), `e2e/pdf-viewer.e2e.ts` (Chromium: real pixels drawn for a generated PDF, broken PDF error, opaque frame facts, parent to frame delivery). Rust: `preview::tests::pdf_bytes_*` (round trip, renamed non-PDF, non-PDF extension, over the 50 MiB cap).

## Large images

`preview_image.rs` / `read_image_preview` (crate `image` 0.25, features png, jpeg, gif, webp, bmp, default features off):

1. Header only (`ImageReader::into_decoder`, no pixel buffer): format must be one of the five, `width * height` at most 100 MP, file at most 256 MiB (a symlink is judged by its target's size), and the decoded buffer (`decoder.total_bytes()`) at most 384 MiB. The last check is explicit because image 0.25 `from_decoder` does not reserve the main buffer against `Limits`; a 10000x10000 RGBA16 solid-colour PNG (small file, 800 MB decoded) is refused. Measured peak for a 308 MB decoded image with downscale: 330 MB.
2. At most 4096 px on both sides and at most 16 MiB: `downsampled = false`, the webview loads the original through the `ferrum-media` protocol (a token granted per image by `media_grant`; no bytes over IPC).
3. Otherwise decode under `image::Limits { max_alloc: 512 MiB }`, downscale with `thumbnail_exact` (allocates only the small output) so the long side is at most 2048 px (never upscale), re-encode (PNG if it has alpha, else JPEG q85). The encoded result must be at most 8 MiB: otherwise it is retried at half the size (down to 256 px), then refused. Returned as base64.
4. SVG is read (at most 2 MiB) and returned inline (`inline = true`, base64 of the original bytes) for a `data:` URL: `ferrum-media` never serves SVG, and an `<img>` renders it inertly. ICO is passthrough through `ferrum-media`.
5. Unreadable header, unsupported content, damaged data, or hostile dimensions are errors with a fixed, path-free message.

Behaviour change: a damaged raster file previously showed a broken-image icon; it now shows the error text.

## ferrum-media protocol (FER-452)

Replaces the Tauri asset protocol (`assetProtocol.enable = false`, cargo feature `protocol-asset` removed). `src-tauri/src/media_protocol.rs`:

- URL: `http://ferrum-media.localhost/<token>` on Windows. The token is 128 random bits (32 hex characters); the path never appears in the URL. The backend maps each token to a canonical path (+ scope, idle TTL 10 min renewed on use, hard life 12 h, LRU 4096).
- Only image, audio and video extensions are served (png, jpg, gif, webp, bmp, ico, tif, mp4, m4v, webm, mov, ogv, mp3, wav, ogg, flac, m4a, aac). HTML, SVG, JS, XML, PDF and everything else are refused at grant time and answer 403 at serve time even if a token is forced.
- Grant refuses: cloud placeholders (also behind a symlink), non-files, SYSTEM-attribute files, `\\.\`, `\\?\` and `\??\` device paths (except `\\?\X:\`), and links whose target has a forbidden type. Serve re-checks the type, the placeholder bit, and that the granted path is still canonical and not a symlink or SYSTEM file (404 otherwise). The reparse-point attribute alone is NOT a reason to refuse: OneDrive/cloud sync files that are already downloaded carry it (typically 0x420) and must keep working; not-yet-local placeholders are refused through `media_meta::offline_attrs`.
- Responses: `Content-Type` from the whitelist, `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`, no CORS header. HTTP Range: 206, malformed or multi-range or out-of-file 416; a request without Range above 64 MiB gets 413; open or large ranges are clamped to 8 MiB (Content-Range stays truthful). At most 8 requests are served at once; the others wait up to 10 s, then 503.
- CSP opens `ferrum-media:` / `http://ferrum-media.localhost` in `img-src` and `media-src` only (not `frame-src`, `script-src` or `connect-src`, so `fetch()` to it is blocked).
- Frontend: `src/lib/ipc/media.ts` (`media_grant` / `media_revoke`), one grant scope per grid, per Quick View image and per media player; thumbnails ask for a token only when their cell becomes visible.
- Known limit: a file with the SYSTEM attribute (for example a hidden `Folder.jpg`) gets no thumbnail and no preview.
