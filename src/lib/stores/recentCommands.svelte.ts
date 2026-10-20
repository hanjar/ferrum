/**
 * FER-580: the last few commands the user ran, for the "Recent commands" section at the top of
 * the empty Command Palette. Only registry action ids are stored (never a path or an argument),
 * in this browser profile's localStorage like the other small local preferences.
 *
 * `runAction()` (actions.ts) is the one funnel every route goes through (palette, keyboard
 * shortcut, menu bar, context menu, Keybar), so it is the single place that records.
 */

export const RECENT_COMMANDS_KEY = 'ferrum.recentCommands.v1';
export const RECENT_COMMANDS_MAX = 3;

/**
 * Actions that never count as "a command the user ran again": the palette itself, plain
 * navigation / cursor-like moves that happen dozens of times a minute (Enter opens the row under
 * the cursor, Tab switches panes, Backspace goes up), and the clipboard reflexes (Ctrl+C / X / V).
 * Without this list the section would always read "Paste, Copy, Go up" and never help.
 */
export const RECENT_EXCLUDED_IDS: readonly string[] = [
  'palette.open',
  'pane.switch',
  'file.open',
  'nav.up',
  'nav.editPath',
  'commands.back',
  'search.close',
  'select.none',
  'file.clipCopy',
  'file.clipCut',
  'file.clipPaste',
  'file.quit',
];

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

/** Newest first, unique, at most `RECENT_COMMANDS_MAX`; anything malformed is dropped. */
export function sanitizeRecent(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  for (const id of raw) {
    if (typeof id !== 'string' || id === '' || out.includes(id)) continue;
    out.push(id);
    if (out.length >= RECENT_COMMANDS_MAX) break;
  }
  return out;
}

function load(): string[] {
  try {
    const text = storage()?.getItem(RECENT_COMMANDS_KEY);
    return text ? sanitizeRecent(JSON.parse(text)) : [];
  } catch {
    return [];
  }
}

class RecentCommandsStore {
  /** Newest first. */
  ids = $state<string[]>(load());

  /** Record a run command id (excluded ids are ignored). Returns true when the list changed. */
  record(id: string): boolean {
    if (RECENT_EXCLUDED_IDS.includes(id) || id === '') return false;
    if (this.ids[0] === id) return false;
    this.ids = sanitizeRecent([id, ...this.ids]);
    try {
      storage()?.setItem(RECENT_COMMANDS_KEY, JSON.stringify(this.ids));
    } catch {
      // Private mode / quota: the in-memory list still works for this session.
    }
    return true;
  }

  /** Test helper: forget everything (memory and storage). */
  clear(): void {
    this.ids = [];
    try {
      storage()?.removeItem(RECENT_COMMANDS_KEY);
    } catch {
      // ignore
    }
  }
}

export const recentCommands = new RecentCommandsStore();
