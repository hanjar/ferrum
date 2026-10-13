<script lang="ts">
  // FER-486: chip lampiran di composer chat. Hanya metadata dan flag dari backend yang tampil;
  // isi file tidak pernah ada di webview. Item sensitif wajib dicentang manual sebelum Kirim.
  import type { ChatAttachment, ChatStore } from '../../ai/chatStore.svelte';
  import { MAX_FILE_BYTES, MAX_TOTAL_BYTES, formatBytes, kindLabel } from '../../ai/attachments';

  let { store }: { store: ChatStore } = $props();

  const items = $derived(store.attachments);
  const dest = $derived(store.attachmentDestination);
  // FER-490: tujuan agent CLI lokal (agent membaca file sendiri) dan tawaran agent.
  const agent = $derived(store.targetAgent);
  const offer = $derived(store.agentOffer);
  const agentDisclosure = $derived(store.pendingAgentDisclosure);
  const contentCount = $derived(items.filter((a) => a.info?.support === 'content').length);
  const listingCount = $derived(items.filter((a) => a.info?.support === 'listing').length);
  const metaCount = $derived(items.filter((a) => a.info?.support === 'metadata').length);
  const textTotal = $derived(
    items
      .filter((a) => a.info?.support === 'content')
      .reduce((sum, a) => sum + Math.min(a.info?.size ?? 0, MAX_FILE_BYTES), 0),
  );

  function detail(a: ChatAttachment): string {
    if (a.status === 'inspecting') return 'memeriksa…';
    if (a.status === 'error') return a.error ?? 'tidak dapat dibaca';
    const info = a.info;
    if (!info) return '';
    const parts = [
      info.kind === 'folder' ? 'folder' : formatBytes(info.size),
      kindLabel(info.kind),
    ];
    if (info.kind === 'folder') parts.splice(1, 1);
    if (info.inArchive) parts.push(agent ? 'dalam arsip (tidak bisa ke agent)' : 'dalam arsip');
    // FER-493: dokumen Office dikirim sebagai teks hasil ekstraksi (ukuran file ≠ ukuran teks).
    if (info.kind === 'document' && info.support === 'content') {
      parts.push('teks diekstrak');
      return parts.join(' · ');
    }
    if (agent) return parts.join(' · ');
    if (info.support === 'metadata') parts.push(info.note ?? 'metadata saja');
    if (info.support === 'listing') parts.push('daftar isi saja');
    if (info.support === 'content' && info.size > MAX_FILE_BYTES)
      parts.push(`akan dipotong ke ${formatBytes(MAX_FILE_BYTES)}`);
    return parts.join(' · ');
  }

  function sensitiveText(a: ChatAttachment): string {
    return a.info?.sensitive === 'content'
      ? 'Isinya terlihat memuat kunci/token/kata sandi.'
      : 'Nama file ini biasanya menyimpan rahasia (kunci, kredensial, .env).';
  }

  const summary = $derived.by(() => {
    if (agent)
      return `${items.length} lampiran disalin ke folder kerja sementara lalu dibaca sendiri oleh agent ${agent.name} (mode baca-saja); isinya dikirim ke ${agent.vendor} saat Anda menekan Kirim. File sensitif di dalam folder tidak disalin.`;
    if (!dest) return '';
    const extra: string[] = [];
    if (listingCount) extra.push(`daftar isi ${listingCount} folder`);
    if (metaCount) extra.push(`metadata ${metaCount} item`);
    const more = extra.length ? ` (+ ${extra.join(', ')})` : '';
    return `Isi ${contentCount} file${more} akan dikirim ke ${dest.hostPort} saat Anda menekan Kirim.`;
  });
</script>

