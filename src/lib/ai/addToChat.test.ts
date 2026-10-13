// FER-486: "Add to chat" target selection and registry wiring.
import { describe, expect, it } from 'vitest';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { actionForEvent, getAction, isEnabled, searchActions } from '../actions';
import { addToChatTargets } from './addToChat';

const ENTRIES = [
  { name: 'notes.txt', path: 'C:\\d\\notes.txt', isDir: false, size: 10, modifiedMs: 0 },
  { name: 'docs', path: 'C:\\d\\docs', isDir: true, size: 0, modifiedMs: 0 },
];

function pane(
  selected: string[],
  cursor?: { name: string; path: string; isDir: boolean },
): PaneApi {
  return {
    path: 'C:\\d',
    selectedPaths: () => selected,
    cursorEntry: () => cursor,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    visibleEntries: () => ENTRIES,
  };
}

describe('Add to chat targets', () => {
  it('uses the selection (names and folder flags from the listing)', () => {
    ui.activePane = 0;
    registerPane(0, pane(['C:\\d\\notes.txt', 'C:\\d\\docs']));
    expect(addToChatTargets()).toEqual([
      { path: 'C:\\d\\notes.txt', name: 'notes.txt', isDir: false },
      { path: 'C:\\d\\docs', name: 'docs', isDir: true },
    ]);
  });

  it('falls back to the cursor item, never the ".." row', () => {
    ui.activePane = 0;
    registerPane(0, pane([], { name: 'notes.txt', path: 'C:\\d\\notes.txt', isDir: false }));
    expect(addToChatTargets()).toHaveLength(1);
    registerPane(0, pane([], { name: '..', path: 'C:\\', isDir: true }));
    expect(addToChatTargets()).toEqual([]);
    expect(isEnabled(getAction('ai.addToChat')!)).toBe(false);
  });

  it('is a palette command with Ctrl+Shift+L', () => {
    ui.activePane = 0;
    registerPane(0, pane(['C:\\d\\notes.txt']));
    expect(searchActions('Add to chat').map((a) => a.id)).toContain('ai.addToChat');
    // FER-485 keyword map: Indonesian aliases find the command too.
    for (const q of ['tanya ai', 'lampirkan']) {
      expect(searchActions(q)[0]?.id, q).toBe('ai.addToChat');
    }
    const hit = actionForEvent(
      { key: 'L', ctrlKey: true, metaKey: false, shiftKey: true, altKey: false },
      undefined,
      'Win32',
    );
    expect(hit?.id).toBe('ai.addToChat');
  });
});

describe('Ask AI agent about selection (FER-490)', () => {
  it('is a palette command found by Indonesian and agent keywords', () => {
    ui.activePane = 0;
    registerPane(0, pane(['C:\\d\\notes.txt']));
    expect(isEnabled(getAction('ai.askAgent')!)).toBe(true);
    expect(searchActions('Ask AI agent').map((a) => a.id)[0]).toBe('ai.askAgent');
    for (const q of ['tanya agent', 'claude', 'codex']) {
      expect(searchActions(q)[0]?.id, q).toBe('ai.askAgent');
    }
    registerPane(0, pane([], { name: '..', path: 'C:\\', isDir: true }));
    expect(isEnabled(getAction('ai.askAgent')!)).toBe(false);
  });
});
