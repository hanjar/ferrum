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

const preview = (overwrites = 0): SyncPreview => ({
  previewId: 'pv',
  summary: {
    total: 4,
    toRight: 3,
    toLeft: 0,
    overwrites,
    deletes: 1,
    conflicts: 0,
    skipped: 0,
    userSkipped: 0,
  },
  conflicts: 0,
  guard: { verdict: 'ok' },
});

function setup(list: ProfileSummary[], over: Partial<SyncProfilesBackend> = {}) {
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
  render(SyncProfilesPage, { store });
  return { store, backend };
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
    expect(screen.getByRole('alertdialog', { name: 'Confirm overwrite' })).toBeTruthy();
    expect(backend.run).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Overwrite 2 files' }));
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
});