{#if items.length}
  <section class="attach" aria-label="Lampiran chat" data-testid="attachment-bar">
    {#if store.pendingDisclosure}
      <div
        class="disclosure"
        role="region"
        aria-label="Pemberitahuan lampiran"
        data-testid="attach-disclosure"
      >
        <p>
          Provider AI berada di luar PC ini (<strong>{store.pendingDisclosure}</strong>). Saat Anda
          menekan Kirim, isi file yang dilampirkan <strong>keluar dari PC ini</strong> dan dikirim ke
          server tersebut. Jangan lampirkan file rahasia.
        </p>
        <button type="button" onclick={() => store.ackAttachmentDisclosure()}>Mengerti</button>
      </div>
    {/if}
    {#if agentDisclosure}
      <div
        class="disclosure"
        role="region"
        aria-label="Pemberitahuan agent"
        data-testid="agent-disclosure"
      >
        <p>
          Agent <strong>{agentDisclosure.name}</strong> berjalan dengan <strong>akun Anda</strong>
          di PC ini dan membaca file lampiran sendiri. Isi file yang dibacanya
          <strong>dikirim ke layanan {agentDisclosure.vendor}</strong> sesuai akun agent tersebut.
          Ferrum memaksa mode baca-saja: {agentDisclosure.mode}. Jangan lampirkan file rahasia.
        </p>
        <button type="button" onclick={() => void store.ackAgentDisclosure()}>Mengerti</button>
      </div>
    {/if}
    {#if offer}
      <div class="offer" data-testid="agent-offer">
        <span>Sebagian lampiran tidak bisa dibaca utuh oleh AI Ferrum.</span>
        <button type="button" onclick={() => store.setTarget(offer.id)}
          >Tanya lewat {offer.name}?</button
        >
      </div>
    {/if}
    <ul class="chips">
      {#each items as a (a.path)}
        <li
          class="chip"
          class:sensitive={!!a.info?.sensitive}
          class:error={a.status === 'error'}
          data-testid="attachment-chip"
          title={a.path}
        >
          <div class="row">
            <span class="name">{a.name}</span>
            <button
              type="button"
              class="x"
              aria-label={`Hapus lampiran ${a.name}`}
              onclick={() => store.removeAttachment(a.path)}>×</button
            >
          </div>
          <span class="meta">{detail(a)}</span>
          {#if a.info?.sensitive}
            <p class="warn" role="alert">{sensitiveText(a)}</p>
            <label class="confirm">
              <input
                type="checkbox"
                checked={a.confirmed}
                data-testid="attachment-confirm"
                onchange={(e) => store.confirmAttachment(a.path, e.currentTarget.checked)}
              />
              Saya yakin, tetap kirim file ini
            </label>
          {/if}
        </li>
      {/each}
    </ul>
    <div class="foot">
      <p class="dest" data-testid="attach-destination">
        {summary}
        {#if dest?.local && !agent}
          <span class="hint">Provider lokal ini dapat meneruskan isi ke model remote.</span>
        {/if}
      </p>
      <button type="button" class="ghost" onclick={() => store.clearAttachments()}
        >Hapus semua</button
      >
    </div>
    {#if textTotal > MAX_TOTAL_BYTES && !agent}
      <p class="hint" data-testid="attach-total-note">
        Total teks melebihi {formatBytes(MAX_TOTAL_BYTES)}; file terakhir akan dipotong atau hanya
        dikirim metadatanya.
      </p>
    {/if}
  </section>
{/if}

<style>
  .attach {
    border-top: 1px solid var(--line);
    padding: 6px 8px 0;
    font-size: 12px;
  }
  .disclosure {
    margin: 0 0 6px;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-left: 3px solid var(--amber);
    border-radius: var(--r);
    background: var(--hover);
    color: var(--text2, var(--text));
  }
  .disclosure p {
    margin: 0 0 6px;
  }
  .offer {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 6px;
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--hover);
  }
  .offer span {
    flex: 1;
  }
  .offer button {
    height: 22px;
    padding: 0 8px;
    border: 1px solid var(--line2);
    background: var(--elev);
    font-size: 11.5px;
  }
  .chips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    max-height: 180px;
    overflow-y: auto;
  }
  .chip {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-width: 100%;
    min-width: 0;
    padding: 4px 6px;
    border: 1px solid var(--line2);
    border-radius: var(--r);
    background: var(--elev);
  }
  .chip.sensitive {
    border-color: var(--red);
    background: color-mix(in srgb, var(--red) 10%, var(--elev));
  }
  .chip.error {
    border-style: dashed;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .meta,
  .hint {
    color: var(--text3);
  }
  .warn {
    margin: 0;
    color: var(--red);
    font-weight: 600;
  }
  .confirm {
    display: flex;
    gap: 4px;
    align-items: center;
    color: var(--red);
  }
  .x {
    margin-left: auto;
    border: 0;
    background: transparent;
    color: var(--text3);
    cursor: pointer;
    padding: 0 2px;
    font-size: 14px;
    line-height: 1;
  }
  .foot {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    margin-top: 4px;
  }
  .dest {
    margin: 0;
    flex: 1;
  }
  .dest .hint {
    display: block;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    color: var(--text);
  }
  .disclosure button,
  .ghost {
    height: 22px;
    padding: 0 8px;
    border: 1px solid var(--line2);
    background: var(--elev);
    font-size: 11.5px;
  }
  .ghost {
    background: transparent;
  }
  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
</style>
