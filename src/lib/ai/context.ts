// AI-03 / FER-183: stable pane/tab IDs and immutable two-pane snapshots.
// Modul ini hanya MEMBACA panel lewat `ContextHost`/`PaneProbe`; tidak mengubah store.
// Snapshot kontrak (`PaneSnapshot`) tidak memuat daftar path; selection dibekukan
// secara lokal di balik `selectionHandle` dan tidak pernah dikirim ke model.
import {
  ContractError,
  parsePaneSnapshot,
  type PaneSnapshot,
  type PaneStateSnapshot,
} from './contracts';

export type PaneName = 'left' | 'right';
export type PaneViewKind = 'directory' | 'branch' | 'search' | 'archive';

/** Entri listing sebagai data polos (tanpa komponen UI). */
export interface ContextEntry {
  readonly name: string;
  readonly path: string;
  /** Direktori nyata; symlink melaporkan false. */
  readonly isDir: boolean;
  readonly isSymlink?: boolean;
  readonly hidden: boolean;
  readonly size?: number;
}

/** Pembacaan read-only satu panel pada satu saat. */
export interface PaneProbe {
  readonly paneId: PaneName;
  /** Identitas objek tab aktif (mis. TabState); dipakai untuk ID tab stabil. */
  readonly tab: object;
  /** Identitas listing yang termuat (mis. array rows); berubah saat re-list. */
  readonly listing: object;
  readonly viewKind: PaneViewKind;
  readonly path: string;
  readonly filter: { readonly mode: string; readonly pattern: string; readonly query: string };
  readonly showHidden: boolean;
  readonly sort: { readonly key: string; readonly dir: 1 | -1 };
  selectedPaths(): readonly string[];
  /** Baris yang lolos filter/hidden, SELURUH daftar (bukan viewport), urutan tampilan. */
  visibleEntries(): readonly ContextEntry[];
  /** Seluruh listing direktori tanpa filter/hidden. */
  allEntries(): readonly ContextEntry[];
}

export interface ContextHost {
  activePane(): PaneName;
  probe(pane: PaneName): PaneProbe | undefined;
}

export type ContextErrorCode = 'pane_unavailable' | 'unknown_snapshot';
export class ContextError extends Error {
  constructor(
    public readonly code: ContextErrorCode,
    message: string,
  ) {
    super(`${code}: ${message}`);
    this.name = 'ContextError';
  }
}

export type StaleReason = 'tab_gone' | 'pane_swapped' | 'context_changed' | 'selection_changed';
export type CheckResult = { ok: true } | { ok: false; reason: StaleReason; message: string };

// ── ID stabil ──────────────────────────────────────────────────────────────
let idCounter = 0;
const ids = new WeakMap<object, string>();
function idOf(prefix: string, obj: object): string {
  let id = ids.get(obj);
  if (!id) {
    id = `${prefix}_${(++idCounter).toString(36)}`;
    ids.set(obj, id);
  }
  return id;
}
/** ID tab stabil: diturunkan dari identitas objek tab, bukan indeks tab. */
export function tabIdOf(tab: object): string {
  return idOf('tab', tab);
}

const REQUEST_ID = /^[A-Za-z0-9_-]{1,128}$/;
const MAX_FILTER_BYTES = 256;

/** Digest selection tak peka urutan: jumlah + sum + xor hash FNV-1a per path. */
export function selectionDigest(paths: readonly string[]): string {
  let sum = 0;
  let xor = 0;
  for (const p of paths) {
    let h = 0x811c9dc5;
    for (let i = 0; i < p.length; i++) h = Math.imul(h ^ p.charCodeAt(i), 0x01000193);
    h >>>= 0;
    sum = (sum + h) >>> 0;
    xor = (xor ^ h) >>> 0;
  }
  return `${paths.length}:${sum}:${xor}`;
}

function filterText(f: PaneProbe['filter']): string {
  let s = `${f.mode}|${f.pattern}|${f.query}`;
  const enc = new TextEncoder();
  while (enc.encode(s).length > MAX_FILTER_BYTES) s = s.slice(0, Math.floor(s.length * 0.9));
  return s;
}

function contractSortKey(key: string): 'name' | 'size' | 'modified' {
  return key === 'size' || key === 'modified' ? key : 'name';
}

function contextKey(p: PaneProbe, tabId: string): string {
  const base = [
    tabId,
    idOf('lst', p.listing),
    p.viewKind,
    p.path,
    p.filter.mode,
    p.filter.pattern,
    p.filter.query,
    p.showHidden ? 'h1' : 'h0',
  ];
  // Preset "Only selected" membuat selection ikut menentukan scope.
  if (p.filter.mode === 'selected') base.push(selectionDigest(p.selectedPaths()));
  return JSON.stringify(base);
}

interface PaneRecord {
  tab: object;
  tabId: string;
  key: string;
  selectionDigest: string;
  selection: readonly string[];
  listingCount: number;
  filterActive: boolean;
}
interface SnapshotRecord {
  panes: Record<PaneName, PaneRecord>;
  showHidden: boolean;
}

interface Counter {
  key: string;
  rev: number;
  digest: string;
  selRev: number;
}

export class ContextService {
  private counters: Record<PaneName, Counter | undefined> = { left: undefined, right: undefined };
  private records = new WeakMap<PaneSnapshot, SnapshotRecord>();
  private handleSeq = 0;

