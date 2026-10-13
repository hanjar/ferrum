import { beforeEach, describe, expect, it } from 'vitest';
import {
  FERRUM_MIME,
  beginInternalDrag,
  dragItemCount,
  dragOriginPane,
  endInternalDrag,
  isInternalDrag,
  readDragPaths,
  setDragPaths,
} from './dragPayload';

/** Map-backed DataTransfer stand-in (jsdom has no DataTransfer). */
function fakeDataTransfer(): DataTransfer {
  const store = new Map<string, string>();
  return {
    effectAllowed: 'all',
    dropEffect: 'none',
    get types(): string[] {
      return [...store.keys()];
    },
    setData(type: string, value: string): void {
      store.set(type, value);
    },
    getData(type: string): string {
      return store.get(type) ?? '';
    },
  } as unknown as DataTransfer;
}

function fakeEvent(dt: DataTransfer = fakeDataTransfer()): DragEvent {
  return { dataTransfer: dt } as unknown as DragEvent;
}

beforeEach(() => endInternalDrag());

describe('drag payload encode/decode', () => {
  it('round-trips paths through the dataTransfer and marks the drag internal', () => {
    const e = fakeEvent();
    setDragPaths(e, ['C:\\a\\x.txt', 'C:\\a\\y.txt']);
    expect(e.dataTransfer?.effectAllowed).toBe('copyMove');
    expect(e.dataTransfer?.getData(FERRUM_MIME)).toBe(
      JSON.stringify({ paths: ['C:\\a\\x.txt', 'C:\\a\\y.txt'] }),
    );
    expect(readDragPaths(e)).toEqual(['C:\\a\\x.txt', 'C:\\a\\y.txt']);
    expect(isInternalDrag(e)).toBe(true);
  });

  it('readDragPaths returns null for malformed JSON', () => {
    const e = fakeEvent();
    e.dataTransfer?.setData(FERRUM_MIME, '{not json');
    expect(readDragPaths(e)).toBeNull();
  });

  it('readDragPaths returns null for JSON of the wrong shape', () => {
    for (const bad of ['["a.txt"]', '{"paths":"x"}', '{"paths":[1,2]}', '{"nope":[]}']) {
      const e = fakeEvent();
      e.dataTransfer?.setData(FERRUM_MIME, bad);
      expect(readDragPaths(e)).toBeNull();
    }
  });

  it('a dataTransfer without the MIME type is a foreign drag', () => {
    const e = fakeEvent();
    expect(readDragPaths(e)).toBeNull();
    expect(isInternalDrag(e)).toBe(false);
  });

  it('survives a missing dataTransfer (set no-ops, read/isInternal false)', () => {
    const e = { dataTransfer: undefined } as unknown as DragEvent;
    expect(() => setDragPaths(e, ['C:\\a'])).not.toThrow();
    expect(readDragPaths(e)).toBeNull();
    expect(isInternalDrag(e)).toBe(false);
  });
});

describe('drag origin tracking', () => {
  it('begin/endInternalDrag track the origin pane and item count', () => {
    expect(dragOriginPane()).toBeNull();
    expect(dragItemCount()).toBe(0);
    beginInternalDrag(0, 3);
    expect(dragOriginPane()).toBe(0);
    expect(dragItemCount()).toBe(3);
    endInternalDrag();
    expect(dragOriginPane()).toBeNull();
    expect(dragItemCount()).toBe(0);
  });

  it('a new drag replaces the previous origin', () => {
    beginInternalDrag(0, 1);
    beginInternalDrag(1, 5);
    expect(dragOriginPane()).toBe(1);
    expect(dragItemCount()).toBe(5);
  });
});
