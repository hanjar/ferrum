import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import HotlistMenu from './HotlistMenu.svelte';
import { hotlist } from '../stores/hotlistStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { resetMemoryBackend } from '../ipc/hotlist';
import type { HotItem } from '../hotlist/model';

const dir = (title: string, path: string): HotItem => ({ type: 'dir', title, path });

/** Shape of the user's real hotlist (see contract-fixtures/hotlist), trimmed. */
const TREE: HotItem[] = [
  {
    type: 'submenu',
    title: '&1. Drive',
    items: [
      dir('&1. Drive', 'C:\\n'),
      {
        type: 'submenu',
        title: '&6. personal',
        items: [dir('3. &Bob', 'C:\\n\\bob'), dir('4. &Charlie', 'C:\\n\\charlie\\')],
      },
    ],
  },
  { type: 'separator' },
  dir('&Telegram', 'C:\\tg'),
  { type: 'submenu', title: 'Dummy', items: [] },
  dir('&Tools', 'C:\\tools'),
  { type: 'command', title: 'kando', command: 'C:\\a b\\kando.exe', args: ['--x'] },
];

let openPath: ReturnType<typeof vi.fn<(path: string) => void>>;
let current = 'C:\\somewhere';

function pane(): PaneApi {
  return {
    get path() {
      return current;
    },
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    openPath,
  };
}

async function openWith(items: HotItem[]) {
  await hotlist.replace(items);
  hotlist.openMenu(0);
  await tick();
  await screen.findByRole('menu', { name: 'Directory Hotlist' });
}

const menuEl = () => document.querySelector<HTMLElement>('.menu')!;
const key = (k: string) => fireEvent.keyDown(menuEl(), { key: k });
const rowNames = () => screen.getAllByRole('menuitem').map((r) => r.getAttribute('aria-label'));

beforeEach(() => {
  openPath = vi.fn();
  current = 'C:\\somewhere';
  registerPane(0, pane());
});

afterEach(async () => {
  cleanup();
  hotlist.closeMenu();
  hotlist.closeConfigure();
  hotlist.cancelNetwork();
  hotlist.say('');
  resetMemoryBackend();
  await hotlist.load();
  vi.restoreAllMocks();
});

describe('HotlistMenu rendering', () => {
  it('renders nothing while closed', () => {
    render(HotlistMenu);
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('shows accelerators, submenu and command markers and the footer actions', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    expect(rowNames()).toEqual([
      'Drive',
      'Telegram',
      'Dummy',
      'Tools',
      'kando',
      'Add current folder',
      'Remove current folder',
      'Configure…',
    ]);
    const next = screen.getByRole('menuitem', { name: 'Drive' });
    expect(next.getAttribute('aria-haspopup')).toBe('menu');
    expect(next.textContent).toContain('1');
    expect(next.textContent).toContain('▸');
    expect(screen.getByRole('menuitem', { name: 'kando' }).textContent).toContain('▶');
    expect(
      screen.getByRole('menuitem', { name: 'Remove current folder' }).getAttribute('aria-disabled'),
    ).toBe('true');
    expect(screen.getAllByRole('separator')).toHaveLength(2);
  });

  it('marks the active pane folder with a check', async () => {
    current = 'c:\\TG\\';
    render(HotlistMenu);
    await openWith(TREE);
    expect(screen.getByRole('menuitem', { name: 'Telegram' }).textContent).toContain('✓');
    expect(
      screen.getByRole('menuitem', { name: 'Add current folder' }).getAttribute('aria-disabled'),
    ).toBe('true');
    expect(
      screen.getByRole('menuitem', { name: 'Remove current folder' }).getAttribute('aria-disabled'),
    ).toBeNull();
  });
});

describe('HotlistMenu keyboard', () => {
  it('accelerators walk 1 → 6 → 4 to Charlie and open that folder', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await key('1');
    await screen.findByRole('menuitem', { name: 'personal' });
    await key('6');
    await screen.findByRole('menuitem', { name: 'Charlie' });
    await key('4');
    expect(openPath).toHaveBeenCalledExactlyOnceWith('C:\\n\\charlie\\');
    expect(hotlist.menuOpen).toBe(false);
  });

  it('arrows move, Right enters a submenu, Left and Escape step back, Escape then closes', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await key('ArrowRight');
    expect(screen.getAllByRole('menu')).toHaveLength(2);
    await key('ArrowLeft');
    expect(screen.getAllByRole('menu').length).toBeGreaterThanOrEqual(1);
    await key('ArrowRight');
    await key('Escape');
    expect(hotlist.menuOpen).toBe(true);
    await key('Escape');
    expect(hotlist.menuOpen).toBe(false);
  });

  it('Enter on a folder opens it; Down skips the separators', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await key('ArrowDown'); // Telegram (separator skipped)
    await key('Enter');
    expect(openPath).toHaveBeenCalledWith('C:\\tg');
  });

  it('a repeated accelerator cycles through the matches before opening anything', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await key('t'); // Telegram + Tools: moves to Telegram, opens nothing
    expect(openPath).not.toHaveBeenCalled();
    await key('Enter');
    expect(openPath).toHaveBeenLastCalledWith('C:\\tg');
  });

  it('Home and End jump to the first and last selectable row', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await key('End');
    await key('Enter'); // Configure…
    expect(hotlist.configureOpen).toBe(true);
  });

  it('an empty submenu shows a placeholder and nothing to enter', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await fireEvent.mouseEnter(screen.getByRole('menuitem', { name: 'Dummy' }));
    expect(await screen.findByText('(empty)')).toBeTruthy();
  });

  it('Ctrl+D inside the menu closes it', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await fireEvent.keyDown(menuEl(), { key: 'd', ctrlKey: true });
    expect(hotlist.menuOpen).toBe(false);
  });
});

