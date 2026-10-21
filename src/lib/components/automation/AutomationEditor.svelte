<script lang="ts">
  /**
   * Editor of an automation (AP-2, SC-1, SC-2.3, WA-1, WA-2, WA-8, RT-1): name, sync profile,
   * trigger (a schedule, or watched folders with an idle delay and a countdown preview), runtime,
   * missed-run policy, advanced policies and the readiness block. Validation texts come
   * from the model (the spec's exact strings) merged with the backend's answer; "Continue to
   * review" stays disabled while any error exists. Nothing here saves anything: saving is the
   * backend's approval flow on the review screen.
   */
  import ProfilePicker from '../sync/ProfilePicker.svelte';
  import WatchPreview from './WatchPreview.svelte';
  import Mnemonic from '../Mnemonic.svelte';
  import { roving } from '../../dialogKit';
  import type { AutomationStore, EditorState } from '../../stores/automationStore.svelte';
  import {
    IDLE_MAX,
    IDLE_MIN,
    MAX_RETRIES,
    MAX_WAIT_MIN_MAX,
    MAX_WAIT_MIN_MIN,
    RUNTIME_APP_DESC,
    RUNTIME_APP_DESC_WATCH,
    RUNTIME_BACKGROUND_DESC,
    RUNTIME_BACKGROUND_DESC_WATCH,
    RUNTIME_BACKGROUND_UNAVAILABLE,
    UNSUPPORTED_ROOT_NOTE,
    WATCH_NOTIFICATION_NOTE,
    WEEKDAYS,
    addDays,
    clampIdle,
    clampMaxWaitMin,
    dstNote,
    errorMessage,
    nextRunLines,
    safeZone,
    zoneClock,
  } from './automationModel';

  let {
    store,
    editor,
    oncreateprofile,
    oneditprofile,
  }: {
    store: AutomationStore;
    editor: EditorState;
    /** Open Sync & Backup > Sync profiles ("Create a sync profile ↗"). */
    oncreateprofile?: () => void;
    /** Open the same page for the chosen profile ("Edit sync profile ↗"). */
    oneditprofile?: (id: string) => void;
  } = $props();

  const form = $derived(editor.form);
  const lines = $derived(nextRunLines(editor.next, form.tz, Date.now()));
  const today = $derived(zoneClock(safeZone(form.tz), new Date()).date);
  const maxDate = $derived(addDays(today, 13));
  const zones = $derived.by(() => {
    try {
      const fn = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] })
        .supportedValuesOf;
      return fn ? fn('timeZone') : [];
    } catch {
      return [];
    }
  });
  const nameErrors = $derived(editor.errors.filter((e) => e.field === 'name'));
  const watch = $derived(form.kind === 'watch');
  const chosen = $derived(store.profile(form.profileId));
  const unsupported = $derived(store.unsupportedRoots(editor));
  const maxWaitMin = $derived(Math.round(form.maxWaitSecs / 60));
  const idleErrors = $derived(editor.errors.filter((e) => e.field === 'trigger.idleSecs'));
  const waitErrors = $derived(editor.errors.filter((e) => e.field === 'trigger.maxWaitSecs'));

  /** Stepper and text input of the idle delay: always an integer of 2 to 3600 (WA-1.3). */
  function setIdle(value: number) {
    store.changeForm({ idleSecs: clampIdle(value) });
  }
  function setMaxWait(minutes: number) {
    store.changeForm({ maxWaitSecs: clampMaxWaitMin(minutes) * 60 });
  }
  /** A text box shows the clamped number, also when it equals the stored one. */
  function commitNumber(
    el: HTMLInputElement,
    apply: (n: number) => void,
    clamp: (n: number) => number,
  ) {
    const n = el.value.trim() === '' ? NaN : Number(el.value);
    apply(n);
    el.value = String(clamp(n));
  }

  function toggleDay(day: number) {
    const days = form.days.includes(day) ? form.days.filter((d) => d !== day) : [...form.days, day];
    store.changeForm({ days });
  }

  const checkLabel = { ok: 'OK', warn: 'Warning', fail: 'Failed' } as const;
  const checkGlyph = { ok: '✓', warn: '!', fail: '✕' } as const;
</script>

