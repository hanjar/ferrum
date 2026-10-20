// FER-576: navigate -> archive -> delete as a chain of runs, through the REAL executor, facade, registry and
// adapters (only the IPC boundary is faked). Pins the safety properties of the owner's request:
//   - nothing is deleted before the user clicks the warning card's confirm button,
//   - the original is deleted only after the archive was created AND passed Test Archive,
//   - it deletes exactly what was archived, and only to the Recycle Bin,
//   - any failure, refusal or cancel stops the rest of the chain.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({ archiveCreate: vi.fn() }));
vi.mock('../ipc/archive', () => ({ archiveCreate: ipc.archiveCreate }));

const { entry } = await import('./contextTestKit');
const { buildKit, ownerChain, CSV, FOLDER, OTHER, USER_TEXT } = await import('./chainTestKit');
import type { Kit } from './chainTestKit';

const ARCHIVE = `${FOLDER}\\pc_20250814.7z`;

function start(kit: Kit): void {
  ipc.archiveCreate.mockImplementation(async () => {
    kit.order.push('archive');
    return ARCHIVE;
  });
  kit.submit();
}
const awaiting = (kit: Kit, runId: string) =>
  vi.waitFor(() => expect(kit.stateOf(runId)).toBe('awaitingApproval'));
const settled = (kit: Kit, runId: string, state: string) =>
  vi.waitFor(() => expect(kit.stateOf(runId)).toBe(state));

beforeEach(() => {
  ipc.archiveCreate.mockReset();
});

describe('the owner example', () => {
  it('opens the file, archives it as 7z ultra, tests the archive, then deletes it to the Recycle Bin', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    start(kit);
    // Step 1 (navigate) runs by itself: read-only, no card to approve.
    await awaiting(kit, 'run-1-c1');
    expect(kit.stateOf('run-1')).toBe('succeeded');
    expect(kit.last('run-1')?.navigate?.summary).toContain('pc_202508014.csv');
    expect(kit.navigate.open).toHaveBeenCalledWith('left', {
      kind: 'file',
      path: CSV,
      folder: FOLDER,
    });
    // Step 2 (archive) waits for its own approval; nothing was created yet.
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(kit.last('run-1-c1')?.plan?.operation).toMatchObject({
      kind: 'archive.create',
      outputName: 'pc_20250814.7z',
    });
    kit.executor.approve('run-1-c1');
    // Step 3 (delete) exists only after the archive passed Test Archive, and waits for its own approval.
    await awaiting(kit, 'run-1-c2');
    expect(kit.stateOf('run-1-c1')).toBe('succeeded');
    const call = ipc.archiveCreate.mock.calls[0];
    expect(call[0]).toEqual([CSV]);
    expect(call[2]).toBe('sevenz');
    expect(call[3]).toBe('skip');
    expect(call[5]).toBe('max');
    expect(kit.verify).toHaveBeenCalledWith(ARCHIVE);
    const card = kit.last('run-1-c2');
    expect(card?.plan?.operation).toMatchObject({
      kind: 'items.delete',
      sourceDirectory: FOLDER,
    });
    expect(card?.chain).toMatchObject({ index: 2, total: 3 });
    expect(card?.chain?.note).toContain('lolos uji');
    expect(card?.warnings?.join(' ')).toContain('Recycle Bin');
    // Still nothing deleted: the warning card must be confirmed.
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
    kit.executor.approve('run-1-c2');
    await settled(kit, 'run-1-c2', 'succeeded');
    expect(kit.recycle.deleteToRecycle).toHaveBeenCalledTimes(1);
    expect(kit.recycle.deleteToRecycle).toHaveBeenCalledWith([CSV]);
    // Order: open -> archive -> test archive -> delete.
    const at = (prefix: string) => kit.order.findIndex((item) => item.startsWith(prefix));
    expect(at('open:')).toBeLessThan(at('archive'));
    expect(at('archive')).toBeLessThan(at('verify:'));
    expect(at('verify:')).toBeLessThan(at('recycle:'));
    // The neighbour that was never selected is untouched.
    expect(JSON.stringify(kit.recycle.deleteToRecycle.mock.calls)).not.toContain('keep-me');
  });

  it('binds "active panel" to the side active at submit, even if the user clicks the other panel', async () => {
    const kit = buildKit({ events: [ownerChain()], active: 'left' });
    start(kit);
    await awaiting(kit, 'run-1-c1');
    kit.host.active = 'right';
    kit.executor.approve('run-1-c1');
    await awaiting(kit, 'run-1-c2');
    expect(ipc.archiveCreate.mock.calls[0][0]).toEqual([CSV]);
    expect(kit.last('run-1-c2')?.plan?.sourceSnapshot.activePaneAtSubmit).toBe('right');
    expect(kit.navigate.open.mock.calls[0][0]).toBe('left');
  });

  it('never sends a file or folder name from the panels to the provider', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    start(kit);
    await awaiting(kit, 'run-1-c1');
    const sent = JSON.stringify(kit.requests);
    expect(kit.requests).toHaveLength(1); // one provider turn; steps 2 and 3 are local
    expect(sent).not.toContain('secret-neighbor');
    expect(sent).not.toContain('keep-me');
    expect(sent).not.toContain('start.txt');
    expect(kit.requests[0].capabilities).toEqual(
      expect.arrayContaining(['archive.create', 'items.delete', 'pane.navigate']),
    );
  });
});

