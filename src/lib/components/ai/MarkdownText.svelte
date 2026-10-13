<script lang="ts">
  // Render teks/Markdown tak tepercaya: elemen Svelte saja, tanpa {@html}.
  import { parseMarkdown, type Inline } from './safeMarkdown';

  let { text }: { text: string } = $props();
  const parsed = $derived(parseMarkdown(text));
</script>

{#snippet inl(parts: Inline[])}
  {#each parts as p, i (i)}
    {#if p.t === 'strong'}<strong>{p.v}</strong>
    {:else if p.t === 'em'}<em>{p.v}</em>
    {:else if p.t === 'code'}<code class="mono">{p.v}</code>
    {:else}{p.v}{/if}
  {/each}
{/snippet}

<div class="md">
  {#each parsed.blocks as b, i (i)}
    {#if b.t === 'p'}
      <p>{@render inl(b.inline)}</p>
    {:else if b.t === 'pre'}
      <pre class="mono">{b.v}</pre>
    {:else if b.t === 'ul'}
      <ul>
        {#each b.items as it, j (j)}<li>{@render inl(it)}</li>{/each}
      </ul>
    {:else}
      <ol>
        {#each b.items as it, j (j)}<li>{@render inl(it)}</li>{/each}
      </ol>
    {/if}
  {/each}
  {#if parsed.truncated}<p class="trunc">Message truncated.</p>{/if}
</div>

<style>
  .md {
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  p,
  ul,
  ol,
  pre {
    margin: 0 0 6px;
  }
  ul,
  ol {
    padding-left: 18px;
    white-space: normal;
  }
  pre {
    background: var(--kbd);
    border-radius: var(--r);
    padding: 6px 8px;
    overflow-x: auto;
  }
  code {
    background: var(--kbd);
    border-radius: 3px;
    padding: 0 3px;
  }
  .trunc {
    color: var(--text3);
  }
</style>
