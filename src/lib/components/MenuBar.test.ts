import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import MenuBar from './MenuBar.svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { dialog } from '../dialogKit';

const { run, available, markAvailable } = vi.hoisted(() => ({
  run: vi.fn(),
  available: vi.fn(() => false),
  markAvailable: vi.fn(() => true),
}));

vi.mock('../actions', () => {
  const actions = [
    { id: 'file.open', label: 'Open', shortcut: { key: 'Enter' }, run: () => run('open') },
    {
      id: 'file.copy',
      label: 'Copy',
      shortcut: { key: 'F5' },
      available,
      run: () => run('copy'),
    },
    {
      id: 'file.new',
      label: 'New folder',
      run: () => {
        dialogs.open({ kind: 'delete', paths: ['example'], permanent: false });
        run('new');
      },
    },
    { id: 'mark.all', label: 'Select all', available: markAvailable, run: () => run('mark') },
    { id: 'cmd.run', label: 'Run command', run: () => run('command') },
    { id: 'cmd.run2', label: 'Run other', run: () => run('command2') },
    { id: 'view.hidden', label: 'Show hidden files', run: () => run('view') },
    { id: 'file.pack', label: 'Pack', disabledReason: 'Not available yet' },
    { id: 'file.zip', label: 'Zip', run: () => run('zip') },
    { id: 'file.tar', label: 'Tar', run: () => run('tar') },
  ];
  const menuGroups = [
    { label: 'Files', ids: ['file.open', 'file.copy', 'file.new', 'file.pack', 'submenu:pack'] },
    { label: 'Mark', ids: ['mark.all'] },
    { label: 'Commands', ids: ['cmd.run', '-', 'cmd.run2'] },
    { label: 'Show', ids: ['view.hidden'] },
  ];
  const menuSubmenus = { 'submenu:pack': { label: 'Archive', ids: ['file.zip', 'file.tar'] } };
  const getAction = (id: string) => actions.find((action) => action.id === id);
  const isEnabled = (action: (typeof actions)[number]) =>
    !!action.run && (action.available?.() ?? true);
  return {
    actions,
    MENU_SEPARATOR: '-',
    menuGroups,
    menuSubmenus,
    getAction,
    isEnabled,
    labelOf: (action: (typeof actions)[number]) => action.label,
    shortcutFor: (action: (typeof actions)[number]) => action.shortcut,
    shortcutLabel: (shortcut: { key: string }) => shortcut.key,
    runAction: (action: (typeof actions)[number]) => {
      if (!isEnabled(action)) return false;
      action.run?.();
      return true;
    },
  };
});

afterEach(() => {
  cleanup();
  dialogs.close();
  run.mockClear();
  available.mockReturnValue(false);
  markAvailable.mockReturnValue(true);
});

function opener(label: string): HTMLButtonElement {
  return screen.getByRole('menuitem', { name: label }) as HTMLButtonElement;
}

function item(label: string): HTMLButtonElement {
  return screen.getByRole('menuitem', { name: label }) as HTMLButtonElement;
}

async function open(label = 'Files') {
  fireEvent.click(opener(label));
  await tick();
  return screen.getByRole('menu');
}

