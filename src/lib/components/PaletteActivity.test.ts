import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';

const ipc = vi.hoisted(() => ({
  activityFavorites: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(async () => []),
  activitySearch: vi.fn<(q: string, kind: string, limit?: number) => Promise<unknown[] | null>>(
    async () => [],
  ),
  activityState: vi.fn(async () => null),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: vi.fn(async () => true),
}));
vi.mock('../ipc/activity', () => ipc);

const tauri = vi.hoisted(() => ({ inTauri: true }));
vi.mock('../ipc/tauri', async (orig) => ({
  ...(await orig<typeof import('../ipc/tauri')>()),
  isInTauri: () => tauri.inTauri,
}));

import CommandPalette from './CommandPalette.svelte';
import App from '../../App.svelte';
import { ui } from '../uiState.svelte';
import { activity } from '../stores/activityStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import type { ActivityItemDto, ActivityMatchDto } from '../ipc/activity';

const entity = (name: string, parent: string, kind: 'folder' | 'file' = 'folder') => ({
  id: `id:${parent}\\${name}`,
  kind,
  provider: 'local',
  volumeId: 'v',
  locator: `${parent}\\${name}`,
  parentPath: parent,
  displayName: name,
  availability: 'available',
});
const item = (name: string, parent = 'C:\\Work', n = 2): ActivityItemDto => ({
  entity: entity(name, parent),
  lastUtcMs: 1,
  episodeCount: n,
  activeDays: 1,
  score: null,
});
const match = (
  name: string,
  parent = 'C:\\Work',
  kind: 'folder' | 'file' = 'folder',
): ActivityMatchDto => ({ entity: entity(name, parent, kind), matchClass: 'exact' });

function fakePane(path = 'C:\\Pane') {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: vi.fn(),
    newTab: vi.fn(),
    closeCurrentTab: vi.fn(),
    goUp: vi.fn(),
    openCursorEntry: vi.fn(),
    openPath: vi.fn(),
    revealPath: vi.fn(),
  } satisfies PaneApi;
}

/** What the mocked backend returns; palette open calls activity.refresh(). */
const data = vi.hoisted(() => ({
  recent: [] as unknown[],
  frequent: [] as unknown[],
  favorites: [] as unknown[],
}));
function seed(next: {
  recent?: ActivityItemDto[];
  frequent?: ActivityItemDto[];
  favorites?: unknown[];
}) {
  if (next.recent) {
    data.recent = next.recent;
    activity.recent = next.recent;
  }
  if (next.frequent) {
    data.frequent = next.frequent;
    activity.frequent = next.frequent;
  }
  if (next.favorites) {
    data.favorites = next.favorites;
    activity.favorites = next.favorites as never;
  }
}

let unregister: (() => void)[] = [];
beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockClear();
  ipc.activitySearch.mockImplementation(async () => []);
  data.recent = [];
  data.frequent = [];
  data.favorites = [];
  ipc.activityRecent.mockImplementation(async () => data.recent as never);
  ipc.activityFrequent.mockImplementation(async () => data.frequent as never);
  ipc.activityFavorites.mockImplementation(async () => data.favorites as never);
  ipc.activityState.mockImplementation(
    async () => ({ recordingEnabled: true, privacyEpoch: 1, disclosureAcked: true }) as never,
  );
  tauri.inTauri = true;
  activity.available = true;
  activity.recording = true;
  activity.privateSession = false;
  seed({ recent: [] });
  activity.frequent = [];
  activity.favorites = [];
  ui.paletteOpen = false;
  ui.activePane = 0;
});
afterEach(() => {
  ui.paletteOpen = false;
  unregister.forEach((u) => u());
  unregister = [];
});

async function openPalette() {
  render(CommandPalette);
  ui.paletteOpen = true;
  await tick();
  return screen.getByRole('dialog', { name: 'Command palette' });
}
const input = () => screen.getByRole('combobox') as HTMLInputElement;
async function type(q: string) {
  await fireEvent.input(input(), { target: { value: q } });
}
const activeOption = () =>
  screen.getAllByRole('option').find((o) => o.getAttribute('aria-selected') === 'true');

describe('palette groups on empty query (FER-258)', () => {
  it('shows labeled Favorites / Recent / Frequent / Perintah groups with quotas', async () => {
    seed({ favorites: [{ path: 'C:\\Pin', label: 'Pin', kind: 'folder' }] });
    seed({ recent: [item('R1'), item('R2'), item('R3'), item('R4')] });
    seed({ frequent: [item('F1')] });
    await openPalette();
    for (const name of ['Favorites', 'Recent', 'Frequent', 'Perintah']) {
      expect(screen.getByRole('group', { name })).toBeTruthy();
    }
    const recent = screen.getByRole('group', { name: 'Recent' });
    expect(recent.querySelectorAll('[role="option"]').length).toBe(3); // quota
    // no filter chips on an empty query
    expect(screen.queryByRole('toolbar', { name: 'Filter hasil' })).toBeNull();
  });
});

