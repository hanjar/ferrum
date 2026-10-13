// FER-195: state machine (spec bagian 8) + dependency scheduler.
import { describe, expect, it } from 'vitest';
import type { RunState } from './chatStore.svelte';
import { DependencyScheduler, canTransition, isFinal, summarizeRun } from './executorState';

const ALL: RunState[] = [
  'planning',
  'resolving',
  'preview',
  'awaitingApproval',
  'queued',
  'running',
  'succeeded',
  'partial',
  'failed',
  'cancelled',
  'needsInput',
  'blockedCapability',
  'stale',
  'unknownOutcome',
];

describe('run state machine', () => {
  it('follows the happy path in order', () => {
    const path: RunState[] = [
      'planning',
      'resolving',
      'preview',
      'awaitingApproval',
      'queued',
      'running',
      'succeeded',
    ];
    for (let i = 0; i < path.length - 1; i++)
      expect(canTransition(path[i], path[i + 1])).toBe(true);
  });

  it('cannot skip approval or jump straight to running/succeeded', () => {
    expect(canTransition('planning', 'queued')).toBe(false);
    expect(canTransition('resolving', 'running')).toBe(false);
    expect(canTransition('preview', 'running')).toBe(false);
    expect(canTransition('awaitingApproval', 'running')).toBe(false);
    expect(canTransition('awaitingApproval', 'succeeded')).toBe(false);
    expect(canTransition('planning', 'succeeded')).toBe(false);
  });

  it('final states do not transition (a cancelled run never turns into running)', () => {
    for (const s of [
      'succeeded',
      'partial',
      'failed',
      'cancelled',
      'blockedCapability',
      'stale',
    ] as RunState[]) {
      expect(isFinal(s)).toBe(true);
      for (const t of ALL) expect(canTransition(s, t)).toBe(false);
    }
  });

  it('unknownOutcome is resolved only by explicit reconciliation, never to queued', () => {
    expect(isFinal('unknownOutcome')).toBe(false);
    expect(canTransition('unknownOutcome', 'queued')).toBe(false);
    expect(canTransition('unknownOutcome', 'running')).toBe(true);
    expect(canTransition('unknownOutcome', 'succeeded')).toBe(true);
  });

  it('any pre-queue state may be cancelled; needsInput is reachable from planning/resolving/awaitingApproval', () => {
    for (const s of [
      'planning',
      'resolving',
      'preview',
      'awaitingApproval',
      'needsInput',
    ] as RunState[]) {
      expect(canTransition(s, 'cancelled')).toBe(true);
    }
    for (const s of ['planning', 'resolving', 'awaitingApproval'] as RunState[]) {
      expect(canTransition(s, 'needsInput')).toBe(true);
    }
    expect(canTransition('running', 'awaitingApproval')).toBe(false);
  });
});

describe('DependencyScheduler', () => {
  const steps = [
    { id: 's1', capability: 'archive.create' as const, dependsOn: [] },
    { id: 's2', capability: 'archive.create' as const, dependsOn: ['s1'] },
    { id: 's3', capability: 'archive.create' as const, dependsOn: ['s1', 's2'] },
  ];

  it('releases a step only after its dependencies completed', () => {
    const s = new DependencyScheduler(steps);
    expect(s.ready().map((x) => x.id)).toEqual(['s1']);
    s.start('s1');
    expect(s.ready()).toEqual([]);
    s.complete('s1');
    expect(s.ready().map((x) => x.id)).toEqual(['s2']);
    s.start('s2');
    s.complete('s2');
    s.start('s3');
    s.complete('s3');
    expect(s.finished).toBe(true);
    expect(s.outcome()).toBe('completed');
  });

  it('a failed step skips its dependents and never schedules them', () => {
    const s = new DependencyScheduler(steps);
    s.start('s1');
    s.fail('s1');
    expect(s.ready()).toEqual([]);
    expect(s.status('s2')).toBe('skipped');
    expect(s.status('s3')).toBe('skipped');
    expect(s.outcome()).toBe('failed');
  });

  it('cancel stops scheduling derived steps; a later complete does not revive them', () => {
    const s = new DependencyScheduler(steps);
    s.start('s1');
    s.cancel();
    s.complete('s1');
    expect(s.ready()).toEqual([]);
    expect(s.status('s2')).toBe('skipped');
    expect(s.outcome()).toBe('partial'); // s1 completed, the rest skipped
  });

  it('rejects starting a step whose dependencies are not completed or that is unknown', () => {
    const s = new DependencyScheduler(steps);
    expect(() => s.start('s2')).toThrow(/dependenc/i);
    expect(() => s.start('nope')).toThrow(/unknown/i);
    s.start('s1');
    expect(() => s.start('s1')).toThrow(/already/i);
  });

  it('cancel before anything ran reports cancelled', () => {
    const s = new DependencyScheduler(steps);
    s.cancel();
    expect(s.outcome()).toBe('cancelled');
  });
});

describe('summarizeRun (status from the engine, never from claims)', () => {
  const job = (state: string, files = 0, failures = 0) =>
    ({
      state,
      processedFiles: files,
      failures: Array.from({ length: failures }, () => ({ path: 'p', message: 'm' })),
    }) as never;
  const lookup = (m: Record<string, unknown>) => (id: string) => m[id] as never;

  it('unknown job is unknownOutcome, never success', () => {
    expect(summarizeRun(['j1'], lookup({})).outcome).toBe('unknownOutcome');
  });
  it('maps queue states', () => {
    expect(summarizeRun(['j1'], lookup({ j1: job('queued') })).outcome).toBe('queued');
    expect(summarizeRun(['j1'], lookup({ j1: job('running', 2) })).outcome).toBe('running');
    expect(summarizeRun(['j1'], lookup({ j1: job('completed', 12) }))).toMatchObject({
      outcome: 'completed',
      completedFiles: 12,
    });
  });
  it('cancel after a partial copy is partial with copied files reported', () => {
    expect(summarizeRun(['j1'], lookup({ j1: job('cancelled', 5) }))).toMatchObject({
      outcome: 'partial',
      completedFiles: 5,
    });
    expect(summarizeRun(['j1'], lookup({ j1: job('cancelled', 0) })).outcome).toBe('cancelled');
  });
  it('failures: failed without progress, partial with progress', () => {
    expect(summarizeRun(['j1'], lookup({ j1: job('failed', 0, 1) })).outcome).toBe('failed');
    expect(summarizeRun(['j1'], lookup({ j1: job('failed', 4, 2) }))).toMatchObject({
      outcome: 'partial',
      failed: 2,
    });
  });
  it('mixed jobs: running while any running; partial when one completed and one failed', () => {
    const m = { j1: job('completed', 2), j2: job('queued') };
    expect(summarizeRun(['j1', 'j2'], lookup(m)).outcome).toBe('running');
    expect(
      summarizeRun(['j1', 'j2'], lookup({ j1: job('completed', 2), j2: job('failed', 0, 1) }))
        .outcome,
    ).toBe('partial');
  });
  it('no jobs is not terminal-success by itself', () => {
    expect(summarizeRun([], lookup({})).outcome).toBe('unknownOutcome');
  });
});
