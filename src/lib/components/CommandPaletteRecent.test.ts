import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';

const ipc = vi.hoisted(() => ({
  activityFavorites: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(async () => []),
  activitySearch: vi.fn(async () => []),
  activityState: vi.fn(async () => null),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: vi.fn(async () => true),
}));
vi.mock('../ipc/activity', () => ipc);

import CommandPalette from './CommandPalette.svelte';
import App from '../../App.svelte';
import { ui } from '../uiState.svelte';
import { activity } from '../stores/activityStore.svelte';
import { getAction, runAction } from '../actions';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane } from '../stores/panesRegistry.svelte';
import { recentCommands } from '../stores/recentCommands.svelte';
import { dialog, modalDepth, resetModalDepth } from '../dialogKit';

beforeEach(() => {
  // jsdom has no matchMedia (the theme actions read it).
  window.matchMedia = vi.fn(
    () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) as never,
  );
  recentCommands.clear();
  activity.favorites = [{ path: 'C:\\Pin', label: 'Pin', kind: 'folder' }] as never;
  activity.recent = [];
  activity.frequent = [];
  ui.paletteOpen = false;
  resetModalDepth();
});
afterEach(() => {
  ui.paletteOpen = false;
  resetModalDepth();
});

async function openPalette() {
  render(CommandPalette);
  ui.paletteOpen = true;
  await tick();
  return screen.getByRole('dialog', { name: 'Command palette' });
}
const groupLabels = (root: HTMLElement) =>
  Array.from(root.querySelectorAll('[role="group"]')).map((g) => g.getAttribute('aria-label'));
const optionLabels = (root: HTMLElement, group: string) =>
  Array.from(
    root.querySelector(`[role="group"][aria-label="${group}"]`)?.querySelectorAll('.label') ?? [],
  ).map((el) => el.textContent);
const selectedIndex = () =>
  screen.getAllByRole('option').findIndex((o) => o.getAttribute('aria-selected') === 'true');

describe('Recent commands section (FER-580)', () => {
  it('is hidden while nothing was run', async () => {
    const pal = await openPalette();
    expect(groupLabels(pal)).not.toContain('Recent commands');
  });

  it('is the first group, above Favorites, newest first', async () => {
    runAction(getAction('show.tree')!);
    runAction(getAction('show.vertical')!);
    const pal = await openPalette();
    const labels = groupLabels(pal);
    expect(labels[0]).toBe('Recent commands');
    expect(labels.indexOf('Favorites')).toBeGreaterThan(0);
    expect(optionLabels(pal, 'Recent commands')).toEqual(['Vertical Arrangement', 'Tree']);
  });

  it('shows at most 3, unique', async () => {
    for (const id of ['show.tree', 'show.vertical', 'theme.toggle', 'show.tree', 'view.hidden']) {
      runAction(getAction(id)!);
    }
    const pal = await openPalette();
    expect(optionLabels(pal, 'Recent commands')).toHaveLength(3);
    expect(recentCommands.ids).toEqual(['view.hidden', 'show.tree', 'theme.toggle']);
  });

  it('lists only commands that are available right now', async () => {
    runAction(getAction('show.tree')!);
    runAction(getAction('show.vertical')!);
    const vertical = getAction('show.vertical')!;
    const prev = vertical.available;
    vertical.available = () => false;
    try {
      const pal = await openPalette();
      expect(optionLabels(pal, 'Recent commands')).toEqual(['Tree']);
    } finally {
      vertical.available = prev;
    }
  });

  it('is not shown once the user types', async () => {
    runAction(getAction('show.tree')!);
    const pal = await openPalette();
    await fireEvent.input(screen.getByRole('combobox'), { target: { value: 'theme' } });
    expect(groupLabels(pal)).not.toContain('Recent commands');
  });

  it('is selected first; arrows walk into the next group; Enter runs it again', async () => {
    runAction(getAction('show.tree')!);
    runAction(getAction('show.vertical')!);
    const pal = await openPalette();
    expect(selectedIndex()).toBe(0);
    await fireEvent.keyDown(pal, { key: 'ArrowDown' });
    expect(selectedIndex()).toBe(1);
    await fireEvent.keyDown(pal, { key: 'ArrowDown' });
    expect(selectedIndex()).toBe(2); // first row after the Recent commands block
    await fireEvent.keyDown(pal, { key: 'ArrowUp' });
    await fireEvent.keyDown(pal, { key: 'ArrowUp' });
    const vertical = ui.verticalLayout;
    await fireEvent.keyDown(pal, { key: 'Enter' });
    expect(ui.verticalLayout).toBe(!vertical); // the newest recent command ran again
    expect(ui.paletteOpen).toBe(false);
    expect(recentCommands.ids).toEqual(['show.vertical', 'show.tree']);
  });

  it('opening the palette (Ctrl+K action) is never recorded', () => {
    runAction(getAction('palette.open')!);
    expect(recentCommands.ids).toEqual([]);
  });
});

describe('S3: the palette over another modal', () => {
  it('palette.open is unavailable while a modal is open, so Ctrl+K cannot reach pane actions', () => {
    const host = document.createElement('div');
    host.tabIndex = -1;
    document.body.append(host);
    const act = dialog(host);
    expect(modalDepth()).toBe(1);
    const open = getAction('palette.open')!;
    expect(runAction(open)).toBe(false);
    expect(ui.paletteOpen).toBe(false);
    act.destroy();
    expect(runAction(open)).toBe(true);
    expect(ui.paletteOpen).toBe(true);
    host.remove();
  });
});

