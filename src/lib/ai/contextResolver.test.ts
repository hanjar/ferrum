import { describe, expect, it } from 'vitest';
import { ContextService, type ContextEntry } from './context';
import { ScopeResolver, type ScopeOk } from './contextResolver';
import { FakeHost, entry } from './contextTestKit';
import { visibleRange } from '../virtual';

function setup() {
  const host = new FakeHost();
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  return { host, svc, resolver };
}

function mixed(): ContextEntry[] {
  return [
    entry('a.pdf'),
    entry('b.txt'),
    entry('.hidden.txt', { hidden: true }),
    entry('docs', { isDir: true }),
    entry('.git', { isDir: true, hidden: true }),
    entry('link.lnk', { isSymlink: true }),
  ];
}

function ok(r: ReturnType<ScopeResolver['resolve']>): ScopeOk {
  if (!r.ok) throw new Error(`expected ok, got ${r.error.code}: ${r.error.message}`);
  return r;
}

describe('FER-184 allVisible', () => {
  it('takes the whole visible listing (hidden off) and explains the scope', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    const snap = svc.capture('req_1');
    const r = ok(resolver.resolve(snap, 'left', { kind: 'allVisible', entries: 'all' }));
    expect(r.counts).toEqual({ files: 3, folders: 1, total: 4 });
    expect(r.excluded.hidden).toBe(2);
    expect(r.scopeText).toMatch(/3 file dan 1 folder/);
    expect(r.scopeText).toMatch(/2 item tersembunyi tidak ikut/);
    expect(r.scopeText).toContain('kiri');
    expect(resolver.entries(r.handle).map((e) => e.name)).toEqual([
      'a.pdf',
      'b.txt',
      'docs',
      'link.lnk',
    ]);
  });

  it('respects the name filter and reports how many entries the filter dropped', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    host.left.filter = { mode: 'custom', pattern: '', query: '.pdf' };
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', { kind: 'allVisible', entries: 'all' }),
    );
    expect(r.counts.total).toBe(1);
    expect(r.excluded.filtered).toBeGreaterThan(0);
    expect(r.filterActive).toBe(true);
    expect(r.scopeText).toMatch(/filter/i);
  });

  it('filesOnly keeps regular files only (no folders, no symlinks) and reports skipped folders', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', { kind: 'allVisible', entries: 'filesOnly' }),
    );
    expect(resolver.entries(r.handle).map((e) => e.name)).toEqual(['a.pdf', 'b.txt']);
    expect(r.counts).toEqual({ files: 2, folders: 0, total: 2 });
    expect(r.excluded.nonFiles).toBe(2);
    expect(r.scopeText).toMatch(/hanya file/i);
  });

  it('includes hidden entries when the pane shows hidden files', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    host.left.showHidden = true;
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', { kind: 'allVisible', entries: 'all' }),
    );
    expect(r.counts.total).toBe(6);
    expect(r.excluded.hidden).toBe(0);
  });

  it('resolves activeAtSubmit from the snapshot, not from the live active pane', () => {
    const { host, svc, resolver } = setup();
    host.left.all = [entry('l.txt')];
    host.right.all = [entry('r.txt', { path: 'E:\\Backup\\r.txt' })];
    host.active = 'right';
    const snap = svc.capture('req_1');
    host.active = 'left';
    const r = ok(resolver.resolve(snap, 'activeAtSubmit', { kind: 'allVisible', entries: 'all' }));
    expect(r.pane).toBe('right');
    expect(resolver.entries(r.handle).map((e) => e.name)).toEqual(['r.txt']);
  });

  it('empty listing yields empty_scope, not an empty successful plan', () => {
    const { svc, resolver } = setup();
    const r = resolver.resolve(svc.capture('req_1'), 'left', {
      kind: 'allVisible',
      entries: 'all',
    });
    expect(r).toMatchObject({ ok: false, error: { code: 'empty_scope' } });
  });
});

