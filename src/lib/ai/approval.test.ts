// FER-192/193: approval terikat digest/root/scope/revision/expiry. Facade nyata,
// adapter palsu yang menghitung berapa kali ia benar-benar menulis.
import { describe, expect, it, vi } from 'vitest';
import {
  CommandFacade,
  err,
  ok,
  type AdapterPrepared,
  type CapabilitySource,
  type CommandAdapter,
} from './commands';
import type { IntentEnvelope, PaneSnapshot } from './contracts';
import { ApprovalManager, type ConflictPort } from './approval';
import { DEFAULT_PREFS, evaluatePolicy } from './policy';
import { samplePlan } from './fixtures/chatFixtures';

const snapshot = (requestId = 'req_1'): PaneSnapshot => {
  const p = samplePlan().sourceSnapshot;
  return { ...p, requestId };
};

function setup() {
  let now = 1_000;
  const run = vi.fn(async () =>
    ok({
      jobIds: ['job_1'],
      state: 'queued' as const,
      completed: 0,
      conflictsSkipped: 0,
      notes: [],
    }),
  );
  const adapter: CommandAdapter = {
    id: 'archive.create',
    async prepare(envelope: IntentEnvelope) {
      void envelope;
      const prepared: AdapterPrepared = {
        operation: {
          kind: 'archive.create',
          sourceManifest: 'man_1',
          targetDirectory: 'D:\\Projects\\Invoice',
          outputName: 'Invoice.zip',
        },
        preview: { summary: 's', itemCount: 3, conflictCount: 0, totalBytes: 0, warnings: [] },
        digestParts: ['x'],
        state: {},
      };
      return ok(prepared);
    },
    run,
  };
  const caps: CapabilitySource = {
    availability: () => ({ available: true }),
    adapterFor: () => ok(adapter),
  };
  const facade = new CommandFacade(caps, { now: () => now, ttlMs: 60_000 });
  const approvals = new ApprovalManager(facade, { now: () => now });
  const prepare = async (requestId = 'req_1') => {
    const out = await facade.prepare(
      {
        schemaVersion: 1,
        requestId,
        intent: {
          capability: 'archive.create',
          sourcePane: 'left',
          selector: { kind: 'selected' },
          format: 'zip',
          target: { kind: 'sourceDirectory' },
        },
      },
      { snapshot: snapshot(requestId) },
    );
    if (!out.ok) throw new Error('prepare failed');
    return out.value;
  };
  return { facade, approvals, prepare, run, advance: (ms: number) => (now += ms) };
}