describe('palette keyboard (B3)', () => {
  it('Tab stays inside the palette and focus returns to the opener on Esc', async () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    const pal = await openPalette();
    expect(document.activeElement).toBe(screen.getByRole('combobox'));
    await fireEvent.input(screen.getByRole('combobox'), { target: { value: 'th' } });
    const chips = Array.from(pal.querySelectorAll<HTMLElement>('.modeChip'));
    expect(chips.length).toBe(4);
    // The last tabbable (the selected chip) wraps to the first (the input).
    chips[0].focus();
    await fireEvent.keyDown(chips[0], { key: 'Tab' });
    expect(document.activeElement).toBe(screen.getByRole('combobox'));
    await fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Escape' });
    expect(ui.paletteOpen).toBe(false);
    await tick();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it('arrows move between filter chips; Enter on a chip does not run a result', async () => {
    const pal = await openPalette();
    await fireEvent.input(screen.getByRole('combobox'), { target: { value: 'theme' } });
    const chips = Array.from(pal.querySelectorAll<HTMLElement>('.modeChip'));
    chips[0].focus();
    await fireEvent.keyDown(chips[0], { key: 'ArrowRight' });
    expect(document.activeElement).toBe(chips[1]);
    await fireEvent.keyDown(chips[1], { key: 'End' });
    expect(document.activeElement).toBe(chips[3]);
    const before = ui.theme;
    await fireEvent.keyDown(chips[3], { key: 'Enter' });
    expect(ui.theme).toBe(before);
    expect(recentCommands.ids).toEqual([]);
    expect(ui.paletteOpen).toBe(true);
  });

  it('PageDown / PageUp jump, End / Home jump with an empty input', async () => {
    const pal = await openPalette();
    const count = screen.getAllByRole('option').length;
    await fireEvent.keyDown(pal, { key: 'PageDown' });
    expect(selectedIndex()).toBe(Math.min(6, count - 1));
    await fireEvent.keyDown(pal, { key: 'End' });
    expect(selectedIndex()).toBe(count - 1);
    await fireEvent.keyDown(pal, { key: 'Home' });
    expect(selectedIndex()).toBe(0);
    await fireEvent.keyDown(pal, { key: 'PageUp' });
    expect(selectedIndex()).toBe(0);
  });

  it('Home / End keep the caret in a filled input; Ctrl+End jumps the list', async () => {
    const pal = await openPalette();
    await fireEvent.input(screen.getByRole('combobox'), { target: { value: 'the' } });
    const count = screen.getAllByRole('option').length;
    expect(count).toBeGreaterThan(1);
    const ev = new KeyboardEvent('keydown', { key: 'End', bubbles: true, cancelable: true });
    screen.getByRole('combobox').dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
    expect(selectedIndex()).toBe(0);
    await fireEvent.keyDown(pal, { key: 'End', ctrlKey: true });
    expect(selectedIndex()).toBe(count - 1);
  });
});

describe('recording routes (FER-580)', () => {
  it('a keyboard shortcut handled by App is recorded, with no path or argument', async () => {
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    await fireEvent.keyDown(window, { key: 'h', ctrlKey: true });
    expect(recentCommands.ids).toEqual(['view.hidden']);
    expect(localStorage.getItem('ferrum.recentCommands.v1')).toBe('["view.hidden"]');
  });
});

describe('rework R1', () => {
  it('a stale (now disabled) command keeps the palette open with focus inside (S2)', async () => {
    runAction(getAction('show.tree')!);
    const pal = await openPalette();
    const a = getAction('show.tree')!;
    const prev = a.available;
    a.available = () => false; // became unavailable after the list was drawn
    try {
      await fireEvent.keyDown(pal, { key: 'Enter' });
      expect(ui.paletteOpen).toBe(true);
      expect(pal.contains(document.activeElement)).toBe(true);
    } finally {
      a.available = prev;
    }
  });

  it('file.quit is never recorded (N4)', () => {
    recentCommands.record('file.quit');
    expect(recentCommands.ids).toEqual([]);
  });

  it('opened from outside a pane, focus returns to the active file list (N5)', async () => {
    const pane = document.createElement('section');
    pane.className = 'pane';
    pane.dataset.pane = String(ui.activePane);
    const list = document.createElement('div');
    list.tabIndex = 0;
    list.setAttribute('data-pane-list', '');
    pane.append(list);
    const titlebar = document.createElement('button');
    document.body.append(pane, titlebar);
    titlebar.focus();
    const pal = await openPalette();
    await fireEvent.keyDown(pal, { key: 'Escape' });
    await tick();
    expect(document.activeElement).toBe(list);
    pane.remove();
    titlebar.remove();
  });

  it('re-running Delete from Recent only opens the confirmation dialog (N6)', async () => {
    const del = getAction('file.delete')!;
    const unregister = registerPane(0, {
      path: 'C:\\w',
      selectedPaths: () => ['C:\\w\\a.txt'],
      cursorEntry: () => ({ name: 'a.txt', path: 'C:\\w\\a.txt', isDir: false }),
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
    });
    const spy = vi.spyOn(dialogs, 'beginDelete').mockImplementation(() => undefined as never);
    recentCommands.record('file.delete');
    const pal = await openPalette();
    expect(optionLabels(pal, 'Recent commands').length).toBe(1);
    expect(del).toBeDefined();
    await fireEvent.keyDown(pal, { key: 'Enter' });
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith(false); // current selection, goes through the dialog
    spy.mockRestore();
    unregister();
  });
});