describe('FER-184 selected', () => {
  it('uses the selection frozen at submit', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    host.left.selected = ['D:\\Work\\b.txt', 'D:\\Work\\docs'];
    const snap = svc.capture('req_1');
    host.left.selected = [];
    const r = ok(resolver.resolve(snap, 'left', { kind: 'selected' }));
    expect(r.counts).toEqual({ files: 1, folders: 1, total: 2 });
    expect(resolver.entries(r.handle).map((e) => e.path)).toEqual([
      'D:\\Work\\b.txt',
      'D:\\Work\\docs',
    ]);
    expect(r.scopeText).toMatch(/dipilih/);
  });

  it('empty selection gives a clear message and never falls back to select-all', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    const r = resolver.resolve(svc.capture('req_1'), 'left', { kind: 'selected' });
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.error.code).toBe('empty_selection');
      expect(r.error.message).toMatch(/belum ada item yang dipilih/i);
    }
  });

  it('selected paths missing from the listing are stale, not silently dropped', () => {
    const { host, svc, resolver } = setup();
    host.left.all = [entry('a')];
    host.left.selected = ['D:\\Work\\gone'];
    expect(resolver.resolve(svc.capture('req_1'), 'left', { kind: 'selected' })).toMatchObject({
      ok: false,
      error: { code: 'stale_context' },
    });
  });

  it('filesOnly is not applicable to selected (selection is explicit)', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    host.left.selected = ['D:\\Work\\docs'];
    const r = ok(resolver.resolve(svc.capture('req_1'), 'left', { kind: 'selected' }));
    expect(r.counts.folders).toBe(1);
  });
});

describe('FER-184 directoryContents', () => {
  it('ignores the pane filter and honours includeHidden', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    host.left.filter = { mode: 'custom', pattern: '', query: 'zzz' };
    const snap = svc.capture('req_1');
    const a = ok(
      resolver.resolve(snap, 'left', {
        kind: 'directoryContents',
        includeHidden: false,
        entries: 'all',
      }),
    );
    expect(a.counts.total).toBe(4);
    expect(a.excluded.hidden).toBe(2);
    expect(a.scopeText).toMatch(/tanpa filter/i);
    const b = ok(
      resolver.resolve(snap, 'left', {
        kind: 'directoryContents',
        includeHidden: true,
        entries: 'all',
      }),
    );
    expect(b.counts.total).toBe(6);
    expect(b.scopeText).toMatch(/tersembunyi ikut/i);
    const c = ok(
      resolver.resolve(snap, 'left', {
        kind: 'directoryContents',
        includeHidden: true,
        entries: 'filesOnly',
      }),
    );
    expect(resolver.entries(c.handle).map((e) => e.name)).toEqual([
      'a.pdf',
      'b.txt',
      '.hidden.txt',
    ]);
  });
});

describe('FER-184 unsupported views', () => {
  it.each(['branch', 'search', 'archive'] as const)(
    'rejects %s view with an explicit message',
    (kind) => {
      const { host, svc, resolver } = setup();
      host.left.all = mixed();
      host.left.viewKind = kind;
      const r = resolver.resolve(svc.capture('req_1'), 'left', {
        kind: 'allVisible',
        entries: 'all',
      });
      expect(r).toMatchObject({ ok: false, error: { code: 'unsupported_view' } });
      if (!r.ok) expect(r.error.message).toMatch(/belum didukung|tidak didukung/i);
    },
  );
});

