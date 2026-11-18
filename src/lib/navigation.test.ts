import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi, PaneContents } from './stores/panesRegistry.svelte';

const desktopDirMock = vi.fn();
vi.mock('@tauri-apps/api/path', () => ({
  desktopDir: (...a: unknown[]) => desktopDirMock(...a),
}));

const { swapPanes, targetFromSource, openInOtherPane, goBack, openCdTree, openDesktop } =
  await import('./navigation');
const { registerPane } = await import('./stores/panesRegistry.svelte');
const { dialogs } = await import('./stores/dialogStore.svelte');
const { ui } = await import('./uiState.svelte');
const { viewState } = await import('./stores/viewStateStore.svelte');

function pane(path: string, back?: () => void): PaneApi {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    openPath: vi.fn(),
    back,
  };
}

let left: PaneApi;
let right: PaneApi;

beforeEach(() => {
  desktopDirMock.mockReset();
  left = pane('C:\\left\\dir');
  right = pane('D:\\right\\dir');
  registerPane(0, left);
  registerPane(1, right);
  ui.activePane = 0;
  dialogs.close();
});

afterEach(() => {
  dialogs.close();
});

describe('source/target controls (F2-22)', () => {
  it('swaps the two pane locations without touching selections via history', () => {
    expect(swapPanes()).toBe(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith('C:\\left\\dir');
    expect(left.openPath).toHaveBeenCalledExactlyOnceWith('D:\\right\\dir');
  });

  it('target = source sends only the target pane to the source location', () => {
    expect(targetFromSource()).toBe(true);
    expect(left.openPath).not.toHaveBeenCalled();
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith('C:\\left\\dir');
  });

  it('refuses to operate without both panes', () => {
    registerPane(1, right)();
    expect(swapPanes()).toBe(false);
    expect(targetFromSource()).toBe(false);
    expect(dialogs.current).toBeNull();
  });
});

describe('Ctrl+Right / Ctrl+Left open in other pane (FER-482)', () => {
  const LEFT = 'C:\\left\\dir';
  const RIGHT = 'D:\\right\\dir';
  const cursor = (name: string, isDir: boolean) => () => ({
    name,
    path: `${LEFT}\\${name}`,
    isDir,
  });

  it('opens the folder under the cursor in the other pane when pointing toward it', () => {
    left.cursorEntry = cursor('invoice', true);
    expect(openInOtherPane(true)).toBe(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith(`${LEFT}\\invoice`);
    expect(left.openPath).not.toHaveBeenCalled();
  });

  it('opens an archive under the cursor', () => {
    left.cursorEntry = cursor('pack.zip', false);
    openInOtherPane(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith(`${LEFT}\\pack.zip`);
  });

  it('a plain file opens the current folder instead', () => {
    left.cursorEntry = cursor('notes.txt', false);
    openInOtherPane(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith(LEFT);
  });

  it('pointing away from the other pane opens the current folder even on a folder', () => {
    left.cursorEntry = cursor('invoice', true);
    openInOtherPane(false);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith(LEFT);
  });

  it('works from the right pane and fails without a target', () => {
    ui.activePane = 1;
    right.cursorEntry = () => ({ name: 'x', path: `${RIGHT}\\x`, isDir: true });
    openInOtherPane(true);
    expect(left.openPath).toHaveBeenCalledExactlyOnceWith(`${RIGHT}\\x`);
    registerPane(0, left)();
    expect(openInOtherPane(true)).toBe(false);
  });
});

describe('Ctrl+Right / Ctrl+Left in the Recent view (FER-600)', () => {
  const recentPane = (item: { name: string; path: string; isDir: boolean } | undefined) => {
    const p = pane('C:\\previous\\folder');
    Object.defineProperty(p, 'readOnlyView', { get: () => true });
    p.cursorEntry = () => item;
    registerPane(0, p);
    right.revealPath = vi.fn();
    return p;
  };

  it('a file opens its parent folder in the other pane with the cursor on it', () => {
    const src = recentPane({ name: 'note.txt', path: 'E:\\Docs\\sub\\note.txt', isDir: false });
    expect(openInOtherPane(true)).toBe(true);
    expect(right.revealPath).toHaveBeenCalledExactlyOnceWith('E:\\Docs\\sub\\note.txt');
    expect(right.openPath).not.toHaveBeenCalled();
    expect(src.openPath).not.toHaveBeenCalled();
  });

  it('also for Ctrl+Left (away direction), never the tab previous folder', () => {
    recentPane({ name: 'a.zip', path: 'E:\\Docs\\a.zip', isDir: false });
    openInOtherPane(false);
    expect(right.revealPath).toHaveBeenCalledExactlyOnceWith('E:\\Docs\\a.zip');
  });

  it('a folder opens the folder itself', () => {
    recentPane({ name: 'data', path: 'E:\\Docs\\data', isDir: true });
    expect(openInOtherPane(true)).toBe(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith('E:\\Docs\\data');
    expect(right.revealPath).not.toHaveBeenCalled();
  });

  it('falls back to opening the parent when the target cannot reveal', () => {
    recentPane({ name: 'n.txt', path: 'E:\\Docs\\n.txt', isDir: false });
    right.revealPath = undefined;
    openInOtherPane(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith('E:\\Docs\\');
  });

  it('the .. row or no item does nothing and never throws', () => {
    recentPane({ name: '..', path: 'C:\\previous', isDir: true });
    expect(openInOtherPane(true)).toBe(false);
    recentPane(undefined);
    expect(openInOtherPane(true)).toBe(false);
    expect(right.openPath).not.toHaveBeenCalled();
    expect(right.revealPath).not.toHaveBeenCalled();
  });
});

describe('Ctrl+U swaps whole panes (FER-482)', () => {
  it('exchanges tabs, selection and per-pane view state', () => {
    const mk = (tag: string) => {
      let held: PaneContents = { tabs: [], current: 0, selected: [tag] };
      return {
        take: vi.fn(() => held),
        put: vi.fn((c: PaneContents) => {
          held = c;
        }),
      };
    };
    const a = mk('a');
    const b = mk('b');
    const ca = a.take();
    const cb = b.take();
    left.takeContents = a.take;
    left.putContents = a.put;
    right.takeContents = b.take;
    right.putContents = b.put;
    viewState.setSort(0, 'size', -1);
    viewState.setViewMode(1, 'brief');
    expect(swapPanes()).toBe(true);
    expect(a.put).toHaveBeenCalledExactlyOnceWith(cb);
    expect(b.put).toHaveBeenCalledExactlyOnceWith(ca);
    expect(viewState.sort(1)).toEqual({ key: 'size', dir: -1 });
    expect(viewState.viewMode(0)).toBe('brief');
    expect(left.openPath).not.toHaveBeenCalled();
  });
});

describe('Go Back and CD Tree (F2-22)', () => {
  it('goes back on the active pane and reports unavailable without history support', () => {
    const back = vi.fn();
    registerPane(0, pane('C:\\x', back));
    expect(goBack()).toBe(true);
    expect(back).toHaveBeenCalledOnce();
    registerPane(0, { ...pane('C:\\x'), back: undefined });
    expect(goBack()).toBe(false);
  });

  it('opens the existing tree sidebar instead of reimplementing it', () => {
    ui.sidebarOpen = false;
    expect(openCdTree()).toBe(true);
    expect(ui.sidebarOpen).toBe(true);
    ui.sidebarOpen = true;
  });
});

describe('Open Desktop Folder (F2-22)', () => {
  it('navigates the active pane to the OS desktop directory', async () => {
    desktopDirMock.mockResolvedValue('C:\\Users\\you\\Desktop');
    expect(await openDesktop()).toBe(true);
    expect(left.openPath).toHaveBeenCalledExactlyOnceWith('C:\\Users\\you\\Desktop');
  });

  it('opens the error dialog when the desktop path cannot be resolved', async () => {
    desktopDirMock.mockRejectedValue(new Error('no shell access'));
    expect(await openDesktop()).toBe(false);
    expect(left.openPath).not.toHaveBeenCalled();
    expect(dialogs.current).toEqual({
      kind: 'error',
      title: 'Open Desktop Folder',
      message: 'no shell access',
    });
  });
});

describe('F2-22 action registry integration', () => {
  it('binds hotlist, CD tree, go back, and the source/target controls', async () => {
    const { getAction, isEnabled, runAction } = await import('./actions');
    expect(getAction('commands.hotlist')!.shortcut).toEqual({ key: 'd', ctrlOrCmd: true });
    expect(getAction('commands.cdTree')!.shortcut).toEqual({ key: 'F10', alt: true });
    expect(getAction('commands.sourceTarget')!.shortcut).toEqual({ key: 'u', ctrlOrCmd: true });
    expect(getAction('commands.back')!.shortcut).toEqual({ key: 'ArrowLeft', alt: true });
    for (const id of ['commands.desktop', 'commands.targetSource']) {
      expect(getAction(id)!.shortcut, id).toBeUndefined();
      expect(getAction(id)!.disabledReason, id).toBeUndefined();
      expect(isEnabled(getAction(id)!), id).toBe(true);
    }
    expect(runAction(getAction('commands.sourceTarget')!)).toBe(true);
    expect(right.openPath).toHaveBeenCalledExactlyOnceWith('C:\\left\\dir');
    expect(runAction(getAction('commands.hotlist')!)).toBe(true);
    const { hotlist } = await import('./stores/hotlistStore.svelte');
    expect(hotlist.menuOpen).toBe(true);
    // Ctrl+D again toggles the popup closed.
    expect(runAction(getAction('commands.hotlist')!)).toBe(true);
    expect(hotlist.menuOpen).toBe(false);
    // Go Back stays unavailable while the pane has no back implementation.
    registerPane(0, { ...pane('C:\\x'), back: undefined });
    expect(isEnabled(getAction('commands.back')!)).toBe(false);
    expect(runAction(getAction('commands.back')!)).toBe(false);
  });
});
