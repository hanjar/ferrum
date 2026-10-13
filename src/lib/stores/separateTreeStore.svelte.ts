/**
 * Separate Tree per pane (FER-366, opens the last D33 deferral): each pane
 * can show its OWN drive-tree panel above its file list — Total Commander's
 * per-pane separate tree. Each pane owns a private TreeStore instance (the
 * same lazy listing infra as the global sidebar tree), so expansion,
 * selection, and loading are fully pane-isolated; the global sidebar keeps
 * its own singleton and behaviour. Toggling and expansion state persist to
 * localStorage like the view presets / custom columns pattern; malformed
 * storage recovers silently to the defaults, which are OFF for both panes
 * (zero-config behaviour unchanged).
 */
import type { PaneId } from '../uiState.svelte';
import { TreeStore } from './treeStore.svelte';

const KEY = 'ferrum.separateTree.v1';

/** Persisted shape: one toggle per pane + the expanded paths per pane. */
export interface SeparateTreePersisted {
  enabled: [boolean, boolean];
  expanded: { 0?: string[]; 1?: string[] };
}

/** Defensive parse: anything but the exact shape falls back to defaults. */
export function parseSeparateTreeState(raw: string | null): SeparateTreePersisted {
  const defaults: SeparateTreePersisted = { enabled: [false, false], expanded: {} };
  if (raw === null) return defaults;
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return defaults; // not JSON → defaults
  }
  if (typeof data !== 'object' || data === null) return defaults;
  const d = data as Record<string, unknown>;
  const out: SeparateTreePersisted = { enabled: [false, false], expanded: {} };
  if (
    Array.isArray(d.enabled) &&
    d.enabled.length === 2 &&
    typeof d.enabled[0] === 'boolean' &&
    typeof d.enabled[1] === 'boolean'
  ) {
    out.enabled = [d.enabled[0], d.enabled[1]];
  }
  if (typeof d.expanded === 'object' && d.expanded !== null) {
    const e = d.expanded as Record<string, unknown>;
    for (const pane of ['0', '1'] as const) {
      const list = e[pane];
      if (!Array.isArray(list)) continue;
      // Filter per entry: one bad path must not discard the pane's whole
      // restored expansion.
      const valid = list.filter(
        (p): p is string => typeof p === 'string' && p.length > 0 && p.length <= 1024,
      );
      if (valid.length <= 512) out.expanded[Number(pane) as PaneId] = valid;
    }
  }
  return out;
}

/** Best-effort write; storage being unavailable/full never breaks the UI. */
export function persistSeparateTreeState(
  state: SeparateTreePersisted,
  storage: Storage | undefined = globalThis.localStorage,
): void {
  try {
    storage?.setItem(KEY, JSON.stringify(state));
  } catch {
    /* storage full/unavailable — saving is best-effort */
  }
}

export class SeparateTreeStore {
  /** Per-pane toggle; defaults OFF (zero-config behaviour unchanged). */
  enabled = $state<Record<PaneId, boolean>>({ 0: false, 1: false });
  /** One private tree per pane — never shared with the global sidebar. */
  readonly trees: Record<PaneId, TreeStore>;

  private storage: Storage | undefined;
  /** Persisted expansion of a not-yet-shown tree, applied on first enable. */
  private storedExpansion: Partial<Record<PaneId, string[]>> = {};
  private hydrated: Record<PaneId, boolean> = { 0: false, 1: false };
  private lastSaved: string | null = null;

  constructor(
    makeTree: () => TreeStore = () => new TreeStore(),
    storage: Storage | undefined = globalThis.localStorage,
  ) {
    this.trees = { 0: makeTree(), 1: makeTree() };
    this.storage = storage;
    const stored = parseSeparateTreeState(storage?.getItem(KEY) ?? null);
    this.enabled = { 0: stored.enabled[0], 1: stored.enabled[1] };
    this.storedExpansion = { 0: stored.expanded[0], 1: stored.expanded[1] };
    this.lastSaved = JSON.stringify(this.state());
    // A pane restored as enabled starts showing its tree right away, so it
    // hydrates immediately; a pane toggled on later hydrates in toggle().
    for (const pane of [0, 1] as const) {
      if (this.enabled[pane]) this.hydrate(pane);
    }
  }

  isEnabled(pane: PaneId): boolean {
    return this.enabled[pane];
  }

  /** Toggle one pane's separate tree (the ACTIVE pane — registry decides).
   *  The first enable applies the persisted expansion (lazy refetch via
   *  TreeStore.expand). */
  toggle(pane: PaneId): boolean {
    const next = !this.enabled[pane];
    this.enabled[pane] = next;
    if (next) this.hydrate(pane);
    this.persist();
    return next;
  }

  /** Apply the persisted expansion once per pane lifetime: mark the stored
   *  paths expanded (children refetch lazily through TreeStore.expand). */
  private hydrate(pane: PaneId): void {
    if (this.hydrated[pane]) return;
    this.hydrated[pane] = true;
    for (const p of this.storedExpansion[pane] ?? []) void this.trees[pane].expand(p);
  }

  /** Two-way sync: this pane's tree follows ITS OWN pane's cwd (ancestors
   *  expand lazily top-down, the path is highlighted). Pane-isolated. Uses
   *  cwdAncestors so expansion keys match the rendered row identity. */
  async follow(pane: PaneId, path: string): Promise<void> {
    const tree = this.trees[pane];
    for (const anc of cwdAncestors(path)) await tree.expand(anc);
    tree.select(path);
    this.persist();
  }

  /** Serializes current state; callers track the reactive inputs (the
   *  component effect watches the pane's expanded set, toggles call it
   *  directly). Identical snapshots skip the write (echo guard). */
  persist(): void {
    const state = this.state();
    const snap = JSON.stringify(state);
    if (snap === this.lastSaved) return;
    this.lastSaved = snap;
    persistSeparateTreeState(state, this.storage);
  }

  private state(): SeparateTreePersisted {
    return {
      enabled: [this.enabled[0], this.enabled[1]],
      expanded: {
        0: [...this.trees[0].expanded],
        1: [...this.trees[1].expanded],
      },
    };
  }

  /** Test hook: in-memory state, storage, and trees back to defaults. */
  resetAll(): void {
    this.enabled = { 0: false, 1: false };
    this.storedExpansion = {};
    this.hydrated = { 0: false, 1: false };
    this.lastSaved = null;
    this.trees[0] = new TreeStore();
    this.trees[1] = new TreeStore();
    try {
      globalThis.localStorage?.removeItem(KEY);
    } catch {
      /* best-effort */
    }
  }
}

export const separateTree = new SeparateTreeStore();

/**
 * Ancestors of `path`, root-first, excluding the path itself, shaped like
 * TREE ROW paths: the drive/POSIX root keeps its separator ("C:\", "/"),
 * deeper directories do not ("C:\Users", not "C:\Users\"). Directory
 * entries listed by the backend carry no trailing separator, so follow
 * keys must match that identity or the rendered rows would never show
 * their expansion.
 */
export function cwdAncestors(path: string): string[] {
  const sep = path.includes('\\') ? '\\' : '/';
  const parts = path.split(/[\\/]+/).filter(Boolean);
  const isWin = /^[A-Za-z]:$/.test(parts[0] ?? '');
  const out: string[] = [];
  let acc = isWin ? '' : sep;
  for (let i = 0; i < parts.length - 1; i++) {
    if (i === 0 && isWin) {
      acc = parts[0] + sep;
    } else {
      acc = acc.endsWith(sep) ? acc + parts[i] : acc + sep + parts[i];
    }
    out.push(acc);
  }
  return out;
}
