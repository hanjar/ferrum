<script lang="ts">
  import { ui } from '../uiState.svelte';
  import {
    getAction,
    isEnabled,
    labelOf,
    MENU_SEPARATOR,
    menuGroups,
    menuSubmenus,
    runAction,
    shortcutFor,
    shortcutLabel,
  } from '../actions';
  import type { Action } from '../actions';
  import { dialogs } from '../stores/dialogStore.svelte';
  import { mnemonicKey, modalDepth } from '../dialogKit';
  import Mnemonic from './Mnemonic.svelte';

  /** FER-577: Alt+letter opens a menu. D and B are Alt shortcuts of the app (Alt+D address bar,
   *  Alt+B transfers), so none of the groups uses them. */
  const GROUP_MNEMONIC: Record<string, string> = {
    Files: '&Files',
    Mark: '&Mark',
    Commands: '&Commands',
    View: '&View',
    Sort: '&Sort',
    Show: 'Sho&w',
  };

  // FER-495: `compact` collapses the six groups into one hamburger button whose panel lists them.
  let { compact = false }: { compact?: boolean } = $props();

  const id = $props.id();
  let root: HTMLElement;
  let burgerOpen = $state(false);
  let restoreTo: HTMLElement | null = null;
  let menu = $state<HTMLElement>();
  let openGroup = $state<number | null>(null);
  let triggerIndex = $state(0);
  let awaitingDialog = $state(false);
  // FER-431: at most one submenu open; it is positioned `fixed` because .menu clips overflow.
  let openSub = $state<string | null>(null);
  let subPos = $state({ top: 0, left: 0 });

  $effect(() => {
    if (!awaitingDialog || !dialogs.current) return;
    queueMicrotask(() => {
      if (dialogs.current) {
        document
          .querySelector<HTMLElement>(
            '[role="dialog"] input:not([type="checkbox"]):not([type="radio"]):not([disabled]), [role="dialog"] button:not([disabled])',
          )
          ?.focus();
      }
      awaitingDialog = false;
    });
  });

  function opener(index: number): HTMLButtonElement | null {
    return root?.querySelector<HTMLButtonElement>(`[data-menu-trigger="${index}"]`) ?? null;
  }

  function subPanel(): HTMLElement | null {
    return menu?.querySelector<HTMLElement>('[data-submenu-panel]') ?? null;
  }

  function subTrigger(key: string): HTMLButtonElement | null {
    return (
      Array.from(menu?.querySelectorAll<HTMLButtonElement>('[data-submenu-key]') ?? []).find(
        (node) => node.dataset.submenuKey === key,
      ) ?? null
    );
  }

  /** Enabled items of one menu level (the submenu panel when focus is inside it, else the top menu). */
  function enabledItems(scope: HTMLElement | undefined = menu): HTMLButtonElement[] {
    return scope
      ? Array.from(
          scope.querySelectorAll<HTMLButtonElement>('[role="menuitem"][aria-disabled="false"]'),
        ).filter((node) => node.closest('[role="menu"]') === scope)
      : [];
  }

  function focusInSub(): boolean {
    const panel = subPanel();
    return !!panel && panel.contains(document.activeElement);
  }

  function openSubmenu(key: string, focusFirst: boolean) {
    const trigger = subTrigger(key);
    const entry = menuSubmenus[key];
    if (!trigger || !entry) return;
    const rect = trigger.getBoundingClientRect();
    const width = 240;
    const height = entry.ids.length * 28 + 8;
    const left = rect.right + width > window.innerWidth ? rect.left - width + 2 : rect.right - 2;
    subPos = { top: Math.max(0, Math.min(rect.top - 4, window.innerHeight - height - 4)), left };
    openSub = key;
    if (focusFirst) {
      queueMicrotask(() => {
        if (openSub === key) enabledItems(subPanel() ?? undefined)[0]?.focus();
      });
    }
  }

  function closeSubmenu(restoreFocus = true) {
    const key = openSub;
    openSub = null;
    if (restoreFocus && key) subTrigger(key)?.focus();
  }

  function focusEdge(last = false) {
    const items = enabledItems();
    if (items.length) (last ? items.at(-1) : items[0])?.focus();
    else if (openGroup !== null) opener(openGroup)?.focus();
  }

  function open(index: number, last = false) {
    if (dialogs.current) return;
    triggerIndex = index;
    openGroup = index;
    openSub = null;
    queueMicrotask(() => {
      if (openGroup === index) focusEdge(last);
    });
  }

  function burger(): HTMLButtonElement | null {
    return root?.querySelector<HTMLButtonElement>('[data-menu-burger]') ?? null;
  }

  function closeBurger(restoreFocus = true) {
    openGroup = null;
    openSub = null;
    if (!burgerOpen) return;
    burgerOpen = false;
    if (restoreFocus) queueMicrotask(() => burger()?.focus());
  }

  function openBurger() {
    if (dialogs.current) return;
    burgerOpen = true;
    triggerIndex = 0;
    queueMicrotask(() => opener(0)?.focus());
  }

  /** F10 / lone Alt: focus the menu (the burger when collapsed) without opening it. */
  function focusMenu() {
    if (dialogs.current || ui.paletteOpen || modalDepth() > 0) return;
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || !root.contains(active))
      restoreTo = active as HTMLElement;
    (compact ? burger() : opener(triggerIndex))?.focus();
  }

  function leaveMenu() {
    const target = restoreTo;
    restoreTo = null;
    if (target?.isConnected) target.focus();
    else (document.activeElement as HTMLElement | null)?.blur();
  }

  let altAlone = false;
  /** FER-577: Alt+F / M / C / V / S / W open that menu (never while a modal is open). */
  function openByMnemonic(event: KeyboardEvent): boolean {
    if (!event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return false;
    if (event.key.length !== 1 || dialogs.current || ui.paletteOpen || modalDepth() > 0)
      return false;
    const key = event.key.toLowerCase();
    const index = menuGroups.findIndex((g) => mnemonicKey(GROUP_MNEMONIC[g.label] ?? '') === key);
    if (index < 0) return false;
    event.preventDefault();
    event.stopPropagation();
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || !root.contains(active))
      restoreTo = active as HTMLElement;
    if (compact && !burgerOpen) openBurger();
    open(index);
    return true;
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (modalDepth() > 0) {
      altAlone = false;
      return;
    }
    if (event.key === 'Alt') {
      altAlone = !event.repeat || altAlone;
      return;
    }
    altAlone = false;
    if (!event.defaultPrevented && openByMnemonic(event)) return;
    if (
      event.key === 'F10' &&
      !event.altKey &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.shiftKey &&
      !event.defaultPrevented
    ) {
      event.preventDefault();
      if (root.contains(document.activeElement)) {
        closeBurger(false);
        leaveMenu();
      } else focusMenu();
    }
  }

  function onWindowKeyup(event: KeyboardEvent) {
    if (event.key !== 'Alt') return;
    const alone = altAlone;
    altAlone = false;
    if (!alone || event.ctrlKey || event.shiftKey || event.metaKey) return;
    event.preventDefault();
    focusMenu();
  }

  function onBurgerKeydown(event: KeyboardEvent) {
    if (dialogs.current) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (['ArrowDown', 'ArrowUp', 'ArrowRight', 'Enter', ' ', 'Escape'].includes(event.key)) {
      event.stopPropagation();
    }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (burgerOpen) closeBurger();
      else openBurger();
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp' || event.key === 'ArrowRight') {
      event.preventDefault();
      if (!burgerOpen) openBurger();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      if (burgerOpen) closeBurger();
      else leaveMenu();
    } else if (event.key === 'Tab' && burgerOpen) closeBurger(false);
  }

  function close(restoreFocus = true) {
    if (openGroup === null) return;
    const previous = openGroup;
    openGroup = null;
    openSub = null;
    if (restoreFocus) opener(previous)?.focus();
  }

  function activate(action: Action) {
    if (!isEnabled(action)) return;
    close();
    if (burgerOpen) closeBurger();
    awaitingDialog = true;
    runAction(action);
    if (!dialogs.current && action.id !== 'file.copy' && action.id !== 'file.move')
      awaitingDialog = false;
  }

  function onTriggerKeydown(event: KeyboardEvent, index: number) {
    if (dialogs.current) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (openByMnemonic(event)) return;
    if (
      openGroup !== null ||
      ['ArrowRight', 'ArrowLeft', 'ArrowDown', 'ArrowUp', 'Enter', ' ', 'Escape'].includes(
        event.key,
      )
    ) {
      event.stopPropagation();
    }
    if (compact) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        const step = event.key === 'ArrowDown' ? 1 : -1;
        const next = (index + step + menuGroups.length) % menuGroups.length;
        if (openGroup === null) {
          triggerIndex = next;
          opener(next)?.focus();
        } else open(next);
      } else if (event.key === 'ArrowRight') {
        event.preventDefault();
        open(index);
      } else if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        if (openGroup === index) close();
        else open(index);
      } else if (event.key === 'ArrowLeft' || event.key === 'Escape') {
        event.preventDefault();
        if (openGroup !== null) close();
        else closeBurger();
      } else if (event.key === 'Tab') closeBurger(false);
      return;
    }
    if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
      event.preventDefault();
      const direction = event.key === 'ArrowRight' ? 1 : -1;
      const next = (index + direction + menuGroups.length) % menuGroups.length;
      if (openGroup === null) {
        triggerIndex = next;
        opener(next)?.focus();
      } else open(next);
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      open(index, event.key === 'ArrowUp');
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (openGroup === index) close();
      else open(index);
    } else if (event.key === 'Escape' && openGroup !== null) {
      event.preventDefault();
      close();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      leaveMenu();
    } else if (event.key === 'Tab' && openGroup !== null) {
      close(false);
    }
  }

  function onMenuKeydown(event: KeyboardEvent) {
    if (openByMnemonic(event)) return; // Alt+letter switches menus while one is open
    event.stopPropagation();
    if (openGroup === null) return;
    const focused = document.activeElement as HTMLButtonElement;
    const subKey = focused?.dataset?.submenuKey;
    const inSub = focusInSub();
    if (event.key === 'Escape') {
      event.preventDefault();
      if (inSub) closeSubmenu();
      else close();
    } else if (event.key === 'ArrowLeft' && inSub) {
      event.preventDefault();
      closeSubmenu();
    } else if (event.key === 'ArrowRight' && (inSub || subKey)) {
      event.preventDefault();
      if (subKey) openSubmenu(subKey, true);
    } else if (compact && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
      event.preventDefault();
      if (event.key === 'ArrowLeft') close();
    } else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      event.preventDefault();
      const direction = event.key === 'ArrowRight' ? 1 : -1;
      open((openGroup + direction + menuGroups.length) % menuGroups.length);
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const items = enabledItems(inSub ? (subPanel() ?? undefined) : menu);
      if (!items.length) return;
      const current = items.indexOf(document.activeElement as HTMLButtonElement);
      const direction = event.key === 'ArrowDown' ? 1 : -1;
      items[(current + direction + items.length) % items.length].focus();
    } else if (event.key === 'Home' || event.key === 'End') {
      event.preventDefault();
      if (inSub) {
        const items = enabledItems(subPanel() ?? undefined);
        (event.key === 'End' ? items.at(-1) : items[0])?.focus();
      } else focusEdge(event.key === 'End');
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      const target = event.target;
      if (subKey && target === focused) {
        if (openSub === subKey) closeSubmenu();
        else openSubmenu(subKey, true);
        return;
      }
      const action = getAction(focused.dataset.actionId ?? '');
      if (target === focused && action && menu?.contains(focused)) activate(action);
    } else if (event.key === 'Tab') {
      closeBurger(false);
      close(false);
    }
  }

  function onMenuFocusIn(event: FocusEvent) {
    if (!openSub) return;
    const target = event.target as HTMLElement;
    const panel = subPanel();
    if (panel?.contains(target) || target.dataset.submenuKey === openSub) return;
    openSub = null;
  }

  function onWindowPointerDown(event: PointerEvent) {
    altAlone = false;
    if (openGroup === null && !burgerOpen) return;
    if (event.composedPath().includes(root)) return;
    const target = event.target;
    const focusMoves =
      target instanceof Element &&
      !!target.closest('button, input, select, textarea, a[href], [tabindex]:not([tabindex="-1"])');
    if (burgerOpen) closeBurger(false);
    else close(!focusMoves);
  }

  // Leaving compact mode (window grew) must not leave a stale panel behind.
  $effect(() => {
    if (!compact) {
      burgerOpen = false;
    }
  });
