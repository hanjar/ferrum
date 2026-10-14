/**
 * DSK-05 — treemap layout + component tests (FER-323).
 *
 * Layout tests are pure math (no DOM). Component tests mount the Svelte 5
 * component with @testing-library/svelte and drive the measured plot size
 * through a recording ResizeObserver stub, since jsdom has no layout engine.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Treemap from './Treemap.svelte';
import { OTHER_ID, metricValue, squarify } from './treemapLayout';
import type { DiskUsageChildItem } from '../ipc/types';

/* ── fixtures ────────────────────────────────────────────────────────── */

function item(
  partial: Partial<DiskUsageChildItem> & { id: string; name: string },
): DiskUsageChildItem {
  return {
    kind: 'file',
    logicalBytes: 0,
    allocatedBytes: 0,
    allocatedSupported: true,
    category: 'other',
    percentOfParent: null,
    percentOfScanRoot: null,
    ...partial,
  };
}

/** The brief's proportionality fixture: 72/12/8/6/2 logical bytes. */
function ratioFixture(): DiskUsageChildItem[] {
  return [72, 12, 8, 6, 2].map((v, i) =>
    item({ id: `i${i}`, name: `n${i}`, logicalBytes: v * 1024 * 1024 }),
  );
}

/* ── ResizeObserver stub that tests can fire manually ────────────────── */

class MockResizeObserver {
  static instances: MockResizeObserver[] = [];
  private cb: (entries: ResizeObserverEntry[]) => void;
  constructor(cb: (entries: ResizeObserverEntry[]) => void) {
    this.cb = cb;
    MockResizeObserver.instances.push(this);
  }
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
  emit(w: number, h: number): void {
    this.cb([{ contentRect: { width: w, height: h } } as unknown as ResizeObserverEntry]);
  }
}

beforeEach(() => {
  MockResizeObserver.instances = [];
  vi.stubGlobal('ResizeObserver', MockResizeObserver);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  MockResizeObserver.instances = [];
});

/* ── component mount helper ──────────────────────────────────────────── */

async function mountTreemap(
  items: DiskUsageChildItem[],
  props: {
    metric?: 'logical' | 'allocated';
    selectedId?: string | null;
    coverage?: 'complete' | 'partial' | 'stale';
    scanRootTotal?: number;
  } = {},
) {
  const onselect = vi.fn();
  const onzoom = vi.fn();
  const onreveal = vi.fn();
  render(Treemap, {
    items,
    metric: props.metric ?? 'logical',
    selectedId: props.selectedId ?? null,
    coverage: props.coverage ?? 'complete',
    scanRootTotal: props.scanRootTotal,
    onselect,
    onzoom,
    onreveal,
  });
  await tick();
  fireResize(800, 500);
  await tick();
  return { onselect, onzoom, onreveal };
}

function fireResize(w: number, h: number): void {
  const ro = MockResizeObserver.instances.at(-1);
  if (!ro) throw new Error('component never created a ResizeObserver');
  ro.emit(w, h);
}

function tile(id: string): HTMLElement {
  const el = document.querySelector(`button.tile[data-id="${id}"]`);
  if (!(el instanceof HTMLElement)) throw new Error(`tile "${id}" not found`);
  return el;
}

function tileStyles(): string[] {
  return [...document.querySelectorAll('button.tile')].map((el) => el.getAttribute('style') ?? '');
}

/** Bubbling native dblclick — the installed fireEvent has no dblclick
 *  creator, and Svelte 5 delegates ondblclick to the root, so the event
 *  must bubble. */
function fireDblClick(el: HTMLElement): void {
  el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
}

/* ── layout: squarify ────────────────────────────────────────────────── */

