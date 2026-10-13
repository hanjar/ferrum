// FER-182: local selection handle (frozen manifest) and job-id outputs.
import { describe, expect, it } from 'vitest';
import { ContextService, selectionDigest } from './context';
import { ScopeResolver } from './contextResolver';
import { FakeHost, entry } from './contextTestKit';
import { SelectionHandleStore, summarizeJobs } from './commandsSelection';
import type { CommandReceipt } from './commands';

function stack() {
  const host = new FakeHost();
  host.left.path = 'D:\\Work';
  host.left.all = [
    entry('a.txt'),
    entry('b.txt'),
    entry('sub', { path: 'D:\\Work\\sub', isDir: true }),
  ];
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const scope = resolver.resolve(svc.capture('req_1'), 'left', {
    kind: 'allVisible',
    entries: 'all',
  });
  if (!scope.ok) throw new Error('resolve failed');
  return { host, svc, resolver, scope };
}

describe('SelectionHandleStore', () => {
  it('freezes an independent manifest: later pane selection or resolver release cannot change it', () => {
    const { host, resolver, scope } = stack();
    const store = new SelectionHandleStore();
    const h = store.create(scope, resolver.entries(scope.handle));
    host.left.selected = ['D:\\Work\\a.txt']; // user changes the live selection
    host.left.all = [];
    resolver.release(scope.handle); // the resolver forgets its copy
    expect(store.get(h.handleId)?.paths()).toEqual([
      'D:\\Work\\a.txt',
      'D:\\Work\\b.txt',
      'D:\\Work\\sub',
    ]);
    expect(Object.isFrozen(h.paths())).toBe(true);
    expect(h.count).toBe(3);
    expect(h.digest).toBe(selectionDigest(['D:\\Work\\a.txt', 'D:\\Work\\b.txt', 'D:\\Work\\sub']));
  });

  it('binds the handle to the pane/tab/revision it was resolved from', () => {
    const { scope, resolver } = stack();
    const h = new SelectionHandleStore().create(scope, resolver.entries(scope.handle));
    expect(h).toMatchObject({
      pane: 'left',
      tabId: scope.tabId,
      sourceDirectory: 'D:\\Work',
      contextRevision: scope.contextRevision,
      selectionRevision: scope.selectionRevision,
    });
  });

  it('never serializes paths (model/chat see counts and an opaque id only)', () => {
    const { scope, resolver } = stack();
    const h = new SelectionHandleStore().create(scope, resolver.entries(scope.handle));
    const json = JSON.stringify(h);
    expect(json).not.toContain('D:\\\\Work');
    expect(JSON.parse(json)).toEqual({
      handleId: h.handleId,
      pane: 'left',
      count: 3,
      digest: h.digest,
    });
    expect(h.handleId).toMatch(/^[A-Za-z0-9_-]{1,128}$/);
  });

  it('release drops the manifest; unknown handles are undefined', () => {
    const { scope, resolver } = stack();
    const store = new SelectionHandleStore();
    const h = store.create(scope, resolver.entries(scope.handle));
    expect(store.size).toBe(1);
    store.release(h.handleId);
    store.release(h.handleId); // idempotent
    expect(store.get(h.handleId)).toBeUndefined();
    expect(store.get('nope')).toBeUndefined();
    expect(store.size).toBe(0);
  });

  it('ids are unique per handle', () => {
    const { scope, resolver } = stack();
    const store = new SelectionHandleStore();
    const entries = resolver.entries(scope.handle);
    expect(store.create(scope, entries).handleId).not.toBe(store.create(scope, entries).handleId);
  });
});

describe('summarizeJobs (job ids -> status from the queue, never guessed)', () => {
  const receipt = (
    jobIds: string[],
    state: CommandReceipt['state'] = 'queued',
  ): CommandReceipt => ({
    executionId: 'exe_1',
    planId: 'plan_1',
    jobIds,
    state,
    completed: 0,
    conflictsSkipped: 0,
    notes: [],
  });
  const op = (state: string) => ({ state }) as never;

  it('counts only the receipt job ids, ignoring other operations in the queue', () => {
    const queue = new Map([
      ['op-1', op('completed')],
      ['op-2', op('running')],
      ['op-9', op('failed')], // someone else's job
    ]);
    expect(summarizeJobs(receipt(['op-1', 'op-2']), (id) => queue.get(id))).toEqual({
      total: 2,
      queued: 0,
      running: 1,
      completed: 1,
      failed: 0,
      cancelled: 0,
      unknown: 0,
      terminal: false,
      outcome: 'running',
    });
  });

  it('a job missing from the queue snapshot is unknown, so the run is not reported as success', () => {
    const s = summarizeJobs(receipt(['op-1']), () => undefined);
    expect(s).toMatchObject({ unknown: 1, terminal: false, outcome: 'unknown' });
  });

  it('terminal outcomes: all completed, any failed, any cancelled', () => {
    const lookup = (m: Record<string, string>) => (id: string) => (m[id] ? op(m[id]) : undefined);
    expect(
      summarizeJobs(receipt(['a', 'b']), lookup({ a: 'completed', b: 'completed' })),
    ).toMatchObject({
      terminal: true,
      outcome: 'completed',
    });
    expect(
      summarizeJobs(receipt(['a', 'b']), lookup({ a: 'completed', b: 'failed' })),
    ).toMatchObject({
      terminal: true,
      outcome: 'partial',
      failed: 1,
    });
    expect(summarizeJobs(receipt(['a']), lookup({ a: 'failed' }))).toMatchObject({
      outcome: 'failed',
    });
    expect(
      summarizeJobs(receipt(['a', 'b']), lookup({ a: 'completed', b: 'cancelled' })),
    ).toMatchObject({
      outcome: 'partial',
      cancelled: 1,
    });
    expect(summarizeJobs(receipt(['a']), lookup({ a: 'cancelled' }))).toMatchObject({
      outcome: 'cancelled',
    });
  });

  it('queued/paused jobs are not terminal', () => {
    const lookup = (id: string) => op(id === 'a' ? 'queued' : 'paused');
    expect(summarizeJobs(receipt(['a', 'b']), lookup)).toMatchObject({
      queued: 1,
      running: 1,
      terminal: false,
    });
  });

  it('a completed receipt with no jobs is completed (in-process service), a queued one without jobs is not', () => {
    expect(summarizeJobs(receipt([], 'completed'), () => undefined)).toMatchObject({
      total: 0,
      terminal: true,
      outcome: 'completed',
    });
    expect(summarizeJobs(receipt([], 'queued'), () => undefined)).toMatchObject({
      terminal: false,
      outcome: 'unknown',
    });
  });
});
