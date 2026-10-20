<script lang="ts">
  /**
   * FER-577: a label with a Windows-style access letter, written `&Label`. The letter is
   * underlined ALWAYS (not only while Alt is held) and the owning control (button, label,
   * tab, menu item) is marked with `data-mnemonic` / `aria-keyshortcuts`, which is what
   * `use:dialog` (Alt+letter) and `use:menuKeys` (plain letter) look for.
   */
  import { parseMnemonic } from '../dialogKit';

  let { label, alt = true }: { label: string; alt?: boolean } = $props();
  const parts = $derived(parseMnemonic(label));
  let el = $state<HTMLSpanElement>();

  $effect(() => {
    const owner = el?.closest<HTMLElement>(
      'button,label,a,[role="tab"],[role="menuitem"],[role="radio"]',
    );
    const key = parts.key?.toLowerCase();
    if (!owner || !key) return;
    owner.dataset.mnemonic = key;
    if (alt) owner.setAttribute('aria-keyshortcuts', `Alt+${key.toUpperCase()}`);
    return () => {
      delete owner.dataset.mnemonic;
      owner.removeAttribute('aria-keyshortcuts');
    };
  });
</script>

<span bind:this={el}
  >{parts.before}{#if parts.key !== null}<u class="mn">{parts.key}</u>{/if}{parts.after}</span
>

<style>
  .mn {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
</style>