describe('mode chips (FER-258)', () => {
  it('filter by Folder/File/Perintah and scope the backend query', async () => {
    ipc.activitySearch.mockImplementation(async (_q: string, kind: string) =>
      [match('Switchyard', 'C:\\A'), match('switch.txt', 'C:\\A', 'file')].filter(
        (m) => kind === 'all' || m.entity.kind === kind,
      ),
    );
    await openPalette();
    await type('switch');
    await waitFor(() => expect(ipc.activitySearch).toHaveBeenCalledWith('switch', 'all', 8));
    await screen.findByText('Switchyard');
    expect(screen.getByRole('toolbar', { name: 'Filter hasil' })).toBeTruthy();
    expect(screen.getByText('Switch to other pane (Tab)')).toBeTruthy(); // command present in "Semua"

    await fireEvent.click(screen.getByRole('button', { name: 'File' }));
    await waitFor(() => expect(ipc.activitySearch).toHaveBeenLastCalledWith('switch', 'file', 8));
    await screen.findByText('switch.txt');
    expect(screen.queryByText('Switch to other pane (Tab)')).toBeNull(); // commands hidden
    expect(screen.queryByText('Switchyard')).toBeNull();

    await fireEvent.click(screen.getByRole('button', { name: 'Folder' }));
    await screen.findByText('Switchyard');
    expect(screen.queryByText('switch.txt')).toBeNull();

    ipc.activitySearch.mockClear();
    await fireEvent.click(screen.getByRole('button', { name: 'Perintah' }));
    await screen.findByText('Switch to other pane (Tab)');
    await new Promise((r) => setTimeout(r, 200));
    expect(ipc.activitySearch).not.toHaveBeenCalled(); // commands only: no history lookup
    expect(screen.queryByText('Switchyard')).toBeNull();
  });
});

describe('target pane snapshot and file reveal (FER-258)', () => {
  it('Enter acts on the pane active when the palette opened', async () => {
    const p0 = fakePane('C:\\P0');
    const p1 = fakePane('C:\\P1');
    unregister.push(registerPane(0, p0), registerPane(1, p1));
    seed({ recent: [item('Docs')] });
    ui.activePane = 1;
    await openPalette();
    ui.activePane = 0; // focus moves while the palette is open
    await tick();
    await screen.findByRole('option', { name: /Docs/ });
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Enter' });
    expect(p1.openPath).toHaveBeenCalledWith('C:\\Work\\Docs');
    expect(p0.openPath).not.toHaveBeenCalled();
    expect(ui.paletteOpen).toBe(false);
  });

  it('file result reveals (revealPath), never opens the file or navigates', async () => {
    const p0 = fakePane();
    unregister.push(registerPane(0, p0));
    ipc.activitySearch.mockResolvedValue([match('report.docx', 'C:\\Work\\Q3', 'file')]);
    await openPalette();
    await type('report');
    const opt = await screen.findByRole('option', { name: /report\.docx/ });
    await fireEvent.click(opt);
    expect(p0.revealPath).toHaveBeenCalledWith('C:\\Work\\Q3\\report.docx');
    expect(p0.openPath).not.toHaveBeenCalled();
    expect(ipc.shellOpen).not.toHaveBeenCalled();
  });

  it('file result falls back to opening the parent when the pane cannot reveal', async () => {
    const p0 = { ...fakePane(), revealPath: undefined } as PaneApi;
    unregister.push(registerPane(0, p0));
    ipc.activitySearch.mockResolvedValue([match('a.txt', 'C:\\Work', 'file')]);
    await openPalette();
    await type('a.t');
    await fireEvent.click(await screen.findByRole('option', { name: /a\.txt/ }));
    expect(p0.openPath).toHaveBeenCalledWith('C:\\Work');
  });
});