  constructor(readonly host: ContextHost) {}

  private requireProbe(pane: PaneName): PaneProbe {
    const p = this.host.probe(pane);
    if (!p) throw new ContextError('pane_unavailable', `Panel ${pane} tidak tersedia`);
    return p;
  }

  /** Bekukan keadaan kedua panel saat pesan dikirim. */
  capture(requestId: string): PaneSnapshot {
    if (!REQUEST_ID.test(requestId)) {
      throw new ContractError('invalid_reference', 'Invalid request ID');
    }
    const names: PaneName[] = ['left', 'right'];
    const states: PaneStateSnapshot[] = [];
    const panes = {} as Record<PaneName, PaneRecord>;
    for (const name of names) {
      const p = this.requireProbe(name);
      const tabId = tabIdOf(p.tab);
      const key = contextKey(p, tabId);
      const selection = Object.freeze([...p.selectedPaths()]);
      const digest = selectionDigest(selection);
      const c = this.counters[name];
      const counter: Counter = !c
        ? { key, rev: 1, digest, selRev: 1 }
        : {
            key,
            rev: c.key === key ? c.rev : c.rev + 1,
            digest,
            selRev: c.digest === digest ? c.selRev : c.selRev + 1,
          };
      this.counters[name] = counter;
      states.push(
        Object.freeze({
          paneId: name,
          tabId,
          location: Object.freeze({
            kind: p.viewKind === 'archive' ? 'archive' : 'disk',
            path: p.path,
          }),
          contextRevision: counter.rev,
          selectionRevision: counter.selRev,
          selectionHandle: `sel_${(++this.handleSeq).toString(36)}`,
          filter: filterText(p.filter),
          showHidden: p.showHidden,
          sort: Object.freeze({ key: contractSortKey(p.sort.key), dir: p.sort.dir }),
        } as PaneStateSnapshot),
      );
      panes[name] = {
        tab: p.tab,
        tabId,
        key,
        selectionDigest: digest,
        selection,
        listingCount: p.allEntries().length,
        filterActive: p.filter.mode !== 'all' || p.filter.query.trim() !== '',
      };
    }
    const snapshot = Object.freeze({
      schemaVersion: 1 as const,
      requestId,
      activePaneAtSubmit: this.host.activePane(),
      panes: Object.freeze(states) as unknown as [PaneStateSnapshot, PaneStateSnapshot],
    });
    parsePaneSnapshot(JSON.parse(JSON.stringify(snapshot)));
    this.records.set(snapshot, {
      panes,
      showHidden: this.requireProbe(snapshot.activePaneAtSubmit).showHidden,
    });
    return snapshot;
  }

  private record(snapshot: PaneSnapshot): SnapshotRecord {
    const r = this.records.get(snapshot);
    if (!r) throw new ContextError('unknown_snapshot', 'Snapshot tidak dibuat oleh service ini');
    return r;
  }

  /** Path selection yang dibekukan saat capture (lokal; tidak untuk model). */
  selectionPaths(snapshot: PaneSnapshot, pane: PaneName): readonly string[] {
    return this.record(snapshot).panes[pane].selection;
  }

  /**
   * Apakah panel `pane` masih sama dengan snapshot? Tidak pernah memindahkan target:
   * swap/navigasi/tutup tab dilaporkan sebagai stale untuk re-plan.
   */
  check(snapshot: PaneSnapshot, pane: PaneName, opts: { selection?: boolean } = {}): CheckResult {
    const rec = this.record(snapshot).panes[pane];
    const probe = this.host.probe(pane);
    if (!probe || probe.tab !== rec.tab) {
      const other = this.host.probe(pane === 'left' ? 'right' : 'left');
      if (other && other.tab === rec.tab) {
        return {
          ok: false,
          reason: 'pane_swapped',
          message: `Panel ${pane} ditukar sejak pesan dikirim.`,
        };
      }
      return {
        ok: false,
        reason: 'tab_gone',
        message: `Tab panel ${pane} sudah ditutup atau tidak aktif lagi.`,
      };
    }
    if (contextKey(probe, rec.tabId) !== rec.key) {
      return {
        ok: false,
        reason: 'context_changed',
        message: `Lokasi atau filter panel ${pane} berubah sejak pesan dikirim.`,
      };
    }
    if (opts.selection && selectionDigest(probe.selectedPaths()) !== rec.selectionDigest) {
      return {
        ok: false,
        reason: 'selection_changed',
        message: `Pilihan item di panel ${pane} berubah sejak pesan dikirim.`,
      };
    }
    return { ok: true };
  }

  /** Konteks outbound minimal untuk provider: hanya hitungan dan flag. */
  modelContext(snapshot: PaneSnapshot): {
    activePane: PaneName;
    leftLocation: 'disk' | 'archive';
    rightLocation: 'disk' | 'archive';
    leftCount: number;
    rightCount: number;
    showHidden: boolean;
    filterActive: boolean;
  } {
    const r = this.record(snapshot);
    return {
      activePane: snapshot.activePaneAtSubmit,
      leftLocation: snapshot.panes[0].location.kind,
      rightLocation: snapshot.panes[1].location.kind,
      leftCount: r.panes.left.listingCount,
      rightCount: r.panes.right.listingCount,
      showHidden: r.showHidden,
      filterActive: r.panes.left.filterActive || r.panes.right.filterActive,
    };
  }
}
