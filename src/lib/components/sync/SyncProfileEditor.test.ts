import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import SyncProfileEditor from './SyncProfileEditor.svelte';
import { SyncProfilesStore } from '../../stores/syncProfilesStore.svelte';
import type { ProfileDto, SyncProfilesBackend, ValidateResult } from '../../ipc/syncProfiles';

vi.mock('../../ipc/pathSuggest', () => ({
  pathSuggest: vi.fn(async () => ({
    items: [],
    truncated: false,
    timedOut: false,
    needsConfirm: false,
    host: null,
  })),
}));

const ok: ValidateResult = {
  errors: [],
  identities: {
    left: { kind: 'local', volumeSerial: 'AAAA', fileId: '1', fsName: 'NTFS', drive: 'fixed' },
  },
  kinds: { left: 'fixed', right: 'fixed' },
};

const profile = (over: Partial<ProfileDto> = {}): ProfileDto => ({
  id: 'p1',
  name: 'Photos',
  revision: 3,
  policy: {
    left: 'D:\\A',
    right: 'E:\\B',
    mode: 'mirror',
    from: 'left',
    deletePolicy: 'recycleBin',
    compare: 'sizeAndDate',
    include: [],
    exclude: ['*.tmp', 'node_modules\\'],
    guard: { enabled: true, maxCount: 50, maxPercent: 20 },
  },
  identities: {},
  changes: [
    {
      revision: 3,
      changedMs: 1,
      changes: [{ field: 'Right root', from: 'E:\\Old', to: 'E:\\B' }],
    },
  ],
  createdMs: 1,
  updatedMs: 1,
  readiness: { status: 'ready' },
  ...over,
});

function setup(over: Partial<SyncProfilesBackend> = {}, props: Record<string, unknown> = {}) {
  const backend = {
    list: vi.fn(async () => []),
    get: vi.fn(async () => profile()),
    validate: vi.fn(async () => ok),
    save: vi.fn(async () => ({ status: 'saved' as const, profile: profile({ revision: 4 }) })),
    delete: vi.fn(async () => ({ status: 'deleted' })),
    preview: vi.fn(),
    run: vi.fn(),
    ...over,
  } as unknown as SyncProfilesBackend & { validate: ReturnType<typeof vi.fn> };
  const store = new SyncProfilesStore({ backend, subscribe: async () => () => undefined });
  return { store, backend, props };
}

async function renderEditor(
  s: ReturnType<typeof setup>,
  open: (store: SyncProfilesStore) => Promise<void> | void,
) {
  await open(s.store);
  return render(SyncProfileEditor, {
    store: s.store,
    editor: s.store.editor!,
    activePath: 'D:\\Active',
    inactivePath: 'E:\\Inactive',
    ...s.props,
  });
}