describe('match reasons (FER-254)', () => {
  const withReasons = (
    name: string,
    parent: string,
    matchClass: string,
    reasonCodes?: string[],
  ): ActivityMatchDto =>
    ({ entity: entity(name, parent), matchClass, reasonCodes }) as unknown as ActivityMatchDto;

  it('shows a short Indonesian reason per ReasonCode and keeps a11y roles', async () => {
    ipc.activitySearch.mockResolvedValue([
      withReasons('Reports', 'C:\\A', 'exact', ['exactName', 'frequent']),
      withReasons('Reprots', 'C:\\B', 'fuzzy', ['similarName', 'recent', 'sameWorkspace']),
      withReasons('Quarterly Business Review', 'C:\\C', 'initialism', ['abbreviation']),
      withReasons('Pinned', 'C:\\D', 'prefix', ['favorite', 'visitedInFerrum']),
    ]);
    await openPalette();
    await type('rep');
    await screen.findByRole('option', { name: /Reports/ });
    const sub = (re: RegExp) =>
      screen.getByRole('option', { name: re }).querySelector('.sub')?.textContent;
    expect(sub(/Reports/)).toContain('Nama persis · Sering dibuka');
    expect(sub(/Reprots/)).toContain('Nama mirip · Baru dibuka');
    expect(sub(/Reprots/)).not.toContain('Workspace sama'); // at most two reasons
    expect(sub(/Quarterly/)).toContain('Singkatan');
    expect(sub(/Pinned/)).toContain('Favorit · Pernah diakses di Ferrum');
    expect(screen.getByRole('listbox')).toBeTruthy();
    expect(screen.getAllByRole('option').length).toBeGreaterThanOrEqual(4);
  });

  it('falls back to the match class label when the backend sends no reasons', async () => {
    ipc.activitySearch.mockResolvedValue([
      withReasons('Legacy', 'C:\\L', 'fuzzy'),
      withReasons('Empty', 'C:\\E', 'exact', []),
    ]);
    await openPalette();
    await type('leg');
    await screen.findByRole('option', { name: /Legacy/ });
    const sub = (re: RegExp) =>
      screen.getByRole('option', { name: re }).querySelector('.sub')?.textContent;
    expect(sub(/Legacy/)).toContain('Nama mirip');
    expect(sub(/Empty/)).toContain('Pernah diakses di Ferrum');
  });
});

describe('same-name disambiguation (FER-258)', () => {
  it('shows the distinguishing parent segments only for colliding names', async () => {
    ipc.activitySearch.mockResolvedValue([
      match('Reports', 'C:\\Clients\\Acme'),
      match('Reports', 'C:\\Clients\\Globex'),
      match('Unique', 'C:\\Clients\\Acme'),
    ]);
    await openPalette();
    await type('repo');
    await screen.findAllByRole('option', { name: /Reports/ });
    const reports = screen.getAllByRole('option', { name: /Reports/ });
    expect(reports).toHaveLength(2);
    expect(reports[0].querySelector('.disamb')?.textContent).toBe('Clients\\Acme');
    expect(reports[1].querySelector('.disamb')?.textContent).toBe('Clients\\Globex');
    expect(screen.getByRole('option', { name: /Unique/ }).querySelector('.disamb')).toBeNull();
  });

  it('also disambiguates empty-query Recent vs Frequent duplicates', async () => {
    seed({ recent: [item('Build', 'C:\\X\\app')] });
    seed({ frequent: [item('Build', 'C:\\Y\\lib')] });
    await openPalette();
    const hints = Array.from(document.querySelectorAll('.disamb')).map((e) => e.textContent);
    expect(hints).toEqual(['X\\app', 'Y\\lib']);
  });
});

describe('selection stability and cancellation (FER-259)', () => {
  it('selects the first result from the start (never null)', async () => {
    await openPalette();
    expect(activeOption()).toBeTruthy();
    expect(screen.getAllByRole('option')[0].getAttribute('aria-selected')).toBe('true');
    expect(input().getAttribute('aria-activedescendant')).toBe('palette-opt-0');
  });

  it('async history results do not move the selected item', async () => {
    let release!: (m: ActivityMatchDto[]) => void;
    ipc.activitySearch.mockImplementation(() => new Promise((res) => (release = res as never)));
    await openPalette();
    await type('pane');
    await waitFor(() => expect(ipc.activitySearch).toHaveBeenCalled());
    const before = activeOption()!.textContent;
    expect(before).toBeTruthy();
    release([match('Panels', 'C:\\Z'), match('Paneling', 'C:\\Z')]);
    await screen.findByText('Panels');
    expect(activeOption()!.textContent).toBe(before);
    expect(activeOption()!.id).toBe(screen.getAllByRole('option')[0].id);
  });

  it('keeps an arrow-selected history item selected when the lists change', async () => {
    ipc.activitySearch.mockResolvedValueOnce([match('Alpha1', 'C:\\Z')]);
    await openPalette();
    await type('alph');
    await screen.findByText('Alpha1');
    const opts = screen.getAllByRole('option');
    const idx = opts.findIndex((o) => /Alpha1/.test(o.textContent ?? ''));
    for (let i = 0; i < idx; i++) {
      await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'ArrowDown' });
    }
    expect(activeOption()!.textContent).toContain('Alpha1');
    activity.recent = [item('Zeta')]; // unrelated store refresh lands
    await tick();
    expect(activeOption()!.textContent).toContain('Alpha1');
  });

  it('drops a superseded (stale) response: client-side cancel only', async () => {
    const resolvers: ((m: ActivityMatchDto[]) => void)[] = [];
    ipc.activitySearch.mockImplementation(() => new Promise((res) => resolvers.push(res as never)));
    await openPalette();
    await type('old');
    await waitFor(() => expect(resolvers).toHaveLength(1));
    await type('newer');
    await waitFor(() => expect(resolvers).toHaveLength(2));
    resolvers[1]([match('NewerHit', 'C:\\N')]);
    await screen.findByText('NewerHit');
    resolvers[0]([match('OldHit', 'C:\\O')]); // late reply for the old query
    await tick();
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByText('OldHit')).toBeNull();
    expect(screen.getByText('NewerHit')).toBeTruthy();
  });
});

