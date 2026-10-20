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
import type { ExecutionGuard } from './executor';
import type { ProviderEvent } from './provider';

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
    expect(kit.verify).toHaveBeenCalledWith(ARCHIVE, { members: 1 });
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

  it('restores the typed file after the archive refresh cleared the marks, then builds the delete card for exactly it', async () => {
    const kit = buildKit({ events: [ownerChain()] }); // the fake refresh clears the marks, like the real one
    await toArchiveDone(kit);
    await awaiting(kit, 'run-1-c2');
    expect(kit.navigate.open).toHaveBeenCalledTimes(2);
    expect(kit.host.left.selected).toEqual([CSV]);
    expect(kit.last('run-1-c2')?.plan?.preview.itemCount).toBe(1);
  });

  it('a file the user typed that cannot be shown again ends the chain without a delete', async () => {
    const holder: { kit?: Kit } = {};
    const kit = buildKit({
      events: [ownerChain()],
      verify: async () => {
        holder.kit!.navigate.open.mockResolvedValue(false);
        return { ok: true, message: 'ok' };
      },
    });
    holder.kit = kit;
    await toArchiveDone(kit);
    await settled(kit, 'run-1-c2', 'failed');
    expect(kit.last('run-1-c2')?.errorMessage).toContain('tidak ada yang dihapus');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });
});

