<script lang="ts">
  /**
   * Grid thumbnail (FER-452). Asks for a `ferrum-media` token only once the
   * cell is (nearly) on screen, so a folder of thousands of images never
   * grants thousands of tokens. A failed load (token expired/revoked) is
   * re-granted once.
   *
   * FER-593: with an `svg` queue the path is an SVG; it is read inline through
   * that queue (bounded concurrency, cancelled when the cell scrolls away before
   * its turn) and shown only as a `data:` URL in this <img>. `onfail` tells the
   * grid to fall back to the file icon.
   */
  import { onMount } from 'svelte';
  import type { createGrantBatcher } from '../ipc/media';
  import type { SvgThumbHandle, SvgThumbQueue } from '../ipc/svgThumbs';

  let {
    path,
    batcher,
    svg,
    onfail,
  }: {
    path: string;
    batcher: ReturnType<typeof createGrantBatcher>;
    svg?: SvgThumbQueue;
    onfail?: () => void;
  } = $props();

  let src = $state<string | undefined>(undefined);
  let el = $state<HTMLImageElement | null>(null);
  let retried = false;
  let dead = false;
  let handle: SvgThumbHandle | null = null;

  async function load() {
    const url = await batcher.request(path);
    if (!dead && url) src = url;
  }

  async function loadSvg() {
    if (handle || src || !svg) return;
    const h = (handle = svg.request(path));
    const url = await h.promise;
    if (handle !== h) return; // cancelled (scrolled away) or unmounted
    handle = null;
    if (url) src = url;
    else onfail?.();
  }

  function cancelSvg() {
    handle?.cancel();
    handle = null;
  }

  function onerror() {
    if (dead) return;
    if (svg) {
      onfail?.(); // undecodable SVG: no retry
      return;
    }
    if (retried) return;
    retried = true;
    void load();
  }

  onMount(() => {
    const node = el;
    const start = () => (svg ? void loadSvg() : void load());
    if (!node || typeof IntersectionObserver === 'undefined') {
      start();
      return () => {
        dead = true;
        cancelSvg();
      };
    }
    const io = new IntersectionObserver(
      (entries) => {
        const visible = entries.some((e) => e.isIntersecting);
        if (!svg) {
          if (visible) {
            io.disconnect();
            start();
          }
          return;
        }
        if (src) {
          io.disconnect();
        } else if (visible) {
          start();
        } else {
          cancelSvg(); // scrolled away before its turn: no read
        }
      },
      { rootMargin: '200px' },
    );
    io.observe(node.parentElement ?? node); // the (sized) box, not the empty <img>
    return () => {
      dead = true;
      cancelSvg();
      io.disconnect();
    };
  });
</script>

<img bind:this={el} class="thumb" {src} alt="" decoding="async" {onerror} />

<style>
  .thumb {
    max-width: calc(var(--thumb-box, 72px) * 1.5);
    max-height: var(--thumb-box, 72px);
    border-radius: var(--r);
    object-fit: cover;
  }
</style>