describe('keyboard, ARIA and partial results (FER-260)', () => {
  it('ArrowDown/ArrowUp move aria-selected, clamped at both ends', async () => {
    const dialog = await openPalette();
    const opts = () => screen.getAllByRole('option');
    const selectedIndex = () => opts().findIndex((o) => o.getAttribute('aria-selected') === 'true');
    await fireEvent.keyDown(dialog, { key: 'ArrowUp' });
    expect(selectedIndex()).toBe(0);
    await fireEvent.keyDown(dialog, { key: 'ArrowDown' });
    expect(selectedIndex()).toBe(1);
    expect(input().getAttribute('aria-activedescendant')).toBe('palette-opt-1');
    for (let i = 0; i < opts().length + 3; i++) {
      await fireEvent.keyDown(dialog, { key: 'ArrowDown' });
    }
    expect(selectedIndex()).toBe(opts().length - 1);
  });

  it('Enter runs the selected item and Escape closes without acting', async () => {
    const p0 = fakePane();
    unregister.push(registerPane(0, p0));
    seed({ recent: [item('Docs')] });
    const dialog = await openPalette();
    await fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(ui.paletteOpen).toBe(false);
    expect(p0.openPath).not.toHaveBeenCalled();
    ui.paletteOpen = true;
    await tick();
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Enter' });
    expect(p0.openPath).toHaveBeenCalledWith('C:\\Work\\Docs');
  });

  it('Enter with no results does nothing', async () => {
    const dialog = await openPalette();
    await type('zzzzqq');
    await screen.findByText('No commands match.');
    await fireEvent.keyDown(dialog, { key: 'Enter' });
    expect(ui.paletteOpen).toBe(true);
  });

  it('exposes dialog, combobox, listbox, labeled groups, options and a live status', async () => {
    seed({ recent: [item('Docs')] });
    const dialog = await openPalette();
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(input().getAttribute('aria-controls')).toBe('palette-results');
    const list = screen.getByRole('listbox', { name: 'Results' });
    expect(list.id).toBe('palette-results');
    expect(screen.getByRole('group', { name: 'Recent' })).toBeTruthy();
    for (const o of screen.getAllByRole('option')) {
      expect(['true', 'false']).toContain(o.getAttribute('aria-selected'));
    }
    const live = screen.getByRole('status');
    expect(live.getAttribute('aria-live')).toBe('polite');
    expect(live.textContent).toMatch(/\d+ hasil/);
  });

  it('a stale history reply (null) is announced as partial and keeps commands usable', async () => {
    ipc.activitySearch.mockResolvedValue(null);
    await openPalette();
    await type('switch');
    await waitFor(() => expect(screen.getByRole('status').textContent).toContain('belum lengkap'));
    expect(screen.getByRole('note').textContent).toContain('belum lengkap');
    expect(screen.getByText('Switch to other pane (Tab)')).toBeTruthy(); // command results still present
  });

  it('stale reply with nothing else to show reads "coba lagi"', async () => {
    ipc.activitySearch.mockResolvedValue(null);
    await openPalette();
    await type('qqzzxx');
    await screen.findByText('Riwayat berubah — coba lagi.');
  });
});

describe('shortcut regression with activity enabled (FER-260)', () => {
  it('Ctrl+K opens the palette; F7 typed in its input does not open mkdir', async () => {
    tauri.inTauri = false; // App must boot on mock listings
    activity.available = true;
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    activity.available = true;
    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    await tick();
    expect(ui.paletteOpen).toBe(true);
    const box = document.querySelector('.palette input') as HTMLInputElement;
    fireEvent.keyDown(box, { key: 'F7' });
    await tick();
    expect(document.querySelector('[role="dialog"][aria-label="mkdir"]')).toBeNull();
    fireEvent.keyDown(document.querySelector('.palette')!, { key: 'Escape' });
    await tick();
    expect(ui.paletteOpen).toBe(false);
    // outside the palette F7 still works
    fireEvent.keyDown(window, { key: 'F7' });
    await vi.waitFor(() =>
      expect(document.querySelector('[role="dialog"][aria-label="mkdir"]')).toBeTruthy(),
    );
  });
});
