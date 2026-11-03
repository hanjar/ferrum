import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const beginTransferTo = vi.fn();
vi.mock('./dialogStore.svelte', () => ({
  dialogs: {
    beginTransferTo: (...a: unknown[]) => beginTransferTo(...a),
  },
}));
const shiftMock = vi.fn();
vi.mock('../ipc/dragOut', () => ({
  dragShiftDown: () => shiftMock(),
}));
const paths: Record<number, string> = { 0: 'C:\\left', 1: 'D:\\right' };
vi.mock('./panesRegistry.svelte', () => ({
  paneById: (id: number) => (id in paths ? { path: paths[id] } : undefined),
}));

import { paneAt, routeDrop } from './externalDrop';

/** Fake `elementFromPoint` returning a node inside `<section data-pane="n">` (or none). */
function overPane(n: 0 | 1 | null) {
  const el = document.createElement('div');
  if (n !== null) {
    const section = document.createElement('section');
    section.setAttribute('data-pane', String(n));
    section.appendChild(el);
    document.body.appendChild(section);
  } else {
    document.body.appendChild(el);
  }
  document.elementFromPoint = vi.fn(() => el);
}

const FILES = ['C:\\left\\a.txt', 'C:\\left\\b.txt'];
const POS = { x: 300, y: 200 };

beforeEach(() => {
  beginTransferTo.mockReset();
  shiftMock.mockReset();
  shiftMock.mockResolvedValue(false);
  Object.defineProperty(window, 'devicePixelRatio', { value: 1.5, configurable: true });
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('paneAt', () => {
  it('converts physical pixels with devicePixelRatio and finds the pane', () => {
    overPane(1);
    expect(paneAt(POS)).toBe(1);
    expect(document.elementFromPoint).toHaveBeenCalledWith(200, 200 / 1.5);
  });

  it('is null off a pane or without a position', () => {
    overPane(null);
    expect(paneAt(POS)).toBeNull();
    expect(paneAt(undefined)).toBeNull();
  });
});

describe('routeDrop', () => {
  it('ignores a Ferrum drag dropped on its own pane', async () => {
    overPane(0);
    expect(await routeDrop(FILES, POS, 0)).toBe(false);
    expect(beginTransferTo).not.toHaveBeenCalled();
  });

  it('copies a Ferrum drag dropped on the other pane', async () => {
    overPane(1);
    expect(await routeDrop(FILES, POS, 0)).toBe(true);
    expect(beginTransferTo).toHaveBeenCalledWith('copy', FILES, 'D:\\right');
  });

  it('moves with Shift held', async () => {
    overPane(1);
    shiftMock.mockResolvedValue(true);
    expect(await routeDrop(FILES, POS, 0)).toBe(true);
    expect(beginTransferTo).toHaveBeenCalledWith('move', FILES, 'D:\\right');
  });

  it('copies a drag from outside Ferrum into the pane under the cursor', async () => {
    overPane(0);
    expect(await routeDrop(['E:\\x\\y.png'], POS, null)).toBe(true);
    expect(beginTransferTo).toHaveBeenCalledWith('copy', ['E:\\x\\y.png'], 'C:\\left');
    expect(shiftMock).not.toHaveBeenCalled();
  });

  it('does nothing when the drop is not over a pane', async () => {
    overPane(null);
    expect(await routeDrop(FILES, POS, null)).toBe(false);
    expect(await routeDrop(FILES, POS, 0)).toBe(false);
    expect(beginTransferTo).not.toHaveBeenCalled();
  });

  it('does nothing for an empty path list', async () => {
    overPane(1);
    expect(await routeDrop([], POS, null)).toBe(false);
  });
});