describe('MenuBar', () => {
  it('renders four named buttons and registry-ordered menu items with shortcut and disabled reason', async () => {
    render(MenuBar);
    expect(screen.getByRole('menubar').querySelectorAll('button')).toHaveLength(4);
    expect(['Files', 'Mark', 'Commands', 'Show'].map((label) => opener(label).textContent)).toEqual(
      ['Files', 'Mark', 'Commands', 'Show'],
    );
    expect(screen.queryByRole('menu')).toBeNull();

    const menu = await open();
    expect(opener('Files').getAttribute('aria-expanded')).toBe('true');
    expect(menu.getAttribute('aria-labelledby')).toBe(opener('Files').id);
    expect(
      Array.from(menu.querySelectorAll('[role="menuitem"]')).map((node) =>
        node.textContent?.trim(),
      ),
    ).toEqual(['Open Enter', 'Copy F5', 'New folder', 'Pack', 'Archive ▸']);
    expect(item('Copy').getAttribute('aria-disabled')).toBe('true');
    expect(item('Copy').title).toBe('Unavailable in current context');
    expect(item('Pack').getAttribute('aria-disabled')).toBe('true');
    expect(item('Pack').title).toBe('Not available yet');
    expect(
      document.getElementById(item('Copy').getAttribute('aria-describedby') ?? '')?.textContent,
    ).toBe('Unavailable in current context');
    expect(
      document.getElementById(item('Pack').getAttribute('aria-describedby') ?? '')?.textContent,
    ).toBe('Not available yet');
  });

  it('skips disabled items with ArrowUp/Down/Home/End and never executes them by pointer or keyboard', async () => {
    render(MenuBar);
    const menu = await open();
    await vi.waitFor(() => expect(document.activeElement).toBe(item('Open')));
    fireEvent.keyDown(item('Open'), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(item('New folder'));
    fireEvent.keyDown(item('New folder'), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(item('Archive'));
    fireEvent.keyDown(item('Archive'), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(item('Open'));
    fireEvent.keyDown(item('Open'), { key: 'ArrowUp' });
    expect(document.activeElement).toBe(item('Archive'));
    fireEvent.keyDown(menu, { key: 'Home' });
    expect(document.activeElement).toBe(item('Open'));
    fireEvent.keyDown(menu, { key: 'End' });
    expect(document.activeElement).toBe(item('Archive'));
    fireEvent.click(item('Copy'));
    fireEvent.keyDown(item('Copy'), { key: 'Enter' });
    fireEvent.click(item('Pack'));
    fireEvent.keyDown(item('Pack'), { key: ' ' });
    expect(run).not.toHaveBeenCalled();
    expect(screen.getByRole('menu')).toBeTruthy();
  });

  it('opens from keyboard, switches group with arrows, and executes focused item exactly once', async () => {
    render(MenuBar);
    opener('Files').focus();
    fireEvent.keyDown(opener('Files'), { key: 'ArrowDown' });
    await tick();
    expect(document.activeElement).toBe(item('Open'));
    fireEvent.keyDown(item('Open'), { key: 'ArrowRight' });
    await tick();
    expect(opener('Mark').getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement).toBe(item('Select all'));
    fireEvent.keyDown(item('Select all'), { key: 'ArrowLeft' });
    await tick();
    expect(document.activeElement).toBe(item('Open'));
    fireEvent.keyDown(item('Open'), { key: ' ' });
    await tick();
    expect(run).toHaveBeenCalledExactlyOnceWith('open');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(opener('Files'));
  });

  it('opens upward on ArrowUp, Escape restores opener focus, outside pointer closes without stealing target focus', async () => {
    render(MenuBar);
    opener('Files').focus();
    fireEvent.keyDown(opener('Files'), { key: 'ArrowUp' });
    await tick();
    expect(document.activeElement).toBe(item('Archive'));
    fireEvent.keyDown(item('Archive'), { key: 'Escape' });
    await tick();
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(opener('Files'));
    await open();
    fireEvent.pointerDown(document.body);
    await tick();
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(opener('Files'));

    await open();
    const outside = document.createElement('button');
    outside.textContent = 'Outside';
    document.body.append(outside);
    outside.focus();
    fireEvent.pointerDown(outside);
    await tick();
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });

  it('runs clicked enabled item once, closes menu, and restores opener focus', async () => {
    render(MenuBar);
    await open('Mark');
    fireEvent.click(item('Select all'));
    await tick();
    expect(run).toHaveBeenCalledExactlyOnceWith('mark');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(opener('Mark'));
  });

  it('keeps focus on opener for group with no enabled items, navigates onward, and Escape closes', async () => {
    markAvailable.mockReturnValue(false);
    render(MenuBar);
    await open();
    fireEvent.keyDown(item('Open'), { key: 'ArrowRight' });
    await tick();
    expect(opener('Mark').getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement).toBe(opener('Mark'));
    fireEvent.keyDown(opener('Mark'), { key: 'ArrowRight' });
    await tick();
    expect(document.activeElement).toBe(item('Run command'));
    fireEvent.keyDown(item('Run command'), { key: 'ArrowLeft' });
    await tick();
    expect(document.activeElement).toBe(opener('Mark'));
    fireEvent.keyDown(opener('Mark'), { key: 'Escape' });
    await tick();
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(opener('Mark'));
    expect(run).not.toHaveBeenCalled();
  });

  it('roves trigger tab stop to last focused group', async () => {
    render(MenuBar);
    opener('Files').focus();
    fireEvent.keyDown(opener('Files'), { key: 'ArrowRight' });
    expect(document.activeElement).toBe(opener('Mark'));
    expect(opener('Mark').tabIndex).toBe(0);
    expect(opener('Files').tabIndex).toBe(-1);
    fireEvent.keyDown(opener('Mark'), { key: 'Tab' });
    expect(opener('Mark').tabIndex).toBe(0);
  });

  it('closes empty group when Tab leaves focused opener without trapping navigation', async () => {
    markAvailable.mockReturnValue(false);
    render(MenuBar);
    await open('Mark');
    expect(document.activeElement).toBe(opener('Mark'));
    const global = vi.fn();
    window.addEventListener('keydown', global);
    try {
      fireEvent.keyDown(opener('Mark'), { key: 'Tab' });
      await tick();
      expect(screen.queryByRole('menu')).toBeNull();
      expect(global).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener('keydown', global);
    }
  });

  it('moves focus into dialog opened by menu action and does not reopen behind modal', async () => {
    render(MenuBar);
    render(Dialogs);
    await open();
    fireEvent.click(item('New folder'));
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Cancel' })),
    );
    expect(opener('Files').getAttribute('aria-expanded')).toBe('false');
    fireEvent.keyDown(opener('Files'), { key: 'Enter' });
    await tick();
    expect(screen.queryByRole('menu')).toBeNull();
    expect(run).toHaveBeenCalledExactlyOnceWith('new');
  });

  it('does not swallow global shortcuts while closed trigger has focus', () => {
    render(MenuBar);
    opener('Files').focus();
    const global = vi.fn();
    window.addEventListener('keydown', global);
    try {
      fireEvent.keyDown(opener('Files'), { key: 'F5' });
      fireEvent.keyDown(opener('Files'), { key: 'k', ctrlKey: true });
      expect(global).toHaveBeenCalledTimes(2);
    } finally {
      window.removeEventListener('keydown', global);
    }
  });
  it('prevents menu keys reaching global shortcuts and handles Enter activation once', async () => {
    render(MenuBar);
    await open('Commands');
    const global = vi.fn();
    window.addEventListener('keydown', global);
    try {
      fireEvent.keyDown(item('Run command'), { key: 'Enter' });
      await tick();
      expect(run).toHaveBeenCalledExactlyOnceWith('command');
      expect(global).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener('keydown', global);
    }
  });

  it('FER-429: renders separators that arrow keys skip', async () => {
    render(MenuBar);
    const menu = await open('Commands');
    expect(menu.querySelectorAll('[role="separator"]')).toHaveLength(1);
    expect(item('Run command')).toBe(document.activeElement);
    await fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(item('Run other')).toBe(document.activeElement);
  });

  describe('FER-431 submenu', () => {
    const trigger = () => item('Archive');

    it('ArrowRight opens the submenu and focuses its first item; ArrowLeft and Escape close only the submenu', async () => {
      render(MenuBar);
      await open();
      trigger().focus();
      expect(trigger().getAttribute('aria-haspopup')).toBe('menu');
      expect(trigger().getAttribute('aria-expanded')).toBe('false');
      fireEvent.keyDown(trigger(), { key: 'ArrowRight' });
      await tick();
      expect(trigger().getAttribute('aria-expanded')).toBe('true');
      expect(screen.getByRole('menu', { name: 'Archive' })).toBeTruthy();
      expect(opener('Files').getAttribute('aria-expanded')).toBe('true');
      expect(document.activeElement).toBe(item('Zip'));

      fireEvent.keyDown(item('Zip'), { key: 'ArrowDown' });
      expect(document.activeElement).toBe(item('Tar'));
      fireEvent.keyDown(item('Tar'), { key: 'ArrowDown' });
      expect(document.activeElement).toBe(item('Zip'));
      fireEvent.keyDown(item('Zip'), { key: 'ArrowRight' });
      expect(document.activeElement).toBe(item('Zip'));
      expect(opener('Files').getAttribute('aria-expanded')).toBe('true');

      fireEvent.keyDown(item('Zip'), { key: 'ArrowLeft' });
      await tick();
      expect(screen.queryByRole('menu', { name: 'Archive' })).toBeNull();
      expect(document.activeElement).toBe(trigger());
      expect(opener('Files').getAttribute('aria-expanded')).toBe('true');

      fireEvent.keyDown(trigger(), { key: 'Enter' });
      await tick();
      expect(document.activeElement).toBe(item('Zip'));
      fireEvent.keyDown(item('Zip'), { key: 'Escape' });
      await tick();
      expect(screen.queryByRole('menu', { name: 'Archive' })).toBeNull();
      expect(document.activeElement).toBe(trigger());
      expect(screen.getByRole('menu')).toBeTruthy();

      fireEvent.keyDown(trigger(), { key: 'Escape' });
      await tick();
      expect(screen.queryByRole('menu')).toBeNull();
      expect(document.activeElement).toBe(opener('Files'));
      expect(run).not.toHaveBeenCalled();
    });

    it('Enter on a submenu item runs it once and closes everything', async () => {
      render(MenuBar);
      await open();
      trigger().focus();
      fireEvent.keyDown(trigger(), { key: 'ArrowRight' });
      await tick();
      fireEvent.keyDown(item('Zip'), { key: 'ArrowDown' });
      fireEvent.keyDown(item('Tar'), { key: 'Enter' });
      await tick();
      expect(run).toHaveBeenCalledExactlyOnceWith('tar');
      expect(screen.queryByRole('menu')).toBeNull();
      expect(document.activeElement).toBe(opener('Files'));
    });

    it('top-level End reaches the submenu trigger and ArrowDown wraps without opening it', async () => {
      render(MenuBar);
      const menu = await open();
      fireEvent.keyDown(menu, { key: 'End' });
      expect(document.activeElement).toBe(trigger());
      fireEvent.keyDown(trigger(), { key: 'ArrowDown' });
      expect(document.activeElement).toBe(item('Open'));
      expect(screen.queryByRole('menu', { name: 'Archive' })).toBeNull();
    });

    it('clicking the trigger toggles the submenu', async () => {
      render(MenuBar);
      await open();
      fireEvent.click(trigger());
      await tick();
      expect(screen.getByRole('menu', { name: 'Archive' })).toBeTruthy();
      fireEvent.click(trigger());
      await tick();
      expect(screen.queryByRole('menu', { name: 'Archive' })).toBeNull();
    });
  });
});

describe('FER-577: Alt+letter opens a menu', () => {
  it('underlines the access letters and Alt+F / Alt+W open Files / Show', async () => {
    render(MenuBar);
    const bar = screen.getByRole('menubar');
    expect(Array.from(bar.querySelectorAll('u')).map((u) => u.textContent)).toEqual([
      'F',
      'M',
      'C',
      'w',
    ]);
    fireEvent.keyDown(window, { key: 'f', altKey: true });
    await tick();
    expect(screen.getByRole('menu')).toBeTruthy();
    await vi.waitFor(() => expect(document.activeElement).toBe(item('Open')));
    fireEvent.keyDown(window, { key: 'w', altKey: true });
    await tick();
    await vi.waitFor(() => expect(document.activeElement).toBe(item('Show hidden files')));
    expect(opener('Show').getAttribute('aria-expanded')).toBe('true');
  });

  it('does not touch Alt+D / Alt+B (app shortcuts), Ctrl+Alt, or any Alt while a modal is open', async () => {
    render(MenuBar);
    const d = new KeyboardEvent('keydown', { key: 'd', altKey: true, cancelable: true });
    window.dispatchEvent(d);
    expect(d.defaultPrevented).toBe(false);
    const ctrl = new KeyboardEvent('keydown', {
      key: 'f',
      altKey: true,
      ctrlKey: true,
      cancelable: true,
    });
    window.dispatchEvent(ctrl);
    expect(ctrl.defaultPrevented).toBe(false);
    expect(screen.queryByRole('menu')).toBeNull();

    const host = document.createElement('div');
    host.tabIndex = -1;
    document.body.append(host);
    const modal = dialog(host);
    fireEvent.keyDown(window, { key: 'f', altKey: true });
    await tick();
    expect(screen.queryByRole('menu')).toBeNull();
    modal.destroy();
    host.remove();
  });
});
