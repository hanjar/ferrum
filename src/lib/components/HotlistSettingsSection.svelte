<script lang="ts">
  // "Hotlist" section of Settings (FER-458): where hotlist.json lives. The user can point it at a
  // synced folder (e.g. cloud drive). Changing it never overwrites silently: with no file at the new
  // place the hotlist is MOVED there (copied), with a hotlist already there the user chooses to USE
  // it. An unavailable file keeps the last good copy read-only until it can be read again.
  // FER-467 R7: the location must be really local; network drives, junctions/symlinks, reserved
  // device names and alternate data streams are refused with a reason.
  import { hotlist } from '../stores/hotlistStore.svelte';
  import { HotlistError, type HotlistStatus } from '../ipc/hotlist';

  let input = $state('');
  let editing = $state(false);
  let pending = $state<{ path: string | null; exists: boolean } | null>(null);
  let message = $state('');
  let busy = $state(false);

  const STATUS_TEXT: Partial<Record<HotlistStatus, string>> = {
    missing:
      'The hotlist file is gone from this location. Showing the last saved copy; changes are paused.',
    cloud:
      "The file is online-only (not downloaded). Ferrum won't download it. Make it available offline, then reload.",
    unreadable:
      "The file or its folder can't be read right now (drive not ready or sync pending). Showing the last saved copy; changes are paused.",
    invalid:
      'The file is empty or not a valid hotlist. It is left untouched; showing the last saved copy.',
    unsafe:
      'The chosen location is not safe to use (see below). Nothing is read from or written to it; showing the last saved copy. Pick another location or use the default.',
  };

  /** FER-467 R7: why a location is refused. Shared by the check and by a refused saved location. */
  const LOCATION_PROBLEMS: Record<string, string> = {
    location_network:
      'That location is on a network drive or another computer. Pick a folder on this PC (a synced folder such as a cloud drive is fine).',
    location_link:
      'That path goes through a junction or symbolic link, so the file could end up somewhere else. Use the real folder instead.',
    location_reserved:
      'That name is reserved by Windows (CON, NUL, COM1, …), ends in a dot or space, or is one of Ferrum’s own files. Choose another file name.',
    location_stream:
      'Alternate data streams (a ":" after the file name) are not allowed. Use a plain file path.',
  };

  const ERRORS: Record<string, string> = {
    invalid_path: 'Use a full local path to a .json file, for example D:\\Drive\\hotlist.json.',
    target_unusable:
      "That location can't be used: the folder isn't reachable, the file isn't a hotlist, or it is online-only.",
    target_exists: 'A hotlist already exists there. Choose to use it instead.',
    target_missing: 'There is no hotlist file there yet.',
    parent_missing: 'That folder does not exist.',
    unsupported: 'Changing the location needs the desktop app.',
    ...LOCATION_PROBLEMS,
  };

  const where = $derived(hotlist.customLocation ?? 'Default (app settings folder)');

  function fail(e: unknown) {
    message = ERRORS[e instanceof HotlistError ? e.kind : ''] ?? 'That did not work.';
  }

  async function check(path: string | null) {
    busy = true;
    message = '';
    pending = null;
    try {
      pending = { path, exists: await hotlist.api.checkLocation(path) };
    } catch (e) {
      fail(e);
    } finally {
      busy = false;
    }
  }

  async function apply(mode: 'move' | 'use') {
    if (!pending) return;
    busy = true;
    try {
      hotlist.applyView(await hotlist.api.setLocation(pending.path, mode));
      message = mode === 'move' ? 'Hotlist moved.' : 'Now using that hotlist file.';
      pending = null;
      editing = false;
    } catch (e) {
      fail(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="hot" id="settings-hotlist" data-cat="Hotlist">
  <div class="head">
    <div class="name">Hotlist file location</div>
    <div class="desc">
      Where the Directory Hotlist (Ctrl+D) is saved. Put it in a synced folder to share it between
      PCs.
    </div>
  </div>

  <div class="where">
    <span class="mono" data-testid="hotlist-location">{where}</span>
    <button onclick={() => void hotlist.load()}>Reload</button>
    {#if !editing}
      <button
        onclick={() => {
          editing = true;
          input = hotlist.customLocation ?? '';
          pending = null;
          message = '';
        }}>Change…</button
      >
    {/if}
    {#if hotlist.customLocation && !editing}
      <button onclick={() => void check(null)} disabled={busy}>Use default</button>
    {/if}
  </div>

  {#if editing}
    <div class="edit">
      <input
        aria-label="Hotlist file path"
        bind:value={input}
        spellcheck="false"
        placeholder="D:\Drive\hotlist.json"
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            void check(input.trim());
          }
        }}
      />
      <button onclick={() => void check(input.trim())} disabled={busy || input.trim() === ''}
        >Check</button
      >
      <button
        class="ghost"
        onclick={() => {
          editing = false;
          pending = null;
          message = '';
        }}>Cancel</button
      >
    </div>
  {/if}

  {#if pending}
    <div class="choice" role="group" aria-label="Location choice">
      {#if pending.exists}
        <p>A hotlist file already exists there. Your current hotlist will not overwrite it.</p>
        <button class="primary" onclick={() => void apply('use')} disabled={busy}
          >Use the existing file</button
        >
      {:else}
        <p>There is no hotlist file there yet.</p>
        <button class="primary" onclick={() => void apply('move')} disabled={busy}
          >Move my hotlist there</button
        >
      {/if}
      <button class="ghost" onclick={() => (pending = null)}>Cancel</button>
    </div>
  {/if}

  {#if message}<p class="msg" role="status">{message}</p>{/if}
  {#if STATUS_TEXT[hotlist.status]}
    <p class="msg warn" role="status">{STATUS_TEXT[hotlist.status]}</p>
  {/if}
  {#if hotlist.locationProblem && LOCATION_PROBLEMS[hotlist.locationProblem]}
    <p class="msg warn" role="status" data-testid="location-problem">
      {LOCATION_PROBLEMS[hotlist.locationProblem]}
    </p>
  {/if}
</div>

<style>
  .hot {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 8px;
    scroll-margin-top: 8px;
  }
  .name {
    font-size: calc(13px + var(--uiStep, 0px));
    color: var(--text);
  }
  .desc {
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .where,
  .edit,
  .choice {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    word-break: break-all;
  }
  input {
    flex: 1;
    min-width: 200px;
    height: 26px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  input:focus {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  p {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .warn {
    color: var(--text);
  }
  button {
    height: 26px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
  }
  button:disabled {
    opacity: 0.5;
  }
  .primary {
    background: var(--accent);
    color: #fff;
  }
  .ghost {
    background: transparent;
    color: var(--text2);
  }
</style>
