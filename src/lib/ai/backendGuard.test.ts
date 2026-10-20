import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import type { ApprovalTicket } from './approval';
import { createBackendGuard } from './backendGuard';
import { samplePlan } from './fixtures/chatFixtures';

const ticket = {
  runId: 'run_1',
  handleId: 'h_1',
  planId: 'plan_1',
  planDigest: 'digest_1',
  revision: 2,
  root: 'D:\\Out',
  itemScope: 'scope_handle',
  itemCount: 2,
  conflictDecision: 'skip',
  expiresAtMs: 1_900_000_000_000,
  bindingDigest: 'b',
} as unknown as ApprovalTicket;
const plan = samplePlan();
const paths = ['D:\\In\\a.txt', 'D:\\In\\b.txt'];
const syncPlan = {
  ...plan,
  operation: {
    kind: 'directories.sync',
    leftRoot: 'D:/L',
    rightRoot: 'D:/R',
    leftManifest: 'm1',
    rightManifest: 'm2',
  },
} as never;

beforeEach(() => invokeMock.mockReset());

describe('backend approval guard', () => {
  it('registers the sources, root and output name, then authorizes with the backend scope digest', async () => {
    invokeMock
      .mockResolvedValueOnce({ scopeDigest: 'scope_from_backend' })
      .mockResolvedValueOnce(undefined);
    const guard = createBackendGuard({ sourcePaths: () => paths });
    expect(await guard.register!(ticket, plan)).toEqual({ ok: true });
    const [name, args] = invokeMock.mock.calls[0] as [string, { request: Record<string, unknown> }];
    expect(name).toBe('ai_register_approval');
    expect(args.request).toMatchObject({
      handleId: 'h_1',
      planId: 'plan_1',
      planDigest: 'digest_1',
      revision: 2,
      sourcePaths: paths,
      expiresAtMs: 1_900_000_000_000,
    });
    expect(Object.keys(args.request).sort()).toEqual([
      'expiresAtMs',
      'handleId',
      'outputName',
      'planDigest',
      'planId',
      'revision',
      'root',
      'sourcePaths',
    ]);

    expect(await guard.authorize(ticket, plan)).toEqual({ ok: true });
    expect(invokeMock).toHaveBeenLastCalledWith('ai_authorize_execute', {
      claim: expect.objectContaining({
        handleId: 'h_1',
        itemScope: 'scope_from_backend',
        root: 'D:\\Out',
      }),
    });
  });

  it('turns a stale rejection into a short message without raw paths', async () => {
    invokeMock.mockResolvedValueOnce({ scopeDigest: 's' }).mockRejectedValueOnce({
      kind: 'stale',
      reasons: ['source_changed'],
      summary: 'Rencana usang (1 alasan); tidak ada yang ditulis.',
    });
    const guard = createBackendGuard({ sourcePaths: () => paths });
    await guard.register!(ticket, plan);
    const verdict = await guard.authorize(ticket, plan);
    expect(verdict).toEqual({
      ok: false,
      message: 'Rencana usang (1 alasan); tidak ada yang ditulis.',
    });
  });

  it('fails closed: no registration means no authorization, and nothing is sent to the backend', async () => {
    const guard = createBackendGuard({ sourcePaths: () => paths });
    const verdict = await guard.authorize(ticket, plan);
    expect(verdict.ok).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('fails closed when the sources are unavailable, the backend rejects, or the reply is malformed', async () => {
    expect(
      (await createBackendGuard({ sourcePaths: () => undefined }).register!(ticket, plan)).ok,
    ).toBe(false);
    expect((await createBackendGuard({ sourcePaths: () => [] }).register!(ticket, plan)).ok).toBe(
      false,
    );
    expect(invokeMock).not.toHaveBeenCalled();

    invokeMock.mockRejectedValueOnce({ kind: 'source_unreadable', summary: 'Item tidak terbaca.' });
    expect(await createBackendGuard({ sourcePaths: () => paths }).register!(ticket, plan)).toEqual({
      ok: false,
      message: 'Item tidak terbaca.',
    });

    invokeMock.mockResolvedValueOnce({ nope: true });
    expect(
      (await createBackendGuard({ sourcePaths: () => paths }).register!(ticket, plan)).ok,
    ).toBe(false);

    invokeMock.mockRejectedValueOnce(new Error('ipc down with /secret/path'));
    const verdict = await createBackendGuard({ sourcePaths: () => paths }).register!(ticket, plan);
    expect(verdict.ok).toBe(false);
    expect(JSON.stringify(verdict)).not.toContain('/secret/path');
  });

  it('fails closed for a sync plan when the app supplies no sync scope', async () => {
    const verdict = await createBackendGuard({ sourcePaths: () => paths }).register!(
      ticket,
      syncPlan,
    );
    expect(verdict.ok).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  describe('directories.sync', () => {
    const scope = {
      leftRoot: 'D:/L',
      rightRoot: 'D:/R',
      sourcePaths: ['D:/L/a.txt', 'D:/R/b.txt', 'D:/L', 'D:/R'],
      absentPaths: ['D:/R/a.txt'],
    };
    const guard = () =>
      createBackendGuard({ sourcePaths: () => undefined, syncScope: () => scope });

    it('registers both roots, the fingerprinted paths and the new targets, then authorizes with both roots', async () => {
      invokeMock
        .mockResolvedValueOnce({ scopeDigest: 'sync_scope' })
        .mockResolvedValueOnce(undefined);
      const g = guard();
      expect(await g.register!(ticket, syncPlan)).toEqual({ ok: true });
      const [name, args] = invokeMock.mock.calls[0] as [
        string,
        { request: Record<string, unknown> },
      ];
      expect(name).toBe('ai_register_approval');
      expect(args.request).toMatchObject({
        handleId: 'h_1',
        root: 'D:/L',
        secondRoot: 'D:/R',
        sourcePaths: scope.sourcePaths,
        absentPaths: scope.absentPaths,
      });
      expect('outputName' in args.request).toBe(false);
      expect(await g.authorize(ticket, syncPlan)).toEqual({ ok: true });
      expect(invokeMock).toHaveBeenLastCalledWith('ai_authorize_execute', {
        claim: {
          handleId: 'h_1',
          planId: 'plan_1',
          planDigest: 'digest_1',
          revision: 2,
          root: 'D:/L',
          secondRoot: 'D:/R',
          itemScope: 'sync_scope',
        },
      });
    });

    it('fails closed when the scope is empty or does not match the plan roots', async () => {
      const mismatched = createBackendGuard({
        sourcePaths: () => undefined,
        syncScope: () => ({ ...scope, rightRoot: 'D:/Other' }),
      });
      expect((await mismatched.register!(ticket, syncPlan)).ok).toBe(false);
      const empty = createBackendGuard({
        sourcePaths: () => undefined,
        syncScope: () => ({ ...scope, sourcePaths: [] }),
      });
      expect((await empty.register!(ticket, syncPlan)).ok).toBe(false);
      expect(invokeMock).not.toHaveBeenCalled();
    });

    it('a backend rejection (stale/junction) gives its short message and no authorization', async () => {
      invokeMock.mockResolvedValueOnce({ scopeDigest: 's' }).mockRejectedValueOnce({
        kind: 'stale',
        reasons: ['root_mismatch'],
        summary: 'Rencana usang (1 alasan); tidak ada yang ditulis.',
      });
      const g = guard();
      await g.register!(ticket, syncPlan);
      expect(await g.authorize(ticket, syncPlan)).toEqual({
        ok: false,
        message: 'Rencana usang (1 alasan); tidak ada yang ditulis.',
      });
    });

    it('an IPC failure never produces approval or authorization', async () => {
      invokeMock.mockRejectedValueOnce(new Error('down at D:/secret'));
      const verdict = await guard().register!(ticket, syncPlan);
      expect(verdict.ok).toBe(false);
      expect(JSON.stringify(verdict)).not.toContain('secret');
      expect((await guard().authorize(ticket, syncPlan)).ok).toBe(false);
    });
  });

  it('revoke drops the registration and tells the backend', async () => {
    invokeMock.mockResolvedValueOnce({ scopeDigest: 's' }).mockResolvedValue(undefined);
    const guard = createBackendGuard({ sourcePaths: () => paths });
    await guard.register!(ticket, plan);
    guard.revoke!(ticket);
    expect(invokeMock).toHaveBeenLastCalledWith('ai_revoke_approval', { handleId: 'h_1' });
    expect((await guard.authorize(ticket, plan)).ok).toBe(false);
  });
});

describe('backend approval guard: items.delete (FER-576)', () => {
  const deletePlan = {
    ...plan,
    capability: 'items.delete',
    operation: { kind: 'items.delete', sourceManifest: 'man-del', sourceDirectory: 'D:\\In' },
  } as never;

  it('registers the folder as root, the frozen items, and no output name; authorizes with the same root', async () => {
    invokeMock.mockResolvedValueOnce({ scopeDigest: 'scope_del' }).mockResolvedValueOnce(undefined);
    const guard = createBackendGuard({ sourcePaths: () => paths });
    const delTicket = { ...ticket, root: 'D:\\In' } as unknown as ApprovalTicket;
    expect(await guard.register!(delTicket, deletePlan)).toEqual({ ok: true });
    const [, reg] = invokeMock.mock.calls[0] as [string, { request: Record<string, unknown> }];
    expect(reg.request).toMatchObject({ root: 'D:\\In', sourcePaths: paths, outputName: null });
    expect(await guard.authorize(delTicket, deletePlan)).toEqual({ ok: true });
    const [name, auth] = invokeMock.mock.calls[1] as [string, { claim: Record<string, unknown> }];
    expect(name).toBe('ai_authorize_execute');
    expect(auth.claim).toMatchObject({ root: 'D:\\In', itemScope: 'scope_del' });
  });

  it('fails closed when the frozen items are not available', async () => {
    const guard = createBackendGuard({ sourcePaths: () => undefined });
    const result = await guard.register!(ticket, deletePlan);
    expect(result.ok).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
