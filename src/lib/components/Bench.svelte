<script lang="ts">
  import FileList from './FileList.svelte';
  import { toRow, type Row } from '../stores/paneStore.svelte';
  import type { FileEntry } from '../ipc/types';
  import { ROW_H } from '../virtual';

  let { n }: { n: number } = $props();

  const rows: Row[] = Array.from({ length: n }, (_, i) => {
    const e: FileEntry = {
      name: `bench-${String(i).padStart(6, '0')}.txt`,
      path: `C:\\bench\\${i}`,
      isDir: false,
      isSymlink: false,
      size: i,
      modifiedMs: 1_790_512_000_000,
      hidden: false,
    };
    return toRow(e);
  });

  let container = $state<HTMLElement | undefined>();
  let result = $state('measuring…');

  $effect(() => {
    const el = container?.querySelector('.body') as HTMLElement | null;
    if (!el) return;
    void runBench(el);
  });

  async function runBench(el: HTMLElement) {
    await new Promise((r) => requestAnimationFrame(r));

    const t0 = performance.now();
    const domRows = el.querySelectorAll('.row').length;

    // Auto-scroll pass: step through the list over ~150 frames.
    const frames = 150;
    const maxScroll = n * ROW_H;
    let frame = 0;
    let worstHandler = 0;
    await new Promise<void>((done) => {
      const step = () => {
        const h0 = performance.now();
        el.scrollTop = (maxScroll * frame) / frames;
        el.dispatchEvent(new Event('scroll'));
        worstHandler = Math.max(worstHandler, performance.now() - h0);
        frame++;
        if (frame < frames) requestAnimationFrame(step);
        else done();
      };
      requestAnimationFrame(step);
    });
    const scrollMs = performance.now() - t0;

    const mem = (
      performance as Performance & {
        memory?: { usedJSHeapSize: number; totalJSHeapSize: number };
      }
    ).memory;

    result = JSON.stringify({
      n,
      domRows,
      scrollPassMs: Math.round(scrollMs),
      worstScrollHandlerMs: +worstHandler.toFixed(2),
      avgFrameMs: +(scrollMs / frames).toFixed(2),
      usedHeapMB: mem ? Math.round(mem.usedJSHeapSize / 1048576) : null,
    });
  }
</script>

<div bind:this={container} style="height:100vh;display:flex;flex-direction:column;">
  <div id="bench-result">{result}</div>
  <div style="flex:1;min-height:0;display:flex;flex-direction:column;">
    <FileList {rows} />
  </div>
</div>
