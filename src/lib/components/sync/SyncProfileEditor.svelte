<script lang="ts">
  /**
   * Sync profile editor (SP-9.2/9.3): one scrolling form. Every check shown here (errors, drive
   * kinds, readiness) comes from the backend; the form only edits the draft and sends it back.
   */
  import PathField from './PathField.svelte';
  import type { SyncProfilesStore, EditorState } from '../../stores/syncProfilesStore.svelte';
  import {
    changeLine,
    errorsFor,
    excludeExample,
    fieldMessage,
    modeSentence,
    patternsOf,
    readinessLabel,
    readinessReason,
    recycleBinBlockReason,
    rootCaption,
    type DeletePolicy,
    type SyncMode,
  } from './syncProfileModel';

  let {
    store,
    editor,
    activePath,
    inactivePath,
    oncancel,
  }: {
    store: SyncProfilesStore;
    editor: EditorState;
    activePath?: string;
    inactivePath?: string;
    oncancel?: () => void;
  } = $props();

  const form = $derived(editor.form);
  const msgs = (...fields: string[]) =>
    errorsFor(editor.errors, ...fields).map((e) => fieldMessage(e, form));

  const modes: { value: SyncMode; label: string }[] = [
    { value: 'oneWay', label: 'Update' },
    { value: 'mirror', label: 'Mirror' },
    { value: 'twoWay', label: 'Two-way' },
  ];
  const policies: { value: DeletePolicy; label: string; text: string }[] = [
    {
      value: 'recycleBin',
      label: 'Recycle Bin',
      text: 'Deleted files go to the Recycle Bin of the side they were deleted on.',
    },
    {
      value: 'versioning',
      label: 'Versioning folder',
      text: 'Deleted and overwritten files are moved to a versioning folder, so nothing is lost.',
    },
    {
      value: 'permanent',
      label: 'Delete permanently',
      text: 'Deleted files are removed for good. Ferrum asks you to confirm this in a Windows dialog when you save.',
    },
  ];

  const recycleReason = $derived(recycleBinBlockReason(form, editor.kinds));
  const excludePatterns = $derived(patternsOf(form.excludeText));
  const nothingToFix = $derived(editor.errors.length === 0);
  const allMessages = $derived([...new Set(editor.errors.map((e) => fieldMessage(e, form)))]);

  function touch() {
    store.touch();
  }
  function setMode(m: SyncMode) {
    form.mode = m;
    touch();
  }
  function setPolicy(p: DeletePolicy) {
    form.deletePolicy = p;
    touch();
  }
  function num(e: Event & { currentTarget: HTMLInputElement }): number {
    const v = e.currentTarget.valueAsNumber;
    return Number.isNaN(v) ? 0 : v;
  }

  const policyProblems = $derived(
    errorsFor(editor.errors, 'left', 'right', 'versioningRoot').filter(
      (e) => e.code === 'recycle_bin_unavailable' || e.field === 'versioningRoot',
    ),
  );
  const rootProblems = (field: 'left' | 'right') =>
    errorsFor(editor.errors, field).filter((e) => e.code !== 'recycle_bin_unavailable');

  function identityText(side: 'left' | 'right'): string {
    const id = editor.identities[side];
    if (!id) return 'Not checked yet';
    if (id.kind === 'network') return `Network share ${id.uncPrefix}`;
    return `Volume ${id.volumeSerial}`;
  }
</script>

<form
  class="editor"
  aria-label={editor.id ? 'Edit sync profile' : 'New sync profile'}
  onsubmit={(e) => {
    e.preventDefault();
    void store.save();
  }}
