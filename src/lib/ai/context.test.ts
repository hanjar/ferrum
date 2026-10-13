import { describe, expect, it } from 'vitest';
import { parsePaneSnapshot } from './contracts';
import { ContextService, tabIdOf } from './context';
import { FakeHost, entry } from './contextTestKit';

describe('FER-183 stable tab IDs', () => {
  it('returns the same ID for the same tab object and distinct IDs for others', () => {
    const a = {};
    const b = {};
    expect(tabIdOf(a)).toBe(tabIdOf(a));
    expect(tabIdOf(a)).not.toBe(tabIdOf(b));
    expect(tabIdOf(a)).toMatch(/^tab_[0-9a-z]+$/);
  });
});

describe('FER-183 snapshot capture', () => {
  it('produces a frozen snapshot that satisfies the AI-01 PaneSnapshot contract', () => {
    const h = new FakeHost();
    h.left.all = [entry('a.txt'), entry('b.txt')];
    h.left.selected = ['D:\\Work\\a.txt'];
    h.active = 'right';
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    expect(() => parsePaneSnapshot(JSON.parse(JSON.stringify(snap)))).not.toThrow();
    expect(snap.activePaneAtSubmit).toBe('right');
    expect(snap.panes[0].paneId).toBe('left');
    expect(snap.panes[0].tabId).toBe(tabIdOf(h.left.tab));
    expect(snap.panes[0].location).toEqual({ kind: 'disk', path: 'D:\\Work' });
    expect(Object.isFrozen(snap)).toBe(true);
    expect(Object.isFrozen(snap.panes)).toBe(true);
    expect(Object.isFrozen(snap.panes[0])).toBe(true);
    expect(Object.isFrozen(snap.panes[0].location)).toBe(true);
  });

  it('is immutable against later host changes (snapshot keeps the old location and selection)', () => {
    const h = new FakeHost();
    h.left.all = [entry('a.txt')];
    h.left.selected = ['D:\\Work\\a.txt'];
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    const before = JSON.stringify(snap);
    h.left.path = 'D:\\Other';
    h.left.selected = [];
    expect(JSON.stringify(snap)).toBe(before);
    expect(svc.selectionPaths(snap, 'left')).toEqual(['D:\\Work\\a.txt']);
  });

  it('freezes a private copy of the selection (mutating the host array does not leak in)', () => {
    const h = new FakeHost();
    h.left.selected = ['D:\\Work\\a.txt'];
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    h.left.selected.push('D:\\Work\\b.txt');
    expect(svc.selectionPaths(snap, 'left')).toEqual(['D:\\Work\\a.txt']);
    expect(Object.isFrozen(svc.selectionPaths(snap, 'left'))).toBe(true);
  });

  it('reports archive locations and maps extended sorts to a contract sort key', () => {
    const h = new FakeHost();
    h.left.viewKind = 'archive';
    h.left.sort = { key: 'extension', dir: -1 };
    const snap = new ContextService(h).capture('req_1');
    expect(snap.panes[0].location.kind).toBe('archive');
    expect(snap.panes[0].sort).toEqual({ key: 'name', dir: -1 });
  });

  it('encodes filter preset, pattern and query in the bounded filter string', () => {
    const h = new FakeHost();
    h.left.filter = { mode: 'custom', pattern: '*.pdf', query: 'inv' };
    h.left.showHidden = true;
    const snap = new ContextService(h).capture('req_1');
    expect(snap.panes[0].filter).toContain('*.pdf');
    expect(snap.panes[0].filter).toContain('inv');
    expect(snap.panes[0].showHidden).toBe(true);
    h.left.filter = { mode: 'custom', pattern: 'x'.repeat(1000), query: '' };
    expect(() => parsePaneSnapshot(new ContextService(h).capture('req_2'))).not.toThrow();
  });

  it('throws a typed error when a pane probe is unavailable', () => {
    const h = new FakeHost();
    h.probe = (p) => (p === 'right' ? undefined : h.left.probe());
    expect(() => new ContextService(h).capture('req_1')).toThrowError(
      /pane_unavailable|unavailable/,
    );
  });

  it('rejects empty and malformed request IDs', () => {
    const svc = new ContextService(new FakeHost());
    expect(() => svc.capture('bad id')).toThrow();
  });
});

