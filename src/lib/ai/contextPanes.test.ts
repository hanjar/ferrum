import { describe, expect, it } from 'vitest';
import { PaneStore, toRow, type Row } from '../stores/paneStore.svelte';
import { filterRows, sortRows } from '../selection';
import { filterByView } from '../viewFilter';
import type { FileEntry } from '../ipc/types';
import { ContextService, tabIdOf } from './context';
import { ScopeResolver } from './contextResolver';
import { buildPaneProbe, registerPaneContext, registryHost } from './contextPanes';
import { ContextFlow } from './contextFlow';

function fe(i: number, over: Partial<FileEntry> = {}): FileEntry {
  return {
    name: `f${i}.dat`,
    path: `D:\\Big\\f${i}.dat`,
    isDir: false,
    isSymlink: false,
    size: i,
    modifiedMs: 0,
    hidden: false,
    ...over,
  } as FileEntry;
}

/** Meniru pipeline FilePane (sort -> view filter -> query/hidden) di atas PaneStore NYATA. */
function mountPane(
  store: PaneStore,
  paneId: 'left' | 'right',
  state: { q: string; hidden: boolean; selected: Set<string> },
) {
  const visible = (): Row[] => {
    let rows = sortRows(store.rows, { key: 'name', dir: 1 });
    rows = filterByView(rows, { mode: 'all', pattern: '', selectedPaths: state.selected });
    return filterRows(rows, { query: state.q, showHidden: state.hidden });
  };
  const probe = buildPaneProbe(() => ({
    paneId,
    tab: store.tab,
    rows: store.rows,
    visible,
    selected: () => state.selected,
    path: store.path,
    branchActive: false,
    inArchive: false,
    filterMode: 'all',
    filterPattern: '',
    filterQuery: state.q,
    showHidden: state.hidden,
    sort: { key: 'name', dir: 1 },
  }));
  return registerPaneContext(paneId, probe);
}

