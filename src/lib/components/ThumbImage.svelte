<script lang="ts">
  /**
   * Grid thumbnail (FER-452). Asks for a `ferrum-media` token only once the
   * cell is (nearly) on screen, so a folder of thousands of images never
   * grants thousands of tokens. A failed load (token expired/revoked) is
   * re-granted once.
   */
  import { onMount } from 'svelte';
  import type { createGrantBatcher } from '../ipc/media';

  let { path, batcher }: { path: string; batcher: ReturnType<typeof createGrantBatcher> } =
    $props();

  let src = $state<string | undefined>(undefined);
  let el = $state<HTMLImageElement | null>(null);
  let retried = false;
  let dead = false;

  async function load() {
    const url = await batcher.request(path);
    if (!dead && url) src = url;
  }

  function onerror() {
    if (retried || dead) return;
    retried = true;
    void load();
  }

  onMount(() => {
    const node = el;
    if (!node || typeof IntersectionObserver === 'undefined') {
      void load();
      return () => {
        dead = true;
      };
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          void load();
        }
      },
      { rootMargin: '200px' },
    );
    io.observe(node.parentElement ?? node); // the (sized) box, not the empty <img>
    return () => {
      dead = true;
      io.disconnect();
    };
  });
</script>

<img bind:this={el} class="thumb" {src} alt="" decoding="async" {onerror} />

<style>
  .thumb {
    max-width: 110px;
    max-height: 72px;
    border-radius: var(--r);
    object-fit: cover;
  }
</style>
