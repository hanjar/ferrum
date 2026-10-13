import { describe, expect, it } from 'vitest';
import type { IntentEnvelope } from './contracts';
import { ContextService } from './context';
import { ScopeResolver } from './contextResolver';
import { ContextFlow } from './contextFlow';
import { FakeHost, entry } from './contextTestKit';

const archiveIntent = (over: Record<string, unknown> = {}, requestId = 'req_1'): IntentEnvelope =>
  ({
    schemaVersion: 1,
    requestId,
    intent: {
      capability: 'archive.create',
      sourcePane: 'left',
      selector: { kind: 'allVisible', entries: 'filesOnly' },
      format: 'zip',
      target: { kind: 'sourceDirectory' },
      ...over,
    },
  }) as IntentEnvelope;

const syncIntent: IntentEnvelope = {
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'directories.sync',
    leftPane: 'left',
    rightPane: 'right',
    mode: 'bidirectional',
  },
};

function setup() {
  const host = new FakeHost();
  host.left.path = 'D:\\Projects\\Invoice';
  host.left.all = [
    entry('a.txt', { path: 'D:\\Projects\\Invoice\\a.txt' }),
    entry('b.txt', { path: 'D:\\Projects\\Invoice\\b.txt' }),
    entry('sub', { path: 'D:\\Projects\\Invoice\\sub', isDir: true }),
  ];
  host.right.path = 'E:\\Backup';
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  return { host, svc, resolver, flow };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('FER-186 archive intent resolution', () => {
  it('resolves the example flow: scope, default target, explanation', () => {
    const { svc, flow } = setup();
    const snap = svc.capture('req_1');
    const r = flow.resolveArchive(snap, archiveIntent());
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.scope.counts.files).toBe(2);
      expect(r.target.outputPath).toBe('D:\\Projects\\Invoice\\Invoice.zip');
      expect(r.explanation).toContain('2 file');
    }
  });

  it('rejects an envelope whose request ID does not match the snapshot', () => {
    const { svc, flow } = setup();
    expect(flow.resolveArchive(svc.capture('req_1'), archiveIntent({}, 'req_other'))).toMatchObject(
      {
        ok: false,
        error: { code: 'request_mismatch' },
      },
    );
  });

  it('rejects other capabilities', () => {
    const { svc, flow } = setup();
    expect(flow.resolveArchive(svc.capture('req_1'), syncIntent)).toMatchObject({
      ok: false,
      error: { code: 'wrong_capability' },
    });
  });

  it('does not resolve (and creates no manifest) when the request was cancelled', () => {
    const { svc, flow, resolver } = setup();
    const snap = svc.capture('req_1');
    const r = flow.resolveArchive(snap, archiveIntent(), { cancelled: () => true });
    expect(r).toMatchObject({ ok: false, error: { code: 'cancelled' } });
    expect(resolver.manifestCount()).toBe(0);
  });

  it('releases the manifest when target resolution fails', () => {
    const { svc, flow, resolver } = setup();
    const snap = svc.capture('req_1');
    const r = flow.resolveArchive(snap, archiveIntent({ outputName: 'bad/name' }));
    expect(r).toMatchObject({ ok: false, error: { code: 'invalid_name' } });
    expect(resolver.manifestCount()).toBe(0);
  });

  it('empty selection for "yang dipilih" is a clear message, never select-all', () => {
    const { svc, flow } = setup();
    const r = flow.resolveArchive(
      svc.capture('req_1'),
      archiveIntent({ selector: { kind: 'selected' } }),
    );
    expect(r).toMatchObject({ ok: false, error: { code: 'empty_selection' } });
  });
});