describe('FER-183/186 against the real PaneStore', () => {
  it('keeps a stable tab ID across navigation and changes it when the last tab is closed (reset)', async () => {
    const store = new PaneStore(async () => [fe(1)], 'D:\\Home');
    expect(store.tab).toBeUndefined(); // before bootTabs there is no tab: probe must be unavailable
    await store.newTab('D:\\Big');
    const id1 = tabIdOf(store.tab);
    await store.open('D:\\Big\\sub');
    expect(tabIdOf(store.tab)).toBe(id1);
    await store.newTab('D:\\Other');
    const id2 = tabIdOf(store.tab);
    expect(id2).not.toBe(id1);
    store.selectTab(0);
    expect(tabIdOf(store.tab)).toBe(id1);
    await store.closeTab(1); // closing another tab does not disturb the active one's ID
    expect(tabIdOf(store.tab)).toBe(id1);
    await store.closeTab(0); // last tab: reset in place to a fresh tab
    expect(tabIdOf(store.tab)).not.toBe(id1);
  });

  it('resolves allVisible over a real 100k-row store beyond any viewport, and flags stale after real navigation', async () => {
    const N = 100_000;
    const entries = Array.from({ length: N }, (_, i) => fe(i, { hidden: i % 100 === 0 }));
    const left = new PaneStore(async () => entries, 'D:\\Home');
    const right = new PaneStore(async () => [fe(1, { path: 'E:\\Backup\\x' })], 'E:\\Home');
    await left.newTab('D:\\Big');
    await right.newTab('E:\\Backup');
    const ls = { q: '', hidden: false, selected: new Set<string>() };
    const rs = { q: '', hidden: false, selected: new Set<string>() };
    const offL = mountPane(left, 'left', ls);
    const offR = mountPane(right, 'right', rs);
    try {
      const svc = new ContextService(registryHost(() => 'left'));
      const resolver = new ScopeResolver(svc);
      const flow = new ContextFlow(svc, resolver);
      const snap = svc.capture('req_1');
      const env = {
        schemaVersion: 1 as const,
        requestId: 'req_1',
        intent: {
          capability: 'archive.create' as const,
          sourcePane: 'left' as const,
          selector: { kind: 'allVisible' as const, entries: 'filesOnly' as const },
          format: 'zip' as const,
          target: { kind: 'sourceDirectory' as const },
        },
      };
      const r = flow.resolveArchive(snap, env);
      if (!r.ok) throw new Error(r.error.message);
      expect(r.scope.counts.total).toBe(N - N / 100);
      expect(r.scope.excluded.hidden).toBe(N / 100);
      expect(r.target.finalName).toBe('Big.zip');
      expect(resolver.entries(r.scope.handle)).toHaveLength(N - N / 100);

      // Navigasi nyata setelah snapshot -> plan lama ditahan.
      await left.open('D:\\Elsewhere');
      expect(flow.resolveArchive(snap, env)).toMatchObject({
        ok: false,
        error: { code: 'stale_context' },
      });
      expect(flow.revalidate(snap, r)).toMatchObject({ ok: false, reason: 'context_changed' });
      // Manifest yang sudah disetujui tetap utuh.
      expect(resolver.entries(r.scope.handle)).toHaveLength(N - N / 100);
    } finally {
      offL();
      offR();
    }
  }, 60_000);

  it('a refresh (new listing) after submit invalidates the snapshot', async () => {
    const store = new PaneStore(async () => [fe(1)], 'D:\\Home');
    await store.newTab('D:\\Big');
    const other = new PaneStore(async () => [], 'E:\\');
    await other.newTab('E:\\Backup');
    const st = { q: '', hidden: false, selected: new Set<string>() };
    const off1 = mountPane(store, 'left', st);
    const off2 = mountPane(other, 'right', st);
    try {
      const svc = new ContextService(registryHost(() => 'left'));
      const snap = svc.capture('req_1');
      expect(svc.check(snap, 'left')).toEqual({ ok: true });
      await store.refresh();
      expect(svc.check(snap, 'left')).toMatchObject({ ok: false, reason: 'context_changed' });
    } finally {
      off1();
      off2();
    }
  });

  it('maps symlinked directories to non-directories and exposes the unmounted pane as unavailable', () => {
    const row = toRow(fe(1, { isDir: true, isSymlink: true, name: 'lnk', path: 'D:\\lnk' }));
    const probe = buildPaneProbe(() => ({
      paneId: 'left',
      tab: {},
      rows: [row],
      visible: () => [row],
      selected: () => [],
      path: 'D:\\',
      branchActive: false,
      inArchive: false,
      filterMode: 'all',
      filterPattern: '',
      filterQuery: '',
      showHidden: false,
      sort: { key: 'name', dir: 1 },
    }))()!;
    expect(probe.allEntries()[0]).toMatchObject({ isDir: false, isSymlink: true });
    const host = registryHost(() => 'left');
    expect(host.probe('right')).toBeUndefined();
    expect(() => new ContextService(host).capture('req_1')).toThrow(/pane_unavailable/);
  });

  it('reports branch / search / archive view kinds from pane flags', () => {
    const mk = (
      over: Partial<Parameters<typeof buildPaneProbe>[0] extends () => infer R ? R : never>,
    ) =>
      buildPaneProbe(() => ({
        paneId: 'left',
        tab: {},
        rows: [],
        visible: () => [],
        selected: () => [],
        path: 'D:\\',
        branchActive: false,
        inArchive: false,
        filterMode: 'all',
        filterPattern: '',
        filterQuery: '',
        showHidden: false,
        sort: { key: 'name', dir: 1 },
        ...over,
      }))()!.viewKind;
    expect(mk({})).toBe('directory');
    expect(mk({ branchActive: true })).toBe('branch');
    expect(mk({ inArchive: true })).toBe('archive');
    expect(mk({ searchActive: true })).toBe('search');
  });
});
