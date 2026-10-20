import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import Keybar from './Keybar.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { dialog, resetModalDepth } from '../dialogKit';

let unregister: (() => void) | undefined;

afterEach(() => {
  cleanup();
  dialogs.close();
  unregister?.();
  unregister = undefined;
  ui.activePane = 0;
});

function mount(selected: string[]) {
  const pane: PaneApi = {
    path: 'C:\\bench',
    selectedPaths: () => selected,
    cursorEntry: () =>
      selected.length ? { name: 'notes.txt', path: selected[0], isDir: false } : undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  ui.activePane = 0;
  unregister = registerPane(0, pane);
  return render(Keybar);
}

describe('keybar registry parity', () => {
  it('disables selection actions without selected files while New folder stays available', () => {
    const { getByRole } = mount([]);
    for (const label of ['F2 Rename', 'F5 Copy', 'F6 Move', 'F8 Delete']) {
      expect((getByRole('button', { name: label }) as HTMLButtonElement).disabled).toBe(true);
    }
    expect((getByRole('button', { name: 'F7 New folder' }) as HTMLButtonElement).disabled).toBe(
      false,
    );
    fireEvent.click(getByRole('button', { name: 'F8 Delete' }));
    expect(dialogs.current).toBeNull();
  });

  it('FER-364: Shift-click F8 opens the permanent delete (pre-checked confirm)', () => {
    const beginDelete = vi.spyOn(dialogs, 'beginDelete');
    const { getByRole } = mount(['C:\\bench\\notes.txt']);
    fireEvent.click(getByRole('button', { name: 'F8 Delete' }), { shiftKey: true });
    expect(beginDelete).toHaveBeenCalledWith(true);
    beginDelete.mockRestore();
  });
});

describe('keybar behind a modal (FER-577 S3)', () => {
  it('does not run F7 / F8 while any modal is open', async () => {
    const { getByRole } = mount(['C:\bench\notes.txt']);
    const host = document.createElement('div');
    host.tabIndex = -1;
    document.body.append(host);
    const act = dialog(host);
    await fireEvent.click(getByRole('button', { name: 'F8 Delete' }));
    await fireEvent.click(getByRole('button', { name: 'F7 New folder' }));
    expect(dialogs.current).toBeNull();
    act.destroy();
    host.remove();
    resetModalDepth();
    await fireEvent.click(getByRole('button', { name: 'F7 New folder' }));
    expect(dialogs.current?.kind).toBe('mkdir');
  });
});
