<script lang="ts">
  /**
   * Quick View PDF viewer (FER-453). The PDF is drawn by pdf.js inside an iframe
   * with sandbox="allow-scripts" only: opaque origin, no Tauri IPC, no popups, no
   * forms, no same-origin access to this page. The iframe never gets a path; the
   * bytes come from read_pdf_bytes and are posted to it (ArrayBuffer, transferred).
   * Every message from the iframe is checked against event.source and the
   * message contract; anything else is ignored, and the text is shown as text only.
   */
  import { onDestroy } from 'svelte';
  import { readPdfBytes } from '../ipc/pdf';
  import { PDF_MSG, parseViewerMessage } from '../preview/pdfBridge';

  interface Props {
    path: string;
  }
  let { path }: Props = $props();

  type Phase = 'loading' | 'waiting' | 'loaded' | 'error';
  let phase = $state<Phase>('loading');
  let errorText = $state('');
  let frame = $state<HTMLIFrameElement | null>(null);

  let bytes: ArrayBuffer | null = null;
  let viewerReady = false;
  let sent = false;
  let destroyed = false;

  function sendIfReady(): void {
    if (sent || destroyed || !viewerReady || !bytes) return;
    const win = frame?.contentWindow;
    if (!win) return;
    sent = true;
    // A fresh copy in this realm: the buffer from invoke is not always transferable
    // (WebView2 throws DataCloneError on it). The copy is transferred, so no second copy stays.
    const data = new Uint8Array(bytes).slice().buffer;
    bytes = null;
    // Target '*': the frame has an opaque origin, so its origin cannot be named.
    // The payload goes only to this frame's window object.
    win.postMessage({ type: PDF_MSG.load, data }, '*', [data]);
    phase = 'waiting';
  }

  function fail(text: string): void {
    phase = 'error';
    errorText = text;
  }

  function onViewerMessage(ev: MessageEvent): void {
    if (!frame || ev.source !== frame.contentWindow) return;
    const msg = parseViewerMessage(ev.data);
    if (!msg) return;
    switch (msg.type) {
      case PDF_MSG.ready:
        viewerReady = true;
        sendIfReady();
        break;
      case PDF_MSG.loaded:
        phase = 'loaded';
        break;
      case PDF_MSG.error:
        fail(msg.message);
        break;
    }
  }

  $effect(() => {
    const target = path;
    window.addEventListener('message', onViewerMessage);
    void (async () => {
      try {
        const data = await readPdfBytes(target);
        if (destroyed) return;
        bytes = data;
        sendIfReady();
      } catch (e) {
        if (destroyed) return;
        const message = (e as { message?: unknown } | null)?.message;
        fail(typeof message === 'string' ? message : 'This PDF could not be previewed.');
      }
    })();
    return () => {
      window.removeEventListener('message', onViewerMessage);
    };
  });

  onDestroy(() => {
    destroyed = true;
    bytes = null;
  });
</script>

<div class="pdfViewer">
  {#if phase === 'error'}
    <p class="error" role="alert">{errorText}</p>
  {:else if phase === 'loading' || phase === 'waiting'}
    <p class="empty">Loading PDF…</p>
  {/if}
  <iframe
    bind:this={frame}
    class="frame"
    title="PDF preview"
    sandbox="allow-scripts"
    src="./pdf-viewer.html"
    referrerpolicy="no-referrer"
    hidden={phase === 'error'}
  ></iframe>
</div>

<style>
  .pdfViewer {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 320px;
    flex: 1;
  }
  .frame {
    flex: 1;
    width: 100%;
    min-height: 320px;
    border: 1px solid var(--border, #ccc);
    border-radius: 4px;
    background: #525659;
  }
  .frame[hidden] {
    display: none;
  }
  .empty {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .error {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--red);
    word-break: break-word;
  }
</style>
