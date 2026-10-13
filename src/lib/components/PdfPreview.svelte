<script lang="ts">
  /**
   * Quick View PDF entry (FER-103, viewer FER-453). Renders the PDF with the
   * sandboxed pdf.js viewer. Over the size cap and PDF-in-archive keep their fixed
   * text. "Open in default app" (open_external, an explicit user action) stays available.
   */
  import { openExternal } from '../ipc/shellOpen';
  import { PDF_PREVIEW_MAX_BYTES } from '../preview/pdf';
  import { formatSize } from '../format';
  import PdfViewer from './PdfViewer.svelte';

  interface Props {
    path: string;
    /** Backend said the file exceeds the PDF cap. */
    tooLarge?: boolean;
    /** The PDF is a member of an archive: not supported. */
    inArchive?: boolean;
  }
  let { path, tooLarge = false, inArchive = false }: Props = $props();

  let openError = $state('');

  async function openDefault() {
    openError = '';
    try {
      await openExternal(path);
    } catch (e) {
      openError = e instanceof Error ? e.message : String(e);
    }
  }
</script>

{#if inArchive}
  <p class="empty">PDF inside an archive is not previewed — extract it first.</p>
{:else if tooLarge}
  <p class="empty">
    PDF larger than the preview limit ({formatSize(PDF_PREVIEW_MAX_BYTES)}) — not previewed.
  </p>
  <button class="open" onclick={openDefault}>Open in default app</button>
{:else}
  <PdfViewer {path} />
  <button class="open" onclick={openDefault}>Open in default app</button>
{/if}
{#if openError}
  <p class="error" role="alert">{openError}</p>
{/if}

<style>
  .empty {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-word;
  }
  .error {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--red);
  }
  .open {
    align-self: flex-start;
  }
</style>
