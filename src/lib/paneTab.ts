/**
 * FER-489: Tab / Shift+Tab in the main window only move between the two panes' file lists
 * (Total Commander). Modals, popups, the AI chat and the search panel keep native Tab.
 */
import { ui, type PaneId } from './uiState.svelte';
import { paneById } from './stores/panesRegistry.svelte';
import { contextMenu } from './stores/contextMenuStore.svelte';
import { historyMenu } from './stores/historyMenuStore.svelte';
import { hotlist } from './stores/hotlistStore.svelte';

/** Containers whose own keyboard handling (focus traps, menus, panels) must keep Tab. */
const OWN_TAB_SCOPES =
  '[role="dialog"],[role="menu"],[role="listbox"],[aria-modal="true"],aside.chat,[role="search"]';

const other = (id: PaneId): PaneId => (id === 0 ? 1 : 0);

function paneIdOf(el: Element | null): PaneId | undefined {
  const raw = el?.closest('section.pane')?.getAttribute('data-pane');
  return raw === '0' ? 0 : raw === '1' ? 1 : undefined;
}

/**
 * Returns true when the key was handled (caller already must not run other shortcuts).
 * Call it only for an unmodified Tab / Shift+Tab that no component has handled.
 */
export function handlePaneTab(e: KeyboardEvent): boolean {
  if (e.ctrlKey || e.metaKey || e.altKey || e.key !== 'Tab') return false;
  if (ui.settingsOpen || ui.activityOpen) return false;
  if (contextMenu.isOpen || historyMenu.isOpen || hotlist.menuOpen) return false;
  const t = e.target instanceof Element ? e.target : null;
  if (t?.closest(OWN_TAB_SCOPES)) return false;

  const isText =
    t !== null &&
    (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || (t as HTMLElement).isContentEditable);
  const fromPane = paneIdOf(t);
  let dest: PaneId;
  if (isText) {
    // Text fields inside a pane (address bar, filter) hand focus back to their own list.
    if (fromPane === undefined) return false;
    dest = fromPane;
  } else if (t?.closest('[data-pane-list]') && fromPane !== undefined) {
    dest = other(fromPane);
  } else if (!t || t === document.body || t === document.documentElement) {
    dest = ui.activePane;
  } else {
    dest = other(ui.activePane);
  }
  const target = paneById(dest);
  if (!target?.focusList || !paneById(other(dest))?.focusList) return false; // single pane
  ui.activate(dest);
  target.focusList();
  return true;
}
