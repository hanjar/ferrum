<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    ui,
    PALETTES,
    type PaletteName,
    type Theme,
    type Density,
    type IconStyle,
    type IconTheme,
    type FontLevel,
    type TabWidth,
    PREVIEW_TEXT_LIMITS,
  } from '../uiState.svelte';
  import {
    COPY_BUFFER_MAX_KIB,
    COPY_BUFFER_MIN_KIB,
    COPY_THREAD_CHOICES,
    supportsPermissionPreservation,
  } from '../ipc/prefs';
  import { TERMINAL_CHOICES, type TerminalChoice } from '../ipc/shellOpen';
  import { prefs } from '../stores/prefsStore.svelte';
  import { APP_NAME, BUILD_TAG } from '../version';
  import Icon from './Icon.svelte';
  import { ICON_THEMES } from '../fileIcon';
  import { activity } from '../stores/activityStore.svelte';
  import { activityPage } from '../stores/activityPageStore.svelte';
  import ActivityPrivacyControls from './ActivityPrivacyControls.svelte';
  import { isInTauri } from '../ipc/tauri';
  import { diskUsageCacheDelete, diskUsageCacheList } from '../ipc/diskUsage';
  import { sanitizeWarnHours } from '../ipc/prefs';
  import { formatSize } from '../format';
  import AiSettingsSection from './ai/AiSettingsSection.svelte';
  import HotlistSettingsSection from './HotlistSettingsSection.svelte';
  import IndexerSettingsSection from './IndexerSettingsSection.svelte';
  import ArchiveSettingsSection from './ArchiveSettingsSection.svelte';

  let { onclose }: { onclose?: () => void } = $props();

  const themes: { value: Theme; label: string }[] = [
    { value: 'dark', label: 'Dark' },
    { value: 'light', label: 'Light' },
    { value: 'system', label: 'System' },
  ];
  const themeValues = themes.map((t) => t.value);
  const paletteValues = PALETTES.map((p) => p.value);
  const densities: { value: Density; label: string }[] = [
    { value: 'compact', label: 'Compact' },
    { value: 'comfortable', label: 'Comfortable' },
  ];
  const densityValues = densities.map((d) => d.value);
  const iconStyles: { value: IconStyle; label: string }[] = [
    { value: 'colored', label: 'Colored' },
    { value: 'monochrome', label: 'Monochrome' },
  ];
  const iconValues = iconStyles.map((i) => i.value);
  const iconThemes: { value: IconTheme; label: string }[] = ICON_THEMES.map((t) => ({
    value: t,
    label: t,
  }));
  const iconThemeValues = iconThemes.map((i) => i.value);
  const fontLevels: { value: FontLevel; label: string }[] = [
    { value: 's', label: 'Small' },
    { value: 'm', label: 'Medium' },
    { value: 'l', label: 'Large' },
    { value: 'xl', label: 'Extra Large' },
  ];
  const terminalValues = TERMINAL_CHOICES.map((t) => t.value);

  // v3 layout: Appearance is the only functional category this phase; the
  // rest are honest placeholders (epic scope forbids expanding them).
  const categories: { label: string; active: boolean }[] = [
    { label: 'Appearance', active: true },
    { label: 'Tabs', active: true },
    { label: 'File Operations', active: true },
    { label: 'Keyboard Shortcuts', active: false },
    { label: 'Preview', active: true },
    { label: 'Hotlist', active: true },
    { label: 'Indexer', active: true },
    { label: 'Archive', active: true },
    { label: 'AI', active: true },
    { label: 'Network', active: false },
    { label: 'Advanced', active: false },
  ];

  // FER-429: the categories are anchors into the one scrolling body. Clicking one scrolls to
  // its first row (marked `data-cat`); the highlight follows whatever section is at the top.
  const categoryDesc: Record<string, string> = {
    Appearance: 'Theme, density and how files are drawn.',
    Tabs: 'How tabs open, restore and close.',
    'File Operations': 'Copy, delete and conflict behaviour.',
    Preview: 'What the preview panel renders.',
    Hotlist: 'Where the Directory Hotlist is saved.',
    Indexer: 'FerrumIndexer service: status and privacy mode.',
    Archive: 'WinRAR (Rar.exe) setup for RAR creation.',
    AI: 'AI assistant provider, key and connection test.',
  };
  let body: HTMLElement | undefined = $state();
  let currentCat = $state('Appearance');

  function catAnchors(): HTMLElement[] {
    return Array.from(body?.querySelectorAll<HTMLElement>('[data-cat]') ?? []);
  }

  function goCategory(label: string) {
    const target = catAnchors().find((el) => el.dataset.cat === label);
    if (!body || !target) return;
    const offset = target.getBoundingClientRect().top - body.getBoundingClientRect().top;
    body.scrollTo?.({ top: label === 'Appearance' ? 0 : body.scrollTop + offset - 6 });
    currentCat = label;
  }

  function trackCategory() {
    if (!body) return;
    const anchors = catAnchors();
    if (!anchors.length) return;
    const top = body.getBoundingClientRect().top;
    const atEnd = body.scrollTop + body.clientHeight >= body.scrollHeight - 2;
    let label = anchors[0].dataset.cat!;
    for (const el of anchors)
      if (el.getBoundingClientRect().top - top <= 24) label = el.dataset.cat!;
    currentCat = atEnd ? anchors[anchors.length - 1].dataset.cat! : label;
  }

  /** Decimal labels for the text-preview limits (100 KB .. 4 MB). */
  function limitLabel(bytes: number): string {
    return bytes >= 1_000_000 ? `${bytes / 1_000_000} MB` : `${bytes / 1000} KB`;
  }
  const previewLimitValues = PREVIEW_TEXT_LIMITS.map(String);

  let modal: HTMLElement | undefined = $state();
  let previouslyFocused: HTMLElement | null = null;

  onMount(() => {
    previouslyFocused = document.activeElement as HTMLElement | null;
    modal?.focus();
    // Restore focus to the opener when the dialog goes away.
    return () => previouslyFocused?.focus?.();
  });

  const conflictPolicies = [
    { value: 'ask', label: 'Ask' },
    { value: 'overwrite', label: 'Overwrite' },
    { value: 'skip', label: 'Skip' },
    { value: 'keepBoth', label: 'Keep both' },
  ] as const;
  const permissionsSupported = supportsPermissionPreservation(
    typeof navigator === 'undefined' ? 'unknown' : navigator.platform.toLowerCase(),
  );

  onMount(() => {
    if (isInTauri()) void activity.refresh().catch(() => undefined);
  });
  onDestroy(() => activityPage.cancelScope());

  // FER-464: Disk usage scan cache (roots outside the index only).
  let cacheSummary = $state('');
  async function refreshCacheSummary(): Promise<void> {
    const list = await diskUsageCacheList();
    const bytes = list.reduce((n, e) => n + e.sizeBytes, 0);
    cacheSummary =
      list.length === 0 ? 'Belum ada cache' : `${list.length} root · ${formatSize(bytes)}`;
  }
  async function clearScanCache(): Promise<void> {
    await diskUsageCacheDelete();
    await refreshCacheSummary();
  }
  onMount(() => {
    if (isInTauri()) void refreshCacheSummary();
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose?.();
      return;
    }
    if (e.key !== 'Tab' || !modal) return;
    const focusables = modal.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input, select, [href], [tabindex]:not([tabindex="-1"])',
    );
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  /** Radio semantics for segmented groups: arrows/Home/End move AND select. */
  function radioKey(e: KeyboardEvent, values: string[], current: string, set: (v: string) => void) {
    const idx = values.indexOf(current);
    const go = (i: number) => {
      const v = values[(i + values.length) % values.length];
      set(v);
      const btn = (e.currentTarget as HTMLElement).querySelector<HTMLButtonElement>(
        `[data-value="${v}"]`,
      );
      btn?.focus();
    };
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
      e.preventDefault();
      go(idx + 1);
    } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
      e.preventDefault();
      go(idx - 1);
    } else if (e.key === 'Home') {
      e.preventDefault();
      go(0);
    } else if (e.key === 'End') {
      e.preventDefault();
      go(values.length - 1);
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onmousedown={() => onclose?.()}></div>
<div
  class="modal"
  role="dialog"
  aria-modal="true"
  aria-label="Settings"
  tabindex="-1"
  bind:this={modal}
  onkeydown={onKey}
>
  <aside class="cats">
    <h2>Settings</h2>
    <nav>
      {#each categories as c (c.label)}
        <button
          class="cat"
          class:sel={c.label === currentCat}
          disabled={!c.active}
          title={c.label}
          aria-current={c.label === currentCat ? 'true' : undefined}
          onclick={() => goCategory(c.label)}
        >
          {c.label}
        </button>
      {/each}
    </nav>
    <div class="catFoot" style="margin-top:auto;font-size:10.5px;color:var(--text3);padding:0 8px;">
      {APP_NAME} · build {BUILD_TAG}
    </div>
  </aside>

  <section class="panel">
    <header class="pHead">
      <div class="pTitle">
        <h3>{currentCat}</h3>
        <p>{categoryDesc[currentCat]}</p>
      </div>
      <button class="icon" title="Close" onclick={() => onclose?.()}>
        <Icon name="close" size={11} />
      </button>
    </header>

    {#if prefs.saveError}
      <div class="saveError" role="status">
        Couldn't save settings — they will retry on the next change.
      </div>
    {/if}

    <div class="body" bind:this={body} onscroll={trackCategory}>
      <div class="rowSplit" data-cat="Appearance">
        <div class="lbl">
          <div class="name">Theme</div>
          <div class="desc">Dark is tuned for long sessions in low light</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="Theme"
          tabindex="-1"
          onkeydown={(e) => radioKey(e, themeValues, ui.theme, (v) => ui.setTheme(v as Theme))}
        >
          {#each themes as t (t.value)}
            <button
              role="radio"
              data-value={t.value}
              tabindex={ui.theme === t.value ? 0 : -1}
              aria-checked={ui.theme === t.value}
              class:sel={ui.theme === t.value}
              onclick={() => ui.setTheme(t.value)}
            >
              {t.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Color theme</div>
          <div class="desc">Accent, file-type and status colors</div>
        </div>
        <div
          class="chips"
          role="radiogroup"
          aria-label="Color theme"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(e, paletteValues, ui.palette, (v) => {
              ui.palette = v as PaletteName;
            })}
        >
          {#each PALETTES as p (p.value)}
            <button
              role="radio"
              data-value={p.value}
              tabindex={ui.palette === p.value ? 0 : -1}
              aria-checked={ui.palette === p.value}
              class="chip"
              class:sel={ui.palette === p.value}
              onclick={() => {
                ui.palette = p.value;
              }}
            >
              <span class="dot" style:background={p.dot}></span>
              <span class="chipLabel">{p.label}</span>
              {#if ui.palette === p.value}
                <span class="checkMark" aria-hidden="true">✓</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Density</div>
          <div class="desc">Compact fits ~30% more rows; Comfortable is easier on touchpads</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="Density"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(e, densityValues, ui.density, (v) => {
              ui.density = v as Density;
            })}
        >
          {#each densities as d (d.value)}
            <button
              role="radio"
              data-value={d.value}
              tabindex={ui.density === d.value ? 0 : -1}
              aria-checked={ui.density === d.value}
              class:sel={ui.density === d.value}
              onclick={() => {
                ui.density = d.value;
              }}
            >
              {d.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Icon theme</div>
          <div class="desc">Shape of file-type icons</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="Icon theme"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(e, iconThemeValues, ui.iconTheme, (v) => {
              ui.iconTheme = v as IconTheme;
            })}
        >
          {#each iconThemes as i (i.value)}
            <button
              role="radio"
              data-value={i.value}
              tabindex={ui.iconTheme === i.value ? 0 : -1}
              aria-checked={ui.iconTheme === i.value}
              class:sel={ui.iconTheme === i.value}
              onclick={() => {
                ui.iconTheme = i.value;
              }}
            >
              {i.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">File icons</div>
          <div class="desc">Colour tags by file type</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="File icons"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(e, iconValues, ui.iconStyle, (v) => {
              ui.iconStyle = v as IconStyle;
            })}
        >
          {#each iconStyles as i (i.value)}
            <button
              role="radio"
              data-value={i.value}
              tabindex={ui.iconStyle === i.value ? 0 : -1}
              aria-checked={ui.iconStyle === i.value}
              class:sel={ui.iconStyle === i.value}
              onclick={() => {
                ui.iconStyle = i.value;
              }}
            >
              {i.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Interface font size</div>
          <div class="desc">Applies to the whole shell, lists included</div>
        </div>
        <select
          class="fontSelect"
          aria-label="Interface font size"
          value={ui.uiFontLevel}
          onchange={(e) => (ui.uiFontLevel = e.currentTarget.value as FontLevel)}
        >
          {#each fontLevels as f (f.value)}
            <option value={f.value}>{f.label}</option>
          {/each}
        </select>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Show file extensions</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.showExtensions}
          aria-label="Show file extensions"
          onchange={(e) => (ui.showExtensions = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Show hidden files</div>
          <div class="desc">Dotfiles and system items (Ctrl+H)</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.showHidden}
          aria-label="Show hidden files"
          onchange={(e) => (ui.showHidden = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Folders first</div>
          <div class="desc">Keep folders above files when sorting</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.foldersFirst}
          aria-label="Folders first"
          onchange={(e) => (ui.foldersFirst = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Address bar on its own row</div>
          <div class="desc">
            Full-width path row above the toolbar, so long paths stay readable in narrow panes
          </div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.addressOwnRow}
          aria-label="Address bar on its own row"
          onchange={(e) => (ui.addressOwnRow = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Tawarkan scan cepat (Disk usage dan Sync)</div>
          <div class="desc">
            Drive NTFS lokal: tanya dulu sebelum scan apakah mau baca MFT lewat helper admin (UAC)
          </div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.diskUsageFastOffer}
          aria-label="Tawarkan scan cepat (Disk usage dan Sync)"
          onchange={(e) => (ui.diskUsageFastOffer = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Peringatan cache scan Disk usage (jam)</div>
          <div class="desc">
            Hasil scan tersimpan (drive non-NTFS, jaringan, atau indeks tidak aktif) diberi
            peringatan kalau lebih tua dari ini. 24 = 1 hari
          </div>
        </div>
        <input
          class="numInput"
          type="number"
          min="1"
          max="8760"
          step="1"
          aria-label="Peringatan cache scan Disk usage (jam)"
          value={ui.diskUsageCacheWarnHours}
          onchange={(e) => {
            const el = e.currentTarget as HTMLInputElement;
            ui.diskUsageCacheWarnHours = sanitizeWarnHours(Math.round(Number(el.value)));
            el.value = String(ui.diskUsageCacheWarnHours);
          }}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Cache scan Disk usage</div>
          <div class="desc">
            Disimpan di profil Windows Anda saja (nama file bisa sensitif). {cacheSummary}
          </div>
        </div>
        <button class="privacyBtn" onclick={() => void clearScanCache()}>Hapus semua cache</button>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Terminal</div>
          <div class="desc">Used by Open Terminal Here</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="Terminal"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(e, terminalValues, ui.terminal, (v) => (ui.terminal = v as TerminalChoice))}
        >
          {#each TERMINAL_CHOICES as t (t.value)}
            <button
              role="radio"
              data-value={t.value}
              tabindex={ui.terminal === t.value ? 0 : -1}
              aria-checked={ui.terminal === t.value}
              class:sel={ui.terminal === t.value}
              onclick={() => (ui.terminal = t.value as TerminalChoice)}
            >
              {t.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit" data-cat="Tabs">
        <div class="lbl">
          <div class="name">Open new tabs next to current</div>
          <div class="desc">Instead of at the end of the tab strip</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.tabs.openNextToCurrent}
          aria-label="Open new tabs next to current"
          onchange={(e) =>
            (ui.tabs.openNextToCurrent = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Restore tabs on launch</div>
          <div class="desc">Reopen the last session's tabs per pane</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.tabs.restoreTabs}
          aria-label="Restore tabs on launch"
          onchange={(e) => (ui.tabs.restoreTabs = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Close tab with middle click</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.tabs.closeOnMiddleClick}
          aria-label="Close tab with middle click"
          onchange={(e) =>
            (ui.tabs.closeOnMiddleClick = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Tab width</div>
          <div class="desc">Fit shrinks to the label; fixed is uniform</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="Tab width"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(
              e,
              ['fit', 'fixed'],
              ui.tabs.tabWidth,
              (v) => (ui.tabs.tabWidth = v as TabWidth),
            )}
        >
          {#each [{ value: 'fit', label: 'Fit' }, { value: 'fixed', label: 'Fixed' }] as t (t.value)}
            <button
              role="radio"
              data-value={t.value}
              tabindex={ui.tabs.tabWidth === t.value ? 0 : -1}
              aria-checked={ui.tabs.tabWidth === t.value}
              class:sel={ui.tabs.tabWidth === t.value}
              onclick={() => (ui.tabs.tabWidth = t.value as TabWidth)}
            >
              {t.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit" data-cat="File Operations">
        <div class="lbl">
          <div class="name">Confirm before deleting</div>
          <div class="desc">Applies to Trash. Permanent deletion always asks for confirmation.</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          bind:checked={prefs.fileOperations.confirmBeforeDeleting}
          aria-label="Confirm before deleting"
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Verify after copy</div>
          <div class="desc">Compare the copy byte for byte before reporting success</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          bind:checked={prefs.fileOperations.verifyAfterCopy}
          aria-label="Verify after copy"
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Copy buffer (KiB)</div>
          <div class="desc">{COPY_BUFFER_MIN_KIB}–{COPY_BUFFER_MAX_KIB} KiB</div>
        </div>
        <input
          class="numInput"
          type="number"
          min={COPY_BUFFER_MIN_KIB}
          max={COPY_BUFFER_MAX_KIB}
          step="64"
          aria-label="Copy buffer (KiB)"
          bind:value={prefs.fileOperations.copyBufferKiB}
          onchange={(e) => {
            const value = Number((e.currentTarget as HTMLInputElement).value);
            prefs.fileOperations.copyBufferKiB = Math.min(
              COPY_BUFFER_MAX_KIB,
              Math.max(COPY_BUFFER_MIN_KIB, Number.isFinite(value) ? Math.round(value) : 1024),
            );
          }}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Thread copy</div>
          <div class="desc">
            Otomatis menyesuaikan jumlah thread dengan kecepatan disk; bisa diubah langsung di
            dialog progres
          </div>
        </div>
        <select
          class="fontSelect"
          aria-label="Thread copy"
          value={String(prefs.fileOperations.copyThreads)}
          onchange={(e) =>
            (prefs.fileOperations.copyThreads = Number(
              (e.currentTarget as HTMLSelectElement).value,
            ))}
        >
          {#each COPY_THREAD_CHOICES as n (n)}
            <option value={String(n)}>{n === 0 ? 'Otomatis' : String(n)}</option>
          {/each}
        </select>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Tulis aman (flush) untuk copy</div>
          <div class="desc">
            Tulis tiap file ke disk sebelum dipublikasikan. Lebih lambat; Move selalu menulis aman
            sebelum menghapus sumber
          </div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          bind:checked={prefs.fileOperations.copySafeWrite}
          aria-label="Tulis aman (flush) untuk copy"
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Prioritas copy</div>
          <div class="desc">Latar belakang mengalah ke aplikasi lain (CPU dan disk)</div>
        </div>
        <select
          class="fontSelect"
          aria-label="Prioritas copy"
          bind:value={prefs.fileOperations.copyPriority}
        >
          <option value="normal">Normal</option>
          <option value="background">Latar belakang</option>
        </select>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">When a file already exists</div>
          <div class="desc">Default choice offered by copy and move</div>
        </div>
        <select
          class="fontSelect"
          aria-label="When a file already exists"
          bind:value={prefs.fileOperations.conflictPolicy}
        >
          {#each conflictPolicies as policy (policy.value)}
            <option value={policy.value}>{policy.label}</option>
          {/each}
        </select>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Preserve timestamps</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          bind:checked={prefs.fileOperations.preserveTimestamps}
          aria-label="Preserve timestamps"
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Preserve permissions</div>
          <div class="desc">
            {permissionsSupported
              ? 'Copy file mode bits where the platform supports it'
              : 'Permission preservation is unsupported on this platform.'}
          </div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          bind:checked={prefs.fileOperations.preservePermissions}
          disabled={!permissionsSupported}
          aria-label="Preserve permissions"
        />
      </div>

      <div class="subHead" data-cat="Preview">
        <h3>Preview</h3>
        <p>What the preview panel renders</p>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Open preview panel on launch</div>
          <div class="desc">Show the Quick View panel when Ferrum starts</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.preview.openOnLaunch}
          aria-label="Open preview panel on launch"
          onchange={(e) =>
            (ui.preview.openOnLaunch = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Max text preview</div>
          <div class="desc">Larger files show metadata only</div>
        </div>
        <div
          class="segmented"
          role="radiogroup"
          aria-label="Max text preview"
          tabindex="-1"
          onkeydown={(e) =>
            radioKey(
              e,
              previewLimitValues,
              String(ui.preview.maxTextBytes),
              (v) => (ui.preview.maxTextBytes = Number(v)),
            )}
        >
          {#each PREVIEW_TEXT_LIMITS as n (n)}
            <button
              role="radio"
              data-value={String(n)}
              tabindex={ui.preview.maxTextBytes === n ? 0 : -1}
              aria-checked={ui.preview.maxTextBytes === n}
              class:sel={ui.preview.maxTextBytes === n}
              onclick={() => (ui.preview.maxTextBytes = n)}
            >
              {limitLabel(n)}
            </button>
          {/each}
        </div>
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Syntax highlighting</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.preview.syntaxHighlight}
          aria-label="Syntax highlighting"
          onchange={(e) =>
            (ui.preview.syntaxHighlight = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Autoplay video</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.preview.autoplayVideo}
          aria-label="Autoplay video"
          onchange={(e) =>
            (ui.preview.autoplayVideo = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>

      <div class="rowSplit">
        <div class="lbl">
          <div class="name">Read EXIF & media metadata</div>
        </div>
        <input
          class="switch"
          type="checkbox"
          role="switch"
          checked={ui.preview.readMetadata}
          aria-label="Read EXIF & media metadata"
          onchange={(e) =>
            (ui.preview.readMetadata = (e.currentTarget as HTMLInputElement).checked)}
        />
      </div>
      {#if isInTauri() && activity.available}
        <div class="rowSplit actBlock">
          <div class="lbl">
            <div class="name">Activity &amp; privacy</div>
            <div class="desc">Recent / Frequent history and what Ferrum remembers</div>
          </div>
          <div class="actCol">
            <ActivityPrivacyControls />
            <button
              class="privacyBtn"
              onclick={() => {
                onclose?.();
                ui.activityOpen = true;
              }}
            >
              Buka halaman Activity
            </button>
          </div>
        </div>
      {/if}
      <HotlistSettingsSection />
      <IndexerSettingsSection />
      <ArchiveSettingsSection />
      <AiSettingsSection />
    </div>
  </section>
</div>

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
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(960px, 94vw);
    max-height: min(86vh, 720px);
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: grid;
    grid-template-columns: 208px minmax(0, 1fr);
    overflow: hidden;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  .cats {
    background: var(--pane);
    border-right: 1px solid var(--line);
    padding: 14px 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .cats h2 {
    margin: 0 6px 6px;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .cats nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .cat {
    border: 0;
    text-align: left;
    padding: 6px 8px;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: pointer;
  }
  .cat:disabled {
    cursor: default;
  }
  .cat:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .cat:hover:not(:disabled) {
    background: var(--hover);
    color: var(--text);
  }
  .cat.sel {
    background: var(--accentSoft);
    color: var(--text);
    font-weight: 500;
  }
  .cat:disabled {
    color: var(--text3);
    opacity: 0.55;
  }
  .panel {
    display: flex;
    flex-direction: column;
    min-width: 0;
    max-height: min(86vh, 720px);
  }
  .pHead {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 16px 10px;
    border-bottom: 1px solid var(--line);
  }
  .pTitle h3 {
    margin: 0;
    font-size: calc(15px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .pTitle p {
    margin: 2px 0 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .icon {
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .icon:hover {
    background: var(--hover2);
    color: var(--text2);
  }
  .body {
    padding: 6px 16px 16px;
    overflow: auto;
    display: flex;
    flex-direction: column;
  }
  .rowSplit {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 10px 0;
    border-bottom: 1px solid var(--line);
  }
  .rowSplit:last-child {
    border-bottom: 0;
  }
  .lbl {
    min-width: 0;
    flex: 1 1 240px;
  }
  .name {
    font-size: calc(13px + var(--uiStep, 0px));
    color: var(--text);
  }
  .desc {
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
    margin-top: 2px;
  }
  .sectionSub {
    font-size: 11.5px;
    color: var(--text3);
    margin: -4px 0 8px;
  }
  /* FER-396: Preview sub-section header inside the shared body. */
  .subHead {
    margin: 18px 0 4px;
  }
  .subHead h3 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
    color: var(--text);
  }
  .subHead p {
    margin: 2px 0 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  /* FER-395: the Tabs section stacks several checks and one group. */
  section .check + .check,
  section .check + .segmented,
  .tabWidth {
    margin-top: 8px;
  }
  .segmented {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r);
    background: var(--hover);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .segmented button {
    border: 0;
    padding: 5px 10px;
    border-radius: calc(var(--r) - 1px);
    background: transparent;
    color: var(--text3);
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
    white-space: nowrap;
  }
  .segmented button:hover {
    color: var(--text2);
    background: var(--hover2);
  }
  .segmented button.sel {
    background: var(--elev);
    color: var(--text);
    box-shadow: var(--tabShadow);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 6px;
    max-width: 320px;
  }
  .chip {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 9px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover);
    color: var(--text2);
    font: inherit;
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .privacyBtn {
    margin-top: 8px;
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
  }
  .privacyBtn:hover:not(:disabled) {
    background: var(--hover);
  }
  .actBlock {
    align-items: flex-start;
  }
  .actCol {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    min-width: 0;
  }
  .chip:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .chip.sel {
    background: var(--accentSoft);
    color: var(--text);
    box-shadow: inset 0 0 0 1px var(--accentLine);
  }
  .chip .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    flex: none;
  }
  .chipLabel {
    white-space: nowrap;
  }
  .checkMark {
    color: var(--accentInk);
    font-weight: 600;
  }
  .numInput {
    width: 90px;
    padding: 5px 7px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--hover);
    color: var(--text);
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .fontSelect {
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
    background: var(--hover);
    border: 0;
    border-radius: var(--r);
    padding: 5px 8px;
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .switch {
    appearance: none;
    width: 34px;
    height: 20px;
    flex: none;
    border-radius: 10px;
    background: var(--hover2);
    box-shadow: inset 0 0 0 1px var(--line);
    position: relative;
    cursor: default;
    transition: background 120ms;
    margin: 0;
  }
  .switch::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--text2);
    transition:
      transform 120ms,
      background 120ms;
  }
  .switch:checked {
    background: var(--accentSoft);
    box-shadow: inset 0 0 0 1px var(--accentLine);
  }
  .switch:checked::after {
    transform: translateX(14px);
    background: var(--accent);
  }
  .saveError {
    margin: 10px 16px 0;
    padding: 8px 10px;
    border-radius: var(--r);
    background: var(--redSoft);
    color: var(--redInk);
    font-size: calc(12.5px + var(--uiStep, 0px));
    box-shadow: inset 0 0 0 1px color-mix(in oklch, var(--red) 35%, transparent);
  }
  @keyframes fmFade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  @keyframes fmUp {
    from {
      opacity: 0;
      transform: translate(-50%, calc(-50% + 8px));
    }
    to {
      opacity: 1;
      transform: translate(-50%, -50%);
    }
  }
</style>
