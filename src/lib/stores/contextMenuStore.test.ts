import { afterEach, describe, expect, it } from 'vitest';
import {
  clampMenuPosition,
  contextMenu,
  estimateMenuHeight,
  ITEM_HEIGHT,
  MENU_PADDING,
  MENU_WIDTH,
  SEPARATOR_HEIGHT,
  type ContextMenuItem,
} from './contextMenuStore.svelte';

const item = (label = 'Do it'): ContextMenuItem => ({ label, run: () => {} });

afterEach(() => contextMenu.close());

describe('estimateMenuHeight', () => {
  it('counts items, separators and padding', () => {
    expect(estimateMenuHeight([])).toBe(MENU_PADDING);
    const mixed: ContextMenuItem[] = [item(), item(), { separator: true }, item()];
    expect(estimateMenuHeight(mixed)).toBe(MENU_PADDING + 3 * ITEM_HEIGHT + SEPARATOR_HEIGHT);
  });
});

describe('clampMenuPosition', () => {
  it('keeps the menu inside the viewport', () => {
    expect(clampMenuPosition(900, 700, 200, 1024, 768)).toEqual({
      x: 1024 - MENU_WIDTH,
      y: 768 - 200,
    });
    expect(clampMenuPosition(10, 20, 200, 1024, 768)).toEqual({ x: 10, y: 20 });
  });

  it('clamps the vertical position, never negative on tiny viewports', () => {
    // 236px menu fits horizontally at x=50 in a 320px viewport; the 600px
    // tall menu cannot fit vertically and pins to the top edge.
    expect(clampMenuPosition(50, 50, 600, 320, 240)).toEqual({ x: 50, y: 0 });
    // x beyond the right edge pins to viewport width minus menu width.
    expect(clampMenuPosition(400, 50, 600, 320, 240)).toEqual({ x: 84, y: 0 });
  });
});

describe('contextMenu store', () => {
  it('open() stores items and clamps to the window (jsdom 1024×768)', () => {
    const items: ContextMenuItem[] = [item('A')];
    contextMenu.open(5000, 5000, items);
    expect(contextMenu.isOpen).toBe(true);
    expect(contextMenu.items).toBe(items);
    expect(contextMenu.x).toBe(window.innerWidth - MENU_WIDTH);
    expect(contextMenu.y).toBe(window.innerHeight - estimateMenuHeight(items));
  });

  it('close() hides without clearing the items', () => {
    contextMenu.open(10, 10, [item('A')]);
    contextMenu.close();
    expect(contextMenu.isOpen).toBe(false);
    expect(contextMenu.items).toHaveLength(1);
  });

  it('open() records the owner token and replaces the previous one', () => {
    const a = {};
    const b = {};
    contextMenu.open(0, 0, [item('A')], a);
    expect(contextMenu.owner).toBe(a);
    contextMenu.open(0, 0, [item('B')], b);
    expect(contextMenu.owner).toBe(b);
  });
});
