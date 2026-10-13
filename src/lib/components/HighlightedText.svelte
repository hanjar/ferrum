<script lang="ts">
  import { detectLang, tokenize } from '../preview/highlight';

  let {
    text,
    fileName,
    kind,
    enabled,
  }: { text: string; fileName: string; kind: 'text' | 'json' | 'markdown'; enabled: boolean } =
    $props();

  const tokens = $derived(enabled ? tokenize(text, detectLang(fileName, kind)) : []);
</script>

<!-- Tokens are rendered as Svelte text nodes (escaped); never via {@html}. -->
{#if enabled}
  <pre class="text hl">{#each tokens as t, i (i)}{#if t.type === 'plain'}{t.text}{:else}<span
          class="tok-{t.type}">{t.text}</span
        >{/if}{/each}</pre>
{:else}
  <pre class="text">{text}</pre>
{/if}

<style>
  .text {
    margin: 0;
    font-family: 'Geist Mono', monospace;
    font-size: calc(11px + var(--uiStep, 0px));
    line-height: 1.5;
    color: var(--text2);
    white-space: pre-wrap;
    word-break: break-all;
    user-select: text;
  }
  .tok-keyword,
  .tok-key {
    color: var(--accentInk);
  }
  .tok-string {
    color: var(--greenInk);
  }
  .tok-number,
  .tok-bool {
    color: var(--amberInk);
  }
  .tok-comment,
  .tok-punct {
    color: var(--text3);
  }
  .tok-comment {
    font-style: italic;
  }
  .tok-heading {
    color: var(--accentInk);
    font-weight: 700;
  }
  .tok-link {
    color: var(--accentInk);
    text-decoration: underline;
  }
</style>
