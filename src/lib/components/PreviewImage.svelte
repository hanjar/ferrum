<script lang="ts">
  /**
   * Disk image in Quick View (FER-103). Asks the backend first: small images
   * load straight from disk (`ferrum-media`), big ones arrive as a shrunk
   * copy (max 2048 px) so the original is never loaded whole into the
   * webview. A late response after unmount is dropped. Callers must key the
   * component by path.
   */
  import { onMount } from 'svelte';
  import { mediaRevoke, newMediaScope } from '../ipc/media';
  import { readImagePreview, imagePreviewSrc, type ImagePreview } from '../ipc/previewImage';

  interface Props {
    path: string;
    name: string;
  }
  let { path, name }: Props = $props();

  let info = $state<ImagePreview | null>(null);
  let src = $state('');
  let error = $state('');
  // FER-452: grants of this instance only; revoked when the selection moves on.
  const scope = newMediaScope('preview');

  // One load per mounted instance: QuickView keys this component by path, so
  // `path` is constant for its lifetime. (A $effect that resets state and
  // starts the load looped in the real webview: effect_update_depth_exceeded.)
  onMount(() => {
    let stale = false;
    readImagePreview(path)
      .then(async (r) => {
        const s = await imagePreviewSrc(path, r, scope);
        if (stale) return;
        if (!s) throw new Error('Image cannot be previewed');
        src = s;
        info = r;
      })
      .catch((e: unknown) => {
        if (!stale) error = e instanceof Error ? e.message : String(e);
      });
    return () => {
      stale = true;
      void mediaRevoke(scope);
    };
  });
</script>

{#if error}
  <p class="error" role="alert">{error}</p>
{:else if info}
  <img class="image" {src} alt="Preview of {name}" />
  {#if info.downsampled}
    <p class="note">
      Downsampled preview ({info.width}×{info.height}) of the {info.originalWidth}×{info.originalHeight}
      original.
    </p>
  {/if}
{:else}
  <p class="note">Loading image…</p>
{/if}

<style>
  .image {
    max-width: 100%;
    max-height: 100%;
    display: block;
    margin: 0 auto;
  }
  .note,
  .error {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-word;
  }
  .error {
    color: var(--red);
  }
</style>
