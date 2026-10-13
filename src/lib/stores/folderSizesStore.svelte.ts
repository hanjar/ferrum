/**
 * FER-477a: folder sizes for the Size column, from the FerrumIndexer.
 *
 * Opt-in (Settings → Indexer, `ui.indexerFolderSizes`). The service answers only in its High
 * performance mode — in the shared mode it refuses (an aggregate would reveal the size of content
 * the user may not see), and every other problem also leaves the cell at "—". The file list asks
 * for the folders it currently shows; requests are batched per parent folder, debounced, cached
 * for a short time, and paused after a refusal so a shared-mode PC is not asked on every scroll.
 */
import { indexerFolderSizes } from '../ipc/indexer';
import { formatSize } from '../format';
import { ui } from '../uiState.svelte';

/** How long a size (or an unknown) is reused before it is asked again. */
export const FOLDER_SIZE_TTL_MS = 30_000;
/** After a refusal that will not change soon (shared mode, degraded, not running): wait. */
export const FOLDER_SIZE_BACKOFF_MS = 60_000;
const DEBOUNCE_MS = 120;
/** Cached paths kept at most (oldest dropped). */
const MAX_CACHED = 5000;

/** Refusals that apply to every folder for a while (no point asking per scroll). */
const GLOBAL_REASONS = new Set(['shared_mode', 'degraded', 'unavailable', 'ipc', 'protocol']);

export interface FolderRef {
  /** Full path of the folder. */
  path: string;
  /** Its name (last component). */
  name: string;
}

function key(path: string): string {
  return path.replace(/\//g, '\\').toLowerCase();
}

/** Parent directory of `path` whose last component is `name`, as `X:\` or `X:\a\b`. */
export function parentFor(path: string, name: string): string | null {
  const p = path.replace(/\//g, '\\');
  if (!name || !p.toLowerCase().endsWith('\\' + name.toLowerCase())) return null;
  const parent = p.slice(0, p.length - name.length - 1);
  return /^[A-Za-z]:$/.test(parent) ? `${parent}\\` : parent;
}

export class FolderSizes {
  /** path key → bytes, or null when the index has no answer. Reactive. */
  sizes = $state<Record<string, number | null>>({});
  /** Last refusal reason (for Settings), null when the last answer was good. */
  reason = $state<string | null>(null);

  // Plain bookkeeping, never rendered: only `sizes`/`reason` are reactive.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  private fetchedAt = new Map<string, number>();
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  private queued = new Map<string, Map<string, string>>(); // parent → (path key → name)
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  private inFlight = new Set<string>();
  private blockedUntil = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private generation = 0;

  constructor(
    private fetcher: typeof indexerFolderSizes = indexerFolderSizes,
    private now: () => number = () => Date.now(),
  ) {}

  /** The Size cell text for a folder row: a size, or null for "—". */
  text(path: string): string | null {
    if (!ui.indexerFolderSizes) return null;
    const v = this.sizes[key(path)];
    return typeof v === 'number' ? formatSize(v) : null;
  }

  /** Raw bytes (sorting, tests). */
  bytes(path: string): number | null {
    const v = this.sizes[key(path)];
    return typeof v === 'number' ? v : null;
  }

  /** The list shows these folders: fetch what is missing or old (debounced). */
  want(folders: readonly FolderRef[]): void {
    if (!ui.indexerFolderSizes || folders.length === 0) return;
    const t = this.now();
    if (t < this.blockedUntil) return;
    let added = false;
    for (const f of folders) {
      const k = key(f.path);
      const at = this.fetchedAt.get(k);
      if ((at !== undefined && t - at < FOLDER_SIZE_TTL_MS) || this.inFlight.has(k)) continue;
      const parent = parentFor(f.path, f.name);
      // Only drive-letter folders can be in the index (UNC is never asked; inner paths of an
      // archive under a drive are, and the service answers "not indexed" for them).
      if (!parent || !/^[A-Za-z]:\\/.test(parent)) continue;
      let q = this.queued.get(parent);
      if (!q) {
        // eslint-disable-next-line svelte/prefer-svelte-reactivity
        q = new Map();
        this.queued.set(parent, q);
      }
      if (!q.has(k)) {
        q.set(k, f.name);
        added = true;
      }
    }
    if (added && this.timer === null) {
      this.timer = setTimeout(() => {
        this.timer = null;
        void this.flush();
      }, DEBOUNCE_MS);
    }
  }

  /** Send every queued batch now (also used by tests). */
  async flush(): Promise<void> {
    const batches = [...this.queued.entries()];
    this.queued.clear();
    const gen = this.generation;
    await Promise.all(
      batches.map(async ([parent, items]) => {
        const keys = [...items.keys()];
        const names = [...items.values()];
        for (const k of keys) this.inFlight.add(k);
        try {
          const r = await this.fetcher(parent, names);
          if (gen !== this.generation) return; // cleared meanwhile (setting turned off)
          const t = this.now();
          const next = { ...this.sizes };
          keys.forEach((k, i) => {
            next[k] = r.available ? (r.sizes[i] ?? null) : null;
            this.fetchedAt.set(k, t);
          });
          this.sizes = next;
          this.reason = r.available ? null : r.reason;
          if (!r.available && r.reason && GLOBAL_REASONS.has(r.reason)) {
            this.blockedUntil = t + FOLDER_SIZE_BACKOFF_MS;
          }
          this.trim();
        } finally {
          for (const k of keys) this.inFlight.delete(k);
        }
      }),
    );
  }

  /** Forget everything (setting turned off, or the mode changed). */
  clear(): void {
    this.generation++;
    this.sizes = {};
    this.reason = null;
    this.fetchedAt.clear();
    this.queued.clear();
    this.blockedUntil = 0;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private trim(): void {
    if (this.fetchedAt.size <= MAX_CACHED) return;
    const old = [...this.fetchedAt.entries()].sort((a, b) => a[1] - b[1]);
    const drop = old.slice(0, this.fetchedAt.size - MAX_CACHED);
    const next = { ...this.sizes };
    for (const [k] of drop) {
      this.fetchedAt.delete(k);
      delete next[k];
    }
    this.sizes = next;
  }
}

export const folderSizes = new FolderSizes();