describe('FER-184 staleness binds the manifest to the snapshot', () => {
  it('refuses to resolve after navigation, relisting, swap or tab close', () => {
    for (const mutate of [
      (h: FakeHost) => (h.left.path = 'D:\\Elsewhere'),
      (h: FakeHost) => (h.left.listing = {}),
      (h: FakeHost) => h.swap(),
      (h: FakeHost) => (h.left.tab = {}),
    ]) {
      const { host, svc, resolver } = setup();
      host.left.all = mixed();
      const snap = svc.capture('req_1');
      mutate(host);
      const r = resolver.resolve(snap, 'left', { kind: 'allVisible', entries: 'all' });
      expect(r).toMatchObject({ ok: false, error: { code: 'stale_context' } });
    }
  });

  it('a manifest resolved before a change stays exactly as approved', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    const snap = svc.capture('req_1');
    const r = ok(resolver.resolve(snap, 'left', { kind: 'allVisible', entries: 'all' }));
    const before = resolver.entries(r.handle).map((e) => e.path);
    host.left.all = [entry('new.txt')];
    host.left.path = 'D:\\Elsewhere';
    expect(resolver.entries(r.handle).map((e) => e.path)).toEqual(before);
    expect(Object.isFrozen(resolver.entries(r.handle))).toBe(true);
    expect(resolver.revalidate(snap, r)).toMatchObject({ ok: false, reason: 'context_changed' });
  });

  it('revalidate is ok while unchanged and unknown handles are rejected', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    const snap = svc.capture('req_1');
    const r = ok(resolver.resolve(snap, 'left', { kind: 'allVisible', entries: 'all' }));
    expect(resolver.revalidate(snap, r)).toEqual({ ok: true });
    expect(() => resolver.entries('scope_nope')).toThrow(/unknown/i);
  });

  it('selected scope revalidation fails when the live selection changed', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    host.left.selected = ['D:\\Work\\b.txt'];
    const snap = svc.capture('req_1');
    const r = ok(resolver.resolve(snap, 'left', { kind: 'selected' }));
    host.left.selected = ['D:\\Work\\a.pdf'];
    expect(resolver.revalidate(snap, r)).toMatchObject({ ok: false, reason: 'selection_changed' });
  });
});

describe('FER-184 privacy', () => {
  it('the public manifest carries counts and handles, never the path list', () => {
    const { host, svc, resolver } = setup();
    host.left.all = mixed();
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', { kind: 'allVisible', entries: 'all' }),
    );
    const json = JSON.stringify(r);
    expect(json).not.toContain('a.pdf');
    expect(json).not.toContain('b.txt');
  });
});

describe('FER-184 100k listing', () => {
  const N = 100_000;
  function big(): ContextEntry[] {
    const out: ContextEntry[] = new Array(N);
    for (let i = 0; i < N; i++) {
      out[i] = {
        name: `f${i}.dat`,
        path: `D:\\Work\\f${i}.dat`,
        isDir: i % 1000 === 0,
        isSymlink: false,
        hidden: i % 10 === 5,
      };
    }
    return out;
  }

  it('allVisible covers the entire listing, not the rendered virtual window', () => {
    const { host, svc, resolver } = setup();
    host.left.all = big();
    const window = visibleRange(0, 600, N); // ~30 rows are rendered by the virtual list
    expect(window.end - window.start).toBeLessThan(100);
    const t0 = performance.now();
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', { kind: 'allVisible', entries: 'all' }),
    );
    const ms = performance.now() - t0;
    const expectedHidden = host.left.all.filter((e) => e.hidden).length;
    expect(r.counts.total).toBe(N - expectedHidden);
    expect(r.counts.folders).toBe(host.left.all.filter((e) => e.isDir && !e.hidden).length);
    expect(r.excluded.hidden).toBe(expectedHidden);
    expect(resolver.entries(r.handle)).toHaveLength(N - expectedHidden);
    expect(ms).toBeLessThan(3000);
  });

  it('selected with 100k paths resolves fully and the model context stays tiny', () => {
    const { host, svc, resolver } = setup();
    host.left.all = big();
    host.left.selected = host.left.all.map((e) => e.path);
    const snap = svc.capture('req_1');
    const r = ok(resolver.resolve(snap, 'left', { kind: 'selected' }));
    expect(r.counts.total).toBe(N);
    expect(JSON.stringify(svc.modelContext(snap)).length).toBeLessThan(400);
    expect(JSON.stringify(r).length).toBeLessThan(2000);
  });

  it('directoryContents with hidden covers all 100k', () => {
    const { host, svc, resolver } = setup();
    host.left.all = big();
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', {
        kind: 'directoryContents',
        includeHidden: true,
        entries: 'all',
      }),
    );
    expect(r.counts.total).toBe(N);
  });
});

