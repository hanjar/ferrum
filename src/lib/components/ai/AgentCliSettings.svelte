<script lang="ts">
  // FER-490 Settings → AI → "Agent CLI": agent lokal yang terdeteksi (path lengkap, versi, status,
  // mode baca-saja yang dipaksa), agent default dan Deteksi ulang. Daftar dan path berasal dari
  // backend; webview tidak bisa memilih program atau flag.
  import { onMount } from 'svelte';
  import {
    agentCli as defaultStore,
    type AgentCliStore,
    type AgentInfo,
  } from '../../ai/agentCli.svelte';

  let { store = defaultStore }: { store?: AgentCliStore } = $props();

  onMount(() => {
    void store.load();
  });

  const view = $derived(store.view);
  const supported = $derived(store.supported);
  const terminalAgents = $derived(store.terminalAgents);

  /** FER-494: kemampuan kedua, terpisah dari baca-saja di chat. */
  function terminalText(a: AgentInfo): string {
    if (a.terminal) return 'tersedia';
    return a.terminalReason ? `tidak tersedia (${a.terminalReason})` : 'tidak tersedia';
  }

  function statusText(a: AgentInfo): string {
    const { status, reason } = a;
    if (status === 'supported' && a.broadRead)
      return a.enabled ? 'didukung, baca luas (diaktifkan)' : 'baca luas — nonaktif';
    if (status === 'supported') return 'didukung';
    if (status === 'notFound') return reason ? `tidak ditemukan (${reason})` : 'tidak ditemukan';
    return reason ?? 'tidak didukung';
  }
</script>

<section class="agents" aria-labelledby="agent-cli-title" data-testid="agent-cli-settings">
  <h3 id="agent-cli-title">Agent CLI</h3>
  <p class="hint">
    Pertanyaan tentang lampiran (PDF, Office, file besar) bisa dijawab agent CLI lokal yang membaca
    file sendiri. Agent berjalan dengan akun Anda dan selalu dalam mode baca-saja yang dipaksa oleh
    agent itu; agent tanpa mode baca-saja tidak diaktifkan.
  </p>
  <p class="hint">
    <strong>Buka di terminal</strong>: semua agent yang ditemukan dan lolos cek lokasi bisa dibuka
    interaktif di terminal, di folder pilihan Anda. Selanjutnya Anda yang memimpin, dengan izin dan
    konfigurasi agent itu sendiri. Ferrum tidak menambahkan flag apa pun dan tidak mengirim teks
    sebagai argumen.
  </p>
  {#if view?.testProfile}
    <p class="hint" data-testid="agent-cli-test-profile">
      Profil uji aktif: hanya agent palsu dari profil yang dipakai.
    </p>
  {/if}
  {#if store.error}<p class="err" role="alert">{store.error}</p>{/if}
  {#if view}
    <ul class="list">
      {#each view.agents as a (a.id)}
        <li class="agent {a.status}" data-testid="agent-row" data-agent={a.id}>
          <div class="head">
            <strong>{a.name}</strong>
            {#if a.version}<span class="ver">{a.version}</span>{/if}
          </div>
          <p class="cap">
            Baca-saja di chat: <span data-testid="agent-status-text">{statusText(a)}</span>
          </p>
          <p class="cap">
            Buka di terminal: <span data-testid="agent-terminal-text">{terminalText(a)}</span>
            {#if a.terminal && a.terminalNote}<span class="note"> — {a.terminalNote}</span>{/if}
          </p>
          {#if a.path}
            <code class="path" data-testid="agent-path">{a.path}</code>
          {/if}
          {#if a.mode}
            <p class="mode">Mode baca-saja: {a.mode}</p>
          {/if}
          {#if a.status === 'supported' && a.broadRead}
            <p class="danger" role="alert" data-testid="agent-broad-warning">
              {a.name} dapat membaca file lain di PC ini; isi bisa terkirim ke {a.vendor}.
            </p>
            <label class="optin">
              <input
                type="checkbox"
                data-testid="agent-enable"
                checked={a.enabled}
                disabled={store.loading}
                onchange={(e) => void store.setEnabled(a.id, e.currentTarget.checked)}
              />
              Aktifkan {a.name} (baca luas) atas risiko saya
            </label>
          {/if}
        </li>
      {/each}
    </ul>
    <div class="row">
      <label for="agent-default">Agent default</label>
      <select
        id="agent-default"
        data-testid="agent-default"
        value={view.defaultAgent ?? ''}
        disabled={!supported.length || store.loading}
        onchange={(e) => void store.setDefault(e.currentTarget.value as never)}
      >
        {#if !supported.length}<option value="">(tidak ada agent yang didukung)</option>{/if}
        {#each supported as a (a.id)}
          <option value={a.id}>{a.name}</option>
        {/each}
      </select>
    </div>
    <div class="row">
      <label for="agent-terminal-default">Agent untuk "Buka di terminal"</label>
      <select
        id="agent-terminal-default"
        data-testid="agent-terminal-default"
        value={store.terminalDefault?.id ?? ''}
        disabled={!terminalAgents.length || store.loading}
        onchange={(e) => void store.setTerminalDefault(e.currentTarget.value as never)}
      >
        {#if !terminalAgents.length}<option value="">(tidak ada agent yang ditemukan)</option>{/if}
        {#each terminalAgents as a (a.id)}
          <option value={a.id}>{a.name}</option>
        {/each}
      </select>
      <button
        type="button"
        data-testid="agent-redetect"
        disabled={store.loading}
        onclick={() => void store.redetect()}
        >{store.loading ? 'Mendeteksi…' : 'Deteksi ulang'}</button
      >
    </div>
  {:else if store.loading}
    <p class="hint">Mendeteksi agent…</p>
  {/if}
</section>

<style>
  .agents {
    margin-top: 16px;
  }
  h3 {
    margin: 0 0 6px;
    font-size: 13px;
  }
  .hint {
    margin: 0 0 8px;
    color: var(--text3);
    font-size: 12px;
  }
  .err {
    color: var(--red);
    font-size: 12px;
  }
  .list {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .agent {
    border: 1px solid var(--line);
    border-radius: var(--r);
    padding: 6px 8px;
    font-size: 12px;
  }
  .agent.supported {
    border-left: 3px solid var(--green);
  }
  .agent.unsupported {
    border-left: 3px solid var(--amber);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }
  .ver {
    color: var(--text3);
  }
  .path {
    display: block;
    margin-top: 2px;
    overflow-wrap: anywhere;
    font-size: 11.5px;
    color: var(--text3);
  }
  .mode {
    margin: 4px 0 0;
    color: var(--text3);
  }
  .cap {
    margin: 2px 0 0;
  }
  .note {
    color: var(--text3);
  }
  .danger {
    margin: 4px 0 0;
    color: var(--red);
    font-weight: 600;
  }
  .optin {
    display: flex;
    gap: 6px;
    align-items: center;
    margin-top: 4px;
    color: var(--red);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    margin-top: 4px;
  }
  select {
    background: var(--elev);
    color: var(--text);
    border: 1px solid var(--line2);
    border-radius: var(--r);
    padding: 2px 6px;
    font: inherit;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 2px 10px;
    color: var(--text);
    background: var(--elev);
    border: 1px solid var(--line2);
  }
  button:disabled,
  select:disabled {
    opacity: 0.5;
    cursor: default;
  }
  button:focus-visible,
  select:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
</style>