describe('SyncProfileEditor', () => {
  it('shows the revision and the last change of an existing profile (SP-6.3)', async () => {
    const s = setup();
    await renderEditor(s, (st) => st.openEdit('p1'));
    expect(screen.getByTestId('editor-revision').textContent).toContain('Revision 3');
    expect(screen.getByTestId('editor-revision').textContent).toContain(
      'Right root: E:\\Old → E:\\B',
    );
  });

  it('mirrors the backend validation errors and disables Save', async () => {
    const s = setup({
      validate: vi.fn(async () => ({
        errors: [
          { field: 'name', code: 'name_required' },
          { field: 'right', code: 'root_missing' },
        ],
        identities: {},
        kinds: {},
      })),
    });
    await renderEditor(s, (st) => st.openNew({ left: 'D:\\A', right: 'Z:\\none' }));
    await waitFor(() => {
      const problems = screen.getByLabelText('Problems');
      expect(problems.textContent).toContain('Enter a name for this profile.');
      expect(problems.textContent).toContain("This folder doesn't exist.");
    });
    const save = screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
  });

  it('"Use active pane" fills the left root and the inactive pane fills the right root (SP-9.2)', async () => {
    const s = setup();
    await renderEditor(s, (st) => st.openNew());
    const buttons = screen.getAllByRole('button', { name: 'Use active pane' });
    await fireEvent.click(buttons[0]);
    await fireEvent.click(buttons[1]);
    expect((screen.getByLabelText('Left folder') as HTMLInputElement).value).toBe('D:\\Active');
    expect((screen.getByLabelText('Right folder') as HTMLInputElement).value).toBe('E:\\Inactive');
    expect(s.store.editor?.form.left).toBe('D:\\Active');
    expect(s.store.editor?.dirty).toBe(true);
  });

  it('toggles mode and side and shows one sentence per mode (SP-2.4)', async () => {
    const s = setup();
    await renderEditor(s, (st) => st.openNew());
    const sentence = () => screen.getByTestId('mode-sentence').textContent ?? '';
    expect(sentence()).toMatch(/^Left always wins.*Extra files on the right are kept/);

    await fireEvent.click(screen.getByRole('radio', { name: 'Mirror' }));
    expect(s.store.editor?.form.mode).toBe('mirror');
    expect(sentence()).toMatch(/right becomes an exact copy/);

    await fireEvent.click(screen.getByRole('radio', { name: 'Right → Left' }));
    expect(s.store.editor?.form.from).toBe('right');
    expect(sentence()).toMatch(/^Right always wins.*left becomes an exact copy/);

    await fireEvent.click(screen.getByRole('radio', { name: 'Two-way' }));
    expect(screen.queryByRole('radio', { name: 'Right → Left' })).toBeNull();
    expect(screen.getByLabelText('Propagate deletions')).toBeTruthy();
    expect(sentence()).toMatch(/Conflicts are held/);
  });

  it('disables the Recycle Bin card with the reason when the receiving side is not a fixed drive (SP-3.1)', async () => {
    const s = setup({
      validate: vi.fn(async () => ({
        errors: [],
        identities: {},
        kinds: { left: 'fixed', right: 'network' },
      })),
    });
    await renderEditor(s, (st) => {
      st.openNew({ left: 'D:\\A', right: '\\\\nas\\b', mode: 'mirror', from: 'left' });
    });
    await waitFor(() => expect(s.store.editor?.kinds.right).toBe('network'));
    const recycle = screen.getByRole('radio', { name: /Recycle Bin/ }) as HTMLInputElement;
    expect(recycle.disabled).toBe(true);
    const card = recycle.closest('label')!;
    expect(card.textContent).toContain(
      "Recycle Bin isn't available on \\\\nas\\b — choose a versioning folder or permanent delete",
    );
    expect(
      (screen.getByRole('radio', { name: /Versioning folder/ }) as HTMLInputElement).disabled,
    ).toBe(false);
    expect(
      (screen.getByRole('radio', { name: /Delete permanently/ }) as HTMLInputElement).disabled,
    ).toBe(false);
  });

  it('keeps the Recycle Bin card enabled for a one-way profile (it never deletes)', async () => {
    const s = setup({
      validate: vi.fn(async () => ({ errors: [], identities: {}, kinds: { right: 'network' } })),
    });
    await renderEditor(s, (st) => st.openNew({ left: 'D:\\A', right: '\\\\nas\\b' }));
    await waitFor(() => expect(s.store.editor?.kinds.right).toBe('network'));
    expect((screen.getByRole('radio', { name: /Recycle Bin/ }) as HTMLInputElement).disabled).toBe(
      false,
    );
  });

  it('shows the live exclude example (SP-4.5)', async () => {
    const s = setup();
    await renderEditor(s, (st) => st.openEdit('p1'));
    expect(screen.getByTestId('exclude-example').textContent).toBe(
      'Excluded: 2 patterns · e.g. `*.tmp`, `node_modules\\`',
    );
  });

  it('a stale save reloads the current version with the explanation (SP-6.2)', async () => {
    const get = vi
      .fn()
      .mockResolvedValueOnce(profile())
      .mockResolvedValue(profile({ name: 'Changed elsewhere', revision: 5 }));
    const s = setup({ get, save: vi.fn(async () => ({ status: 'stale' as const })) });
    await renderEditor(s, (st) => st.openEdit('p1'));
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(s.store.editor?.notice?.text).toBe(
        'This profile was changed elsewhere. Review the current version.',
      ),
    );
    expect(s.store.editor?.revision).toBe(5);
    expect(s.store.editor?.form.name).toBe('Changed elsewhere');
  });

  it('shows the permanent-delete and guard-off notes without sending any confirmation text', async () => {
    const s = setup();
    await renderEditor(s, (st) => st.openNew({ name: 'x', left: 'D:\\A', right: 'E:\\B' }));
    await fireEvent.click(screen.getByRole('radio', { name: /Delete permanently/ }));
    await fireEvent.click(screen.getByLabelText('Stop runs that would delete a lot'));
    expect(document.body.textContent).toContain('Windows dialog');
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(s.backend.save).toHaveBeenCalled());
    const sent = JSON.stringify((s.backend.save as ReturnType<typeof vi.fn>).mock.calls[0][0]);
    expect(sent).toContain('"deletePolicy":"permanent"');
    expect(sent).not.toMatch(/confirm|prompt|caption/i);
  });
});