describe('gates between the archive and the delete of the typed file', () => {
  /** Nav (run-1), archive card (run-1-c1), delete card (run-1-c2). */
  async function archived(kit: Kit): Promise<void> {
    start(kit);
    await awaiting(kit, 'run-1-c1');
    kit.executor.approve('run-1-c1');
  }

  function reopenWith(kit: Kit, marks: (target: string) => string[]): void {
    let opens = 0;
    kit.navigate.open.mockImplementation(async (side, target) => {
      opens++;
      const pane = side === 'left' ? kit.host.left : kit.host.right;
      pane.path = target.kind === 'dir' ? target.path : target.folder;
      pane.all = [entry('pc_202508014.csv', { path: CSV }), entry('keep-me.txt', { path: OTHER })];
      pane.selected = opens === 1 ? [CSV] : marks(CSV);
      pane.listing = {};
      return true;
    });
  }

  it('refuses the delete when the typed file is not the only mark when the delete card is built', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    reopenWith(kit, (file) => [file, OTHER]);
    await archived(kit);
    await settled(kit, 'run-1-c2', 'stale');
    expect(kit.last('run-1-c2')?.errorMessage).toContain('bukan persis file');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a mark that is gone when the delete card is built never produces a delete', async () => {
    const kit = buildKit({ events: [ownerChain()] });
    reopenWith(kit, () => []);
    await archived(kit);
    await vi.waitFor(() => expect(['stale', 'needsInput']).toContain(kit.stateOf('run-1-c2')));
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('S1: refuses the delete when the BACKEND fingerprint of the sources changed since the archive card', async () => {
    let calls = 0;
    const digests = new Map<string, string>();
    const guard: ExecutionGuard = {
      register: async (ticket) => {
        digests.set(ticket.handleId, calls++ === 0 ? 'digest-at-archive' : 'digest-after-edit');
        return { ok: true };
      },
      authorize: async () => ({ ok: true }),
      scopeDigestOf: (ticket) => digests.get(ticket.handleId),
    };
    const kit = buildKit({ events: [ownerChain()], guard });
    await archived(kit);
    await settled(kit, 'run-1-c2', 'stale');
    expect(kit.last('run-1-c2')?.errorMessage).toContain('Isi item berubah');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('S1: the same backend fingerprint lets the delete card through', async () => {
    const digests = new Map<string, string>();
    const guard: ExecutionGuard = {
      register: async (ticket) => {
        digests.set(ticket.handleId, 'same-digest');
        return { ok: true };
      },
      authorize: async () => ({ ok: true }),
      scopeDigestOf: (ticket) => digests.get(ticket.handleId),
    };
    const kit = buildKit({ events: [ownerChain()], guard });
    await archived(kit);
    await awaiting(kit, 'run-1-c2');
  });

  it('S1: a guard that cannot tell the digest fails closed for the delete', async () => {
    const guard: ExecutionGuard = {
      register: async () => ({ ok: true }),
      authorize: async () => ({ ok: true }),
      scopeDigestOf: () => undefined,
    };
    const kit = buildKit({ events: [ownerChain()], guard });
    await archived(kit);
    await settled(kit, 'run-1-c2', 'stale');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('S1: a guard without scopeDigestOf at all also fails closed (review R2)', async () => {
    const guard: ExecutionGuard = {
      register: async () => ({ ok: true }),
      authorize: async () => ({ ok: true }),
    };
    const kit = buildKit({ events: [ownerChain()], guard });
    await archived(kit);
    await settled(kit, 'run-1-c2', 'stale');
    expect(kit.last('run-1-c2')?.errorMessage).toContain('Sidik jari backend');
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('S2: symlinks or junctions inside the sources stop the chain before the archive is written', async () => {
    const kit = buildKit({ events: [ownerChain()], census: { archiveMembers: 1, links: 2 } });
    start(kit);
    await settled(kit, 'run-1-c1', 'stale');
    expect(kit.last('run-1-c1')?.errorMessage).toContain('tidak bisa dipastikan lengkap');
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('S2: an unknown member count stops the chain too (fail closed)', async () => {
    const kit = buildKit({ events: [ownerChain()], census: 'absent' });
    start(kit);
    await settled(kit, 'run-1-c1', 'stale');
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('S2: the number of members the archive must hold goes to Test Archive', async () => {
    const kit = buildKit({ events: [ownerChain()], census: { archiveMembers: 7, links: 0 } });
    await archived(kit);
    await awaiting(kit, 'run-1-c2');
    expect(kit.verify).toHaveBeenCalledWith(ARCHIVE, { members: 7 });
  });

  it('a plain archive is never measured: no census latency when no delete is involved (review R2)', async () => {
    const plain = buildKit({
      events: [
        {
          kind: 'intent',
          intent: {
            schemaVersion: 1,
            requestId: 'req_1',
            intent: {
              capability: 'archive.create',
              sourcePane: 'left',
              selector: { kind: 'allVisible', entries: 'all' },
              format: 'zip',
              target: { kind: 'sourceDirectory' },
            },
          },
        },
      ],
      userText: 'arsipkan folder kiri',
      census: { archiveMembers: 9, links: 9 },
    });
    plain.submit();
    await awaiting(plain, 'run-1');
    // The census said "9 links": had it been used, a warning about links would be on the card.
    expect(JSON.stringify(plain.last('run-1'))).not.toContain('symlink');
    expect(plain.censusCalls()).toBe(0);
  });

  it('a chain that deletes without opening the typed file first is refused', async () => {
    const kit = buildKit({
      events: [
        {
          kind: 'chain',
          requestId: 'req_1',
          steps: [
            {
              capability: 'archive.create',
              sourcePane: 'left',
              selector: { kind: 'selected' },
              format: '7z',
              target: { kind: 'sourceDirectory' },
            },
            { capability: 'items.delete', sourcePane: 'left', selector: { kind: 'selected' } },
          ],
        },
      ],
      userText: 'arsipkan yang kupilih ke 7z lalu hapus',
    });
    kit.submit();
    await settled(kit, 'run-1', 'blockedCapability');
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('R2-S1: a follow-up cannot turn the archive card into a delete card', async () => {
    const swap: ProviderEvent = {
      kind: 'intent',
      intent: {
        schemaVersion: 1,
        requestId: 'req_1',
        intent: { capability: 'items.delete', sourcePane: 'left', selector: { kind: 'selected' } },
      },
    };
    const kit = buildKit({ events: [ownerChain(), swap] });
    start(kit);
    await awaiting(kit, 'run-1-c1');
    const card = kit.last('run-1-c1')!;
    const result = kit.executor.followUp(
      'run-1-c1',
      { planId: card.plan!.planId, revision: card.revision ?? 1 },
      'jangan hapus dulu, tolong pakai zip saja ya',
    );
    expect(result.accepted).toBe(true);
    await vi.waitFor(() => expect(kit.stateOf('run-1-c1')).toBe('needsInput'));
    expect(kit.last('run-1-c1')?.errorMessage).toContain('tidak valid');
    expect(kit.recycle.check).not.toHaveBeenCalled();
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
    expect(message).toContain('menolak bila itu tidak terjamin');
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

describe('a chain that deletes is strict about what it works on (review R1 S5/S6)', () => {
  it('S6: refuses a navigation to a FOLDER in a chain that deletes', async () => {
    const chain = ownerChain();
    if (chain.kind !== 'chain') throw new Error('test setup');
    chain.steps[0] = { capability: 'pane.navigate', pane: 'activeAtSubmit', path: FOLDER };
    const kit = buildKit({ events: [chain], userText: `hapus isi ${FOLDER}` });
    start(kit);
    await settled(kit, 'run-1', 'failed');
    expect(kit.last('run-1')?.errorMessage).toContain('path FILE');
    expect(kit.runIds()).toEqual(['run-1']);
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('S6: refuses the archive step when the selection is not exactly the typed file', async () => {
    const kit = buildKit({ events: [ownerChain()], navSelectsExtra: true });
    start(kit);
    await settled(kit, 'run-1-c1', 'stale');
    expect(kit.last('run-1-c1')?.errorMessage).toContain('bukan persis file');
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('S6: refuses a chain that deletes when the request names two paths', async () => {
    const kit = buildKit({
      events: [ownerChain()],
      userText: `${USER_TEXT} dan simpan di D:\\backup`,
    });
    start(kit);
    await settled(kit, 'run-1', 'blockedCapability');
    expect(kit.navigate.open).not.toHaveBeenCalled();
  });

  it('S5: refuses a chain with a delete step when the request does not ask to delete', async () => {
    const kit = buildKit({ events: [ownerChain()], userText: `arsipkan ${CSV} ke 7z` });
    start(kit);
    await settled(kit, 'run-1', 'blockedCapability');
    expect(kit.navigate.open).not.toHaveBeenCalled();
  });

  it('S5: a standalone delete intent for a request that does not ask to delete gets no card', async () => {
    const kit = buildKit({
      events: [
        {
          kind: 'intent',
          intent: {
            schemaVersion: 1,
            requestId: 'req_1',
            intent: {
              capability: 'items.delete',
              sourcePane: 'left',
              selector: { kind: 'selected' },
            },
          },
        },
      ],
      userText: 'jelaskan file ini',
    });
    kit.host.left.selected = [kit.host.left.all[0].path];
    kit.submit();
    await vi.waitFor(() => expect(kit.stateOf('run-1')).toBe('needsInput'));
    expect(kit.last('run-1')?.errorMessage).toContain('tidak meminta menghapus');
    expect(kit.recycle.check).not.toHaveBeenCalled();
  });
});

describe('the Recycle Bin gate shows through to the card (review R1 B1)', () => {
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
    kit.host.left.all = [entry('pc_202508014.csv', { path: CSV })];
    kit.host.left.selected = [CSV];
  }

  it('shows the REAL size the backend measured on the card', async () => {
    const kit = buildKit({
      events: [standalone],
      userText: 'hapus',
      recycleCheck: (paths) => paths.map((path) => ({ path, ok: true, bytes: 123_456 })),
    });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    expect(kit.last('run-1')?.plan?.preview.totalBytes).toBe(123_456);
  });

  it.each([
    ['recycle_disabled', 'dimatikan'],
    ['recycle_unknown', 'tidak bisa dibaca'],
    ['too_big', 'melebihi kuota'],
    ['mount_point', 'di-mount'],
    ['redirected', 'dialihkan'],
    ['size_unknown', 'tidak dapat diukur'],
    ['link_item', 'symlink'],
    ['capacity_unknown', 'tidak diketahui'],
  ])('refuses the plan with a clear reason: %s', async (code, text) => {
    const kit = buildKit({
      events: [standalone],
      userText: 'hapus',
      recycleCheck: (paths) => paths.map((path) => ({ path, ok: false, reason: code })),
    });
    withSelection(kit);
    kit.submit();
    await settled(kit, 'run-1', 'needsInput');
    expect(kit.last('run-1')?.errorMessage).toContain(text);
    expect(kit.recycle.deleteToRecycle).not.toHaveBeenCalled();
  });

  it('a delete the shell did NOT send to the Recycle Bin is reported as a failure, never a success', async () => {
    const kit = buildKit({
      events: [standalone],
      userText: 'hapus',
      recycleDelete: async (paths) => ({
        requested: paths.length,
        deleted: 0,
        failed: [],
        bypassed: [paths[0]],
        skipped: 0,
      }),
    });
    withSelection(kit);
    kit.submit();
    await awaiting(kit, 'run-1');
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'failed');
    expect(kit.last('run-1')?.errorMessage).toContain('secara permanen');
  });

  it('a partial batch with a bypass is partial and says so loudly', async () => {
    const kit = buildKit({
      events: [standalone],
      userText: 'hapus',
      recycleDelete: async (paths) => ({
        requested: paths.length,
        deleted: 1,
        failed: [],
        bypassed: [paths[1]],
        skipped: 1,
      }),
    });
    kit.host.left.path = FOLDER;
    kit.host.left.all = [
      entry('a.txt', { path: `${FOLDER}/a.txt` }),
      entry('b.txt', { path: `${FOLDER}/b.txt` }),
      entry('c.txt', { path: `${FOLDER}/c.txt` }),
    ];
    kit.host.left.selected = kit.host.left.all.map((e) => e.path);
    kit.submit();
    await awaiting(kit, 'run-1');
    kit.executor.approve('run-1');
    await settled(kit, 'run-1', 'partial');
    const message = kit.last('run-1')?.errorMessage ?? '';
    expect(message).toContain('PERINGATAN');
    expect(message).toContain('TIDAK masuk Recycle Bin');
  });
});

describe('the user text of the owner example', () => {
  it('is what the navigation path is matched against', () => {
    expect(USER_TEXT).toContain(CSV);
  });
});
