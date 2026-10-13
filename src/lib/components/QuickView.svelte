<script lang="ts">
  /**
   * Quick View panel (F2-17, Ctrl+Q): shows a bounded, non-executing
   * preview of the active pane's cursor entry. Text is rendered as text
   * (Svelte escapes), images load through the `ferrum-media` protocol,
   * binary/oversize/unsupported content gets an explicit message.
   */
  import {
    previewStore,
    archiveMemberOf,
    previewImageSrc,
    previewText,
    formatPreviewLimit,
  } from '../stores/previewStore.svelte';
  import { formatDate, formatSize } from '../format';
  import { ui } from '../uiState.svelte';
  import HighlightedText from './HighlightedText.svelte';
  import MediaPlayer from './MediaPlayer.svelte';
  import MediaMetadata from './MediaMetadata.svelte';
  import FileIcon from './FileIcon.svelte';
  import { kindOf } from '../kind';
  import PdfPreview from './PdfPreview.svelte';
  import PreviewImage from './PreviewImage.svelte';
  import { isPdfName } from '../preview/pdf';

  const store = previewStore;

  const kindLabel: Record<string, string> = {
    text: 'Text',
    json: 'JSON',
    markdown: 'Markdown',
    image: 'Image',
    video: 'Video',
    audio: 'Audio',
    pdf: 'PDF',
    binary: 'Binary',
  };
</script>

{#if ui.quickViewOpen}
  <aside class="panel" aria-label="Quick View">
    <div class="headRow">
      <h2>Quick View</h2>
      <span class="name" title={store.target?.path ?? ''}>{store.target?.name ?? '—'}</span>
      <button
        class="close"
        aria-label="Close Quick View"
        onclick={() => (ui.quickViewOpen = false)}
      >
        ✕
      </button>
    </div>

    {#if store.target}
      <p class="meta mono">
        {store.target.isDir ? 'folder' : formatSize(store.target.size)} ·
        {formatDate(store.target.modifiedMs)}{#if store.data && !store.data.tooLarge}
          · {kindLabel[store.data.kind] ?? 'Preview'}{/if}
      </p>
    {/if}

    <div class="body">
      {#if !store.target}
        <p class="empty">Move the cursor onto a file to preview it.</p>
      {:else if store.target.isDir}
        <p class="empty">Folders have no preview.</p>
      {:else if store.state === 'loading'}
        <p class="empty">Loading preview…</p>
      {:else if store.state === 'error'}
        <p class="error" role="alert">{store.errorText}</p>
      {:else if store.data}
        {#if store.data.kind === 'pdf' || (isPdfName(store.target.name) && archiveMemberOf(store.target.path))}
          {#key store.target.path}
            <PdfPreview
              path={store.target.path}
              tooLarge={store.data.tooLarge}
              inArchive={archiveMemberOf(store.target.path) !== null}
            />
          {/key}
        {:else if store.data.tooLarge}
          <div class="tooLarge">
            <p class="empty">
              File larger than the text preview limit ({formatPreviewLimit(
                ui.preview.maxTextBytes,
              )}) — showing metadata only.
            </p>
            <dl class="basic">
              <dt>Name</dt>
              <dd>{store.target.name}</dd>
              <dt>Size</dt>
              <dd>{formatSize(store.target.size)}</dd>
              <dt>Modified</dt>
              <dd>{formatDate(store.target.modifiedMs)}</dd>
              <dt>Type</dt>
              <dd>
                {store.target.name.includes('.')
                  ? store.target.name.split('.').pop()?.toUpperCase()
                  : 'File'}
              </dd>
            </dl>
          </div>
        {:else if store.data.kind === 'image'}
          {@const imgSrc = previewImageSrc(store.target.path, store.data)}
          {#if !archiveMemberOf(store.target.path)}
            {#key store.target.path}
              <PreviewImage path={store.target.path} name={store.target.name} />
            {/key}
          {:else if imgSrc}
            <img class="image" src={imgSrc} alt="Preview of {store.target.name}" />
          {:else}
            <p class="empty">Image format not supported for preview.</p>
          {/if}
        {:else if store.data.kind === 'video' || store.data.kind === 'audio'}
          {#if archiveMemberOf(store.target.path)}
            <p class="empty">Audio/video inside an archive is not previewed — extract it first.</p>
          {:else}
            {#key store.target.path}
              <MediaPlayer
                path={store.target.path}
                name={store.target.name}
                kind={store.data.kind}
                autoplay={ui.preview.autoplayVideo}
              />
            {/key}
          {/if}
        {:else if store.data.kind === 'binary'}
          <div class="otherIcon">
            <FileIcon kind={kindOf(store.target)} name={store.target.name} variant="preview" />
          </div>
          <p class="empty">Binary file — no preview.</p>
        {:else}
          <HighlightedText
            text={previewText(store.data.data)}
            fileName={store.target.name}
            kind={store.data.kind}
            enabled={ui.preview.syntaxHighlight}
          />
        {/if}
        {#if store.data.truncated}
          <p class="truncated">
            Preview truncated to the first {formatPreviewLimit(ui.preview.maxTextBytes)}.
          </p>
        {/if}
        {#if !store.data.tooLarge && !archiveMemberOf(store.target.path) && ['image', 'video', 'audio'].includes(store.data.kind)}
          <MediaMetadata path={store.target.path} />
        {/if}
      {/if}
    </div>
  </aside>
{/if}

<style>
  .otherIcon {
    display: flex;
    justify-content: center;
    padding: 16px 0 4px;
  }
  .panel {
    width: 340px;
    flex: none;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px;
    background: var(--pane);
    border-left: 1px solid var(--line);
    overflow: hidden;
  }
  .headRow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  h2 {
    margin: 0;
    font-size: calc(13px + var(--uiStep, 0px));
    font-weight: 600;
    white-space: nowrap;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .close {
    border: 0;
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .close:hover {
    color: var(--text);
  }
  .meta {
    margin: 0;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mono {
    font-family: 'Geist Mono', monospace;
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border-radius: var(--r);
    background: var(--bg);
    padding: 8px;
  }
  .empty,
  .error,
  .truncated {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-word;
  }
  .error {
    color: var(--red);
  }
  .truncated {
    border-top: 1px solid var(--line);
    padding-top: 6px;
    margin-top: 6px;
  }
  .basic {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 10px;
    margin: 6px 0 0;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .basic dt {
    color: var(--text3);
  }
  .basic dd {
    margin: 0;
    word-break: break-all;
  }
  .image {
    max-width: 100%;
    max-height: 100%;
    display: block;
    margin: 0 auto;
  }
</style>
