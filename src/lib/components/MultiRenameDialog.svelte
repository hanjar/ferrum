<script lang="ts">
  import { focusModal } from '../focusModal';
  import { multiRename } from '../stores/multiRenameStore.svelte';
  import type { CaseMode } from '../multiRename';

  const d = multiRename;
  const preview = $derived(d.open ? d.preview() : []);
  const issues = $derived(preview.filter((item) => item.issue));
  const PREVIEW_LIMIT = 100;

  const caseModes: { value: CaseMode; label: string }[] = [
    { value: 'none', label: 'Keep case' },
    { value: 'lower', label: 'lower case' },
    { value: 'upper', label: 'UPPER CASE' },
    { value: 'firstUpper', label: 'First upper' },
  ];

  function issueText(issue: string | undefined): string {
    switch (issue) {
      case 'duplicate':
        return 'duplicate target';
      case 'collision':
        return 'name already exists';
      case 'invalid':
        return 'invalid name';
      default:
        return '';
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      d.close();
    }
  }
</script>

{#if d.open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => d.close()}></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Multi-Rename Tool"
    tabindex="-1"
    use:focusModal
    onkeydown={onKey}
  >
    <h2>Multi-Rename Tool</h2>
    <p class="path mono">{d.paths.length} selected item{d.paths.length === 1 ? '' : 's'}</p>

    <div class="rules">
      <label class="field">
        <span>Name pattern</span>
        <input
          bind:value={d.options.pattern}
          placeholder="[N] — name, [E] ext, [C] counter, [DATE], [TIME]"
          spellcheck="false"
          disabled={d.busy}
        />
      </label>
      <div class="pair">
        <label class="field">
          <span>Find</span>
          <input bind:value={d.options.find} spellcheck="false" disabled={d.busy} />
        </label>
        <label class="field">
          <span>Replace</span>
          <input bind:value={d.options.replace} spellcheck="false" disabled={d.busy} />
        </label>
      </div>
      <div class="pair">
        <label class="field">
          <span>Case</span>
          <select bind:value={d.options.caseMode} disabled={d.busy}>
            {#each caseModes as m (m.value)}
              <option value={m.value}>{m.label}</option>
            {/each}
          </select>
        </label>
        <label class="field narrow">
          <span>Counter start</span>
          <input type="number" bind:value={d.options.counterStart} disabled={d.busy} />
        </label>
        <label class="field narrow">
          <span>Step</span>
          <input type="number" bind:value={d.options.counterStep} disabled={d.busy} />
        </label>
        <label class="field narrow">
          <span>Pad</span>
          <input
            type="number"
            min="0"
            max="6"
            bind:value={d.options.counterPad}
            disabled={d.busy}
          />
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={d.options.regex} disabled={d.busy} />
          Find is a regular expression
        </label>
      </div>
    </div>

    <div class="previewHead" role="row">
      <span>Original</span>
      <span>New name</span>
    </div>
    <ul class="preview" aria-label="Rename preview">
      {#each preview.slice(0, PREVIEW_LIMIT) as item, i (i)}
        <li class:bad={!!item.issue} class:changed={item.target !== item.original}>
          <span class="mono old">{item.original}</span>
          <span class="mono">
            {item.target}
            {#if item.issue}<em class="issue">{issueText(item.issue)}</em>{/if}
          </span>
        </li>
      {/each}
      {#if preview.length > PREVIEW_LIMIT}
        <li class="dim">… and {preview.length - PREVIEW_LIMIT} more</li>
      {/if}
    </ul>

    {#if issues.length > 0}
      <p class="error" role="alert">
        {issues.length} problem{issues.length === 1 ? '' : 's'} — fix them before renaming.
      </p>
    {/if}
    {#if d.error}<p class="error" role="alert">{d.error}</p>{/if}
    {#if d.lastReport}
      <p class="report">
        Renamed {d.lastReport.renamed} of {d.lastReport
          .requested}.{#if d.lastReport.failures.length > 0}
          {d.lastReport.failures.length} failed: {d.lastReport.failures[0].message}{/if}
      </p>
    {/if}

    <div class="actions">
      <button class="ghost" onclick={() => d.close()} disabled={d.busy}>Close</button>
      <button class="ghost" onclick={() => void d.undo()} disabled={!d.canUndo()}>
        Undo last rename
      </button>
      <button class="primary" onclick={() => void d.run()} disabled={!d.canRun()}>Rename</button>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: 60;
    animation: fmFade 140ms ease-out;
  }
  .modal {
    position: fixed;
    left: 50%;
    top: 14%;
    transform: translateX(-50%);
    width: min(640px, 94vw);
    padding: 18px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 10px;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  h2 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .path {
    margin: 0;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  p {
    margin: 0;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .rules {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .pair {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 14px;
    align-items: end;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
    min-width: 130px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .field.narrow {
    flex: 0 0 84px;
    min-width: 84px;
  }
  input,
  select {
    height: 28px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line2);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  input:focus-visible,
  select:focus-visible {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    padding-bottom: 5px;
  }
  .previewHead {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .preview {
    margin: 0;
    padding: 0;
    list-style: none;
    max-height: 180px;
    overflow: auto;
    font-size: calc(11.5px + var(--uiStep, 0px));
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .preview li {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    padding: 2px 6px;
    border-radius: var(--r);
  }
  .preview li.changed:not(.bad) {
    background: var(--accentSoft);
  }
  .preview li.bad {
    color: var(--red);
  }
  .preview li.dim {
    display: block;
    color: var(--text3);
  }
  .old {
    color: var(--text3);
  }
  .issue {
    font-style: normal;
    font-size: calc(10.5px + var(--uiStep, 0px));
    margin-left: 6px;
    color: var(--red);
  }
  .error {
    color: var(--red);
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .report {
    color: var(--text2);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
  button {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  .ghost {
    background: transparent;
    color: var(--text2);
  }
  .ghost:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .primary {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .primary:hover {
    filter: brightness(1.08);
  }
  button:disabled {
    opacity: 0.5;
  }
</style>
