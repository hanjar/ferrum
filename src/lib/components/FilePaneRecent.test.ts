import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';

const recentIpc = vi.hoisted(() => ({
  recentWindows: vi.fn(async () => [] as unknown[]),
  recentNewItems: vi.fn(async () => [] as unknown[]),
  recentDefaultFolders: vi.fn(async () => ['C:\\Users\\a\\Downloads']),
  recentValidateFolders: vi.fn(async (f: string[]) => f),
}));
vi.mock('../ipc/recent', () => recentIpc);

const shellOpenMock = vi.fn();
const activityRecentMock = vi.fn();
vi.mock('../ipc/activity', async (orig) => ({
  ...(await orig<typeof import('../ipc/activity')>()),
  shellOpen: (...args: unknown[]) => shellOpenMock(...args),
  activityRecent: (...args: unknown[]) => activityRecentMock(...args),
}));

import FilePane from './FilePane.svelte';
import { getAction, isEnabled, menuGroups } from '../actions';
import { branch } from '../stores/branchStore.svelte';
import { getPane } from '../stores/panesRegistry.svelte';
import { recentStore } from '../stores/recentStore.svelte';
import { activity } from '../stores/activityStore.svelte';
import { routeDrop } from '../stores/externalDrop';
import { ui } from '../uiState.svelte';

const ent = (kind: 'file' | 'folder', locator: string, parentPath: string, name: string) => ({
  entity: {
    id: locator,
    kind,
    provider: 'fs',
    volumeId: 'v',
    locator,
    parentPath,
    displayName: name,
    availability: 'available',
  },
  lastUtcMs: kind === 'folder' ? 2000 : 1000,
  episodeCount: 1,
  activeDays: 1,
  score: null,
});

let pane: ReturnType<typeof render>;

