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
  } = {},
) {
  const onselect = vi.fn();
  const onzoom = vi.fn();
  render(Treemap, {
    items,
    metric: props.metric ?? 'logical',
    selectedId: props.selectedId ?? null,
    coverage: props.coverage ?? 'complete',
    onselect,
    onzoom,
  });
  await tick();
  fireResize(800, 500);
  await tick();
  return { onselect, onzoom };
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

  it('double-click zooms folders only; files never act', async () => {
    const items = [
      item({ id: 'd', name: 'folder', kind: 'dir', logicalBytes: 500 }),
      item({ id: 'f', name: 'file.bin', logicalBytes: 500 }),
    ];
    const { onzoom, onselect } = await mountTreemap(items);
    fireDblClick(tile('d'));
    expect(onzoom).toHaveBeenCalledTimes(1);
    expect(onzoom).toHaveBeenCalledWith('d');
    // double-click on a file: no zoom, no open, no execute — nothing at all
    fireDblClick(tile('f'));
    expect(onzoom).toHaveBeenCalledTimes(1);
    // but a plain click still selects the file
    fireEvent.click(tile('f'));
    expect(onselect).toHaveBeenCalledWith('f');
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
});