>
  <div class="scroll">
    {#if editor.id}
      <p class="rev" data-testid="editor-revision">
        Revision {editor.revision}
        {#if editor.changes[0]}
          · last change: {editor.changes[0].changes.map(changeLine).join('; ')}
        {/if}
      </p>
    {/if}
    {#if editor.notice}
      <p
        class="banner {editor.notice.kind}"
        role={editor.notice.kind === 'error' ? 'alert' : 'status'}
      >
        {editor.notice.text}
      </p>
    {/if}

    <section aria-labelledby="sec-name">
      <h3 id="sec-name">Name</h3>
      <div class="field">
        <label for="sp-name">Profile name</label>
        <input
          id="sp-name"
          type="text"
          autocomplete="off"
          aria-invalid={msgs('name').length > 0}
          bind:value={form.name}
          oninput={touch}
        />
        {#each msgs('name') as m (m)}<span class="err">{m}</span>{/each}
      </div>
    </section>

    <section aria-labelledby="sec-folders">
      <h3 id="sec-folders">Folders</h3>
      <PathField
        id="sp-left"
        label="Left folder"
        bind:value={form.left}
        caption={rootCaption(editor.identities.left, editor.kinds.left)}
        errors={rootProblems('left').map((e) => fieldMessage(e, form))}
        {activePath}
        oninput={touch}
      />
      <PathField
        id="sp-right"
        label="Right folder"
        bind:value={form.right}
        caption={rootCaption(editor.identities.right, editor.kinds.right)}
        errors={rootProblems('right').map((e) => fieldMessage(e, form))}
        activePath={inactivePath ?? activePath}
        oninput={touch}
      />
    </section>

    <section aria-labelledby="sec-direction">
      <h3 id="sec-direction">Direction</h3>
      <div class="segmented" role="radiogroup" aria-label="Sync mode">
        {#each modes as m (m.value)}
          <button
            type="button"
            role="radio"
            aria-checked={form.mode === m.value}
            class:sel={form.mode === m.value}
            onclick={() => setMode(m.value)}>{m.label}</button
          >
        {/each}
      </div>
      {#if form.mode !== 'twoWay'}
        <div class="segmented" role="radiogroup" aria-label="Direction">
          <button
            type="button"
            role="radio"
            aria-checked={form.from === 'left'}
            class:sel={form.from === 'left'}
            onclick={() => {
              form.from = 'left';
              touch();
            }}>Left → Right</button
          >
          <button
            type="button"
            role="radio"
            aria-checked={form.from === 'right'}
            class:sel={form.from === 'right'}
            onclick={() => {
              form.from = 'right';
              touch();
            }}>Right → Left</button
          >
        </div>
      {:else}
        <label class="check">
          <input type="checkbox" bind:checked={form.propagateDeletes} onchange={touch} />
          Propagate deletions
        </label>
      {/if}
      <p class="hint" data-testid="mode-sentence">{modeSentence(form.mode, form.from)}</p>
      {#each msgs('from', 'propagateDeletes') as m (m)}<span class="err">{m}</span>{/each}
    </section>

    <section aria-labelledby="sec-deletions">
      <h3 id="sec-deletions">Deletions</h3>
      <div class="cards" role="radiogroup" aria-label="Delete policy">
        {#each policies as p (p.value)}
          {@const blocked = p.value === 'recycleBin' ? recycleReason : null}
          <label class="card" class:sel={form.deletePolicy === p.value} class:off={!!blocked}>
            <span class="cardHead">
              <input
                type="radio"
                name="sp-delete-policy"
                value={p.value}
                checked={form.deletePolicy === p.value}
                disabled={!!blocked}
                onchange={() => setPolicy(p.value)}
              />
              <span class="cardTitle">{p.label}</span>
            </span>
            <span class="cardText">{blocked ?? p.text}</span>
          </label>
        {/each}
      </div>
      {#if form.deletePolicy === 'versioning'}
        <div class="field">
          <label for="sp-versioning">Versioning folder</label>
          <input
            id="sp-versioning"
            type="text"
            spellcheck="false"
            autocomplete="off"
            aria-invalid={msgs('versioningRoot').length > 0}
            bind:value={form.versioningRoot}
            oninput={touch}
          />
          {#each msgs('versioningRoot') as m (m)}<span class="err">{m}</span>{/each}
        </div>
      {:else if form.deletePolicy !== 'permanent'}
        <p class="hint">
          Overwritten files replace the target in place; only versioning keeps the old copy.
        </p>
      {/if}
      {#each msgs('deletePolicy') as m (m)}<span class="err">{m}</span>{/each}
    </section>

    <section aria-labelledby="sec-compare">
      <h3 id="sec-compare">Compare</h3>
      <div class="segmented" role="radiogroup" aria-label="Compare mode">
        <button
          type="button"
          role="radio"
          aria-checked={form.compare === 'sizeAndDate'}
          class:sel={form.compare === 'sizeAndDate'}
          onclick={() => {
            form.compare = 'sizeAndDate';
            touch();
          }}>Size and date</button
        >
        <button
          type="button"
          role="radio"
          aria-checked={form.compare === 'content'}
          class:sel={form.compare === 'content'}
          onclick={() => {
            form.compare = 'content';
            touch();
          }}>Content</button
        >
      </div>
    </section>

    <section aria-labelledby="sec-filters">
      <h3 id="sec-filters">Filters</h3>
      <div class="field">
        <label for="sp-exclude">Exclude (one pattern per line)</label>
        <textarea
          id="sp-exclude"
          rows="3"
          spellcheck="false"
          placeholder="node_modules\&#10;*.tmp"
          bind:value={form.excludeText}
          oninput={touch}
        ></textarea>
        <span class="hint" data-testid="exclude-example">{excludeExample(excludePatterns)}</span>
        {#each msgs('exclude') as m (m)}<span class="err">{m}</span>{/each}
      </div>
      <div class="field">
        <label for="sp-include">Include only (optional, one pattern per line)</label>
        <textarea
          id="sp-include"
          rows="2"
          spellcheck="false"
          bind:value={form.includeText}
          oninput={touch}
        ></textarea>
        {#each msgs('include') as m (m)}<span class="err">{m}</span>{/each}
      </div>
    </section>

    <section aria-labelledby="sec-safety">
      <h3 id="sec-safety">Safety</h3>
      <label class="check">
        <input type="checkbox" bind:checked={form.guardEnabled} onchange={touch} />
        Stop runs that would delete a lot
      </label>
      {#if form.guardEnabled}
        <div class="guard">
          <label for="sp-guard-count">More than (items)</label>
          <input
            id="sp-guard-count"
            type="number"
            min="1"
            max="100000"
            value={form.guardMaxCount}
            oninput={(e) => {
              form.guardMaxCount = num(e);
              touch();
            }}
          />
          <label for="sp-guard-percent">and more than (% of the target)</label>
          <input
            id="sp-guard-percent"
            type="number"
            min="1"
            max="100"
            value={form.guardMaxPercent}
            oninput={(e) => {
              form.guardMaxPercent = num(e);
              touch();
            }}
          />
        </div>
      {:else}
        <p class="hint">
          Without the guard a run can delete any number of files. Ferrum asks you to confirm turning
          it off in a Windows dialog when you save.
        </p>
      {/if}
      {#each msgs('guard') as m (m)}<span class="err">{m}</span>{/each}
    </section>

    <section aria-labelledby="sec-ready">
      <h3 id="sec-ready">Readiness</h3>
      <ul class="checks" aria-label="Readiness checks">
        <li class:bad={rootProblems('left').length > 0}>
          <span class="mark" aria-hidden="true"
            >{rootProblems('left').length ? '✗' : editor.validated ? '✓' : '·'}</span
          >
          Left folder: {rootProblems('left').length
            ? fieldMessage(rootProblems('left')[0], form)
            : editor.validated
              ? 'found'
              : 'not checked yet'}
          <span class="muted">({identityText('left')})</span>
        </li>
        <li class:bad={rootProblems('right').length > 0}>
          <span class="mark" aria-hidden="true"
            >{rootProblems('right').length ? '✗' : editor.validated ? '✓' : '·'}</span
          >
          Right folder: {rootProblems('right').length
            ? fieldMessage(rootProblems('right')[0], form)
            : editor.validated
              ? 'found'
              : 'not checked yet'}
          <span class="muted">({identityText('right')})</span>
        </li>
        <li class:bad={policyProblems.length > 0}>
          <span class="mark" aria-hidden="true"
            >{policyProblems.length ? '✗' : editor.validated ? '✓' : '·'}</span
          >
          Delete policy: {policyProblems.length
            ? fieldMessage(policyProblems[0], form)
            : editor.validated
              ? 'can be used here'
              : 'not checked yet'}
        </li>
        {#if editor.readiness}
          <li class:bad={editor.readiness.status === 'notReady'}>
            <span class="mark" aria-hidden="true"
              >{editor.readiness.status === 'ready'
                ? '✓'
                : editor.readiness.status === 'notReady'
                  ? '✗'
                  : '·'}</span
            >
            Saved profile: {readinessLabel(editor.readiness)}
            {#if editor.readiness.status === 'notReady'}— {readinessReason(editor.readiness)}{/if}
          </li>
        {/if}
      </ul>
      <p class="hint">
        Each folder's drive is recorded when you save. If a drive letter later points to another
        disk, or a folder was deleted and created again, the profile is not run until you open and
        save it again; that creates a new revision.
      </p>
    </section>

    {#if editor.id && editor.changes.length > 0}
      <section aria-labelledby="sec-history">
        <h3 id="sec-history">Revision history</h3>
        <ul class="history">
          {#each editor.changes as rev (rev.revision)}
            <li>
              <strong>Revision {rev.revision}</strong>
              {#each rev.changes as c (c.field)}<div>{changeLine(c)}</div>{/each}
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  </div>

  <footer>
    <div class="problems" aria-label="Problems" role={allMessages.length ? 'alert' : undefined}>
      {#each allMessages as m (m)}<div>{m}</div>{/each}
    </div>
    <button type="button" onclick={() => oncancel?.()}>Cancel</button>
    <button type="submit" class="primary" disabled={!nothingToFix || editor.busy === 'saving'}>
      {editor.busy === 'saving' ? 'Saving…' : 'Save'}
    </button>
  </footer>
</form>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
    gap: 8px;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding-right: 4px;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  h3 {
    margin: 0;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text3);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .rev {
    margin: 0;
    font-size: 12px;
    color: var(--text2);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  label {
    font-size: 12px;
    color: var(--text2);
  }
  input[type='text'],
  input[type='number'],
  textarea {
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--bg);
    color: var(--text);
    font-family: inherit;
    font-size: 12.5px;
  }
  input[type='text'] {
    height: 26px;
    padding-top: 0;
    padding-bottom: 0;
  }
  input[type='number'] {
    width: 90px;
    height: 26px;
    padding-top: 0;
    padding-bottom: 0;
  }
  input[aria-invalid='true'] {
    border-color: var(--red);
  }
  textarea {
    resize: vertical;
  }
  .segmented {
    display: inline-flex;
    align-self: flex-start;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r);
    background: var(--hover);
  }
  .segmented button {
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: calc(var(--r) - 2px);
    background: transparent;
    color: var(--text3);
    font-size: 12px;
    cursor: default;
  }
  .segmented button.sel {
    background: var(--elev);
    box-shadow: var(--tabShadow);
    color: var(--text);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 8px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    cursor: default;
  }
  .card.sel {
    border-color: var(--accent);
    background: var(--hover);
  }
  .card.off {
    opacity: 0.6;
  }
  .cardHead {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .card input {
    margin: 0;
  }
  .card:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .cardTitle {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
  }
  .cardText {
    font-size: 11.5px;
    color: var(--text3);
  }
  .guard {
    display: grid;
    grid-template-columns: auto auto;
    justify-content: start;
    align-items: center;
    gap: 6px 10px;
  }
  .hint {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  .err {
    font-size: 11.5px;
    color: var(--redInk);
  }
  .banner {
    margin: 0;
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--hover);
    font-size: 12px;
    color: var(--text2);
  }
  .banner.error {
    border: 1px solid var(--red);
  }
  .banner.success {
    border: 1px solid var(--green);
  }
  .checks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
    color: var(--text2);
  }
  .checks li.bad {
    color: var(--redInk);
  }
  .mark {
    display: inline-block;
    width: 14px;
  }
  .muted {
    color: var(--text3);
  }
  .history {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 11.5px;
    color: var(--text2);
  }
  footer {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--line);
  }
  .problems {
    flex: 1;
    min-width: 0;
    font-size: 11.5px;
    color: var(--redInk);
  }
  footer button {
    height: 26px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  footer button.primary {
    background: var(--accent);
    color: #fff;
  }
  footer button:disabled {
    opacity: 0.45;
  }
</style>
