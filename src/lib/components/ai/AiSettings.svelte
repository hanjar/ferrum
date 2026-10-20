<script module lang="ts">
  export interface AiSettingsState {
    setupSaved: boolean;
    keyConfigured: boolean;
  }
</script>

<script lang="ts">
  // Setup provider AI (FER-412, urutan FER-429): endpoint -> model -> key -> Simpan -> Test
  // connection -> hasil. Satu tombol Save menyimpan setup dan (bila diisi) key; Test connection
  // baru aktif setelah semuanya tersimpan dan selalu menguji nilai TERSIMPAN, bukan isi form.
  // Keamanan: key hanya hidup di elemen <input type=password> (tanpa state/storage/log); saat
  // disimpan nilainya dikirim ke backend lalu field langsung dikosongkan. Hanya kode galat aman
  // yang ditampilkan. Setup non-rahasia disimpan di backend lewat providerSettings (FER-430), jadi
  // sama untuk build dev dan exe; form diisi dari sana begitu pemuatan backend selesai.
  import { onDestroy, onMount } from 'svelte';
  import { backendConnectionTransport } from '../../ai/backendBridge';
  import { providerSaid } from '../../ai/providerErrors';
  import {
    OUTBOUND_DISCLOSURE,
    testConnection,
    type BackendConnectionTransport,
  } from '../../ai/provider-setup';
  import {
    CREDENTIAL_REF,
    credentialClear,
    credentialSave,
    credentialStatus,
    DEFAULT_SETUP_FORM,
    initProviderSetup,
    loadStoredSetup,
    saveProviderSetup,
    testProfileActive,
    type CredentialErrorKind,
    type SetupForm,
  } from '../../ai/providerSettings';

  let {
    onchange,
    transport = backendConnectionTransport,
  }: {
    /** Dipanggil setelah setup/key berubah (host dapat menyegarkan badge status AI). */
    onchange?: (state: AiSettingsState) => void;
    /** Transport tes koneksi; default memanggil `ai_test_connection`. Dapat diganti di tes. */
    transport?: BackendConnectionTransport;
  } = $props();

  const stored = loadStoredSetup();
  let endpoint = $state(stored?.endpoint ?? DEFAULT_SETUP_FORM.endpoint);
  let model = $state(stored?.model ?? DEFAULT_SETUP_FORM.model);
  let timeoutMs = $state(String(stored?.timeoutMs ?? DEFAULT_SETUP_FORM.timeoutMs));
  let structured = $state<SetupForm['structured']>(
    stored?.structured ?? DEFAULT_SETUP_FORM.structured,
  );

  const snapshot = (f: SetupForm) =>
    JSON.stringify([f.endpoint, f.model, Number(f.timeoutMs), f.structured]);
  const form = (): SetupForm => ({ endpoint, model, timeoutMs: Number(timeoutMs), structured });
  // Snapshot setup terakhir yang tersimpan; form yang berbeda darinya = ada perubahan belum disimpan.
  let savedSnapshot = $state(stored ? snapshot(stored) : undefined);
  const initialSnapshot = snapshot(form());
  let setupSaved = $derived(savedSnapshot !== undefined);
  let keyConfigured = $state<boolean | undefined>(undefined);
  let keyEl = $state<HTMLInputElement | undefined>();
  let keyFilled = $state(false);
  let busy = $state(false);
  // FER-457: with a test profile the real credential store (shared Credential Manager) is not changed.
  let testProfile = $state(false);
  let dirty = $derived(keyFilled || savedSnapshot !== snapshot(form()));
  let canTest = $derived(setupSaved && keyConfigured === true && !dirty && !busy);

  type Note = { tone: 'ok' | 'bad' | 'info'; text: string; said?: string } | undefined;
  let saveNote = $state<Note>();
  let testNote = $state<Note>();
  let testing = $state(false);
  let controller: AbortController | undefined;
  let destroyed = false;

  const SETUP_ERRORS: Record<string, string> = {
    unsafe_endpoint: 'Endpoint is not allowed: use HTTPS, or HTTP only for localhost with a port',
    invalid_config: 'Setup is incomplete or invalid: check endpoint, model and timeout',
    storage_unavailable: 'Setup could not be saved',
    // FER-512: the native confirmation (new host) was answered with No.
    not_confirmed: 'Not saved: the native confirmation was declined',
    // FER-512: an AI request may only use the saved endpoint.
    endpoint_mismatch: 'The saved setup uses a different endpoint; save this setup first',
  };
  const KEY_ERRORS: Record<CredentialErrorKind, string> = {
    invalid_value:
      'Key was not accepted: use 8-2048 visible characters without spaces or line breaks',
    unsupported: 'Credential store is not supported on this platform',
    credential_failure: 'Key could not be stored or verified in the credential store',
    test_profile: 'Profil uji: kredensial asli tidak diubah',
  };
  const TEST_ERRORS: Record<string, string> = {
    auth: 'Provider rejected the key',
    credential: 'No key stored yet',
    rate_limited: 'Provider rate limit reached',
    quota_exceeded: 'Provider quota exhausted',
    model_unavailable: 'Model not available',
    network: 'Endpoint unreachable',
    unreachable: 'Endpoint unreachable: is the AI service running?',
    timeout: 'Request timed out',
    bad_request: 'Provider rejected the request',
    server_error: 'Provider server error',
  };

  const notify = () => onchange?.({ setupSaved, keyConfigured: keyConfigured === true });

  // Alasan Test connection belum aktif, ditampilkan di bawah tombol.
  let testHint = $derived(
    !setupSaved
      ? 'Save the endpoint and model first.'
      : keyConfigured === false
        ? 'Save an API key first.'
        : dirty
          ? 'You have unsaved changes. Save first: the test uses the saved values.'
          : undefined,
  );

  onMount(async () => {
    const [status, setup, inTestProfile] = await Promise.all([
      credentialStatus(),
      initProviderSetup(),
      testProfileActive(),
    ]);
    if (destroyed) return;
    testProfile = inTestProfile;
    keyConfigured = status.ok ? status.configured : false;
    // Prefill from the backend unless the user already started typing.
    if (setup && !keyFilled && snapshot(form()) === initialSnapshot) {
      endpoint = setup.endpoint;
      model = setup.model;
      timeoutMs = String(setup.timeoutMs);
      structured = setup.structured;
      savedSnapshot = snapshot(setup);
    }
    notify();
  });
  onDestroy(() => {
    destroyed = true;
    controller?.abort();
  });

  // Simpan setup lalu (bila field key terisi) key. Setup tidak valid -> key tidak dikirim.
  async function save() {
    if (busy) return;
    busy = true;
    const result = await saveProviderSetup(form());
    busy = false;
    if (destroyed) return;
    if (!result.ok) {
      saveNote = {
        tone: 'bad',
        text: `${SETUP_ERRORS[result.kind] ?? 'Setup rejected'} (${result.kind})`,
      };
      return;
    }
    savedSnapshot = snapshot(result.setup);
    testNote = undefined;
    const value = keyEl?.value ?? '';
    if (!value) {
      saveNote = { tone: 'ok', text: 'Saved' };
      notify();
      return;
    }
    busy = true;
    // Kosongkan field SEBELUM menunggu backend: key tidak tinggal di DOM selama IPC berjalan.
    if (keyEl) keyEl.value = '';
    keyFilled = false;
    const keyResult = await credentialSave(value);
    busy = false;
    if (destroyed) return;
    if (keyResult.ok) {
      keyConfigured = true;
      saveNote = { tone: 'ok', text: 'Saved. Key stored in the Windows credential store' };
    } else {
      saveNote = {
        tone: 'bad',
        text: `Setup saved, but the key was not: ${KEY_ERRORS[keyResult.kind]} (${keyResult.kind})`,
      };
    }
    notify();
  }

  async function clearKey() {
    busy = true;
    const result = await credentialClear();
    busy = false;
    if (destroyed) return;
    if (result.ok) {
      keyConfigured = false;
      testNote = undefined;
      saveNote = { tone: 'info', text: 'Key removed' };
    } else {
      saveNote = { tone: 'bad', text: `${KEY_ERRORS[result.kind]} (${result.kind})` };
    }
    notify();
  }

  async function runTest() {
    const saved = loadStoredSetup();
    if (testing || !canTest || !saved) return;
    testing = true;
    testNote = { tone: 'info', text: 'Testing connection...' };
    const abort = new AbortController();
    controller = abort;
    const label = `${saved.model} at ${saved.endpoint}`;
    const result = await testConnection(
      { ...saved, credentialRef: CREDENTIAL_REF },
      transport,
      abort.signal,
    );
    testing = false;
    if (destroyed) return;
    if (result.ok) testNote = { tone: 'ok', text: `Connection OK: ${label}` };
    else if (result.kind === 'cancelled') testNote = { tone: 'info', text: 'Test cancelled' };
    else {
      const text = TEST_ERRORS[result.kind] ?? SETUP_ERRORS[result.kind] ?? 'Connection failed';
      const said = providerSaid(result.detail, saved.model);
      testNote = {
        tone: 'bad',
        text: `Connection failed: ${text} (${result.kind})`,
        said,
      };
    }
  }

  function cancelTest() {
    controller?.abort();
  }

  // Keyboard di dalam bagian ini tidak diteruskan ke shortcut file manager.
  function stopKeys(event: KeyboardEvent) {
    if (event.key !== 'Escape') event.stopPropagation();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="ai" aria-labelledby="ai-settings-title" onkeydown={stopKeys}>
  <h3 id="ai-settings-title" class="sectionTitle">AI assistant provider</h3>
  <p class="sub">
    Use an OpenAI-compatible chat endpoint. The key is kept in the operating system credential
    store.
  </p>

  <form
    aria-label="AI provider setup"
    onsubmit={(event) => {
      event.preventDefault();
      void save();
    }}
  >
    <label class="field">
      <span>Endpoint</span>
      <input
        type="url"
        bind:value={endpoint}
        placeholder="https://host/v1"
        autocomplete="off"
        spellcheck="false"
      />
    </label>
    <label class="field">
      <span>Model</span>
      <input type="text" bind:value={model} autocomplete="off" spellcheck="false" />
    </label>
    <div class="row">
      <label class="field">
        <span>Timeout (ms)</span>
        <input type="number" min="100" max="120000" step="100" bind:value={timeoutMs} />
      </label>
      <label class="field">
        <span>Structured output</span>
        <select bind:value={structured}>
          <option value="json_object">JSON object</option>
          <option value="none">None</option>
        </select>
      </label>
    </div>
    <label class="field">
      <span>API key</span>
      <input
        type="password"
        bind:this={keyEl}
        oninput={() => (keyFilled = (keyEl?.value ?? '') !== '')}
        disabled={testProfile}
        placeholder={keyConfigured ? 'Leave empty to keep the stored key' : ''}
        autocomplete="off"
        spellcheck="false"
      />
    </label>
    <div class="keyline">
      <p class="muted" role="status" aria-label="Key status">
        {keyConfigured === undefined
          ? 'Checking key...'
          : keyConfigured
            ? 'Key stored'
            : 'No key stored'}
      </p>
      <button
        type="button"
        class="link"
        disabled={!keyConfigured || busy || testProfile}
        onclick={clearKey}>Clear key</button
      >
    </div>
    {#if testProfile}
      <p class="muted" role="status" aria-label="Test profile">
        Profil uji: kredensial asli tidak diubah (key tidak bisa disimpan atau dihapus di sini).
      </p>
    {/if}

    <div class="actions">
      <button type="submit" class="primary" disabled={busy}>Save</button>
      <span class="state" role="status" aria-label="Save state">
        {!setupSaved ? 'Not saved yet' : dirty ? 'Unsaved changes' : 'All changes saved'}
      </span>
    </div>
    {#if saveNote}
      <p
        class={saveNote.tone}
        role={saveNote.tone === 'bad' ? 'alert' : 'status'}
        aria-label="Save result"
      >
        {saveNote.text}
      </p>
    {/if}
  </form>

  <div class="test" role="group" aria-label="Connection test">
    <div class="actions">
      {#if testing}
        <button type="button" onclick={cancelTest}>Cancel test</button>
      {:else}
        <button type="button" disabled={!canTest} onclick={runTest}>Test connection</button>
      {/if}
    </div>
    {#if testHint && !testing}
      <p class="muted" aria-label="Test hint">{testHint}</p>
    {/if}
    {#if testNote}
      <p
        class="result {testNote.tone}"
        role={testNote.tone === 'bad' ? 'alert' : 'status'}
        aria-label="Connection status"
      >
        {testNote.text}
        {#if testNote.said}<span class="said" data-testid="provider-said">{testNote.said}</span
          >{/if}
      </p>
    {/if}
  </div>

  <ul class="disclosure" aria-label="What is sent to the provider">
    {#each OUTBOUND_DISCLOSURE as line (line)}
      <li>{line}</li>
    {/each}
  </ul>
</section>

<style>
  .ai {
    display: flex;
    flex-direction: column;
    gap: 10px;
    color: var(--text);
  }
  .sectionTitle {
    margin: 0;
    font-size: 11px;
    font-weight: 500;
    color: var(--text3);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .sub,
  .muted,
  .disclosure {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .row .field {
    flex: 1;
    min-width: 0;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  input,
  select {
    background: var(--elev);
    color: var(--text);
    border: 1px solid var(--line2);
    border-radius: var(--r);
    padding: 5px 8px;
    font: inherit;
    min-width: 0;
  }
  input:focus-visible,
  select:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 4px 10px;
    color: var(--text);
    background: transparent;
    border: 1px solid var(--line2);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  p.ok,
  p.bad,
  p.info {
    margin: 0;
    font-size: 12px;
  }
  p.bad {
    color: var(--danger, #c0392b);
  }
  .disclosure {
    padding-left: 16px;
  }
  .keyline {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .link {
    border: none;
    padding: 0 2px;
    font-size: 11.5px;
    color: var(--text2);
    text-decoration: underline;
  }
  .state {
    font-size: 11.5px;
    color: var(--text3);
  }
  .test {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 10px;
    border-top: 1px solid var(--line2);
  }
  p.result .said {
    display: block;
    margin-top: 4px;
    opacity: 0.85;
  }
  p.result {
    padding: 6px 8px;
    border-radius: var(--r);
    border: 1px solid var(--line2);
  }
  p.result.ok {
    color: var(--ok, #2e7d32);
    border-color: currentColor;
  }
  p.result.bad {
    border-color: currentColor;
  }
</style>
