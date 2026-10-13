/**
 * F2-16 — SearchPanel component + Alt+F7 registry integration in browser
 * (mock) mode: the panel opens, validation errors surface honestly, results
 * from the store render and navigate the pane.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import SearchPanel from './SearchPanel.svelte';
import App from '../../App.svelte';
import { searchStore } from '../stores/searchStore.svelte';
import { getAction, runAction } from '../actions';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { branch } from '../stores/branchStore.svelte';
import { ui } from '../uiState.svelte';
import type { FileEntry } from '../ipc/types';

function fakePane(path = 'C:\\dir'): PaneApi & { opened: string[]; revealed: string[] } {
  const state = { opened: [] as string[], revealed: [] as string[] };
  return {
    path,
    opened: state.opened,
    revealed: state.revealed,
    revealPath: (p: string) => {
      state.revealed.push(p);
    },
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    openPath: (p: string) => {
      state.opened.push(p);
    },
  };
}

let pane: ReturnType<typeof fakePane>;

beforeEach(() => {
  searchStore.reset();
  pane = fakePane();
  registerPane(0, pane);
  registerPane(1, fakePane());
});

afterEach(() => {
  cleanup();
  searchStore.reset();
});

describe('SearchPanel', () => {
  it('renders only while open and exposes the filter form', async () => {
    const r = render(SearchPanel);
    expect(r.container.querySelector('aside')).toBeNull();
    searchStore.togglePanel();
    await tick();
    expect(r.container.querySelector('aside[aria-label="Search"]')).toBeTruthy();
    expect(r.container.querySelector('input[aria-label="Search pattern"]')).toBeTruthy();
    expect(r.container.querySelector('input[aria-label="Content text"]')).toBeTruthy();
  });

  it('Alt+F7 toggles the panel through the action registry', () => {
    const action = getAction('commands.search')!;
    expect(action.shortcut).toEqual({ key: 'F7', alt: true });
    expect(runAction(action)).toBe(true);
    expect(searchStore.panelOpen).toBe(true);
    runAction(getAction('commands.search')!);
    expect(searchStore.panelOpen).toBe(false);
  });

  it('start without a pattern explains what is missing; browser mode fails honestly', async () => {
    render(SearchPanel);
    searchStore.togglePanel();
    await tick();
    searchStore.root = '';
    await searchStore.start();
    expect(searchStore.errorText).toMatch(/name pattern or content/i);
    expect(searchStore.running).toBe(false);
    // With a pattern the root falls back to the active pane path, so the
    // search legitimately starts — and browser dev reports the honest
    // "requires the desktop app" error instead of pretending to search.
    searchStore.options.pattern = '*.txt';
    searchStore.root = '';
    await searchStore.start();
    expect(searchStore.errorText).toMatch(/desktop app/i);
    expect(searchStore.running).toBe(false);
  });

  it('renders progressive results and navigates the pane on click', async () => {
    render(SearchPanel);
    searchStore.togglePanel();
    await tick();
    searchStore.testInFlight('search-x');
    searchStore.testEvent({
      id: 'search-x',
      entries: [
        {
          name: 'found.txt',
          path: 'C:\\deep\\found.txt',
          isDir: false,
          isSymlink: false,
          size: 12,
          modifiedMs: 1,
          hidden: false,
        },
      ],
      done: false,
    });
    await tick();
    const btn = Array.from(document.querySelectorAll('button.result')).find((b) =>
      b.textContent?.includes('found.txt'),
    );
    expect(btn).toBeTruthy();
    fireEvent.click(btn as HTMLElement);
    expect(pane.opened).toEqual(['C:\\deep']);
  });
});

describe('SearchPanel inside the app shell', () => {
  it('Alt+F7 in the app shows the panel next to the panes', async () => {
    const r = render(App);
    await vi_waitForRows();
    fireEvent.keyDown(window, { key: 'F7', altKey: true });
    await tick();
    expect(r.container.querySelector('aside[aria-label="Search"]')).toBeTruthy();
    searchStore.panelOpen = false; // cleanup for later suites
  });

  async function vi_waitForRows() {
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
  }
});

const mk = (name: string, dir = 'C:\\deep'): FileEntry => ({
  name,
  path: `${dir}\\${name}`,
  isDir: false,
  isSymlink: false,
  size: 5,
  modifiedMs: 1,
  hidden: false,
});

async function openWithResults(names: string[]) {
  const r = render(SearchPanel);
  searchStore.togglePanel();
  await tick();
  searchStore.testInFlight('s1');
  searchStore.testEvent({ id: 's1', entries: names.map((n) => mk(n)), done: true });
  await tick();
  return r;
}

describe('FER-99 result list: keyboard, copy, feed', () => {
  it('Up/Down/Home/End move the cursor and Enter reveals in the active pane', async () => {
    await openWithResults(['a.txt', 'b.txt', 'c.txt']);
    const list = document.querySelector('ul[role="listbox"]') as HTMLElement;
    await fireEvent.keyDown(list, { key: 'ArrowDown' });
    expect(searchStore.cursor).toBe(0);
    await fireEvent.keyDown(list, { key: 'ArrowDown' });
    expect(searchStore.cursor).toBe(1);
    await fireEvent.keyDown(list, { key: 'End' });
    expect(searchStore.cursor).toBe(2);
    await fireEvent.keyDown(list, { key: 'ArrowDown' });
    expect(searchStore.cursor).toBe(2); // clamped
    await fireEvent.keyDown(list, { key: 'Home' });
    expect(searchStore.cursor).toBe(0);
    await fireEvent.keyDown(list, { key: 'ArrowUp' });
    expect(searchStore.cursor).toBe(0);
    await fireEvent.keyDown(list, { key: 'ArrowDown' });
    await fireEvent.keyDown(list, { key: 'Enter' });
    expect(pane.revealed).toEqual(['C:\\deep\\b.txt']);
    await tick();
    expect(list.getAttribute('aria-activedescendant')).toBe('search-opt-1');
  });

  it('Enter with no cursor does nothing; Escape closes when idle, cancels when running', async () => {
    await openWithResults(['a.txt']);
    const list = document.querySelector('ul[role="listbox"]') as HTMLElement;
    await fireEvent.keyDown(list, { key: 'Enter' });
    expect(pane.revealed).toEqual([]);
    await fireEvent.keyDown(list, { key: 'Escape' });
    expect(searchStore.panelOpen).toBe(false);

    searchStore.togglePanel();
    searchStore.testInFlight('s2');
    await tick();
    await fireEvent.keyDown(document.querySelector('ul[role="listbox"]') as HTMLElement, {
      key: 'Escape',
    });
    expect(searchStore.panelOpen).toBe(true); // cancel path, panel stays
  });

  it('Copy results puts one path per line on the clipboard', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    await openWithResults(['a.txt', 'b.txt']);
    const btn = Array.from(document.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'Copy results',
    ) as HTMLElement;
    await fireEvent.click(btn);
    await vi.waitFor(() => expect(writeText).toHaveBeenCalled());
    expect(writeText).toHaveBeenCalledWith('C:\\deep\\a.txt\nC:\\deep\\b.txt');
    await vi.waitFor(() => expect(searchStore.notice).toBe('Copied 2 paths'));
  });

  it('Copy and Feed are disabled/no-ops without results', async () => {
    render(SearchPanel);
    searchStore.togglePanel();
    await tick();
    expect(await searchStore.copyResults()).toBe(false);
    expect(searchStore.feedToPane()).toBe(false);
  });

  it('Feed to pane sends the results to the active pane as a virtual listing', async () => {
    await openWithResults(['a.txt', 'b.txt']);
    ui.activePane = 1;
    const btn = Array.from(document.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'Feed to pane',
    ) as HTMLElement;
    await fireEvent.click(btn);
    expect(branch.activeFor(1)).toBe(true);
    expect(branch.activeFor(0)).toBe(false);
    expect(branch.rows(1).map((r) => r.entry.path)).toEqual(['C:\\deep\\a.txt', 'C:\\deep\\b.txt']);
    expect(branch.stateFor(1)?.label).toBe('Search results · 2 items');
    branch.exit(1);
    ui.activePane = 0;
  });
});