describe('FER-183 revisions', () => {
  it('keeps revisions stable when nothing meaningful changes, even across scroll/sort', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const a = svc.capture('req_1');
    h.left.sort = { key: 'size', dir: -1 };
    const b = svc.capture('req_2');
    expect(b.panes[0].contextRevision).toBe(a.panes[0].contextRevision);
    expect(b.panes[0].selectionRevision).toBe(a.panes[0].selectionRevision);
    expect(b.panes[0].selectionHandle).not.toBe(a.panes[0].selectionHandle);
  });

  it.each([
    ['navigation', (h: FakeHost) => (h.left.path = 'D:\\Other')],
    ['filter query', (h: FakeHost) => (h.left.filter = { mode: 'all', pattern: '', query: 'x' })],
    [
      'filter preset',
      (h: FakeHost) => (h.left.filter = { mode: 'programs', pattern: '', query: '' }),
    ],
    ['hidden toggle', (h: FakeHost) => (h.left.showHidden = true)],
    ['relisting', (h: FakeHost) => (h.left.listing = {})],
    ['view kind', (h: FakeHost) => (h.left.viewKind = 'branch')],
  ])('bumps contextRevision on %s', (_n, mutate) => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const a = svc.capture('req_1');
    mutate(h);
    const b = svc.capture('req_2');
    expect(b.panes[0].contextRevision).toBeGreaterThan(a.panes[0].contextRevision);
    expect(b.panes[1].contextRevision).toBe(a.panes[1].contextRevision);
  });

  it('bumps selectionRevision (not contextRevision) when the selection changes', () => {
    const h = new FakeHost();
    h.left.all = [entry('a'), entry('b')];
    const svc = new ContextService(h);
    const a = svc.capture('req_1');
    h.left.selected = ['D:\\Work\\a'];
    const b = svc.capture('req_2');
    expect(b.panes[0].selectionRevision).toBeGreaterThan(a.panes[0].selectionRevision);
    expect(b.panes[0].contextRevision).toBe(a.panes[0].contextRevision);
    h.left.selected = ['D:\\Work\\b'];
    expect(svc.capture('req_3').panes[0].selectionRevision).toBeGreaterThan(
      b.panes[0].selectionRevision,
    );
  });

  it('selection digest is order independent', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    h.left.selected = ['a', 'b', 'c'];
    const a = svc.capture('req_1');
    h.left.selected = ['c', 'a', 'b'];
    expect(svc.capture('req_2').panes[0].selectionRevision).toBe(a.panes[0].selectionRevision);
  });
});

describe('FER-183 staleness check', () => {
  it('is ok when nothing changed', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    expect(svc.check(snap, 'left')).toEqual({ ok: true });
  });

  it('detects navigation as context_changed and names the old location', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    h.left.path = 'D:\\Elsewhere';
    const r = svc.check(snap, 'left');
    expect(r).toMatchObject({ ok: false, reason: 'context_changed' });
  });

  it('detects tab close (tab object gone from both panes)', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    h.left.tab = {};
    expect(svc.check(snap, 'left')).toMatchObject({ ok: false, reason: 'tab_gone' });
  });

  it('detects a pane swap rather than silently retargeting', () => {
    const h = new FakeHost();
    h.left.all = [entry('a')];
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    h.swap();
    expect(svc.check(snap, 'left')).toMatchObject({ ok: false, reason: 'pane_swapped' });
    expect(svc.check(snap, 'right')).toMatchObject({ ok: false, reason: 'pane_swapped' });
  });

  it('detects selection change only when selection matters', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    h.left.selected = ['D:\\Work\\a'];
    const snap = svc.capture('req_1');
    h.left.selected = ['D:\\Work\\a', 'D:\\Work\\b'];
    expect(svc.check(snap, 'left')).toEqual({ ok: true });
    expect(svc.check(snap, 'left', { selection: true })).toMatchObject({
      ok: false,
      reason: 'selection_changed',
    });
  });

  it('active pane change after submit does not invalidate the snapshot', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    h.active = 'right';
    expect(svc.check(snap, 'left')).toEqual({ ok: true });
    expect(snap.activePaneAtSubmit).toBe('left');
  });

  it('reports a pane as unavailable when its probe disappears', () => {
    const h = new FakeHost();
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    h.probe = () => undefined;
    expect(svc.check(snap, 'left')).toMatchObject({ ok: false, reason: 'tab_gone' });
  });
});

describe('FER-183 outbound model context', () => {
  it('contains counts and flags but no paths or file names', () => {
    const h = new FakeHost();
    h.left.all = [entry('secret-plan.docx'), entry('.env', { hidden: true })];
    h.left.selected = ['D:\\Work\\secret-plan.docx'];
    const svc = new ContextService(h);
    const snap = svc.capture('req_1');
    const ctx = svc.modelContext(snap);
    const json = JSON.stringify(ctx);
    expect(json).not.toContain('secret-plan');
    expect(json).not.toContain('Work');
    expect(json).not.toContain('Backup');
    expect(ctx).toMatchObject({
      activePane: 'left',
      leftLocation: 'disk',
      rightLocation: 'disk',
      leftCount: 2,
    });
  });
});
