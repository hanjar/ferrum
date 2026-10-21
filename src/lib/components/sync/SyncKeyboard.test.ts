// FER-577 B4: keyboard of the Sync profiles page (list rows, confirmations, editor access letters).
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';

vi.mock('../../stores/panesRegistry.svelte', () => ({
  activePane: () => ({ path: 'D:\\Work' }),
  inactivePane: () => ({ path: 'E:\\Backup' }),
}));
vi.mock('../../ipc/pathSuggest', () => ({
  pathSuggest: vi.fn(async () => ({
    items: [],
    truncated: false,
    timedOut: false,
    needsConfirm: false,
    host: null,
  })),
}));

import SyncProfilesPage from './SyncProfilesPage.svelte';
import { SyncProfilesStore } from '../../stores/syncProfilesStore.svelte';
import type { ProfileSummary, SyncProfilesBackend } from '../../ipc/syncProfiles';
import { duplicateMnemonics, modalDepth, resetModalDepth } from '../../dialogKit';

afterEach(() => {
  cleanup();
  resetModalDepth();
});

const summary = (id: string, name: string): ProfileSummary => ({
  id,
  name,
  revision: 1,
  left: 'D:\\A',
  right: 'E:\\B',
  modeLabel: 'Mirror left → right',
  deleteLabel: 'Recycle Bin',
  readiness: { status: 'ready' },
  usedBy: 0,
});

function setup(list: ProfileSummary[]) {
  const backend = {
    list: vi.fn(async () => list),
    get: vi.fn(),
    validate: vi.fn(async () => ({ errors: [], identities: {}, kinds: {} })),
    save: vi.fn(),
    delete: vi.fn(async () => ({ status: 'deleted' })),
    preview: vi.fn(),
    run: vi.fn(),
  } as unknown as SyncProfilesBackend;
  const store = new SyncProfilesStore({ backend, subscribe: async () => () => undefined });
  const onclose = vi.fn();
  render(SyncProfilesPage, { store, onclose });
  return { store, backend, onclose };
}
const modal = () =>
  document.querySelector('[role="dialog"][aria-label="Sync profiles"]') as HTMLElement;
const key = (el: Element, k: string, init: KeyboardEventInit = {}) =>
  fireEvent.keyDown(el, { key: k, ...init });

describe('Sync profiles keyboard (FER-577 B4)', () => {
  it('rows are one stop each: arrows and Home/End walk them, Enter edits', async () => {
    const { store, backend } = setup([
      summary('p1', 'Photos'),
      summary('p2', 'Docs'),
      summary('p3', 'Music'),
    ]);
    await screen.findByText('Docs');
    expect(duplicateMnemonics(modal())).toEqual([]);
    const rows = [...document.querySelectorAll<HTMLElement>('li.profile')];
    expect(rows.map((r) => r.tabIndex)).toEqual([0, -1, -1]);
    rows[0].focus();
    await key(rows[0], 'ArrowDown');
    expect(document.activeElement).toBe(rows[1]);
    await key(rows[1], 'End');
    expect(document.activeElement).toBe(rows[2]);
    await key(rows[2], 'Home');
    expect(document.activeElement).toBe(rows[0]);
    (backend.get as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('x'));
    await key(rows[1], 'Enter');
    expect(backend.get).toHaveBeenCalledWith('p2');
    expect(store).toBeTruthy();
  });

  it('Delete on a row opens the confirmation with Cancel focused; Enter there does not delete; Esc cancels', async () => {
    const { store, backend } = setup([summary('p1', 'Photos')]);
    await screen.findByText('Photos');
    const row = document.querySelector<HTMLElement>('li.profile')!;
    row.focus();
    await key(row, 'Delete');
    await waitFor(() => expect(store.confirmDeleteId).toBe('p1'));
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    await waitFor(() => expect(document.activeElement).toBe(cancel));
    expect(
      screen.getByRole('button', { name: 'Delete profile' }).hasAttribute('data-mnemonic'),
    ).toBe(false);
    expect(modalDepth()).toBe(1);
    await key(cancel, 'Enter');
    expect(backend.delete).not.toHaveBeenCalled();
    await key(cancel, 'Escape');
    expect(store.confirmDeleteId).toBeNull();
    expect(backend.delete).not.toHaveBeenCalled();
  });

  it('Alt+N opens the editor; its access letters are unique and Alt+N focuses the name', async () => {
    const { store } = setup([summary('p1', 'Photos')]);
    await screen.findByText('Photos');
    await key(modal(), 'n', { altKey: true });
    await waitFor(() => expect(store.editor).toBeTruthy());
    await screen.findByLabelText('Profile name');
    expect(duplicateMnemonics(modal())).toEqual([]);
    await key(modal(), 'n', { altKey: true });
    expect(document.activeElement).toBe(screen.getByLabelText('Profile name'));
    // "Delete permanently" is only reachable with arrows / Tab: no accelerator.
    const perm = screen.getByRole('radio', { name: /Delete permanently/ });
    expect(perm.closest('label')?.hasAttribute('data-mnemonic')).toBe(false);
    // Mode radios rove with the arrows and select.
    const mirror = screen.getByRole('radio', { name: /Mirror/ });
    mirror.focus();
    await key(mirror, 'ArrowRight');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: /Two-way/ }));
    await key(document.activeElement!, 'Home');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: /Update/ }));
  });
});
