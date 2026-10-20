import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  RECENT_COMMANDS_KEY,
  RECENT_COMMANDS_MAX,
  RECENT_EXCLUDED_IDS,
  recentCommands,
  sanitizeRecent,
} from './recentCommands.svelte';
import { actions, getAction, runAction } from '../actions';

beforeEach(() => recentCommands.clear());
afterEach(() => vi.restoreAllMocks());

const stored = () => JSON.parse(localStorage.getItem(RECENT_COMMANDS_KEY) ?? 'null');

describe('recent commands store (FER-580)', () => {
  it('keeps the newest first', () => {
    recentCommands.record('a');
    recentCommands.record('b');
    recentCommands.record('c');
    expect(recentCommands.ids).toEqual(['c', 'b', 'a']);
  });

  it('is unique: running a command again moves it to the front', () => {
    recentCommands.record('a');
    recentCommands.record('b');
    recentCommands.record('a');
    expect(recentCommands.ids).toEqual(['a', 'b']);
  });

  it('keeps at most 3', () => {
    for (const id of ['a', 'b', 'c', 'd', 'e']) recentCommands.record(id);
    expect(RECENT_COMMANDS_MAX).toBe(3);
    expect(recentCommands.ids).toEqual(['e', 'd', 'c']);
  });

  it('persists only action ids and reads them back', () => {
    recentCommands.record('show.tree');
    recentCommands.record('view.hidden');
    expect(stored()).toEqual(['view.hidden', 'show.tree']);
    expect(sanitizeRecent(stored())).toEqual(['view.hidden', 'show.tree']);
  });

  it('drops malformed stored data', () => {
    expect(sanitizeRecent('x')).toEqual([]);
    expect(sanitizeRecent([1, '', null, 'a', 'a', 'b', 'c', 'd'])).toEqual(['a', 'b', 'c']);
  });

  it('survives a storage that throws', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    expect(() => recentCommands.record('a')).not.toThrow();
    expect(recentCommands.ids).toEqual(['a']);
  });

  it('ignores excluded ids (palette itself, Enter-opens, Tab, go up, clipboard reflexes)', () => {
    for (const id of RECENT_EXCLUDED_IDS) recentCommands.record(id);
    expect(recentCommands.ids).toEqual([]);
  });
});

describe('runAction records (FER-580)', () => {
  it('records a command that ran, whatever the route (all go through runAction)', () => {
    const a = getAction('show.tree')!;
    expect(runAction(a)).toBe(true);
    expect(recentCommands.ids).toEqual(['show.tree']);
  });

  it('does not record a command that could not run', () => {
    const a = getAction('show.tree')!;
    const prev = a.available;
    a.available = () => false;
    try {
      expect(runAction(a)).toBe(false);
    } finally {
      a.available = prev;
    }
    expect(recentCommands.ids).toEqual([]);
  });

  it('does not record the palette action', () => {
    const a = getAction('palette.open')!;
    expect(runAction(a)).toBe(true);
    expect(recentCommands.ids).toEqual([]);
  });

  it('every excluded id is a real registry action', () => {
    const known = new Set(actions.map((a) => a.id));
    for (const id of RECENT_EXCLUDED_IDS) expect(known.has(id), id).toBe(true);
  });
});
