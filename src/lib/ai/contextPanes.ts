// AI-03 / FER-183: jembatan read-only dari panel nyata ke ContextHost.
// FilePane (milik integrator/UI) cukup memanggil `registerPaneContext(...)` dengan
// `buildPaneProbe(...)` pada onMount; modul ini tidak mengubah store atau komponen.
import type { ContextEntry, ContextHost, PaneName, PaneProbe, PaneViewKind } from './context';

/** Bentuk struktural FileEntry/Row yang dibutuhkan (tanpa import store Svelte). */
export interface RowLike {
  readonly entry: {
    readonly name: string;
    readonly path: string;
    readonly isDir: boolean;
    readonly isSymlink?: boolean;
    readonly hidden: boolean;
    readonly size?: number;
  };
}

export interface PaneProbeInput {
  paneId: PaneName;
  /** Objek tab aktif (mis. `store.tab`): sumber ID tab stabil. */
  tab: object | undefined;
  /** Array rows hasil listing (mis. `store.rows`): berubah identitasnya saat re-list. */
  rows: readonly RowLike[];
  /** Baris yang tampil setelah sort/filter/hidden — SELURUH daftar, bukan viewport. */
  visible: () => readonly RowLike[];
  selected: () => Iterable<string>;
  path: string;
  branchActive: boolean;
  searchActive?: boolean;
  inArchive: boolean;
  filterMode: string;
  filterPattern: string;
  filterQuery: string;
  showHidden: boolean;
  sort: { key: string; dir: 1 | -1 };
}

/** Aturan sama dengan PaneEntryInfo: symlink tidak dihitung direktori. */
export function toContextEntry(r: RowLike): ContextEntry {
  const e = r.entry;
  return {
    name: e.name,
    path: e.path,
    isDir: e.isDir && !e.isSymlink,
    isSymlink: !!e.isSymlink,
    hidden: e.hidden,
    size: e.size,
  };
}

function viewKindOf(i: PaneProbeInput): PaneViewKind {
  if (i.searchActive) return 'search';
  if (i.branchActive) return 'branch';
  if (i.inArchive) return 'archive';
  return 'directory';
}

/** Bangun probe dari state panel; getter dievaluasi saat dipanggil (data terkini). */
export function buildPaneProbe(input: () => PaneProbeInput): () => PaneProbe | undefined {
  return () => {
    const i = input();
    if (!i.tab) return undefined; // PaneStore belum punya tab (sebelum bootTabs)
    return {
      paneId: i.paneId,
      tab: i.tab,
      listing: i.rows,
      viewKind: viewKindOf(i),
      path: i.path,
      filter: { mode: i.filterMode, pattern: i.filterPattern, query: i.filterQuery },
      showHidden: i.showHidden,
      sort: i.sort,
      selectedPaths: () => [...i.selected()],
      visibleEntries: () => i.visible().map(toContextEntry),
      allEntries: () => i.rows.map(toContextEntry),
    };
  };
}

const registry = new Map<PaneName, () => PaneProbe | undefined>();

/** Daftarkan penyedia probe untuk satu panel; mengembalikan fungsi unregister. */
export function registerPaneContext(
  pane: PaneName,
  provider: () => PaneProbe | undefined,
): () => void {
  registry.set(pane, provider);
  return () => {
    if (registry.get(pane) === provider) registry.delete(pane);
  };
}

/** Host berbasis registry; `activePane` disuplai pemanggil (mis. dari ui.activePane). */
export function registryHost(activePane: () => PaneName): ContextHost {
  return {
    activePane,
    probe: (pane) => registry.get(pane)?.(),
  };
}