describe('the delete step is gated by a verified archive of exactly the same selection', () => {
  async function toArchiveDone(kit: Kit): Promise<void> {
    start(kit);
    await awaiting(kit, 'run-1-c1');
    kit.executor.approve('run-1-c1');
  }

  it('a failed Test Archive stops the chain: no delete run, nothing deleted', async () => {
    const kit = buildKit({
      events: [ownerChain()],
      verify: async () => ({ ok: false, message: '1 entri rusak' }),
    });
    await toArchiveDone(kit);
    await settled(kit, 'run-1-c1', 'failed');
    expect(kit.last('run-1-c1')?.errorMessage).toContain('uji arsip GAGAL');
    expect(kit.last('run-1-c1')?.errorMessage).toContain('TIDAK dihapus');
    expect(kit.runIds()).toEqual(['run-1', 'run-1-c1']);
    expect(kit.recycle.check).not.toHaveBeenCalled();
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a Test Archive that throws also stops the chain', async () => {
    const kit = buildKit({
      events: [ownerChain()],
      verify: async () => {
        throw new Error('boom');
      },
    });
    await toArchiveDone(kit);
    await settled(kit, 'run-1-c1', 'failed');
    expect(kit.runIds()).toEqual(['run-1', 'run-1-c1']);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('without a Test Archive port the original is never deleted (fail closed)', async () => {
    const kit = buildKit({ events: [ownerChain()], noVerify: true });
    await toArchiveDone(kit);
    await settled(kit, 'run-1-c1', 'failed');
    expect(kit.last('run-1-c1')?.errorMessage).toContain('tidak dapat diuji');
    expect(kit.runIds()).toEqual(['run-1', 'run-1-c1']);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('if the selection changed after the archive, the delete is refused (it must be exactly what was archived)', async () => {
    // The change happens while the archive is being tested, i.e. before the delete card is prepared.
    const holder: { kit?: Kit } = {};
    const kit = buildKit({
      events: [ownerChain()],
      verify: async () => {
        holder.kit!.host.left.selected = [CSV, OTHER];
        return { ok: true, message: 'ok' };
      },
    });
    holder.kit = kit;
    await toArchiveDone(kit);
    await settled(kit, 'run-1-c2', 'stale');
    expect(kit.last('run-1-c2')?.errorMessage).toContain('persis yang diarsipkan');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a user who clears the selection after the archive gets a question, not a delete', async () => {
    const holder: { kit?: Kit } = {};
    const kit = buildKit({
      events: [ownerChain()],
      verify: async () => {
        holder.kit!.host.left.selected = [];
        return { ok: true, message: 'ok' };
      },
    });
    holder.kit = kit;
    await toArchiveDone(kit);
    await settled(kit, 'run-1-c2', 'needsInput');
    expect(kit.last('run-1-c2')?.errorMessage).toContain('Belum ada item');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });
});

describe('any refusal, failure or cancel stops the rest', () => {
  it('cancelling the archive card creates no further run and writes nothing', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    start(kit);
    await awaiting(kit, 'run-1-c1');
    kit.executor.cancel('run-1-c1');
    expect(kit.stateOf('run-1-c1')).toBe('cancelled');
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(kit.runIds()).toEqual(['run-1', 'run-1-c1']);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('cancelling the delete card deletes nothing', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    start(kit);
    await awaiting(kit, 'run-1-c1');
    kit.executor.approve('run-1-c1');
    await awaiting(kit, 'run-1-c2');
    kit.executor.cancel('run-1-c2');
    expect(kit.stateOf('run-1-c2')).toBe('cancelled');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a failed archive (engine error) stops the chain: no verification, no delete', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    ipc.archiveCreate.mockRejectedValue(
      Object.assign(new Error('disk full'), { code: 'io', name: 'FsError' }),
    );
    kit.submit();
    await awaiting(kit, 'run-1-c1');
    kit.executor.approve('run-1-c1');
    await vi.waitFor(() => expect(['failed', 'unknownOutcome']).toContain(kit.stateOf('run-1-c1')));
    expect(kit.verify).not.toHaveBeenCalled();
    expect(kit.runIds()).toEqual(['run-1', 'run-1-c1']);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a path that does not exist ends the chain at step 1', async () => {
    const kit = buildKit({ events: [ownerChain()], disk: {} });
    start(kit);
    await settled(kit, 'run-1', 'failed');
    expect(kit.last('run-1')?.errorMessage).toContain('tidak ditemukan');
    expect(kit.navigate.open).not.toHaveBeenCalled();
    expect(kit.runIds()).toEqual(['run-1']);
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('a pane that does not show the folder ends the chain at step 1', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    kit.navigate.open.mockResolvedValue(false);
    start(kit);
    await settled(kit, 'run-1', 'failed');
    expect(kit.runIds()).toEqual(['run-1']);
  });
});

describe('navigation only ever opens a path the user typed', () => {
  it('a forged navigate event with a path that is not in the request opens nothing', async () => {
    const kit = buildKit({
      events: [{ kind: 'paneNavigate', pane: 'left', path: 'C:\\Windows\\System32' }],
    });
    start(kit);
    await settled(kit, 'run-1', 'failed');
    expect(kit.last('run-1')?.errorMessage).toContain('tidak tertulis di permintaan');
    expect(kit.navigate.resolve).not.toHaveBeenCalled();
    expect(kit.navigate.open).not.toHaveBeenCalled();
  });

  it('a forged chain whose first path is not in the request opens nothing and runs nothing', async () => {
    const forged = ownerChain();
    if (forged.kind !== 'chain') throw new Error('test setup');
    forged.steps[0] = { capability: 'pane.navigate', pane: 'activeAtSubmit', path: 'C:\\Users' };
    const kit = buildKit({ events: [forged] });
    start(kit);
    await settled(kit, 'run-1', 'failed');
    expect(kit.navigate.open).not.toHaveBeenCalled();
    expect(kit.runIds()).toEqual(['run-1']);
  });

  it('a single typed navigation opens the folder and ends, with no plan, ledger or approval', async () => {
    const kit = buildKit({
      events: [{ kind: 'paneNavigate', pane: 'right', path: FOLDER }],
      userText: `buka ${FOLDER} di panel kanan`,
    });
    kit.submit();
    await settled(kit, 'run-1', 'succeeded');
    expect(kit.navigate.open).toHaveBeenCalledWith('right', { kind: 'dir', path: FOLDER });
    expect(kit.last('run-1')?.plan).toBeUndefined();
  });

  it('is not offered, and a forced event is blocked, when no navigation port exists', async () => {
    const kit = buildKit({ events: [ownerChain()], noNavigate: true });
    start(kit);
    await settled(kit, 'run-1', 'blockedCapability');
    expect(kit.requests[0].capabilities).not.toContain('pane.navigate');
  });
});

describe('the delete card', () => {
  const standalone = {
    kind: 'intent' as const,
    intent: {
      schemaVersion: 1 as const,
      requestId: 'req_1',
      intent: {
        capability: 'items.delete' as const,
        sourcePane: 'left' as const,
        selector: { kind: 'selected' as const },
      },
    },
  };
  function withSelection(kit: Kit) {
    kit.host.left.path = FOLDER;
    kit.host.left.all = [
      entry('pc_202508014.csv', { path: CSV }),
      entry('keep-me.txt', { path: OTHER }),
      entry('sub', { path: `${FOLDER}\\sub`, isDir: true }),
    ];
    kit.host.left.selected = [CSV, `${FOLDER}\\sub`];
  }

  it('is never auto-approved, even with auto-create on, and nothing is deleted before the click', async () => {
    const kit = buildKit({ events: [standalone], autoCreate: true });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
    const card = kit.last('run-1');
    expect(card?.plan?.preview.itemCount).toBe(2);
    expect(card?.warnings?.join(' ')).toContain('1 folder');
    expect(card?.scopeNote).toContain('Recycle Bin');
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'succeeded');
    expect(kit.recycle.deleteToRecycle).toHaveBeenCalledWith([CSV, `${FOLDER}\\sub`]);
  });

  it('lists the item names locally for the warning card', async () => {
    const kit = buildKit({ events: [standalone] });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    const page = await kit.executor.listItems('run-1', 0, 10);
    expect(page.total).toBe(2);
    expect(JSON.stringify(page.rows)).toContain('pc_202508014.csv');
  });

  it('with nothing selected it asks the user to select; it never falls back to "everything"', async () => {
    const kit = buildKit({ events: [standalone] });
    withSelection(kit);
    kit.host.left.selected = [];
    kit.submit();
    await settled(kit, 'run-1', 'needsInput');
    expect(kit.last('run-1')?.errorMessage).toContain('Belum ada item');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('refuses the whole plan when any item cannot go to the Recycle Bin (no permanent fallback)', async () => {
    const kit = buildKit({
      events: [standalone],
      recycleCheck: (paths) =>
        paths.map((path) =>
          path === CSV ? { path, ok: false, reason: 'no_recycle_bin' } : { path, ok: true },
        ),
    });
    withSelection(kit);
    kit.submit();
    await settled(kit, 'run-1', 'needsInput');
    const message = kit.last('run-1')?.errorMessage ?? '';
    expect(message).toContain('tidak bisa dipindahkan ke Recycle Bin');
    expect(message).toContain('pc_202508014.csv');
    expect(message).toContain('tidak pernah menghapus permanen');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('refuses when the eligibility check itself fails (fail closed)', async () => {
    const kit = buildKit({
      events: [standalone],
      recycleCheck: () => {
        throw new Error('ipc down');
      },
    });
    withSelection(kit);
    kit.submit();
    await settled(kit, 'run-1', 'needsInput');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('reports a backend refusal as a failure with nothing deleted and never retries', async () => {
    const kit = buildKit({
      events: [standalone],
      recycleDelete: async () => {
        throw Object.assign(new Error('refused'), { code: 'unsupported' });
      },
    });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'failed');
    expect(kit.last('run-1')?.errorMessage).toContain('tidak ada yang dihapus');
    expect(kit.last('run-1')?.errorMessage).toContain('Recycle Bin');
    expect(kit.recycle.deleteToRecycle).toHaveBeenCalledTimes(1);
  });

  it('reports a partial result honestly', async () => {
    const kit = buildKit({
      events: [standalone],
      recycleDelete: async (paths) => ({
        requested: paths.length,
        deleted: 1,
        failed: [{ path: CSV, message: 'locked' }],
      }),
    });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'partial');
    expect(kit.last('run-1')?.errorMessage).toContain('1 item masuk Recycle Bin, 1 gagal');
  });

  it('an unknown IPC failure is an unknown outcome, never replayed', async () => {
    const kit = buildKit({
      events: [standalone],
      recycleDelete: async () => {
        throw new TypeError('channel closed');
      },
    });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'unknownOutcome');
    expect(kit.recycle.deleteToRecycle).toHaveBeenCalledTimes(1);
  });

  it('a pane that changed after the preview makes the approval stale: nothing is deleted', async () => {
    const kit = buildKit({ events: [standalone] });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    kit.host.left.path = 'D:\\Elsewhere';
    kit.host.left.listing = {};
    kit.executor.approve('run-1');
    await vi.waitFor(() => expect(['stale', 'failed']).toContain(kit.stateOf('run-1')));
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('registers the items with the backend guard when the card is built, and the guard can veto', async () => {
    const register = vi.fn(async () => ({ ok: true as const }));
    const authorize = vi.fn(async () => ({ ok: false as const, message: 'Item berubah.' }));
    const kit = buildKit({ events: [standalone], guard: { register, authorize } });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    expect(register).toHaveBeenCalledTimes(1);
    const plan = (
      register.mock.calls[0] as unknown as [unknown, { operation: { kind: string } }]
    )[1];
    expect(plan.operation.kind).toBe('items.delete');
    expect(authorize).not.toHaveBeenCalled();
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'stale');
    expect(authorize).toHaveBeenCalledTimes(1);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a guard that cannot register fails closed: no card to approve', async () => {
    const kit = buildKit({
      events: [standalone],
      guard: {
        register: async () => ({ ok: false as const, message: 'Validasi backend gagal.' }),
        authorize: async () => ({ ok: true as const }),
      },
    });
    withSelection(kit);
    kit.submit();
    await settled(kit, 'run-1', 'failed');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('is not offered, and a forced intent is blocked, when no Recycle Bin port exists', async () => {
    const kit = buildKit({ events: [standalone], noDelete: true });
    withSelection(kit);
    kit.submit();
    await settled(kit, 'run-1', 'blockedCapability');
    expect(kit.requests[0].capabilities).not.toContain('items.delete');
  });

  it('refuses a follow-up that tries to reshape a delete', async () => {
    const kit = buildKit({ events: [standalone] });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    const card = kit.last('run-1')!;
    const result = kit.executor.followUp(
      'run-1',
      { planId: card.plan!.planId, revision: card.revision ?? 1 },
      'hanya csv saja',
    );
    expect(result.accepted).toBe(false);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });
});

describe('the user text of the owner example', () => {
  it('is what the navigation path is matched against', () => {
    expect(USER_TEXT).toContain(CSV);
  });
});