describe('HotlistMenu actions', () => {
  it('Add current folder saves the active pane folder', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Add current folder' }));
    await waitFor(() => expect(hotlist.isListed('C:\\somewhere')).toBe(true));
    expect(hotlist.menuOpen).toBe(false);
  });

  it('Remove current folder removes it', async () => {
    current = 'C:\\tools';
    render(HotlistMenu);
    await openWith(TREE);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Remove current folder' }));
    await waitFor(() => expect(hotlist.isListed('C:\\tools')).toBe(false));
  });

  it('clicking a folder opens it; clicking outside closes without acting', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Tools' }));
    expect(openPath).toHaveBeenCalledWith('C:\\tools');
    hotlist.openMenu(0);
    await tick();
    await fireEvent.mouseDown(document.querySelector('.scrim')!);
    expect(hotlist.menuOpen).toBe(false);
    expect(openPath).toHaveBeenCalledOnce();
  });

  it('hovering a submenu row opens it', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    await fireEvent.mouseEnter(screen.getByRole('menuitem', { name: 'Drive' }));
    expect(screen.getAllByRole('menu')).toHaveLength(2);
  });
});

describe('HotlistMenu command items', () => {
  it('sends the place in the tree and what was shown; no webview confirmation', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    const run = vi.spyOn(hotlist.api, 'runCommand').mockResolvedValue({ launched: true });
    await fireEvent.click(screen.getByRole('menuitem', { name: 'kando' }));
    expect(run).toHaveBeenCalledExactlyOnceWith([5], TREE[5]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('a hotlist that changed underneath reloads and says so instead of running', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    const { HotlistError } = await import('../ipc/hotlist');
    vi.spyOn(hotlist.api, 'runCommand').mockRejectedValue(new HotlistError('stale'));
    const load = vi.spyOn(hotlist, 'load');
    await fireEvent.click(screen.getByRole('menuitem', { name: 'kando' }));
    await waitFor(() => expect(hotlist.notice).toMatch(/changed on disk/));
    expect(load).toHaveBeenCalled();
  });
});

describe('HotlistMenu network folders (FER-467 R8)', () => {
  const NET: HotItem[] = [
    dir('&Share', '\\\\filesrv\\team'),
    dir('&Dav', '//dav.example@SSL/DavWWWRoot/x'),
    dir('&Local', 'C:\\l'),
    { type: 'dir', title: '&Pair', path: 'C:\\a', targetPath: '\\\\other\\b' },
  ];

  beforeEach(() => {
    localStorage.removeItem('ferrum.network.knownHosts.v1');
  });

  it('tags folders on a network share and nothing else', async () => {
    render(HotlistMenu);
    await openWith(NET);
    const tagged = (name: string) =>
      screen.getByRole('menuitem', { name }).querySelector('.net') !== null;
    expect(tagged('Share')).toBe(true);
    expect(tagged('Dav')).toBe(true);
    expect(tagged('Pair')).toBe(true); // the target path is on the network
    expect(tagged('Local')).toBe(false);
  });

  it('asks once per new host before opening; Cancel opens nothing', async () => {
    render(HotlistMenu);
    await openWith(NET);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Share' }));
    let dialog = await screen.findByRole('dialog', { name: 'Open network folder' });
    expect(dialog.textContent).toContain('filesrv');
    expect(dialog.textContent).toMatch(/NTLM/);
    expect(openPath).not.toHaveBeenCalled();
    await fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(openPath).not.toHaveBeenCalled();

    hotlist.openMenu(0);
    await screen.findByRole('menu', { name: 'Directory Hotlist' });
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Share' }));
    dialog = await screen.findByRole('dialog', { name: 'Open network folder' });
    await fireEvent.click(screen.getByRole('button', { name: 'Open' }));
    expect(openPath).toHaveBeenCalledExactlyOnceWith('\\\\filesrv\\team');

    // Same host again: no question.
    hotlist.openMenu(0);
    await screen.findByRole('menu', { name: 'Directory Hotlist' });
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Share' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(openPath).toHaveBeenCalledTimes(2);
  });

  it('local folders never ask', async () => {
    render(HotlistMenu);
    await openWith(NET);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Local' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(openPath).toHaveBeenCalledOnce();
  });
});

describe('HotlistMenu read-only', () => {
  it('shows the last copy with a warning and disables add/remove', async () => {
    render(HotlistMenu);
    await openWith(TREE);
    hotlist.applyView({
      doc: { version: 2, items: TREE },
      status: 'unreadable',
      readOnly: true,
      customLocation: 'D:\\h.json',
    });
    vi.spyOn(hotlist, 'load').mockResolvedValue();
    await tick();
    expect(screen.getByRole('status').textContent).toMatch(/read-only/);
    expect(
      screen.getByRole('menuitem', { name: 'Add current folder' }).getAttribute('aria-disabled'),
    ).toBe('true');
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Add current folder' }));
    expect(hotlist.isListed('C:\\somewhere')).toBe(false);
  });
});

describe('HotlistMenu notice', () => {
  it('renders the store notice', async () => {
    render(HotlistMenu);
    hotlist.say('The hotlist could not be saved.');
    expect(await screen.findByText('The hotlist could not be saved.')).toBeTruthy();
  });
});