describe('squarify layout', () => {
  it('keeps areas proportional to bytes for the 72/12/8/6/2 fixture', () => {
    const items = ratioFixture();
    const res = squarify(items, 1000, 1000, { metric: 'logical' });
    expect(res.otherCount).toBe(0);
    expect(res.rects.length).toBe(5);
    expect(res.totalValue).toBe(100 * 1024 * 1024);
    const canvas = 1000 * 1000;
    for (const r of res.rects) {
      const bytes = r.item ? r.item.logicalBytes : 0;
      const expected = (bytes / res.totalValue) * canvas;
      // rounding may shift each edge by ≤ 0.5px — allow 1% plus px slack
      expect(Math.abs(r.w * r.h - expected)).toBeLessThan(expected * 0.01 + 2000);
      // overflow rounding ≤ 1px: rects stay inside the canvas
      expect(r.x).toBeGreaterThanOrEqual(0);
      expect(r.y).toBeGreaterThanOrEqual(0);
      expect(r.x + r.w).toBeLessThanOrEqual(1001);
      expect(r.y + r.h).toBeLessThanOrEqual(1001);
    }
  });

  it('lays a single item over the whole canvas', () => {
    const res = squarify([item({ id: 'only', name: 'only', logicalBytes: 42 })], 1000, 1000, {
      metric: 'logical',
    });
    expect(res.rects.length).toBe(1);
    const r = res.rects[0] as (typeof res.rects)[number];
    expect(r.id).toBe('only');
    expect(r.label).toBe('only');
    expect(r.value).toBe(42);
    expect(r.x).toBe(0);
    expect(r.y).toBe(0);
    expect(r.w).toBe(1000);
    expect(r.h).toBe(1000);
    expect(r.item?.id).toBe('only');
  });

  it('groups small tiles into Other with exact count and byte sums', () => {
    const items = [
      item({ id: 'big1', name: 'big1', logicalBytes: 5000 }),
      item({ id: 'big2', name: 'big2', logicalBytes: 3000 }),
      item({ id: 'big3', name: 'big3', logicalBytes: 1500 }),
      ...[1, 2, 3, 4, 5].map((i) => item({ id: `t${i}`, name: `t${i}`, logicalBytes: 100 })),
    ];
    const res = squarify(items, 1000, 1000, { metric: 'logical' });
    expect(res.otherCount).toBe(5);
    const other = res.rects.find((r) => r.id === OTHER_ID);
    expect(other).toBeTruthy();
    expect(other?.otherGroup?.count).toBe(5);
    expect(other?.otherGroup?.logicalBytes).toBe(500); // exact member sum
    expect(other?.otherGroup?.value).toBe(500);
    expect(other?.otherGroup?.members.map((m) => m.id)).toEqual(['t1', 't2', 't3', 't4', 't5']);
    expect(other?.label).toBe('Other · 5 item');
    expect(res.rects.filter((r) => r.item).map((r) => r.id)).toEqual(['big1', 'big2', 'big3']);
  });

  it('groups everything beyond the top-12 cut into Other', () => {
    const items = Array.from({ length: 15 }, (_, i) =>
      item({ id: `e${i}`, name: `e${i}`, logicalBytes: 100 }),
    );
    const res = squarify(items, 1000, 1000, { metric: 'logical' });
    expect(res.otherCount).toBe(3);
    expect(res.rects.filter((r) => r.item).length).toBe(12);
  });

  it('gives null-allocated items zero area, keeps them visible, never substitutes logical', () => {
    const items = [
      item({ id: 'a', name: 'a', logicalBytes: 700, allocatedBytes: 700 }),
      item({
        id: 'b',
        name: 'b',
        logicalBytes: 300,
        allocatedBytes: null,
        allocatedSupported: false,
      }),
    ];
    const res = squarify(items, 1000, 1000, { metric: 'allocated' });
    expect(res.totalValue).toBe(700); // logical NOT summed in
    const other = res.rects.find((r) => r.id === OTHER_ID);
    expect(other).toBeTruthy();
    expect(other?.value).toBe(0); // area 0 — b never becomes logical-300
    expect(other?.otherGroup?.allocatedBytes).toBeNull(); // honest partial
    expect(other?.otherGroup?.logicalBytes).toBe(300); // detail keeps the truth
    expect(other?.otherGroup?.members.map((m) => m.id)).toEqual(['b']);
    expect(res.rects.some((r) => r.id === 'b')).toBe(false);
    const ra = res.rects.find((r) => r.id === 'a');
    expect(ra && ra.w * ra.h > 0.999 * 1000 * 1000).toBe(true);
  });

  it('returns no rects for empty input or zero total (empty state)', () => {
    expect(squarify([], 800, 500, { metric: 'logical' }).rects).toEqual([]);
    const zeros = [item({ id: 'z1', name: 'z1' }), item({ id: 'z2', name: 'z2' })];
    const res = squarify(zeros, 800, 500, { metric: 'logical' });
    expect(res.totalValue).toBe(0);
    expect(res.rects).toEqual([]);
    expect(squarify(ratioFixture(), 0, 500, { metric: 'logical' }).rects).toEqual([]);
    expect(squarify(ratioFixture(), 800, Number.NaN, { metric: 'logical' }).rects).toEqual([]);
  });

  it('is deterministic: same input twice, and shuffled input matches sorted input', () => {
    const sorted = [...ratioFixture()].sort((a, b) => a.id.localeCompare(b.id));
    const shuffled = [...ratioFixture()].reverse();
    const a = squarify(sorted, 800, 500, { metric: 'logical' });
    const b = squarify(sorted, 800, 500, { metric: 'logical' });
    const c = squarify(shuffled, 800, 500, { metric: 'logical' });
    expect(JSON.stringify(a)).toBe(JSON.stringify(b));
    expect(a).toEqual(c);
    expect(metricValue(sorted[0], 'logical')).toBe(72 * 1024 * 1024);
  });

  it('handles dominant 99.8% child by laying out its grandchildren inside container', () => {
    const grandchildren: DiskUsageChildItem[] = [
      item({ id: 'gc1', name: 'gc1', logicalBytes: 60000 }),
      item({ id: 'gc2', name: 'gc2', logicalBytes: 30000 }),
      item({ id: 'gc3', name: 'gc3', logicalBytes: 9800 }),
    ];
    const items = [
      item({
        id: 'dominant',
        name: 'dominant',
        kind: 'dir',
        logicalBytes: 99800,
        children: grandchildren,
      }),
      item({ id: 'tiny', name: 'tiny', logicalBytes: 200 }),
    ];
    const res = squarify(items, 1000, 1000, { metric: 'logical' });
    const domRect = res.rects.find((r) => r.id === 'dominant');
    expect(domRect).toBeTruthy();
    expect(domRect?.isDirContainer).toBe(true);
    expect(domRect?.depth).toBe(1);

    // Grandchildren rects exist and have depth 2
    const gc1Rect = res.rects.find((r) => r.id === 'gc1');
    const gc2Rect = res.rects.find((r) => r.id === 'gc2');
    const gc3Rect = res.rects.find((r) => r.id === 'gc3');
    expect(gc1Rect).toBeTruthy();
    expect(gc2Rect).toBeTruthy();
    expect(gc3Rect).toBeTruthy();
    expect(gc1Rect?.depth).toBe(2);

    // Grandchildren are placed inside dominant container's content area
    expect(domRect?.contentRect).toBeDefined();
    if (domRect?.contentRect) {
      expect(gc1Rect!.x).toBeGreaterThanOrEqual(domRect.contentRect.x - 1);
      expect(gc1Rect!.y).toBeGreaterThanOrEqual(domRect.contentRect.y - 1);
      expect(gc1Rect!.x + gc1Rect!.w).toBeLessThanOrEqual(
        domRect.contentRect.x + domRect.contentRect.w + 1,
      );
    }
  });

  it('respects maxDepth cap and stops nesting beyond maxDepth', () => {
    const greatGrandchildren: DiskUsageChildItem[] = [
      item({ id: 'ggc1', name: 'ggc1', logicalBytes: 500 }),
    ];
    const grandchildren: DiskUsageChildItem[] = [
      item({
        id: 'gc1',
        name: 'gc1',
        kind: 'dir',
        logicalBytes: 800,
        children: greatGrandchildren,
      }),
    ];
    const items = [
      item({
        id: 'l1',
        name: 'l1',
        kind: 'dir',
        logicalBytes: 1000,
        children: grandchildren,
      }),
    ];
    // With maxDepth: 2, depth 3 (ggc1) should NOT be laid out
    const res = squarify(items, 1000, 1000, { metric: 'logical', maxDepth: 2 });
    expect(res.rects.some((r) => r.id === 'l1')).toBe(true);
    expect(res.rects.some((r) => r.id === 'gc1')).toBe(true);
    expect(res.rects.some((r) => r.id === 'ggc1')).toBe(false);
  });

  it('groups sub-level items into Other when level has more than 12 items', () => {
    const subItems = Array.from({ length: 15 }, (_, i) =>
      item({ id: `sub_${i}`, name: `sub_${i}`, logicalBytes: 100 }),
    );
    const items = [
      item({
        id: 'dir1',
        name: 'dir1',
        kind: 'dir',
        logicalBytes: 1500,
        children: subItems,
      }),
    ];
    const res = squarify(items, 1000, 1000, { metric: 'logical' });
    const subOther = res.rects.find((r) => r.id.includes(OTHER_ID) && r.depth === 2);
    expect(subOther).toBeTruthy();
    expect(subOther?.otherGroup?.count).toBe(3);
  });

  it('caps at maxRects (<= 2000) under large synthetic fixture (50k nodes) within reasonable time', () => {
    const l1Items: DiskUsageChildItem[] = [];
    let count = 0;
    for (let i = 0; i < 50; i++) {
      const l2Children: DiskUsageChildItem[] = [];
      for (let j = 0; j < 50; j++) {
        const l3Children: DiskUsageChildItem[] = [];
        for (let k = 0; k < 20; k++) {
          l3Children.push(
            item({
              id: `node-${count++}`,
              name: `file-${k}`,
              kind: 'file',
              logicalBytes: 100,
            }),
          );
        }
        l2Children.push(
          item({
            id: `node-${count++}`,
            name: `dir-${j}`,
            kind: 'dir',
            logicalBytes: 2000,
            children: l3Children,
          }),
        );
      }
      l1Items.push(
        item({
          id: `node-${count++}`,
          name: `root-${i}`,
          kind: 'dir',
          logicalBytes: 100000,
          children: l2Children,
        }),
      );
    }
    expect(count).toBeGreaterThanOrEqual(50000);

    const start = performance.now();
    const res = squarify(l1Items, 1200, 800, { metric: 'logical', maxRects: 2000 });
    const durationMs = performance.now() - start;

    expect(res.rects.length).toBeLessThanOrEqual(2000);
    expect(durationMs).toBeLessThan(1000);
  });
});

