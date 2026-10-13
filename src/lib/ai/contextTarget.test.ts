import { describe, expect, it } from 'vitest';
import { ContextService } from './context';
import { ScopeResolver, type ScopeOk } from './contextResolver';
import { resolveArchiveTarget, sanitizeOutputName, type TargetOk } from './contextTarget';
import { FakeHost, entry } from './contextTestKit';

function setup() {
  const host = new FakeHost();
  host.left.path = 'D:\\Projects\\Invoice';
  host.left.all = [entry('a.txt', { path: 'D:\\Projects\\Invoice\\a.txt' })];
  host.right.path = 'E:\\Backup';
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  return { host, svc, resolver };
}
function scopeOf(r: ReturnType<ScopeResolver['resolve']>): ScopeOk {
  if (!r.ok) throw new Error(r.error.message);
  return r;
}
function ok(r: ReturnType<typeof resolveArchiveTarget>): TargetOk {
  if (!r.ok) throw new Error(`${r.error.code}: ${r.error.message}`);
  return r;
}
const all = { kind: 'allVisible', entries: 'all' } as const;

function run(
  s: ReturnType<typeof setup>,
  args: {
    target?: Parameters<typeof resolveArchiveTarget>[2]['target'];
    outputName?: string;
    selector?: Parameters<ScopeResolver['resolve']>[2];
  } = {},
) {
  const snap = s.svc.capture('req_1');
  const scope = scopeOf(s.resolver.resolve(snap, 'left', args.selector ?? all));
  return resolveArchiveTarget(s.svc, snap, {
    sourcePane: 'left',
    target: args.target ?? { kind: 'sourceDirectory' },
    outputName: args.outputName,
    scope,
    manifest: s.resolver.entries(scope.handle),
  });
}

describe('FER-185 default target and name', () => {
  it('defaults to the source directory with <basename>.zip', () => {
    const t = ok(run(setup()));
    expect(t.targetPane).toBe('left');
    expect(t.targetDirectory).toBe('D:\\Projects\\Invoice');
    expect(t.finalName).toBe('Invoice.zip');
    expect(t.outputPath).toBe('D:\\Projects\\Invoice\\Invoice.zip');
    expect(t.renamed).toBe(false);
    expect(t.previewText).toContain('Invoice.zip');
    expect(t.previewText).toContain('1 file');
  });

  it.each([
    ['C:\\', 'archive.zip'],
    ['C:', 'archive.zip'],
    ['/', 'archive.zip'],
    ['D:\\Projects\\Invoice\\', 'Invoice.zip'],
    ['/home/u/My Docs', 'My Docs.zip'],
  ])('derives the name for %s', (path, name) => {
    const s = setup();
    s.host.left.path = path;
    const t = ok(run(s));
    expect(t.finalName).toBe(name);
  });

  it('falls back to archive.zip for reserved device names', () => {
    const s = setup();
    s.host.left.path = 'D:\\CON';
    expect(ok(run(s)).finalName).toBe('archive.zip');
  });

  it('targets another pane by snapshot reference', () => {
    const s = setup();
    const t = ok(run(s, { target: { kind: 'pane', pane: 'right' } }));
    expect(t.targetPane).toBe('right');
    expect(t.targetDirectory).toBe('E:\\Backup');
    expect(t.outputPath).toBe('E:\\Backup\\Invoice.zip');
  });

  it('activeAtSubmit target uses the snapshot active pane even if the live active pane changed', () => {
    const s = setup();
    s.host.active = 'right';
    const snap = s.svc.capture('req_1');
    s.host.active = 'left';
    const scope = scopeOf(s.resolver.resolve(snap, 'left', all));
    const t = ok(
      resolveArchiveTarget(s.svc, snap, {
        sourcePane: 'left',
        target: { kind: 'pane', pane: 'activeAtSubmit' },
        scope,
        manifest: s.resolver.entries(scope.handle),
      }),
    );
    expect(t.targetPane).toBe('right');
  });
});

describe('FER-185 user supplied output name', () => {
  it('appends .zip and keeps an existing extension of any case', () => {
    expect(ok(run(setup(), { outputName: 'invoice-september' })).finalName).toBe(
      'invoice-september.zip',
    );
    expect(ok(run(setup(), { outputName: 'x.ZIP' })).finalName).toBe('x.ZIP');
  });

  it.each([
    '',
    '   ',
    '.zip',
    'a/b',
    'a\\b',
    '..\\x',
    '../x',
    'file:stream',
    'C:evil',
    'CON',
    'aux.zip',
    'LPT1.zip',
    'name.',
    'bad|name',
    'q?',
    'star*',
    'tab\tname',
    'nul\0',
    'a'.repeat(300),
  ])('rejects unsafe name %j', (name) => {
    const r = run(setup(), { outputName: name });
    expect(r).toMatchObject({ ok: false, error: { code: 'invalid_name' } });
  });

  it('sanitizeOutputName trims surrounding spaces', () => {
    expect(sanitizeOutputName('  report ')).toEqual({ ok: true, name: 'report.zip' });
  });
});

