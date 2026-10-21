<script lang="ts">
  // Panel chat AI. Komponen siap-pakai: host (App.svelte, integrator) mengatur mounting/toggle.
  // - Keyboard di dalam panel tidak diteruskan ke shortcut file manager (stopPropagation).
  // - Escape menutup via onclose; fokus kembali ke elemen pemicu saat panel dilepas.
  import { onDestroy, onMount, tick } from 'svelte';
  import { dialog } from '../../dialogKit';
  import Mnemonic from '../Mnemonic.svelte';
  import type { ChatStore } from '../../ai/chatStore.svelte';
  import { describeConnection } from '../../ai/providerErrors';
  import { providerStatus, type ProviderStatus } from '../../ai/providerStatus.svelte';
  import MarkdownText from './MarkdownText.svelte';
  import PlanCard from './PlanCard.svelte';
  import ActivityAnswerCard from './ActivityAnswerCard.svelte';
  import DiskUsageAnswerCard from './DiskUsageAnswerCard.svelte';
  import CopyAdjustCard from './CopyAdjustCard.svelte';
  import IndexerCard from './IndexerCard.svelte';
  import AttachmentBar from './AttachmentBar.svelte';
  import { formatBytes, type SentAttachment } from '../../ai/attachments';
  import { agentCli, skippedText } from '../../ai/agentCli.svelte';
  import { firstAttachmentFolder, openAgentTerminal } from '../../ai/agentTerminal';
  import type { ChatTarget } from '../../ai/chatStore.svelte';
  import { revealInPane, type ActivityLocation } from '../../ai/activityQuery';
  import { copyText } from '../../clipboard';
  import { duWorkspace } from '../../diskUsage/duWorkspace.svelte';
  import { parentOf } from '../../format';
  import { paneById } from '../../stores/panesRegistry.svelte';
  import { activePane, getPane } from '../../stores/panesRegistry.svelte';
  import { followUpChips, starterChips, type Chip, type PaneHint } from './suggestionChips';

  let {
    store,
    onclose,
    returnFocus = null,
    configured = true,
    onopensettings,
    paneHint = registryHint,
    placeActivityItem = (location: ActivityLocation) => revealInPane(location, activePane()),
    revealDiskUsagePath = defaultRevealDiskUsage,
    copyDiskUsagePath = copyText,
    openDiskUsage = () => duWorkspace.openFromPanes(),
    status = providerStatus,
  }: {
    store: ChatStore;
    onclose: () => void;
    returnFocus?: HTMLElement | null;
    /** false: no provider is set up yet; the panel offers a way into the settings. */
    configured?: boolean;
    onopensettings?: () => void;
    /** FER-428: keadaan panel untuk chip starter; default membaca registry pane. */
    paneHint?: () => PaneHint;
    /** ACT-08 R7 v1: "Open in panel" for a history answer; pane API only (default: active pane). */
    placeActivityItem?: (location: ActivityLocation) => boolean;
    /** FER-466: Reveal on a Disk Usage answer (default: leave the workspace, open the parent, select). */
    revealDiskUsagePath?: (path: string) => void;
    copyDiskUsagePath?: (path: string) => Promise<boolean>;
    openDiskUsage?: () => void;
    /** FER-465: status provider (Test connection terakhir); default singleton aplikasi. */
    status?: ProviderStatus;
  } = $props();

  /** FER-466: same as the workspace's own Reveal: leave Disk Usage, open the parent in the entry pane, select. */
  function defaultRevealDiskUsage(path: string): void {
    duWorkspace.exit();
    const api = paneById(duWorkspace.entryPane);
    if (!api) return;
    api.openPath?.(parentOf(path));
    api.selectPaths?.([path], path);
  }

  function registryHint(): PaneHint {
    const left = getPane(0);
    return {
      leftPath: left?.path,
      rightPath: getPane(1)?.path,
      leftSelected: (left?.markedPaths?.() ?? left?.selectedPaths() ?? []).length,
    };
  }

  let inputEl = $state<HTMLTextAreaElement | undefined>();
  let logEl = $state<HTMLDivElement | undefined>();
  // Captured while the component is created: `use:dialog` moves focus into the panel before onMount runs.
  // svelte-ignore state_referenced_locally
  let trigger: HTMLElement | null =
    returnFocus ??
    (typeof document !== 'undefined' ? (document.activeElement as HTMLElement | null) : null);

  onMount(() => {
    inputEl?.focus();
    // FER-490: daftar agent CLI (deteksi sekali di backend) untuk pilihan tujuan.
    if (store.canUseAgents) void agentCli.load();
    // Cek ringan saat panel dibuka; status dibatasi sekali per menit.
    if (configured) void status.check();
  });
  const statusText = $derived(
    status.state === 'ok'
      ? `Provider AI siap${status.hostPort ? ` (${status.hostPort})` : ''}`
      : status.state === 'down' && status.kind
        ? describeConnection(status.kind, status.hostPort)
        : status.state === 'checking'
          ? 'Memeriksa provider AI…'
          : 'Status provider belum diketahui',
  );
  onDestroy(() => {
    const t = trigger;
    if (t && t.isConnected && typeof t.focus === 'function') queueMicrotask(() => t.focus());
  });

  // Gulir ke bawah ketika pesan/run baru masuk.
  $effect(() => {
    void store.messages.length;
    void tick().then(() => {
      if (logEl) logEl.scrollTop = logEl.scrollHeight;
    });
  });

  const planning = $derived(
    store.runs.some((r) => r.state === 'planning' || r.state === 'resolving'),
  );

  // FER-577 (B4): docked, non-modal panel. `use:dialog` adds Alt+letter access keys (they never
  // reach the file-manager shortcuts) and Esc; Tab is NOT trapped, so Tab walks the composer,
  // the cards and then leaves the panel like any other docked pane. The panel hands focus back
  // itself (onDestroy), hence returnFocus: false.
  const panelOpts = {
    modal: false,
    trap: false,
    returnFocus: false,
    initial: () => inputEl ?? null,
    onEscape: () => onclose(),
  };

  function onKeydown(e: KeyboardEvent) {
    // Shortcut file manager tidak boleh tertrigger dari dalam panel chat.
    e.stopPropagation();
  }
  function onInputKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send();
    }
  }
  function send() {
    store.send(store.draft);
  }

  /** FER-486: apa yang benar-benar dikirim untuk satu lampiran (teks aplikasi, tanpa isi). */
  function sentLabel(s: SentAttachment): string {
    if (s.support === 'listing') return 'daftar isi folder dikirim';
    if (s.support === 'metadata') return s.note ?? 'metadata saja';
    const base = `${formatBytes(s.sentBytes)} isi dikirim`;
    return s.note ? `${base} (${s.note})` : base;
  }

  // FER-486: lampiran baru (Add to chat) -> fokus ke kotak pesan.
  $effect(() => {
    if (store.attachments.length) queueMicrotask(() => inputEl?.focus());
  });

  // FER-490: pilihan tujuan pertanyaan berlampiran.
  const agentTargets = $derived(store.canUseAgents ? agentCli.supported : []);
  const targetAgent = $derived(store.targetAgent);
  function onTarget(e: Event) {
    store.setTarget((e.currentTarget as HTMLSelectElement).value as ChatTarget);
  }

  // FER-494: "Buka di terminal" — agent default terminal (Settings) di folder lampiran pertama.
  // Pertanyaan yang sudah diketik hanya disalin ke clipboard, tidak pernah jadi argumen.
  const termAgent = $derived(targetAgent ? agentCli.terminalDefault : undefined);
  const termFolder = $derived(firstAttachmentFolder(store.attachments));
  function openInTerminal() {
    const first = store.attachments[0];
    if (!termAgent || !first) return;
    void openAgentTerminal(termAgent.id, first.path, {
      question: store.draft,
      extraNote: termFolder.others ? 'Hanya folder lampiran pertama yang dibuka.' : undefined,
    });
  }

  const starters = $derived(configured && !store.messages.length ? starterChips(paneHint()) : []);
  const followUps = $derived(followUpChips(store.activeRun));
  function useChip(c: Chip) {
    if (c.mode === 'fill') {
      store.draft = c.text;
      inputEl?.focus();
      return;
    }
    store.send(c.text);
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside class="chat" aria-label="AI assistant" onkeydown={onKeydown} use:dialog={panelOpts}>
  <header>
    <h2>AI assistant</h2>
    {#if configured}
      <span
        class="pstatus {status.state}"
        role="img"
        aria-label={statusText}
        title={statusText}
        data-testid="provider-status"
      ></span>
    {/if}
    <button type="button" class="ghost" aria-label="Close AI assistant" onclick={onclose}
      ><Mnemonic label="&Close" /></button
    >
  </header>

  {#if !configured && onopensettings}
    <div class="setup" role="status" data-testid="provider-unconfigured">
      <span>The AI provider is not configured yet.</span>
      <button type="button" class="ghost" onclick={onopensettings}
        ><Mnemonic label="&Open settings" /></button
      >
    </div>
  {/if}

  <div class="log" role="log" aria-live="polite" aria-label="Conversation" bind:this={logEl}>
    {#if !store.messages.length}
      <p class="empty">
        Ask Ferrum to archive a folder or sync both panels. Operations only run after you press Run.
      </p>
      {#if starters.length}
        <div class="chips" role="group" aria-label="Suggestions" data-testid="starter-chips">
          {#each starters as c (c.id)}
            <button type="button" class="chip" disabled={store.busy} onclick={() => useChip(c)}
              >{c.text}</button
            >
          {/each}
        </div>
      {/if}
    {/if}
    {#each store.messages as m (m.id)}
      {#if m.role === 'user'}
        <div class="msg user">
          <MarkdownText text={m.text ?? ''} />
          {#if m.attachments?.length}
            <p class="attached" data-testid="message-attachments">
              Lampiran: {m.attachments.map((a) => a.name).join(', ')}
            </p>
          {/if}
        </div>
      {:else if m.role === 'answer'}
        <div class="msg answer" data-testid="chat-answer" aria-busy={m.pending ? 'true' : 'false'}>
          {#if m.agent}
            <p class="from" data-testid="answer-agent">Jawaban dari agent {m.agent}</p>
          {/if}
          {#if m.pending && m.agent}
            <div class="agentStatus" role="status" data-testid="agent-status">
              <span>{m.status ?? 'agent sedang membaca…'}</span>
              <button type="button" class="ghost" onclick={() => store.cancelAsk()}
                ><Mnemonic label="Ba&tal" /></button
              >
            </div>
            {#if m.text}<MarkdownText text={m.text} />{/if}
          {:else if m.pending}
            <p class="muted">Menunggu jawaban…</p>
          {:else if m.error}
            <p class="err" role="alert">{m.error}</p>
          {:else}
            <MarkdownText text={m.text ?? ''} />
            {#if m.agentSkipped?.length}
              <p class="muted" data-testid="agent-skipped">
                Tidak disalin ke agent: {m.agentSkipped.map(skippedText).join(', ')}
              </p>
            {/if}
            {#if m.truncated}
              <p class="muted" data-testid="agent-truncated">
                Keluaran agent melewati batas 1 MB; jawaban dipotong.
              </p>
            {/if}
          {/if}
          {#if m.sent?.length}
            <ul class="sent" aria-label="Yang dikirim ke provider" data-testid="sent-notes">
              {#each m.sent as s (s.path)}
                <li>{s.name}: {sentLabel(s)}</li>
              {/each}
            </ul>
          {/if}
        </div>
      {:else if m.role === 'notice'}
        <p class="msg notice" role="status" data-testid="chat-notice">{m.text}</p>
      {:else}
        {@const run = store.run(m.runId ?? '')}
        {#if run?.diskUsage}
          <DiskUsageAnswerCard
            {run}
            reveal={revealDiskUsagePath}
            copyPath={copyDiskUsagePath}
            {openDiskUsage}
          />
        {:else if run?.state === 'answered'}
          <ActivityAnswerCard {run} {store} place={placeActivityItem} />
        {:else if run?.copyAdjust}
          <CopyAdjustCard {run} {store} />
        {:else if run?.indexer}
          <IndexerCard {run} {store} />
        {:else if run}<PlanCard {run} {store} {status} />{/if}
      {/if}
    {/each}
    {#if planning}<p class="muted" aria-hidden="true">…</p>{/if}
  </div>

  {#if followUps.length}
    <div
      class="chips followups"
      role="group"
      aria-label="Change the plan"
      data-testid="followup-chips"
    >
      {#each followUps as c (c.id)}
        <button type="button" class="chip" onclick={() => useChip(c)}
          >{c.mode === 'fill' ? `${c.text.trim()}…` : c.text}</button
        >
      {/each}
    </div>
  {/if}

  <AttachmentBar {store} />

  {#if agentTargets.length}
    <div class="target">
      <label for="chat-target">Tujuan</label>
      <select
        id="chat-target"
        data-testid="chat-target"
        value={store.target}
        disabled={store.asking}
        onchange={onTarget}
      >
        <option value="ferrum">AI Ferrum</option>
        {#each agentTargets as a (a.id)}
          <option value={a.id}>Agent: {a.name}</option>
        {/each}
      </select>
    </div>
  {/if}

  {#if targetAgent && store.attachments.length}
    <div class="terminalOpen">
      <button
        type="button"
        class="ghost"
        data-testid="open-in-terminal"
        disabled={!termAgent}
        title={termAgent
          ? `Buka ${termAgent.name} interaktif di folder lampiran pertama`
          : 'Tidak ada agent yang bisa dibuka di terminal'}
        onclick={openInTerminal}>Buka di terminal{termAgent ? ` (${termAgent.name})` : ''}</button
      >
      {#if termFolder.others}
        <span class="muted" data-testid="open-in-terminal-note"
          >Lampiran dari beberapa folder; hanya folder pertama yang dibuka.</span
        >
      {/if}
    </div>
  {/if}

  <form
    class="composer"
    onsubmit={(e) => {
      e.preventDefault();
      send();
    }}
  >
    <textarea
      bind:this={inputEl}
      bind:value={store.draft}
      rows="2"
      aria-label="Message to AI assistant"
      data-mnemonic="m"
      aria-keyshortcuts="Alt+M"
      placeholder={targetAgent
        ? store.attachments.length
          ? `Tanya ${targetAgent.name} tentang lampiran…`
          : 'Lampirkan file dulu (Add to chat), lalu tanya agent'
        : store.attachments.length
          ? 'Tanya tentang lampiran, mis. "file ini tentang apa?"'
          : store.activeRun?.plan
            ? 'Change the plan ("namanya x.zip", "taruh di kanan", "hanya PDF") or type "cancel"'
            : store.busy
              ? 'Type "cancel" to stop the current request'
              : 'Ask Ferrum…'}
      spellcheck="false"
      onkeydown={onInputKey}
    ></textarea>
    {#if store.asking}
      <button type="button" class="ghost" onclick={() => store.cancelAsk()}
        ><Mnemonic label="Ca&ncel" /></button
      >
    {/if}
    <button type="submit" class="primary" disabled={!store.canSend(store.draft)}
      ><Mnemonic label="&Send" /></button
    >
  </form>
</aside>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    width: 360px;
    max-width: 100%;
    background: var(--pane);
    color: var(--text);
    border-left: 1px solid var(--line2);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 10px;
    border-bottom: 1px solid var(--line);
  }
  h2 {
    margin: 0;
    font-size: 13px;
    margin-right: auto;
  }
  .pstatus {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    margin-right: 10px;
    background: var(--text3);
  }
  .pstatus.ok {
    background: var(--green);
  }
  .pstatus.down {
    background: var(--red);
  }
  .pstatus.checking {
    background: var(--amber);
  }
  .setup {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--line);
    border-left: 3px solid var(--amber);
    background: var(--elev);
    font-size: 12px;
  }
  .log {
    flex: 1;
    overflow-y: auto;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }
  .empty,
  .muted {
    color: var(--text3);
    margin: 0;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chips.followups {
    padding: 6px 8px 0;
  }
  .chip {
    background: var(--elev);
    border: 1px solid var(--line2);
    border-radius: 999px;
    padding: 3px 10px;
    font-size: 12px;
    text-align: left;
  }
  .chip:hover:not(:disabled) {
    background: var(--accentSoft);
  }
  .msg.user {
    align-self: flex-end;
    max-width: 90%;
    background: var(--accentSoft);
    border-radius: var(--rl);
    padding: 6px 10px;
  }
  .msg.answer {
    align-self: stretch;
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--rl);
    padding: 6px 10px;
  }
  .attached,
  .sent {
    margin: 4px 0 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  .sent {
    padding-left: 16px;
  }
  .err {
    margin: 0;
    color: var(--red);
  }
  .from {
    margin: 0 0 4px;
    font-size: 11.5px;
    color: var(--text3);
  }
  .terminalOpen {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    padding: 4px 10px 0;
    font-size: 12px;
  }
  .agentStatus {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
    color: var(--text3);
    font-size: 12px;
  }
  .agentStatus span {
    flex: 1;
  }
  .target {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px 0;
    font-size: 12px;
  }
  .target select {
    flex: 1;
    min-width: 0;
    background: var(--elev);
    color: var(--text);
    border: 1px solid var(--line2);
    border-radius: var(--r);
    padding: 2px 6px;
    font: inherit;
  }
  .msg.notice {
    margin: 0;
    align-self: stretch;
    color: var(--text2, var(--text3));
    border-left: 3px solid var(--amber);
    background: var(--elev);
    border-radius: var(--r);
    padding: 4px 8px;
    font-size: 12px;
  }
  .composer {
    display: flex;
    gap: 6px;
    padding: 8px;
    border-top: 1px solid var(--line);
    align-items: flex-end;
  }
  textarea {
    flex: 1;
    min-width: 0;
    resize: none;
    background: var(--elev);
    color: var(--text);
    border: 1px solid var(--line2);
    border-radius: var(--r);
    padding: 6px 8px;
    font: inherit;
  }
  textarea:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 4px 10px;
    color: var(--text);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--line2);
  }
  .primary {
    background: var(--accent);
    border: 1px solid var(--accent);
    color: #fff;
  }
  /* Layar kecil: panel memenuhi lebar viewport sebagai overlay. */
  @media (max-width: 640px) {
    .chat {
      width: 100vw;
      position: fixed;
      inset: 0;
      z-index: 50;
      border-left: 0;
    }
  }
</style>