/* ── component ───────────────────────────────────────────────────────── */

describe('Treemap component', () => {
  it('click on a tile selects it; click on empty plot deselects', async () => {
    const items = [
      item({ id: 'a', name: 'a', logicalBytes: 500 }),
      item({ id: 'b', name: 'b', logicalBytes: 300 }),
      item({ id: 'c', name: 'c', logicalBytes: 200 }),
    ];
    const { onselect } = await mountTreemap(items);
    expect(tile('a')).toBeTruthy();
    fireEvent.click(tile('a'));
    expect(onselect).toHaveBeenCalledWith('a');
    const plot = document.querySelector('.plot');
    expect(plot).toBeTruthy();
    fireEvent.click(plot as HTMLElement);
    expect(onselect).toHaveBeenLastCalledWith(null);
    expect(onselect).toHaveBeenCalledTimes(2);
  });

  it('single click drills folders or selects files; double-click triggers reveal for both', async () => {
    const items = [
      item({ id: 'd', name: 'folder', kind: 'dir', logicalBytes: 500 }),
      item({ id: 'f', name: 'file.bin', logicalBytes: 500 }),
    ];
    const { onzoom, onselect, onreveal } = await mountTreemap(items);

    // Single click on folder zooms/drills
    fireEvent.click(tile('d'));
    expect(onzoom).toHaveBeenCalledWith('d');

    // Single click on file selects
    fireEvent.click(tile('f'));
    expect(onselect).toHaveBeenCalledWith('f');

    // Double-click on folder reveals in pane
    fireDblClick(tile('d'));
    expect(onreveal).toHaveBeenCalledWith('d');

    // Double-click on file reveals in pane
    fireDblClick(tile('f'));
    expect(onreveal).toHaveBeenCalledWith('f');
  });

  it('highlights the selected tile via selectedId', async () => {
    const items = [
      item({ id: 'a', name: 'a', logicalBytes: 700 }),
      item({ id: 'b', name: 'b', logicalBytes: 300 }),
    ];
    await mountTreemap(items, { selectedId: 'b' });
    expect(tile('b').getAttribute('aria-pressed')).toBe('true');
    expect(tile('b').className).toContain('sel');
    expect(tile('a').getAttribute('aria-pressed')).toBe('false');
  });

  it('shows an empty state without tiles when there is no data', async () => {
    const { onselect } = await mountTreemap([]);
    expect(document.querySelector('.empty')).toBeTruthy();
    expect(document.querySelectorAll('button.tile').length).toBe(0);
    fireEvent.click(document.querySelector('.plot') as HTMLElement);
    expect(onselect).toHaveBeenLastCalledWith(null);
  });

  it('shows the denied badge only when coverage is partial (and stale note)', async () => {
    const items = [item({ id: 'a', name: 'a', logicalBytes: 100 })];
    await mountTreemap(items, { coverage: 'partial' });
    const badge = document.querySelector('.badge');
    expect(badge?.textContent).toContain('denied');
    cleanup();
    await mountTreemap(items, { coverage: 'complete' });
    expect(document.querySelector('.badge')).toBeNull();
  });

  it('labels null allocated as unavailable in the tooltip (logical metric)', async () => {
    const items = [
      item({ id: 'a', name: 'a', logicalBytes: 700, allocatedBytes: 700 }),
      item({
        id: 'b',
        name: 'cloud-placeholder.txt',
        logicalBytes: 300,
        allocatedBytes: null,
        allocatedSupported: false,
        percentOfParent: 30,
        percentOfScanRoot: 30,
      }),
    ];
    await mountTreemap(items);
    fireEvent.mouseMove(tile('b'));
    await tick();
    const tip = document.querySelector('.tip');
    expect(tip).toBeTruthy();
    expect(tip?.textContent).toContain('cloud-placeholder.txt');
    expect(tip?.textContent).toContain('unavailable');
    expect(tip?.textContent).toContain('30.0%');
  });

  it('lists Other group members in its tooltip', async () => {
    const items = [
      item({ id: 'big', name: 'big', logicalBytes: 9500 }),
      ...[1, 2, 3, 4, 5].map((i) => item({ id: `t${i}`, name: `tiny${i}`, logicalBytes: 100 })),
    ];
    await mountTreemap(items);
    fireEvent.mouseMove(tile(OTHER_ID));
    await tick();
    const tip = document.querySelector('.tip');
    expect(tip?.textContent).toContain('Other · 5 item');
    expect(tip?.textContent).toContain('tiny1');
    expect(tip?.textContent).toContain('tiny5');
  });

  it('renders an accessible legend of the categories in use', async () => {
    const items = [
      item({ id: 'd', name: 'd', kind: 'dir', logicalBytes: 500 }),
      item({ id: 'v', name: 'v', category: 'video', logicalBytes: 300 }),
      item({ id: 'ar', name: 'ar', category: 'archive', logicalBytes: 200 }),
    ];
    await mountTreemap(items);
    const ul = document.querySelector('ul[aria-label="Legenda kategori warna"]');
    expect(ul).toBeTruthy();
    const labels = [...(ul as HTMLElement).querySelectorAll('li')].map((li) => li.textContent);
    expect(labels.some((l) => l?.includes('Folder'))).toBe(true);
    expect(labels.some((l) => l?.includes('Video'))).toBe(true);
    expect(labels.some((l) => l?.includes('Archive'))).toBe(true);
  });

  it('renders deterministically: two mounts of the same items produce identical tile styles', async () => {
    const items = ratioFixture();
    await mountTreemap(items);
    const first = tileStyles();
    cleanup();
    await mountTreemap(items);
    expect(tileStyles()).toEqual(first);
    expect(first.length).toBe(5);
  });

  it('regression: 99.8% dominant item displays 99.8% on tile, not 1.0%', async () => {
    const items = [
      item({ id: 'dominant', name: 'dominant', logicalBytes: 99800 }),
      item({ id: 'tiny', name: 'tiny', logicalBytes: 200 }),
    ];
    await mountTreemap(items);
    const domTile = tile('dominant');
    expect(domTile.textContent).toContain('99.8%');
    expect(domTile.textContent).not.toContain('1.0%');
  });

  it('displays both percent of focus and percent of scan root in tooltip when scanRootTotal is provided', async () => {
    const items = [
      item({ id: 'f1', name: 'f1.txt', logicalBytes: 5000 }),
      item({ id: 'f2', name: 'f2.txt', logicalBytes: 5000 }),
    ];
    // Focus total = 10000, scanRootTotal = 100000 (f1 is 50.0% of focus, 5.0% of scan root)
    await mountTreemap(items, { scanRootTotal: 100000 });
    fireEvent.mouseMove(tile('f1'));
    await tick();
    const tip = document.querySelector('.tip');
    expect(tip).toBeTruthy();
    expect(tip?.textContent).toContain('50.0% of focus');
    expect(tip?.textContent).toContain('of scan root: 5.0%');
  });
});
