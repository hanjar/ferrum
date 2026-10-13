/**
 * Search panel state (F2-16). Owns the current search (one at a time — a
 * new start supersedes the previous and its late events are dropped by id),
 * progressive results, and saved searches persisted to localStorage.
 */
import type { UnlistenFn } from '@tauri-apps/api/event';
import {
  defaultSearchOptions,
  onSearchEvent,
  searchCancel,
  searchStart,
  type SearchOptions,
  type SearchStats,
} from '../ipc/search';
import type { FileEntry } from '../ipc/types';
import { isInTauri } from '../ipc/tauri';
import { activePane } from './panesRegistry.svelte';
import { branch } from './branchStore.svelte';
import { ui } from '../uiState.svelte';
import { copyText } from '../clipboard';

export interface SavedSearch {
  name: string;
  root: string;
  options: SearchOptions;
}

const SAVED_KEY = 'ferrum.savedSearches.v1';

export function loadSavedSearches(
  storage: Storage | undefined = globalThis.localStorage,
): SavedSearch[] {
  try {
    const raw = storage?.getItem(SAVED_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (s): s is SavedSearch =>
        typeof s === 'object' &&
        s !== null &&
        typeof (s as SavedSearch).name === 'string' &&
        typeof (s as SavedSearch).root === 'string' &&
        typeof (s as SavedSearch).options === 'object',
    );
  } catch {
    return [];
  }
}

export function persistSavedSearches(
  saved: SavedSearch[],
  storage: Storage | undefined = globalThis.localStorage,
): void {
  try {
    storage?.setItem(SAVED_KEY, JSON.stringify(saved));
  } catch {
    /* storage full/unavailable — saving is best-effort */
  }
}

let seq = 0;

class SearchStore {
  panelOpen = $state(false);
  running = $state(false);
  root = $state('');
  options = $state<SearchOptions>(defaultSearchOptions());
  results = $state<FileEntry[]>([]);
  stats = $state<SearchStats | null>(null);
  errorText = $state('');
  saved = $state<SavedSearch[]>(loadSavedSearches());
  /** Name input bound to the Save button. */
  saveName = $state('');

  /** Keyboard cursor in the result list (-1 = none). */
  cursor = $state(-1);
  /** Transient feedback for Copy / Feed actions. */
  notice = $state('');

  private currentId: string | null = null;
  private unlisten: UnlistenFn | null = null;
  private started = false;

  get capped(): boolean {
    const max = this.options.maxResults ?? 1000;
    return this.results.length >= max;
  }

  togglePanel(): void {
    this.panelOpen = !this.panelOpen;
  }

  /** Subscribe once; browser dev (no Tauri) never starts a listener. */
  private async ensureListener(): Promise<void> {
    if (this.started || !isInTauri()) return;
    this.started = true;
    this.unlisten = await onSearchEvent((e) => this.onEvent(e));
  }

  private onEvent(e: {
    id: string;
    entries: FileEntry[];
    done: boolean;
    stats?: SearchStats;
    error?: string;
  }): void {
    if (e.id !== this.currentId) return; // stale search — ignore
    if (e.entries.length > 0) this.results.push(...e.entries);
    if (e.done) {
      this.running = false;
      this.currentId = null;
      this.stats = e.stats ?? null;
      if (e.error) this.errorText = e.error;
    }
  }

  /** Start a new search; a previous running search is cancelled first so a
   *  query change can never mix result sets. */
  async start(): Promise<void> {
    const pane = activePane();
    const root = this.root.trim() || pane?.path || '';
    const pattern = this.options.pattern.trim();
    if (!root || (!pattern && !(this.options.content ?? '').trim())) {
      this.errorText = 'Enter a name pattern or content text (and search inside a folder).';
      return;
    }
    await this.ensureListener();
    await this.cancel();
    this.root = root;
    this.results = [];
    this.cursor = -1;
    this.notice = '';
    this.stats = null;
    this.errorText = '';
    this.currentId = `search-${Date.now()}-${++seq}`;
    this.running = true;
    try {
      await searchStart(this.currentId, root, {
        ...this.options,
        maxResults: this.options.maxResults ?? 1000,
      });
    } catch (e) {
      this.running = false;
      this.currentId = null;
      this.errorText = e instanceof Error ? e.message : String(e);
    }
  }

