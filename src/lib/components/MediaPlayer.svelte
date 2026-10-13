<script lang="ts">
  /**
   * Inline media preview (PRV-05). Native controls only; never opens an
   * external program or downloads. `autoplay` applies to the path being
   * shown when it is selected; toggling it later neither stops nor
   * restarts the current item. Playback is released on path change and
   * on destroy so no orphan stream survives.
   */
  import { onDestroy, untrack } from 'svelte';
  import { mediaSrc } from '../preview/media';
  import { mediaRevoke, newMediaScope } from '../ipc/media';

  interface Props {
    path: string;
    name: string;
    kind: 'video' | 'audio';
    autoplay: boolean;
  }
  let { path, name, kind, autoplay }: Props = $props();

  let el = $state<HTMLMediaElement | null>(null);
  let failed = $state(false);
  let blocked = $state(false);

  function release(m: HTMLMediaElement) {
    try {
      m.pause();
    } catch {
      /* ignore */
    }
    m.removeAttribute('src');
    try {
      m.load();
    } catch {
      /* ignore */
    }
  }

  // Element/path currently loaded. Plain vars: the effect must only act on a
  // real element or path change, never on an unrelated prop update.
  let loadedEl: HTMLMediaElement | null = null;
  let loadedPath: string | null = null;

  // FER-452: one grant scope per player instance; dropped when the player
  // goes away or switches path.
  const scope = newMediaScope('player');
  let loadSeq = 0;

  function unload() {
    loadSeq++; // a grant still in flight must not start playback
    if (loadedEl) release(loadedEl);
    loadedEl = null;
    loadedPath = null;
    void mediaRevoke(scope);
  }

  $effect(() => {
    const m = el;
    const p = path;
    if (!m) return;
    if (m === loadedEl && p === loadedPath) return;
    // Only the path (and element) retrigger; autoplay is sampled once per path.
    const shouldPlay = untrack(() => autoplay);
    unload();
    loadedEl = m;
    loadedPath = p;
    failed = false;
    blocked = false;
    const mySeq = loadSeq;
    mediaSrc(p, scope).then(
      (src) => {
        if (mySeq !== loadSeq || loadedEl !== m) return; // superseded or destroyed
        if (!src) {
          failed = true;
          return;
        }
        m.src = src;
        if (shouldPlay) {
          m.muted = true; // WebView autoplay policy only allows muted playback
          let result: Promise<void> | undefined;
          try {
            result = m.play();
          } catch {
            blocked = true;
          }
          result?.catch(() => {
            blocked = true;
          });
        }
      },
      () => {
        if (mySeq === loadSeq) failed = true;
      },
    );
  });

  onDestroy(unload);
</script>

<div class="media">
  {#if kind === 'video'}
    <video
      bind:this={el}
      controls
      preload="metadata"
      aria-label={`Video preview: ${name}`}
      onerror={() => (failed = true)}
    ></video>
  {:else}
    <audio
      bind:this={el}
      controls
      preload="metadata"
      aria-label={`Audio preview: ${name}`}
      onerror={() => (failed = true)}
    ></audio>
  {/if}
  {#if failed}
    <p class="note" role="alert">Cannot play this file (unsupported or corrupt format)</p>
  {:else if blocked}
    <p class="note" role="status">Autoplay blocked — press play</p>
  {/if}
</div>

<style>
  .media {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  video {
    max-width: 100%;
    max-height: 100%;
  }
  audio {
    width: 100%;
  }
  .note {
    margin: 0;
    font-size: 12px;
    opacity: 0.8;
  }
</style>