<div class="editor" data-testid="automation-editor">
  <section>
    <label class="field">
      <span class="lbl"><Mnemonic label="&Name" /></span>
      <input
        type="text"
        value={form.name}
        aria-label="Name"
        aria-invalid={nameErrors.length > 0}
        autocomplete="off"
        oninput={(e) => store.changeForm({ name: e.currentTarget.value })}
      />
    </label>
  </section>

  <section>
    <div class="lbl">Sync profile</div>
    {#if store.profiles.length === 0}
      <div class="empty" data-testid="no-profiles">
        <p>Automations need a saved sync profile</p>
        <button type="button" onclick={() => oncreateprofile?.()}>Create a sync profile ↗</button>
      </div>
    {:else}
      <ProfilePicker
        profiles={store.profiles}
        selectedId={form.profileId || null}
        onselect={(id) => store.changeForm({ profileId: id })}
        label="Sync profile"
      />
      {#if form.profileId}
        <button type="button" class="link" onclick={() => oneditprofile?.(form.profileId)}>
          Edit sync profile ↗
        </button>
      {/if}
    {/if}
  </section>

  <section>
    <div class="lbl">Trigger</div>
    <div class="seg" role="radiogroup" aria-label="Trigger" use:roving>
      {#each [['schedule', 'On a &schedule'], ['watch', 'When &files change']] as [value, text] (value)}
        <button
          type="button"
          role="radio"
          aria-checked={form.kind === value}
          class:on={form.kind === value}
          disabled={!!editor.id && form.kind !== value}
          onclick={() => store.changeForm({ kind: value as 'schedule' | 'watch' })}
        >
          <Mnemonic label={text} />
        </button>
      {/each}
    </div>
  </section>

  {#if watch}
    <section data-testid="watch-fields">
      <div class="lbl">Watched folders</div>
      <div class="checks-col">
        {#each [{ side: 'left' as const, text: 'Watch the &left folder', path: chosen?.left, on: form.watchLeft }, { side: 'right' as const, text: 'Watch the &right folder', path: chosen?.right, on: form.watchRight }] as r (r.side)}
          {@const off = unsupported[r.side]}
          <label class="chk" class:off data-root={r.side}>
            <input
              type="checkbox"
              checked={r.on}
              disabled={off}
              aria-label={r.text.replace('&', '')}
              onchange={(e) =>
                store.changeForm(
                  r.side === 'left'
                    ? { watchLeft: e.currentTarget.checked }
                    : { watchRight: e.currentTarget.checked },
                )}
            />
            <span class="chk-main">
              <span><Mnemonic label={r.text} /></span>
              {#if r.path}<span class="path" title={r.path}>{r.path}</span>{/if}
              {#if off}<span class="reason" data-testid="unsupported-{r.side}"
                  >{UNSUPPORTED_ROOT_NOTE}</span
                >{/if}
            </span>
          </label>
        {/each}
      </div>

      <div class="lbl">Run when</div>
      <div class="checks-col" role="group" aria-label="Run when">
        <label class="chk">
          <input
            type="checkbox"
            checked={form.onChanges}
            aria-label="File & folder changes"
            onchange={(e) => store.changeForm({ onChanges: e.currentTarget.checked })}
          />
          <span class="chk-main">File &amp; folder changes</span>
        </label>
        <label class="chk">
          <input
            type="checkbox"
            checked={form.onAvailable}
            aria-label="Location becomes available"
            onchange={(e) => store.changeForm({ onAvailable: e.currentTarget.checked })}
          />
          <span class="chk-main">Location becomes available</span>
        </label>
      </div>

      <div class="grid">
        <div class="field" role="group" aria-label="Idle delay">
          <span class="lbl"><Mnemonic label="&Idle delay (seconds)" /></span>
          <span class="stepper">
            <button
              type="button"
              aria-label="Shorter idle delay"
              disabled={form.idleSecs <= IDLE_MIN}
              onclick={() => setIdle(form.idleSecs - 1)}>−</button
            >
            <input
              type="text"
              inputmode="numeric"
              class="num"
              value={String(form.idleSecs)}
              aria-label="Idle delay"
              aria-invalid={idleErrors.length > 0}
              autocomplete="off"
              onchange={(e) => commitNumber(e.currentTarget, setIdle, clampIdle)}
            />
            <button
              type="button"
              aria-label="Longer idle delay"
              disabled={form.idleSecs >= IDLE_MAX}
              onclick={() => setIdle(form.idleSecs + 1)}>+</button
            >
          </span>
          <span class="hint">{IDLE_MIN} to {IDLE_MAX} s of quiet before a run starts</span>
        </div>
        <div class="field" role="group" aria-label="Longest wait">
          <span class="lbl"><Mnemonic label="Longest &wait (minutes)" /></span>
          <span class="stepper">
            <button
              type="button"
              aria-label="Shorter longest wait"
              disabled={maxWaitMin <= MAX_WAIT_MIN_MIN}
              onclick={() => setMaxWait(maxWaitMin - 1)}>−</button
            >
            <input
              type="text"
              inputmode="numeric"
              class="num"
              value={String(maxWaitMin)}
              aria-label="Longest wait"
              aria-invalid={waitErrors.length > 0}
              autocomplete="off"
              onchange={(e) => commitNumber(e.currentTarget, setMaxWait, clampMaxWaitMin)}
            />
            <button
              type="button"
              aria-label="Longer longest wait"
              disabled={maxWaitMin >= MAX_WAIT_MIN_MAX}
              onclick={() => setMaxWait(maxWaitMin + 1)}>+</button
            >
          </span>
          <span class="hint">One run is queued after this even if changes keep arriving</span>
        </div>
      </div>

      <p class="hint" data-testid="watch-note">{WATCH_NOTIFICATION_NOTE}</p>
      <WatchPreview idleSecs={form.idleSecs} />
    </section>
  {/if}

  {#if !watch}
    <section>
      <div class="lbl">Schedule</div>
      <div class="seg" role="radiogroup" aria-label="Frequency" use:roving>
        {#each [['once', '&Once'], ['daily', '&Daily'], ['weekly', 'Wee&kly']] as [value, text] (value)}
          <button
            type="button"
            role="radio"
            aria-checked={form.freq === value}
            class:on={form.freq === value}
            onclick={() => store.changeForm({ freq: value as 'once' | 'daily' | 'weekly' })}
          >
            <Mnemonic label={text} />
          </button>
        {/each}
      </div>

      <div class="grid">
        {#if form.freq === 'once'}
          <label class="field">
            <span class="lbl"><Mnemonic label="D&ate" /></span>
            <input
              type="date"
              value={form.date}
              min={today}
              max={maxDate}
              aria-label="Date"
              oninput={(e) => store.changeForm({ date: e.currentTarget.value })}
            />
          </label>
        {/if}
        <label class="field">
          <span class="lbl"><Mnemonic label="&Time (24 hours)" /></span>
          <input
            type="time"
            value={form.time}
            aria-label="Time"
            oninput={(e) => store.changeForm({ time: e.currentTarget.value })}
          />
        </label>
        <label class="field">
          <span class="lbl"><Mnemonic label="Time &zone" /></span>
          <input
            type="text"
            value={form.tz}
            list="automation-zones"
            aria-label="Time zone"
            autocomplete="off"
            spellcheck="false"
            oninput={(e) => store.changeForm({ tz: e.currentTarget.value.trim() })}
          />
          <datalist id="automation-zones">
            {#each zones as z (z)}<option value={z}></option>{/each}
          </datalist>
        </label>
      </div>

      {#if form.freq === 'weekly'}
        <div class="days" role="group" aria-label="Weekdays">
          {#each WEEKDAYS as d (d.value)}
            <button
              type="button"
              class:on={form.days.includes(d.value)}
              aria-pressed={form.days.includes(d.value)}
              aria-label={d.long}
              onclick={() => toggleDay(d.value)}
            >
              {d.short}
            </button>
          {/each}
        </div>
      {/if}

      <div class="next" aria-label="Next runs" data-testid="next-runs">
        <div class="lbl">Next runs</div>
        {#if lines.length > 0}
          <ol>
            {#each lines as l, i (i)}
              <li>
                <span>{l.label}</span>
                <span class="rel">{l.relative}</span>
                {#if l.equivalent}<span class="eq">{l.equivalent}</span>{/if}
              </li>
            {/each}
          </ol>
        {:else}
          <p class="hint">Upcoming runs are listed here once the schedule is valid and checked.</p>
        {/if}
      </div>
      {#if dstNote(form.tz)}
        <p class="hint" data-testid="dst-note">{dstNote(form.tz)}</p>
      {/if}
    </section>
  {/if}

  <section>
    <div class="lbl">Runtime</div>
    <div class="cards" role="radiogroup" aria-label="Runtime" use:roving>
      <button
        type="button"
        role="radio"
        class="card"
        class:sel={form.runtime === 'app'}
        aria-checked={form.runtime === 'app'}
        data-runtime="app"
      >
        <span class="top"
          ><span class="name">While Ferrum is running</span><span class="chip ok">Available</span
          ></span
        >
        <span class="desc">{watch ? RUNTIME_APP_DESC_WATCH : RUNTIME_APP_DESC}</span>
      </button>
      <button
        type="button"
        role="radio"
        class="card off"
        class:sel={form.runtime === 'background'}
        aria-checked={form.runtime === 'background'}
        aria-disabled="true"
        disabled
        data-runtime="background"
      >
        <span class="top"
          ><span class="name">In the background</span><span class="chip warn">Unavailable</span
          ></span
        >
        <span class="desc">{watch ? RUNTIME_BACKGROUND_DESC_WATCH : RUNTIME_BACKGROUND_DESC}</span>
        <span class="reason">{RUNTIME_BACKGROUND_UNAVAILABLE}</span>
      </button>
    </div>
  </section>

  {#if !watch}
    <section>
      <div class="lbl">If a run is missed</div>
      <div class="radios" role="radiogroup" aria-label="If a run is missed">
        <label>
          <input
            type="radio"
            name="missed"
            checked={form.missed === 'skip'}
            onchange={() => store.changeForm({ missed: 'skip' })}
          />
          <Mnemonic label="Ski&p it (default)" />
        </label>
        <label>
          <input
            type="radio"
            name="missed"
            checked={form.missed === 'runOnce'}
            onchange={() => store.changeForm({ missed: 'runOnce' })}
          />
          <Mnemonic label="Run once when a&vailable again" />
        </label>
      </div>
    </section>
  {/if}

  <details class="adv">
    <summary>Advanced policies</summary>
    <ul class="policies">
      {#if watch}
        <li>
          Bursts: every new change restarts the idle delay, so one run covers the whole burst.
        </li>
        <li>Changes written by the automation's own runs are ignored.</li>
      {/if}
      <li>Overlapping runs: a run waits while another uses the same folders.</li>
      <li>Conflicts: held for review, never overwritten.</li>
      <li>Offline location: the run waits until the folder is available.</li>
      <li>Profile changes: the automation is held until you review them.</li>
    </ul>
    <label class="field inline">
      <span class="lbl">Retries for transient errors</span>
      <select
        aria-label="Retries"
        value={String(form.retries)}
        onchange={(e) => store.changeForm({ retries: Number(e.currentTarget.value) })}
      >
        {#each Array.from({ length: MAX_RETRIES + 1 }, (_, i) => i) as n (n)}
          <option value={String(n)} selected={form.retries === n}>{n}</option>
        {/each}
      </select>
    </label>
  </details>

  <section class="ready" aria-label="Readiness">
    <div class="lbl">Readiness</div>
    {#if editor.unavailable}
      <p class="err" role="alert">{editor.unavailable}</p>
    {:else if editor.checks.length === 0}
      <p class="hint">{editor.busy ? 'Checking…' : 'Nothing to check yet.'}</p>
    {:else}
      <ul class="checks">
        {#each editor.checks as c (c.id)}
          <li class={c.status} data-check={c.id}>
            <span class="glyph" role="img" aria-label={checkLabel[c.status]}
              >{checkGlyph[c.status]}</span
            >
            <span>{c.text}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  {#if editor.note}
    <p class="hint" role="status" data-testid="editor-note">{editor.note}</p>
  {/if}

  {#if editor.errors.length > 0}
    <div class="errors" role="alert" data-testid="editor-errors">
      <div class="lbl">Fix these to continue</div>
      <ul>
        {#each editor.errors as e (e.field + e.code)}
          <li data-field={e.field}>{errorMessage(e)}</li>
        {/each}
      </ul>
    </div>
  {/if}

  <footer class="footer">
    <!-- Cancel has no access letter: Esc is Cancel. -->
    <button type="button" onclick={() => store.leaveEditor()}>Cancel</button>
    <button
      type="button"
      class="primary"
      disabled={!store.canContinue}
      onclick={() => store.continueToReview()}
    >
      <Mnemonic label="&Continue to review" />
    </button>
  </footer>
</div>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: 14px;
    overflow: auto;
    min-height: 0;
    padding-right: 4px;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text2);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field.inline {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  input[type='text'],
  input[type='date'],
  input[type='time'],
  select {
    height: 26px;
    padding: 0 8px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    font-size: 12.5px;
  }
  input[aria-invalid='true'] {
    border-color: var(--red);
  }
  .grid {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }
  .seg {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r);
    background: var(--hover2);
    align-self: flex-start;
  }
  .seg button,
  .days button {
    height: 24px;
    padding: 0 12px;
    border: 0;
    border-radius: calc(var(--r) - 1px);
    background: transparent;
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .seg button.on,
  .days button.on {
    background: var(--accent);
    color: #fff;
  }
  .days {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .days button {
    background: var(--hover2);
    padding: 0 9px;
  }
  .next ol {
    margin: 0;
    padding-left: 18px;
    font-size: 12px;
  }
  .next li {
    display: flex;
    gap: 8px;
  }
  .rel,
  .eq,
  .hint {
    color: var(--text3);
    font-size: 11.5px;
    margin: 0;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 3px;
    text-align: left;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    cursor: default;
  }
  .card.sel {
    border-color: var(--accent);
    background: var(--hover);
  }
  .card.off {
    opacity: 0.7;
  }
  .card .top {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .card .name {
    font-size: 13px;
    font-weight: 600;
  }
  .card .desc {
    font-size: 11.5px;
    color: var(--text3);
  }
  .card .reason {
    font-size: 11.5px;
    color: var(--amberInk);
  }
  .chip {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--hover2);
    color: var(--text2);
    font-size: 11px;
  }
  .chip.ok {
    background: var(--greenSoft);
    color: var(--greenInk);
  }
  .chip.warn {
    background: var(--amberSoft);
    color: var(--amberInk);
  }
  .checks-col {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .chk {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: 12.5px;
  }
  .chk.off {
    opacity: 0.75;
  }
  .chk-main {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .chk .path {
    font-size: 11.5px;
    color: var(--text3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 420px;
  }
  .chk .reason {
    font-size: 11.5px;
    color: var(--amberInk);
  }
  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .stepper button {
    width: 24px;
    height: 26px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 13px;
    cursor: default;
  }
  .stepper button:disabled {
    opacity: 0.45;
  }
  input.num {
    width: 64px;
    text-align: right;
  }
  .radios {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12.5px;
  }
  .adv summary {
    font-size: 12px;
    font-weight: 600;
    color: var(--text2);
    cursor: default;
  }
  .policies {
    margin: 6px 0;
    padding-left: 18px;
    font-size: 11.5px;
    color: var(--text3);
  }
  .checks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  .checks li {
    display: flex;
    gap: 6px;
    align-items: baseline;
  }
  .checks .glyph {
    width: 14px;
    text-align: center;
    font-weight: 700;
  }
  .checks .ok .glyph {
    color: var(--greenInk);
  }
  .checks .warn .glyph {
    color: var(--amberInk);
  }
  .checks .fail .glyph {
    color: var(--redInk);
  }
  .errors {
    padding: 8px 10px;
    border: 1px solid var(--red);
    border-radius: var(--r);
    background: var(--redSoft);
    font-size: 12px;
  }
  .errors ul {
    margin: 4px 0 0;
    padding-left: 18px;
  }
  .err {
    margin: 0;
    color: var(--redInk);
    font-size: 12px;
  }
  .empty {
    padding: 12px;
    border: 1px dashed var(--line);
    border-radius: var(--r);
    font-size: 12.5px;
  }
  .empty p {
    margin: 0 0 6px;
  }
  .link {
    align-self: flex-start;
    height: 22px;
    padding: 0 6px;
    border: 0;
    background: transparent;
    color: var(--accentInk);
    font-size: 12px;
    text-decoration: underline;
    cursor: default;
  }
  .footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    position: sticky;
    bottom: 0;
    padding-top: 8px;
    background: var(--elev);
  }
  .footer button,
  .empty button {
    height: 26px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .footer button:disabled {
    opacity: 0.45;
  }
  .footer button.primary {
    background: var(--accent);
    color: #fff;
  }
</style>
