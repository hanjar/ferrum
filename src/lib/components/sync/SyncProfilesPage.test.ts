import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';

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
import {
  SyncProfilesError,
  type ProfileSummary,
  type SyncPreview,
  type SyncProfilesBackend,
} from '../../ipc/syncProfiles';

const summary = (over: Partial<ProfileSummary> = {}): ProfileSummary => ({
  id: 'p1',
  name: 'Photos',
  revision: 2,
  left: 'D:\\A',
  right: 'E:\\B',
  modeLabel: 'Mirror left → right',
  deleteLabel: 'Recycle Bin',
  readiness: { status: 'ready' },
  usedBy: 0,
  ...over,
});

const preview = (overwrites = 0, deletes = 0): SyncPreview => ({
  previewId: 'pv',
  summary: {
    total: 4,
    toRight: 3,
    toLeft: 0,
    overwrites,
    deletes,
    conflicts: 0,
    skipped: 0,
    userSkipped: 0,
  },
  conflicts: 0,
  guard: { verdict: 'ok' },
});

function setup(
  list: ProfileSummary[],
  over: Partial<SyncProfilesBackend> = {},
  onclose?: () => void,
) {
  const backend = {
    list: vi.fn(async () => list),
    get: vi.fn(),
    validate: vi.fn(async () => ({ errors: [], identities: {}, kinds: {} })),
    save: vi.fn(),
    delete: vi.fn(async () => ({ status: 'deleted' })),
    preview: vi.fn(async () => preview()),
    run: vi.fn(async () => ({ runId: 1 })),
    ...over,
  } as unknown as SyncProfilesBackend;
  const store = new SyncProfilesStore({ backend, subscribe: async () => () => undefined });
  const { unmount } = render(SyncProfilesPage, { store, onclose });
  return { store, backend, unmount };
}