describe('FER-185 collisions', () => {
  it('suggests a unique name and leaves the existing file untouched', () => {
    const s = setup();
    s.host.left.all = [
      ...s.host.left.all,
      entry('Invoice.zip', { path: 'D:\\Projects\\Invoice\\Invoice.zip' }),
    ];
    const before = JSON.stringify(s.host.left.all);
    const t = ok(run(s));
    expect(t.collision).toBe(true);
    expect(t.renamed).toBe(true);
    expect(t.finalName).toBe('Invoice (2).zip');
    expect(t.requestedName).toBe('Invoice.zip');
    expect(t.previewText).toMatch(/sudah ada/i);
    expect(JSON.stringify(s.host.left.all)).toBe(before);
  });

  it('is case-insensitive and counts hidden existing files', () => {
    const s = setup();
    s.host.left.all = [
      entry('invoice.ZIP', { path: 'D:\\Projects\\Invoice\\invoice.ZIP', hidden: true }),
      entry('Invoice (2).zip', { path: 'D:\\Projects\\Invoice\\Invoice (2).zip' }),
    ];
    expect(
      ok(run(s, { selector: { kind: 'directoryContents', includeHidden: true, entries: 'all' } }))
        .finalName,
    ).toBe('Invoice (3).zip');
  });

  it('checks collisions against the TARGET pane listing, not the source', () => {
    const s = setup();
    s.host.right.all = [entry('Invoice.zip', { path: 'E:\\Backup\\Invoice.zip' })];
    const t = ok(run(s, { target: { kind: 'pane', pane: 'right' } }));
    expect(t.finalName).toBe('Invoice (2).zip');
  });

  it('does not exclude older zips from the input; only the new output is excluded', () => {
    const s = setup();
    s.host.left.all = [entry('Invoice.zip', { path: 'D:\\Projects\\Invoice\\Invoice.zip' })];
    const snap = s.svc.capture('req_1');
    const scope = scopeOf(s.resolver.resolve(snap, 'left', all));
    expect(s.resolver.entries(scope.handle).map((e) => e.name)).toEqual(['Invoice.zip']);
    const t = ok(
      resolveArchiveTarget(s.svc, snap, {
        sourcePane: 'left',
        target: { kind: 'sourceDirectory' },
        scope,
        manifest: s.resolver.entries(scope.handle),
      }),
    );
    expect(t.excludeFromInput).toEqual(['D:\\Projects\\Invoice\\Invoice (2).zip']);
  });
});

describe('FER-185 output inside the source subtree', () => {
  it('flags the selected folder that contains the target directory', () => {
    const s = setup();
    s.host.left.all = [entry('docs', { isDir: true, path: 'D:\\Projects\\Invoice\\docs' })];
    s.host.right.path = 'D:\\Projects\\Invoice\\docs\\out';
    const t = ok(run(s, { target: { kind: 'pane', pane: 'right' } }));
    expect(t.outputInsideSource).toBe(true);
    expect(t.excludeSubtreeOf).toEqual(['D:\\Projects\\Invoice\\docs']);
    expect(t.previewText).toMatch(/tidak ikut diarsipkan/i);
  });

  it('does not match sibling folders sharing a prefix', () => {
    const s = setup();
    s.host.left.all = [entry('docs', { isDir: true, path: 'D:\\Projects\\Invoice\\docs' })];
    s.host.right.path = 'D:\\Projects\\Invoice\\docs2';
    const t = ok(run(s, { target: { kind: 'pane', pane: 'right' } }));
    expect(t.outputInsideSource).toBe(false);
    expect(t.excludeSubtreeOf).toEqual([]);
  });

  it('target equal to the source directory is the plain default case (output is not in the frozen manifest)', () => {
    const t = ok(run(setup()));
    expect(t.outputInsideSource).toBe(false);
    expect(t.excludeFromInput).toEqual(['D:\\Projects\\Invoice\\Invoice.zip']);
  });
});

describe('FER-185 path types and staleness', () => {
  it('rejects an archive pane as the output target (read-only)', () => {
    const s = setup();
    s.host.right.viewKind = 'archive';
    expect(run(s, { target: { kind: 'pane', pane: 'right' } })).toMatchObject({
      ok: false,
      error: { code: 'target_read_only' },
    });
  });

  it('rejects a target pane that navigated away after the snapshot', () => {
    const s = setup();
    const snap = s.svc.capture('req_1');
    const scope = scopeOf(s.resolver.resolve(snap, 'left', all));
    s.host.right.path = 'F:\\Other';
    const r = resolveArchiveTarget(s.svc, snap, {
      sourcePane: 'left',
      target: { kind: 'pane', pane: 'right' },
      scope,
      manifest: s.resolver.entries(scope.handle),
    });
    expect(r).toMatchObject({ ok: false, error: { code: 'stale_context' } });
  });

  it('rejects a swap between resolution and target resolution', () => {
    const s = setup();
    const snap = s.svc.capture('req_1');
    const scope = scopeOf(s.resolver.resolve(snap, 'left', all));
    s.host.swap();
    const r = resolveArchiveTarget(s.svc, snap, {
      sourcePane: 'left',
      target: { kind: 'sourceDirectory' },
      scope,
      manifest: s.resolver.entries(scope.handle),
    });
    expect(r).toMatchObject({ ok: false, error: { code: 'stale_context' } });
  });
});

describe('FER-185 hidden/filter surfacing', () => {
  it('copies hidden and filter notes into warnings and the preview', () => {
    const s = setup();
    s.host.left.all = [
      entry('a.pdf', { path: 'D:\\Projects\\Invoice\\a.pdf' }),
      entry('b.txt', { path: 'D:\\Projects\\Invoice\\b.txt' }),
      entry('.secret', { path: 'D:\\Projects\\Invoice\\.secret', hidden: true }),
    ];
    s.host.left.filter = { mode: 'custom', pattern: '', query: '.pdf' };
    const t = ok(run(s));
    expect(t.warnings.some((w) => /tersembunyi/.test(w))).toBe(true);
    expect(t.warnings.some((w) => /filter/i.test(w))).toBe(true);
    expect(t.previewText).toMatch(/filter/i);
  });
});