</script>

<svelte:window
  onpointerdown={onWindowPointerDown}
  onkeydown={onWindowKeydown}
  onkeyup={onWindowKeyup}
/>

{#snippet row(action: Action)}
  {@const shortcut = shortcutFor(action)}
  <button
    class="item"
    type="button"
    role="menuitem"
    tabindex="-1"
    data-action-id={action.id}
    aria-disabled={!isEnabled(action)}
    title={!isEnabled(action)
      ? (action.disabledReason ?? 'Unavailable in current context')
      : undefined}
    aria-describedby={!isEnabled(action) ? `${id}-reason-${action.id}` : undefined}
    onclick={() => activate(action)}
  >
    <span class="label">{labelOf(action)}</span>
    {#if shortcut}<span class="shortcut" aria-hidden="true">{shortcutLabel(shortcut)}</span>{/if}
  </button>
  {#if !isEnabled(action)}<span id="{id}-reason-{action.id}" hidden
      >{action.disabledReason ?? 'Unavailable in current context'}</span
    >{/if}
{/snippet}

{#snippet groups()}
  {#each menuGroups as group, index (group.label)}
    <div class="group" role="none">
      <button
        id="{id}-trigger-{index}"
        data-menu-trigger={index}
        class:active={openGroup === index}
        type="button"
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={openGroup === index}
        aria-controls="{id}-menu-{index}"
        tabindex={index === triggerIndex ? 0 : -1}
        onfocus={() => (triggerIndex = index)}
        onclick={() => (openGroup === index ? close() : open(index))}
        onkeydown={(event) => onTriggerKeydown(event, index)}
        ><Mnemonic label={GROUP_MNEMONIC[group.label] ?? group.label} /></button
      >
      {#if openGroup === index}
        <div
          id="{id}-menu-{index}"
          class="menu"
          role="menu"
          tabindex="-1"
          aria-labelledby="{id}-trigger-{index}"
          bind:this={menu}
          onkeydown={onMenuKeydown}
          onfocusin={onMenuFocusIn}
        >
          {#each group.ids as actionId, position (`${position}:${actionId}`)}
            {@const action = getAction(actionId)}
            {#if actionId === MENU_SEPARATOR}
              <div class="sep" role="separator"></div>
            {:else if menuSubmenus[actionId]}
              {@const sub = menuSubmenus[actionId]}
              <button
                class="item"
                class:open={openSub === actionId}
                type="button"
                role="menuitem"
                tabindex="-1"
                aria-haspopup="menu"
                aria-expanded={openSub === actionId}
                aria-disabled="false"
                data-submenu-key={actionId}
                onclick={() =>
                  openSub === actionId ? closeSubmenu(false) : openSubmenu(actionId, false)}
                onpointerenter={() => openSubmenu(actionId, false)}
              >
                <span class="label">{sub.label}</span>
                <span class="chev" aria-hidden="true">&#9656;</span>
              </button>
              {#if openSub === actionId}
                <div
                  class="menu submenu"
                  role="menu"
                  tabindex="-1"
                  aria-label={sub.label}
                  data-submenu-panel
                  style="top: {subPos.top}px; left: {subPos.left}px"
                >
                  {#each sub.ids as subId (subId)}
                    {@const subAction = getAction(subId)}
                    {#if subAction}{@render row(subAction)}{/if}
                  {/each}
                </div>
              {/if}
            {:else if action}
              {@render row(action)}
            {/if}
          {/each}
        </div>
      {/if}
    </div>
  {/each}
{/snippet}

<div
  class="menubar"
  class:compact
  role="menubar"
  aria-label="Application menu"
  data-tauri-drag-region
  bind:this={root}
>
  {#if compact}
    <button
      class="burger"
      class:active={burgerOpen}
      type="button"
      role="menuitem"
      aria-label="Menu"
      aria-haspopup="menu"
      aria-expanded={burgerOpen}
      data-menu-burger
      onclick={() => (burgerOpen ? closeBurger() : openBurger())}
      onkeydown={onBurgerKeydown}>&#9776;</button
    >
    {#if burgerOpen}
      <div class="burgerPanel" role="group" aria-label="Menu groups">
        {@render groups()}
      </div>
    {/if}
  {:else}
    {@render groups()}
  {/if}
</div>

<style>
  .menubar {
    display: flex;
    align-items: center;
    gap: 2px;
    flex: none;
    padding: 0 4px;
    position: relative;
    min-width: 0;
    color: var(--text);
  }
  .burger {
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: 16px;
    line-height: 1;
    cursor: default;
  }
  .burger:hover,
  .burger.active {
    background: var(--hover2);
    color: var(--text);
  }
  .burger:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .burgerPanel {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 52;
    min-width: 150px;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--elev);
    box-shadow: var(--shadow);
  }
  .compact .group > button {
    width: 100%;
    text-align: left;
  }
  .compact .menu,
  .compact .group:last-child .menu {
    top: -5px;
    left: 100%;
    right: auto;
  }
  .group {
    position: relative;
  }
  .group > button {
    min-height: 28px;
    padding: 2px 10px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  .group > button:hover,
  .group > button.active {
    background: var(--hover2);
    color: var(--text);
  }
  .group > button:focus-visible,
  .item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .menu {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 52;
    min-width: 220px;
    max-height: min(70vh, 600px);
    overflow-y: auto;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--elev);
    box-shadow: var(--shadow);
  }
  .submenu {
    position: fixed;
    right: auto;
    z-index: 53;
    min-width: 200px;
    max-height: none;
    overflow: visible;
  }
  .item.open {
    background: var(--hover2);
  }
  .chev {
    color: var(--text3);
    font-size: 10px;
  }
  .group:last-child .menu {
    right: 0;
    left: auto;
  }
  .item {
    width: 100%;
    min-height: 28px;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 3px 8px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
    text-align: left;
    cursor: default;
  }
  .item:hover,
  .item:focus-visible {
    background: var(--hover2);
  }
  .item[aria-disabled='true'] {
    color: var(--text3);
    cursor: not-allowed;
  }
  .item[aria-disabled='true']:hover {
    background: transparent;
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
  .label {
    flex: 1;
    white-space: nowrap;
  }
  .shortcut {
    color: var(--text3);
    font-family: 'Geist Mono', monospace;
    font-size: calc(10.5px + var(--uiStep, 0px));
    white-space: nowrap;
  }
</style>