describe('ApprovalManager', () => {
  it('presents a ticket bound to digest, root, scope, revision and expiry', async () => {
    const t = setup();
    const prepared = await t.prepare();
    const ticket = t.approvals.present('run-1', prepared);
    expect(ticket).toMatchObject({
      runId: 'run-1',
      planId: prepared.plan.planId,
      planDigest: prepared.plan.previewDigest,
      revision: 1,
      root: 'D:\\Projects\\Invoice',
      itemScope: 'man_1',
      itemCount: 3,
      expiresAtMs: prepared.handle.expiresAtMs,
    });
    expect(ticket.bindingDigest).toMatch(/^[0-9a-f]{32}$/);
  });

  it('approves only with the exact binding digest the user was shown; run is not called', async () => {
    const t = setup();
    const prepared = await t.prepare();
    const ticket = t.approvals.present('run-1', prepared);
    const wrong = t.approvals.approve('run-1', { bindingDigest: 'f'.repeat(32) });
    expect(wrong).toMatchObject({ ok: false, error: { code: 'stale' } });
    const good = t.approvals.approve('run-1', { bindingDigest: ticket.bindingDigest });
    expect(good.ok).toBe(true);
    expect(t.run).not.toHaveBeenCalled(); // approval alone mutates nothing
  });

  it('rejects approval for a run with no presented ticket (fabricated approval)', () => {
    const t = setup();
    expect(t.approvals.approve('run-9', { bindingDigest: 'a'.repeat(32) })).toMatchObject({
      ok: false,
      error: { code: 'not_approved' },
    });
    // a plain object that looks approved cannot be executed by the facade
    const forged = { handleId: 'hnd_x', planDigest: 'd', approved: true } as never;
    return expect(t.facade.execute(forged)).resolves.toMatchObject({
      ok: false,
      error: { code: 'not_approved' },
    });
  });

  it('rejects approval after expiry', async () => {
    const t = setup();
    const ticket = t.approvals.present('run-1', await t.prepare());
    t.advance(60_001);
    expect(t.approvals.approve('run-1', { bindingDigest: ticket.bindingDigest })).toMatchObject({
      ok: false,
      error: { code: 'expired' },
    });
  });

  it('plan edit invalidates the old approval; a new ticket has a new digest', async () => {
    const t = setup();
    const first = t.approvals.present('run-1', await t.prepare());
    t.approvals.invalidate('run-1');
    expect(t.approvals.approve('run-1', { bindingDigest: first.bindingDigest })).toMatchObject({
      ok: false,
      error: { code: 'not_approved' },
    });
    const second = t.approvals.present('run-1', await t.prepare());
    expect(second.bindingDigest).not.toBe(first.bindingDigest);
    expect(second.revision).toBe(2);
    // the old digest still cannot approve the new plan
    expect(t.approvals.approve('run-1', { bindingDigest: first.bindingDigest }).ok).toBe(false);
    expect(t.approvals.approve('run-1', { bindingDigest: second.bindingDigest }).ok).toBe(true);
  });

  it('gives one approval per batch: repeated approve returns the same token', async () => {
    const t = setup();
    const ticket = t.approvals.present('run-1', await t.prepare());
    const a = t.approvals.approve('run-1', { bindingDigest: ticket.bindingDigest });
    const b = t.approvals.approve('run-1', { bindingDigest: ticket.bindingDigest });
    expect(a.ok && b.ok && a.value === b.value).toBe(true);
    expect(t.approvals.approvalCount('run-1')).toBe(1);
  });

  it('does not let an approval of run A authorise run B', async () => {
    const t = setup();
    const a = t.approvals.present('run-a', await t.prepare());
    t.approvals.present('run-b', await t.prepare());
    expect(t.approvals.approve('run-b', { bindingDigest: a.bindingDigest }).ok).toBe(false);
  });

  it('autoApprove requires an issued autoApproved policy decision', async () => {
    const t = setup();
    const prepared = await t.prepare();
    t.approvals.present('run-1', prepared);
    const forged = { kind: 'autoApproved', risk: 'create', reasons: [] } as never;
    expect(t.approvals.autoApprove('run-1', forged).ok).toBe(false);
    const assisted = evaluatePolicy({
      plan: prepared.plan,
      availability: { available: true },
      prefs: DEFAULT_PREFS,
    });
    expect(t.approvals.autoApprove('run-1', assisted).ok).toBe(false);
    const auto = evaluatePolicy({
      plan: prepared.plan,
      availability: { available: true },
      prefs: { autoCreate: true },
    });
    expect(t.approvals.autoApprove('run-1', auto).ok).toBe(true);
  });

  it('after the approval, execute runs the adapter exactly once', async () => {
    const t = setup();
    const ticket = t.approvals.present('run-1', await t.prepare());
    const a = t.approvals.approve('run-1', { bindingDigest: ticket.bindingDigest });
    if (!a.ok) throw new Error('approve failed');
    await Promise.all([t.facade.execute(a.value), t.facade.execute(a.value)]);
    expect(t.run).toHaveBeenCalledTimes(1);
  });

  it('propagates a facade rejection (unknown handle) as an error result', async () => {
    const t = setup();
    const prepared = await t.prepare();
    t.approvals.present('run-1', prepared);
    t.facade.discard(prepared.handle.handleId);
    const ticket = t.approvals.ticketOf('run-1')!;
    expect(t.approvals.approve('run-1', { bindingDigest: ticket.bindingDigest })).toMatchObject({
      ok: false,
      error: { code: 'invalid_handle' },
    });
    void err;
  });
});

describe('conflict decisions (existing conflict dialog port)', () => {
  const conflicts = [{ itemRef: 'c1', label: 'a.txt', kind: 'both-changed' }];
  it('a decision is part of the binding digest and cannot be overwrite', async () => {
    const t = setup();
    const prepared = await t.prepare();
    const skip = t.approvals.present('run-1', prepared, { conflictDecision: 'skip' });
    const rename = t.approvals.present('run-1', prepared, { conflictDecision: 'rename' });
    expect(skip.bindingDigest).not.toBe(rename.bindingDigest);
    expect(() =>
      t.approvals.present('run-1', prepared, { conflictDecision: 'overwrite' as never }),
    ).toThrow(/overwrite/i);
  });

  it('asks the port once per batch and treats cancel as no approval', async () => {
    const port: ConflictPort = { decide: vi.fn(async () => 'cancel' as const) };
    const { decideConflicts } = await import('./approval');
    expect(await decideConflicts(port, [])).toBe('skip'); // nothing to ask
    expect(port.decide).not.toHaveBeenCalled();
    expect(await decideConflicts(port, conflicts)).toBe('cancel');
    expect(port.decide).toHaveBeenCalledTimes(1);
  });

  it('a failing port is treated as cancel (never an implied overwrite)', async () => {
    const port: ConflictPort = {
      decide: vi.fn(async () => {
        throw new Error('dialog closed');
      }),
    };
    const { decideConflicts } = await import('./approval');
    expect(await decideConflicts(port, conflicts)).toBe('cancel');
  });
});