async function openRecent() {
  getAction('go.recent')!.run!();
  await vi.waitFor(() =>
    expect(pane.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(0),
  );
  await vi.waitFor(() => expect(recentStore.loading).toBe(false));
  await tick();
}
const rowByName = (name: string) =>
  Array.from(pane.container.querySelectorAll<HTMLElement>('.row[data-path]')).find((el) =>
    el.textContent?.includes(name),
  ) as HTMLElement;

beforeEach(async () => {
  recentStore.reset();
  ui.activePane = 0;
  ui.includeWindowsRecent = true;
  activity.privateSession = false;
  shellOpenMock.mockReset();
  shellOpenMock.mockResolvedValue(true);
  activityRecentMock.mockReset();
  activityRecentMock.mockImplementation(async (kind: string) =>
    kind === 'folder'
      ? [ent('folder', 'C:\\Projects\\datasets', 'C:\\Projects', 'datasets')]
      : [ent('file', 'C:\\Elsewhere\\note.txt', 'C:\\Elsewhere', 'note.txt')],
  );
  pane = render(FilePane, { paneId: 0 });
  await vi.waitFor(() =>
    expect(pane.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
  );
});

afterEach(() => {
  branch.exit(0);
  branch.exit(1);
  cleanup();
});

describe('Recent view in a tab (FER-598)', () => {
  it('registers go.recent on Ctrl+E in the Commands menu', () => {
    const a = getAction('go.recent')!;
    expect(a.shortcut).toEqual({ key: 'e', ctrlOrCmd: true });
    expect(menuGroups.find((g) => g.label === 'Commands')!.ids).toContain('go.recent');
  });

  it('turns the tab into a listing, newest first, with a Recent crumb', async () => {
    await openRecent();
    expect(branch.isRecent(0)).toBe(true);
    const paths = Array.from(pane.container.querySelectorAll('.row[data-path]')).map((r) =>
      r.getAttribute('data-path'),
    );
    expect(paths).toEqual(['C:\\Projects\\datasets', 'C:\\Elsewhere\\note.txt']);
    expect(pane.container.textContent).toContain('Recent files and folders');
    expect(rowByName('note.txt').textContent).toContain('C:\\Elsewhere');
    expect(rowByName('note.txt').textContent).toContain('Ferrum');
    expect(pane.container.textContent).toContain('read-only');
  });

  it('Enter on a folder leaves the view and navigates the tab there', async () => {
    await openRecent();
    await fireEvent.click(rowByName('datasets'));
    getPane(0)!.openCursorEntry();
    await vi.waitFor(() => expect(branch.isRecent(0)).toBe(false));
    await vi.waitFor(() => expect(getPane(0)!.path).toBe('C:\\Projects\\datasets'));
  });

  it('Enter on a file opens it with the default app through shellOpen (recorded)', async () => {
    await openRecent();
    await fireEvent.click(rowByName('note.txt'));
    shellOpenMock.mockClear();
    getPane(0)!.openCursorEntry();
    expect(shellOpenMock).toHaveBeenCalledWith('C:\\Elsewhere\\note.txt', true);
    expect(branch.isRecent(0)).toBe(true);
  });

  it('Backspace (go up) and Back return to the folder the tab was on', async () => {
    const before = getPane(0)!.path;
    await openRecent();
    expect(getPane(0)!.canGoBack).toBe(true);
    getAction('nav.up')!.run!();
    await tick();
    expect(branch.isRecent(0)).toBe(false);
    expect(getPane(0)!.path).toBe(before);

    await openRecent();
    getAction('commands.back')!.run!();
    await tick();
    expect(branch.isRecent(0)).toBe(false);
    expect(getPane(0)!.path).toBe(before);
  });

  it('Go to containing folder reveals the cursor item and leaves the view', async () => {
    await openRecent();
    await fireEvent.click(rowByName('datasets'));
    expect(isEnabled(getAction('recent.reveal')!)).toBe(true);
    getAction('recent.reveal')!.run!();
    await vi.waitFor(() => expect(branch.isRecent(0)).toBe(false));
    await vi.waitFor(() => expect(getPane(0)!.path).toBe('C:\\Projects\\'));
  });

  it('is read-only: mutating commands are disabled while it is shown', async () => {
    const ids = [
      'file.copy',
      'file.move',
      'file.mkdir',
      'file.newfile',
      'file.delete',
      'file.delete.permanent',
      'file.rename',
      'file.clipPaste',
      'file.clipCut',
      'file.multiRename',
      'archive.pack',
      'file.pack',
    ];
    expect(isEnabled(getAction('file.mkdir')!)).toBe(true); // a normal listing allows it
    await openRecent();
    expect(getPane(0)!.readOnlyView).toBe(true);
    for (const id of ids) expect(isEnabled(getAction(id)!), id).toBe(false);
    // Reading stays possible, and so does leaving the view.
    expect(isEnabled(getAction('commands.copyItemPaths')!)).toBe(true);
    expect(isEnabled(getAction('nav.up')!)).toBe(true);
  });

  it('the other pane cannot copy into a Recent view', async () => {
    const right = render(FilePane, { paneId: 1 });
    await vi.waitFor(() =>
      expect(right.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    ui.activePane = 1;
    await fireEvent.click(right.container.querySelector('.row[data-path]') as HTMLElement);
    expect(isEnabled(getAction('file.copy')!)).toBe(true);
    ui.activePane = 0;
    await openRecent();
    ui.activePane = 1;
    expect(isEnabled(getAction('file.copy')!)).toBe(false);
    expect(isEnabled(getAction('file.move')!)).toBe(false);
    ui.activePane = 0;
  });

  it('refuses a drop into the Recent view', async () => {
    await openRecent();
    const section = pane.container.querySelector('section') as HTMLElement;
    // jsdom has no elementFromPoint: provide one that always answers with this pane.
    Object.defineProperty(document, 'elementFromPoint', {
      configurable: true,
      value: () => section,
    });
    try {
      expect(await routeDrop(['C:\\x.txt'], { x: 5, y: 5 }, null)).toBe(false);
    } finally {
      delete (document as unknown as Record<string, unknown>).elementFromPoint;
    }
  });
});