describe('SyncProfilesPage', () => {
  it('explains what a profile is when there are none (SP-1.1)', async () => {
    setup([]);
    await waitFor(() => expect(screen.getByTestId('sync-empty')).toBeTruthy());
    expect(screen.getByRole('button', { name: 'New sync profile' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Use current panes' })).toBeTruthy();
  });

  it('"Use current panes" opens the editor with the active pane left and the other right (SP-1.2)', async () => {
    const { store } = setup([]);
    await waitFor(() => expect(screen.getByTestId('sync-empty')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Use current panes' }));
    expect((screen.getByLabelText('Left folder') as HTMLInputElement).value).toBe('D:\\Work');
    expect((screen.getByLabelText('Right folder') as HTMLInputElement).value).toBe('E:\\Backup');
    expect(store.editor?.id).toBeUndefined();
  });

  it('lists a Not-ready profile with its reason and no preview button', async () => {
    setup([summary({ readiness: { status: 'notReady', reason: 'rootOffline' } })]);
    await waitFor(() => expect(screen.getByText('Photos')).toBeTruthy());
    expect(screen.getByText('Not ready')).toBeTruthy();
    expect(screen.getByText(/offline or cannot be reached/)).toBeTruthy();
    expect(
      (screen.getByRole('button', { name: 'Preview sync' }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it('previews, asks to confirm overwrites, and only then runs with confirmOverwrite', async () => {
    const { backend } = setup([summary()], { preview: vi.fn(async () => preview(2)) });
    await waitFor(() => expect(screen.getByText('Photos')).toBeTruthy());
    const run = screen.getByRole('button', { name: 'Run now' }) as HTMLButtonElement;
    expect(run.disabled).toBe(true);
    await fireEvent.click(screen.getByRole('button', { name: 'Preview sync' }));
    await waitFor(() => expect(screen.getByTestId('preview-summary')).toBeTruthy());
    expect(screen.getByTestId('preview-summary').textContent).toContain('2 overwrites');
    await fireEvent.click(run);
    expect(screen.getByRole('alertdialog', { name: 'Confirm run' })).toBeTruthy();
    expect(backend.run).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm and run' }));
    await waitFor(() => expect(backend.run).toHaveBeenCalledTimes(1));
    expect(backend.run).toHaveBeenCalledWith({ id: 'p1', previewId: 'pv', confirmOverwrite: true });
  });

  it('refuses to delete a used profile and says how many automations use it (SP-9.5)', async () => {
    setup([summary({ usedBy: 2 })], {
      delete: vi.fn(async () => Promise.reject(new SyncProfilesError('in_use'))),
    });
    await waitFor(() => expect(screen.getByText('Photos')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    expect(screen.getByRole('alertdialog', { name: 'Confirm delete' })).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Delete profile' }));
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toBe(
        'Used by 2 automations — delete or change them first.',
      ),
    );
  });

  it('shows the stale-run message and keeps the user informed when a dialog is canceled', async () => {
    setup([summary()], {
      run: vi.fn(async () => Promise.reject(new SyncProfilesError('canceled'))),
    });
    await waitFor(() => expect(screen.getByText('Photos')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Preview sync' }));
    await waitFor(() => expect(screen.getByTestId('preview-summary')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Run now' }));
    await waitFor(() =>
      expect(screen.getByText(/Canceled in the confirmation dialog/)).toBeTruthy(),
    );
  });

  it('asks before a Recycle Bin run with deletions and names them (SP-9.4)', async () => {
    const { backend } = setup([summary()], { preview: vi.fn(async () => preview(0, 3)) });
    await waitFor(() => expect(screen.getByText('Photos')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Preview sync' }));
    await waitFor(() => expect(screen.getByTestId('preview-summary')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Run now' }));
    const dlg = screen.getByRole('alertdialog', { name: 'Confirm run' });
    expect(dlg.textContent?.replace(/\s+/g, ' ')).toContain(
      '3 items will be deleted (Recycle Bin)',
    );
    expect(backend.run).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm and run' }));
    await waitFor(() => expect(backend.run).toHaveBeenCalledTimes(1));
    expect(backend.run).toHaveBeenCalledWith({
      id: 'p1',
      previewId: 'pv',
      confirmOverwrite: false,
    });
  });

  describe('leaving a dirty editor', () => {
    async function openDirty() {
      const onclose = vi.fn();
      const r = setup([], {}, onclose);
      await waitFor(() => expect(screen.getByTestId('sync-empty')).toBeTruthy());
      await fireEvent.click(screen.getByRole('button', { name: 'New sync profile' }));
      await fireEvent.input(screen.getByLabelText('Profile name'), { target: { value: 'x' } });
      return { ...r, onclose };
    }

    it('X asks first, keeps the editor on "Keep editing" and closes on "Discard"', async () => {
      const { store, onclose } = await openDirty();
      await fireEvent.click(screen.getByRole('button', { name: 'Close' }));
      expect(screen.getByRole('alertdialog', { name: 'Discard changes' })).toBeTruthy();
      expect(onclose).not.toHaveBeenCalled();
      await fireEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
      expect(store.editor).not.toBeNull();
      await fireEvent.click(screen.getByRole('button', { name: 'Close' }));
      await fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }));
      expect(onclose).toHaveBeenCalledTimes(1);
      expect(store.editor).toBeNull();
    });

    it('Cancel and Escape take the same path and return to the list', async () => {
      const { store, onclose } = await openDirty();
      await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
      expect(screen.getByRole('alertdialog', { name: 'Discard changes' })).toBeTruthy();
      await fireEvent.keyDown(screen.getByRole('dialog', { name: 'Sync profiles' }), {
        key: 'Escape',
      });
      expect(screen.queryByRole('alertdialog', { name: 'Discard changes' })).toBeNull();
      await fireEvent.keyDown(screen.getByRole('dialog', { name: 'Sync profiles' }), {
        key: 'Escape',
      });
      expect(screen.getByRole('alertdialog', { name: 'Discard changes' })).toBeTruthy();
      await fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }));
      expect(store.editor).toBeNull();
      expect(onclose).not.toHaveBeenCalled();
    });

    it('a clean editor closes straight away and unmounting leaves no editor behind', async () => {
      const { store, unmount } = setup([], {}, vi.fn());
      await waitFor(() => expect(screen.getByTestId('sync-empty')).toBeTruthy());
      await fireEvent.click(screen.getByRole('button', { name: 'New sync profile' }));
      await fireEvent.input(screen.getByLabelText('Profile name'), { target: { value: 'x' } });
      expect(store.editor).not.toBeNull();
      unmount();
      expect(store.editor).toBeNull();
    });
  });

  it('a running status can be hidden to release the buttons', async () => {
    const store = new SyncProfilesStore({
      backend: {
        list: async () => [summary()],
        preview: async () => preview(),
        run: async () => ({ runId: 4 }),
      } as unknown as SyncProfilesBackend,
      subscribe: async () => {
        throw new Error('no events');
      },
    });
    render(SyncProfilesPage, { store });
    await waitFor(() => expect(screen.getByText('Photos')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Preview sync' }));
    await waitFor(() => expect(screen.getByTestId('preview-summary')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Run now' }));
    await waitFor(() => expect(screen.getByText(/Progress isn't available/)).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Hide status' }));
    expect(screen.queryByText(/Progress isn't available/)).toBeNull();
  });
});