describe('AI-09 extension filter', () => {
  const list = (): ContextEntry[] => [
    entry('a.pdf'),
    entry('B.PDF'),
    entry('c.txt'),
    entry('d.pdf.exe'),
    entry('.pdf'),
    entry('e.'),
    entry('docs.pdf', { isDir: true }),
    entry('f.docx'),
    entry('.hidden.pdf', { hidden: true }),
  ];

  it('keeps only regular files with the extension, case-insensitively, and drops folders', () => {
    const { host, svc, resolver } = setup();
    host.left.all = list();
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', {
        kind: 'allVisible',
        entries: 'all',
        extensions: ['pdf'],
      }),
    );
    expect(resolver.entries(r.handle).map((e) => e.name)).toEqual(['a.pdf', 'B.PDF']);
    expect(r.counts).toEqual({ files: 2, folders: 0, total: 2 });
    expect(r.excluded.extension).toBe(6); // c.txt, d.pdf.exe, .pdf, e., docs.pdf, f.docx
    expect(r.scopeText).toMatch(/\.pdf/);
  });

  it('supports several extensions and applies on top of filesOnly', () => {
    const { host, svc, resolver } = setup();
    host.left.all = list();
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', {
        kind: 'allVisible',
        entries: 'filesOnly',
        extensions: ['pdf', 'docx'],
      }),
    );
    expect(resolver.entries(r.handle).map((e) => e.name)).toEqual(['a.pdf', 'B.PDF', 'f.docx']);
  });

  it('applies to directoryContents (hidden respected) and to selected', () => {
    const { host, svc, resolver } = setup();
    host.left.all = list();
    host.left.selected = ['D:\\Work\\a.pdf', 'D:\\Work\\c.txt', 'D:\\Work\\docs.pdf'];
    const snap = svc.capture('req_1');
    const d = ok(
      resolver.resolve(snap, 'left', {
        kind: 'directoryContents',
        includeHidden: true,
        entries: 'all',
        extensions: ['pdf'],
      }),
    );
    expect(resolver.entries(d.handle).map((e) => e.name)).toEqual([
      'a.pdf',
      'B.PDF',
      '.hidden.pdf',
    ]);
    const s = ok(resolver.resolve(snap, 'left', { kind: 'selected', extensions: ['pdf'] }));
    expect(resolver.entries(s.handle).map((e) => e.name)).toEqual(['a.pdf']);
  });

  it('reports an empty scope clearly when nothing matches (no silent fallback to everything)', () => {
    const { host, svc, resolver } = setup();
    host.left.all = [entry('a.txt')];
    const r = resolver.resolve(svc.capture('req_1'), 'left', {
      kind: 'allVisible',
      entries: 'all',
      extensions: ['pdf'],
    });
    expect(r).toMatchObject({ ok: false, error: { code: 'empty_scope' } });
    if (!r.ok) expect(r.error.message).toMatch(/\.pdf/);
  });

  it('filters the FULL listing (100k rows), not just rendered rows', () => {
    const { host, svc, resolver } = setup();
    const many: ContextEntry[] = [];
    for (let i = 0; i < 100_000; i++) many.push(entry(`f${i}.${i % 100 === 0 ? 'pdf' : 'txt'}`));
    host.left.all = many;
    const r = ok(
      resolver.resolve(svc.capture('req_1'), 'left', {
        kind: 'allVisible',
        entries: 'all',
        extensions: ['pdf'],
      }),
    );
    expect(r.counts.total).toBe(1000);
  });

  it('ignores unsafe extension values defensively (defence in depth beyond parseIntent)', () => {
    const { host, svc, resolver } = setup();
    host.left.all = list();
    const r = resolver.resolve(svc.capture('req_1'), 'left', {
      kind: 'allVisible',
      entries: 'all',
      extensions: ['*', '../x'],
    });
    expect(r).toMatchObject({ ok: false, error: { code: 'empty_scope' } });
  });
});