  async cancel(): Promise<void> {
    if (this.currentId) await searchCancel(this.currentId);
  }

  /** Jump the ACTIVE pane to the folder containing a result. */
  openResultFolder(path: string): void {
    const i = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
    let dir = i > 0 ? path.slice(0, i) : path;
    if (/^[A-Za-z]:$/.test(dir)) dir += '\\'; // drive root keeps its slash
    activePane()?.openPath?.(dir);
  }

  /** Move the result cursor: delta steps, or 'home'/'end'. Returns the new index. */
  moveCursor(to: number | 'home' | 'end'): number {
    const n = this.results.length;
    if (n === 0) return (this.cursor = -1);
    let next: number;
    if (to === 'home') next = 0;
    else if (to === 'end') next = n - 1;
    else next = Math.min(n - 1, Math.max(0, this.cursor + to));
    return (this.cursor = next);
  }

  /** Enter on a result: reveal it (select it) in the ACTIVE pane. */
  revealCursor(): boolean {
    const r = this.results[this.cursor];
    if (!r) return false;
    const pane = activePane();
    if (pane?.revealPath) pane.revealPath(r.path);
    else this.openResultFolder(r.path);
    return true;
  }

  /** Copy every result path (one per line) to the clipboard. */
  async copyResults(): Promise<boolean> {
    if (this.results.length === 0) return false;
    const ok = await copyText(this.results.map((r) => r.path).join('\n'));
    this.notice = ok
      ? `Copied ${this.results.length} path${this.results.length === 1 ? '' : 's'}`
      : 'Copy failed — clipboard unavailable';
    return ok;
  }

  /** Feed the results into the ACTIVE pane as a virtual listing (the branch
   *  row source). Ctrl+B or any navigation in that pane returns to the folder. */
  feedToPane(): boolean {
    if (this.results.length === 0) return false;
    branch.feed(ui.activePane, this.root, `Search results · ${this.results.length} items`, [
      ...this.results,
    ]);
    this.notice = `Sent ${this.results.length} results to the active pane`;
    return true;
  }

  saveCurrent(): void {
    const name = this.saveName.trim();
    if (!name) return;
    const entry: SavedSearch = { name, root: this.root, options: { ...this.options } };
    this.saved = [entry, ...this.saved.filter((s) => s.name !== name)].slice(0, 50);
    persistSavedSearches(this.saved);
  }

  applySaved(saved: SavedSearch): void {
    this.root = saved.root;
    this.options = { ...saved.options };
  }

  removeSaved(name: string): void {
    this.saved = this.saved.filter((s) => s.name !== name);
    persistSavedSearches(this.saved);
  }

  /** Test hook: clears everything including saved searches. */
  reset(): void {
    void this.unlisten?.();
    this.unlisten = null;
    this.started = false;
    this.currentId = null;
    this.running = false;
    this.results = [];
    this.cursor = -1;
    this.notice = '';
    this.stats = null;
    this.errorText = '';
    this.panelOpen = false;
    this.root = '';
    this.options = defaultSearchOptions();
    this.saved = [];
    this.saveName = '';
  }

  /** Exposed for tests: feed an event through the same path as IPC. */
  testEvent(e: {
    id: string;
    entries: FileEntry[];
    done: boolean;
    stats?: SearchStats;
    error?: string;
  }): void {
    this.onEvent(e);
  }

  /** Exposed for tests: pretend a search is in flight under an id. */
  testInFlight(id: string): void {
    this.currentId = id;
    this.running = true;
  }
}

export const searchStore = new SearchStore();

/** Progress percentage helper for the panel (bounded by maxResults). */
export function searchProgressPct(matched: number, max: number | undefined): number {
  const cap = max ?? 1000;
  if (cap <= 0) return 0;
  return Math.min(100, (matched / cap) * 100);
}