describe('FER-186 late provider response never retargets', () => {
  async function late(mutate: (h: FakeHost) => void) {
    const { host, svc, flow } = setup();
    const snap = svc.capture('req_1');
    await tick(); // provider "thinking"
    mutate(host);
    await tick(); // response arrives late
    return flow.resolveArchive(snap, archiveIntent());
  }

  it('pane swap after submit -> stale, old target is not reused for the new left pane', async () => {
    expect(await late((h) => h.swap())).toMatchObject({
      ok: false,
      error: { code: 'stale_context' },
    });
  });
  it('navigation after submit -> stale', async () => {
    expect(await late((h) => (h.left.path = 'D:\\Else'))).toMatchObject({
      ok: false,
      error: { code: 'stale_context' },
    });
  });
  it('tab close after submit -> stale', async () => {
    expect(await late((h) => (h.left.tab = {}))).toMatchObject({
      ok: false,
      error: { code: 'stale_context' },
    });
  });
  it('hidden toggle after submit -> stale (scope would differ)', async () => {
    expect(await late((h) => (h.left.showHidden = true))).toMatchObject({
      ok: false,
      error: { code: 'stale_context' },
    });
  });
  it('focus change alone is harmless', async () => {
    expect((await late((h) => (h.active = 'right'))).ok).toBe(true);
  });
  it('selection change: allVisible stays valid, selected scope is revalidated as stale', async () => {
    const { host, svc, flow } = setup();
    host.left.selected = ['D:\\Projects\\Invoice\\a.txt'];
    const snap = svc.capture('req_1');
    const r = flow.resolveArchive(snap, archiveIntent({ selector: { kind: 'selected' } }));
    expect(r.ok).toBe(true);
    host.left.selected = ['D:\\Projects\\Invoice\\b.txt'];
    if (r.ok)
      expect(flow.revalidate(snap, r)).toMatchObject({ ok: false, reason: 'selection_changed' });
    const all = flow.resolveArchive(snap, archiveIntent());
    expect(all.ok).toBe(true);
    if (all.ok) expect(flow.revalidate(snap, all)).toEqual({ ok: true });
  });

  it('revalidate catches the target pane drifting after a successful resolution', () => {
    const { host, svc, flow } = setup();
    const snap = svc.capture('req_1');
    const r = flow.resolveArchive(snap, archiveIntent({ target: { kind: 'pane', pane: 'right' } }));
    expect(r.ok).toBe(true);
    host.right.path = 'F:\\Moved';
    if (r.ok)
      expect(flow.revalidate(snap, r)).toMatchObject({ ok: false, reason: 'context_changed' });
  });

  it('after approval, the frozen item list is unaffected by later pane changes', () => {
    const { host, svc, flow, resolver } = setup();
    const snap = svc.capture('req_1');
    const r = flow.resolveArchive(snap, archiveIntent());
    if (!r.ok) throw new Error('expected ok');
    const before = resolver.entries(r.scope.handle).map((e) => e.path);
    host.left.all = [];
    host.left.path = 'Z:\\';
    expect(resolver.entries(r.scope.handle).map((e) => e.path)).toEqual(before);
    expect(r.target.outputPath).toBe('D:\\Projects\\Invoice\\Invoice.zip');
  });
});

describe('FER-186 non-directory views', () => {
  it.each(['branch', 'search', 'archive'] as const)(
    '%s source view is reported as not yet supported',
    (kind) => {
      const { host, svc, flow } = setup();
      host.left.viewKind = kind;
      expect(flow.resolveArchive(svc.capture('req_1'), archiveIntent())).toMatchObject({
        ok: false,
        error: { code: 'unsupported_view' },
      });
    },
  );

  it('archive target pane is rejected as read-only', () => {
    const { host, svc, flow } = setup();
    host.right.viewKind = 'archive';
    expect(
      flow.resolveArchive(
        svc.capture('req_1'),
        archiveIntent({ target: { kind: 'pane', pane: 'right' } }),
      ),
    ).toMatchObject({ ok: false, error: { code: 'target_read_only' } });
  });
});

describe('FER-186 sync root resolution', () => {
  it('accepts two distinct disk roots and describes both', () => {
    const { svc, flow } = setup();
    const r = flow.resolveSync(svc.capture('req_1'), syncIntent);
    expect(r).toMatchObject({
      ok: true,
      leftRoot: 'D:\\Projects\\Invoice',
      rightRoot: 'E:\\Backup',
    });
  });
  it('rejects identical roots (case-insensitive) and nested roots', () => {
    const a = setup();
    a.host.right.path = 'd:\\projects\\invoice\\';
    expect(flow_(a).ok).toBe(false);
    const b = setup();
    b.host.right.path = 'D:\\Projects\\Invoice\\sub';
    expect(flow_(b)).toMatchObject({ ok: false, error: { code: 'nested_roots' } });
    const c = setup();
    c.host.right.path = 'D:\\Projects';
    expect(flow_(c)).toMatchObject({ ok: false, error: { code: 'nested_roots' } });
    const d = setup();
    d.host.right.path = 'D:\\Projects\\Invoice2';
    expect(flow_(d).ok).toBe(true);
  });
  it('rejects non-directory views on either side and stale panes', () => {
    const a = setup();
    a.host.right.viewKind = 'archive';
    expect(flow_(a)).toMatchObject({ ok: false, error: { code: 'unsupported_view' } });
    const b = setup();
    const snap = b.svc.capture('req_1');
    b.host.swap();
    expect(b.flow.resolveSync(snap, syncIntent)).toMatchObject({
      ok: false,
      error: { code: 'stale_context' },
    });
  });
  function flow_(s: ReturnType<typeof setup>) {
    return s.flow.resolveSync(s.svc.capture('req_1'), syncIntent);
  }
});
