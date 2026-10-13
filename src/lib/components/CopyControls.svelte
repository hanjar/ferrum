<script lang="ts">
  // FER-471: live thread/priority controls of one running copy job. The numbers come from the
  // backend (events + command results); a click only asks, it never assumes.
  import { ops } from '../stores/opsStore.svelte';
  import {
    COPY_THREADS_MAX,
    COPY_THREADS_MIN,
    mediaLabel,
    type CopyControlState,
    type CopyPriority,
  } from '../ipc/copyControl';

  let { control }: { control: CopyControlState } = $props();

  const id = $derived(control.operationId);
  const busy = $derived(control.activeWorkers !== control.threads);

  function setPriority(e: Event) {
    const priority = (e.currentTarget as HTMLSelectElement).value as CopyPriority;
    void ops.adjustCopy(id, { kind: 'priority', priority });
  }
</script>

<div class="copyCtl" role="group" aria-label="Kontrol thread copy">
  <span class="lab">Thread</span>
  <button
    class="step"
    aria-label="Kurangi thread"
    title="Kurangi thread (worker berhenti di batas file)"
    disabled={control.threads <= COPY_THREADS_MIN}
    onclick={() => void ops.adjustCopy(id, { kind: 'down' })}>−</button
  >
  <span class="num mono" data-testid="copy-threads" aria-live="polite">{control.threads}</span>
  <button
    class="step"
    aria-label="Tambah thread"
    title="Tambah thread"
    disabled={control.threads >= COPY_THREADS_MAX}
    onclick={() => void ops.adjustCopy(id, { kind: 'up' })}>+</button
  >
  <label class="chk">
    <input
      type="checkbox"
      checked={control.auto}
      aria-label="Otomatis"
      onchange={(e) =>
        void ops.adjustCopy(id, {
          kind: 'auto',
          enabled: (e.currentTarget as HTMLInputElement).checked,
        })}
    />
    Otomatis
  </label>
  <span class="prio">
    <span class="lab">Prioritas</span>
    <select aria-label="Prioritas copy" value={control.priority} onchange={setPriority}>
      <option value="normal">Normal</option>
      <option value="background">Latar belakang</option>
    </select>
  </span>
</div>
<div class="hint" data-testid="copy-hint">
  {mediaLabel(control.media)} · {control.activeWorkers} aktif{busy
    ? ` dari ${control.threads}`
    : ''}{control.auto ? ` · otomatis (maks ${control.autoMax})` : ''}
</div>

<style>
  .copyCtl {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .lab {
    color: var(--text3);
  }
  .prio {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .step {
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    cursor: default;
    line-height: 1;
    padding: 0;
  }
  .step:hover:not(:disabled) {
    background: var(--hover2);
  }
  .step:disabled {
    opacity: 0.4;
  }
  .num {
    min-width: 18px;
    text-align: center;
    color: var(--text);
  }
  .chk {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  select {
    font: inherit;
    color: var(--text);
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--r);
    padding: 1px 4px;
  }
  .hint {
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
</style>
