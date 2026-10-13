/**
 * Tree sidebar state: logical drives as roots, folders as lazy nodes.
 * Expanding a node fetches its children once; collapsing only hides them
 * (cache is kept). Per-path sequence numbers protect against stale responses.
 */
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { listDir, isInTauri } from '../ipc/tauri';
import { listDrives } from '../ipc/drives';
import { FsError, type FileEntry } from '../ipc/types';
import { mockFetcher, mockHome } from '../mockData';
import { compareNames } from '../selection';
import { baseName, type Fetcher } from './paneStore.svelte';

/** A tree root row: a real drive (desktop) or a mock pseudo-root (browser). */
export interface TreeRoot {
  /** Absolute path navigable by a pane. */
  path: string;
  label: string;
  /** Right-aligned meta text, e.g. the drive letter `C:` (prototype style). */
  meta: string;
}

export type DriveProvider = () => Promise<TreeRoot[]>;

/** Listing for tree nodes: real IPC on desktop, prototype fixtures in browser. */
export function defaultFetcher(path: string): Promise<FileEntry[]> {
  return isInTauri() ? listDir(path) : mockFetcher(path);
}

/** Drive letter from a root path (`C:\…` → `C:`), '' when not applicable. */
function driveLetterMeta(path: string): string {
  return /^[A-Za-z]:/.test(path) ? path.slice(0, 2).toUpperCase() : '';
}

/** Root provider: logical drives on desktop, mock pseudo-roots in browser dev. */
export function defaultDriveProvider(): Promise<TreeRoot[]> {
  if (isInTauri()) {
    return listDrives().then((drives) =>
      drives
        .filter((d) => d.status === 'ok' || d.driveType === 'fixed')
        .map((d) => ({
          path: d.path,
          label: d.label || driveLetterMeta(d.path),
          meta: driveLetterMeta(d.path),
        })),
    );
  }
  return Promise.resolve(
    mockHome.map((path) => ({
      path,
      label: baseName(path),
      meta: driveLetterMeta(path),
    })),
  );
}

/** Tree nodes list folders only; within the listing, numeric-aware name order. */
export function sortFolders(entries: FileEntry[]): FileEntry[] {
  return entries.filter((e) => e.isDir).sort((a, b) => compareNames(a.name, b.name));
}

export class TreeStore {
  roots = $state<TreeRoot[]>([]);
  rootsLoading = $state(false);
  rootsError = $state<FsError | null>(null);
  /** Highlighted node path; navigation itself goes through the pane. */
  selected = $state<string | null>(null);

  /** Self-reactive collections (SvelteMap/SvelteSet) — safe to mutate in place. */
  expanded = new SvelteSet<string>();
  children = new SvelteMap<string, FileEntry[]>();
  loading = new SvelteSet<string>();
  errors = new SvelteMap<string, FsError>();

  private seq = new SvelteMap<string, number>();

  constructor(
    private fetcher: Fetcher = defaultFetcher,
    private driveProvider: DriveProvider = defaultDriveProvider,
  ) {}

  /** Load the root rows (drives). Called once when the tree mounts. */
  async init(): Promise<void> {
    this.rootsLoading = true;
    this.rootsError = null;
    try {
      this.roots = await this.driveProvider();
      this.rootsLoading = false;
    } catch (e) {
      this.roots = [];
      this.rootsLoading = false;
      this.rootsError = e instanceof FsError ? e : FsError.fromIPC(e);
    }
  }

  /** Expand (fetching children lazily, once); no-op when already expanded.
   *  `toggle` collapses instead, so restore paths can use this directly. */
  async expand(path: string): Promise<void> {
    if (this.expanded.has(path)) return;
    this.expanded.add(path);
    if (!this.children.has(path) && !this.loading.has(path)) {
      await this.loadChildren(path);
    }
  }

  /** Expand (fetching children lazily, once) or collapse (children stay cached). */
  async toggle(path: string): Promise<void> {
    if (this.expanded.has(path)) {
      this.expanded.delete(path);
      return;
    }
    await this.expand(path);
  }

  /** Force a re-fetch of one node's children, whatever its expanded state.
   *  Supersedes any in-flight request for the node (per-path seq guard). */
  async refresh(path: string): Promise<void> {
    await this.loadChildren(path);
  }

  childrenOf(path: string): FileEntry[] {
    return this.children.get(path) ?? [];
  }

  errorOf(path: string): FsError | undefined {
    return this.errors.get(path);
  }

  isLoading(path: string): boolean {
    return this.loading.has(path);
  }

  /** True once a node's children have been fetched (empty dir vs not yet loaded). */
  hasFetched(path: string): boolean {
    return this.children.has(path);
  }

  select(path: string): void {
    this.selected = path;
  }

  /** Two-way sync (backlog polish): expand the ancestors of the ACTIVE
   *  pane's path and highlight it. Ancestors load lazily, top-down. */
  async followActivePath(path: string): Promise<void> {
    const ancestors = ancestorPaths(path);
    for (const anc of ancestors) {
      await this.expand(anc);
    }
    this.selected = path;
  }

  private async loadChildren(path: string): Promise<void> {
    const mySeq = (this.seq.get(path) ?? 0) + 1;
    this.seq.set(path, mySeq);
    this.loading.add(path);
    this.errors.delete(path);
    try {
      const entries = await this.fetcher(path);
      if (mySeq !== this.seq.get(path)) return; // superseded by a newer request
      this.children.set(path, sortFolders(entries));
      this.loading.delete(path);
    } catch (e) {
      if (mySeq !== this.seq.get(path)) return;
      this.loading.delete(path);
      // No cached children on failure: the chevron stays and re-expanding retries.
      this.children.delete(path);
      this.errors.set(path, e instanceof FsError ? e : FsError.fromIPC(e));
    }
  }
}

/** Ancestor directories of `path`, root-first, excluding the path itself
 *  ("C:\Users\you" -> ["C:\", "C:\Users"]). Keys must match listed row
 *  paths exactly, so only the drive root keeps its trailing separator. */
export function ancestorPaths(path: string): string[] {
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

export const sidebarTree = new TreeStore();
