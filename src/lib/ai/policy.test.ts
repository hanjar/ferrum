import { describe, expect, it } from 'vitest';
import { samplePlan } from './fixtures/chatFixtures';
import { classifyRisk, evaluatePolicy, isIssuedDecision, DEFAULT_PREFS } from './policy';

const READY = { available: true } as const;
const PENDING = { available: false, code: 'pending', reason: 'belum tersedia' } as const;

const syncPlan = () =>
  samplePlan({
    capability: 'directories.sync',
    operation: {
      kind: 'directories.sync',
      leftRoot: 'D:\\Projects\\Invoice',
      rightRoot: 'E:\\Backup',
      leftManifest: 'l1',
      rightManifest: 'r1',
    },
    steps: [{ id: 's1', capability: 'directories.sync', dependsOn: [] }],
  });

describe('policy: risk and availability are computed locally', () => {
  it('defaults to assisted mode (autoCreate off)', () => {
    expect(DEFAULT_PREFS.autoCreate).toBe(false);
    const d = evaluatePolicy({ plan: samplePlan(), availability: READY, prefs: DEFAULT_PREFS });
    expect(d.kind).toBe('needsApproval');
  });

  it('classifies risk by capability and conflicts, ignoring the plan risk field', () => {
    expect(classifyRisk(samplePlan())).toBe('create');
    expect(classifyRisk(syncPlan())).toBe('mutating');
    const withConflict = samplePlan({ preview: { itemCount: 3, conflictCount: 1, totalBytes: 0 } });
    expect(classifyRisk(withConflict)).toBe('mutating');
  });

  it('blocks a capability that is not available and never approves it', () => {
    const d = evaluatePolicy({
      plan: syncPlan(),
      availability: PENDING,
      prefs: { autoCreate: true },
    });
    expect(d).toMatchObject({ kind: 'blocked', reason: 'belum tersedia' });
  });

  it('auto-create applies only to new-output archive plans without conflicts', () => {
    const prefs = { autoCreate: true };
    const ok = evaluatePolicy({ plan: samplePlan(), availability: READY, prefs });
    expect(ok.kind).toBe('autoApproved');
    expect(isIssuedDecision(ok)).toBe(true);
    const conflict = evaluatePolicy({
      plan: samplePlan({ preview: { itemCount: 3, conflictCount: 1, totalBytes: 0 } }),
      availability: READY,
      prefs,
    });
    expect(conflict.kind).toBe('needsApproval');
    // Sync always needs an approved dry-run, even with auto-create on.
    expect(evaluatePolicy({ plan: syncPlan(), availability: READY, prefs }).kind).toBe(
      'needsApproval',
    );
  });

  it('a fabricated decision object is not recognised as issued by policy', () => {
    expect(isIssuedDecision({ kind: 'autoApproved', risk: 'create', reasons: [] })).toBe(false);
    expect(isIssuedDecision(null)).toBe(false);
  });

  it('model-supplied flags on a plan cannot change the decision', () => {
    const plan = { ...samplePlan(), approved: true, risk: 'read', requiresApproval: false };
    const d = evaluatePolicy({
      plan: plan as never,
      availability: READY,
      prefs: DEFAULT_PREFS,
    });
    expect(d.kind).toBe('needsApproval');
  });
});
