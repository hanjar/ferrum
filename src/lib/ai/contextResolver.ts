// AI-03 / FER-184: resolver selector -> manifest lokal yang dibekukan.
// Sumber: listing LENGKAP panel lewat PaneProbe (bukan row virtual yang ter-render).
// Hasil publik hanya hitungan + teks scope + handle opak; daftar path tidak keluar
// dari modul ini kecuali lewat `entries(handle)` untuk executor lokal.
import type { ArchiveSelector, PaneRef, PaneSnapshot, Selector } from './contracts';
import type { ActivityLocation } from './activityQuery';
import { entityBaseName } from './entitySelector';
import type { CheckResult, ContextEntry, ContextService, PaneName, PaneProbe } from './context';

export type ScopeErrorCode =
  'empty_selection' | 'empty_scope' | 'unsupported_view' | 'stale_context' | 'pane_unavailable';

export interface ScopeError {
  code: ScopeErrorCode;
  message: string;
}

export interface ManifestEntry {
  readonly name: string;
  readonly path: string;
  readonly isDir: boolean;
}

export interface ScopeOk {
  ok: true;
  /** Handle opak ke daftar item beku; model/chat hanya melihat ini. */
  handle: string;
  pane: PaneName;
  requestId: string;
  tabId: string;
  contextRevision: number;
  selectionRevision: number;
  sourceDirectory: string;
  selector: ArchiveSelector;
  counts: { files: number; folders: number; total: number };
  excluded: { hidden: number; filtered: number; nonFiles: number; extension: number };
  filterActive: boolean;
  showHidden: boolean;
  /** Kalimat scope untuk preview (Bahasa Indonesia). */
  scopeText: string;
}
export type ScopeResult = ScopeOk | { ok: false; error: ScopeError };

const VIEW_LABEL: Record<string, string> = {
  branch: 'Branch View',
  search: 'hasil pencarian',
  archive: 'arsip',
};

const isRegularFile = (e: ContextEntry): boolean => !e.isDir && !e.isSymlink;
const label = (pane: PaneName): string => (pane === 'left' ? 'kiri' : 'kanan');

/** Ekstensi terakhir nama (huruf kecil, tanpa titik); '' bila tidak ada (".pdf", "a." tidak punya ekstensi). */
export function extensionOf(name: string): string {
  const i = name.lastIndexOf('.');
  return i > 0 && i < name.length - 1 ? name.slice(i + 1).toLowerCase() : '';
}
// Pertahanan berlapis: parseIntent sudah menolak nilai tak aman; nilai yang lolos ke sini tetap disaring.
const SAFE_EXT = /^[a-z0-9]{1,16}$/;

function plural(files: number, folders: number): string {
  return `${files} file dan ${folders} folder`;
}

export class ScopeResolver {
  private manifests = new Map<string, readonly ManifestEntry[]>();
  private seq = 0;

  constructor(private readonly ctx: ContextService) {}

  private fail(code: ScopeErrorCode, message: string): ScopeResult {
    return { ok: false, error: { code, message } };
  }

  resolve(snapshot: PaneSnapshot, ref: PaneRef, selector: Selector): ScopeResult {
    const pane: PaneName = ref === 'activeAtSubmit' ? snapshot.activePaneAtSubmit : ref;
    const state = snapshot.panes[pane === 'left' ? 0 : 1];
    const stale: CheckResult = this.ctx.check(snapshot, pane);
    if (!stale.ok)
      return this.fail(
        'stale_context',
        `${stale.message} Susun ulang rencana dengan konteks terbaru.`,
      );
    const probe: PaneProbe | undefined = this.ctx.host.probe(pane);
    if (!probe) return this.fail('pane_unavailable', `Panel ${label(pane)} tidak tersedia.`);
    if (probe.viewKind !== 'directory') {
      return this.fail(
        'unsupported_view',
        `Panel ${label(pane)} sedang menampilkan ${VIEW_LABEL[probe.viewKind] ?? probe.viewKind}; ` +
          'perintah ini belum didukung di tampilan tersebut. Buka folder biasa lalu ulangi.',
      );
    }

    const all = probe.allEntries();
    let items: ContextEntry[];
    const excluded = { hidden: 0, filtered: 0, nonFiles: 0, extension: 0 };
    let text: string;
    const where = `panel ${label(pane)} (${probe.path})`;

    if (selector.kind === 'selected') {
      const paths = this.ctx.selectionPaths(snapshot, pane);
      if (paths.length === 0) {
        return this.fail(
          'empty_selection',
          `Belum ada item yang dipilih di panel ${label(pane)}. Pilih item dulu, atau minta "semua file di panel ${label(pane)}".`,
        );
      }
      const byPath = new Map<string, ContextEntry>();
      for (const e of all) byPath.set(e.path, e);
      items = [];
      for (const p of paths) {
        const e = byPath.get(p);
        if (!e) {
          return this.fail(
            'stale_context',
            `Item yang dipilih di panel ${label(pane)} sudah tidak ada di listing. Pilih ulang.`,
          );
        }
        items.push(e);
      }
    } else if (selector.kind === 'allVisible') {
      const visible = probe.visibleEntries();
      const seen = new Set<string>();
      for (const e of visible) seen.add(e.path);
      for (const e of all) if (e.hidden && !seen.has(e.path)) excluded.hidden++;
      excluded.filtered = Math.max(0, all.length - visible.length - excluded.hidden);
      items = selector.entries === 'filesOnly' ? visible.filter(isRegularFile) : [...visible];
      if (selector.entries === 'filesOnly') excluded.nonFiles = visible.length - items.length;
    } else {
      const base = probe.allEntries();
      const kept = selector.includeHidden ? base : base.filter((e) => !e.hidden);
      excluded.hidden = base.length - kept.length;
      items = selector.entries === 'filesOnly' ? kept.filter(isRegularFile) : [...kept];
      if (selector.entries === 'filesOnly') excluded.nonFiles = kept.length - items.length;
    }

    // AI-09: filter ekstensi dijalankan pada listing PENUH (bukan baris yang ter-render); hanya file biasa.
    const exts = (selector.extensions ?? []).filter((x) => SAFE_EXT.test(x));
    const extLabel = exts.map((x) => `.${x}`).join(', ');
    if (selector.extensions !== undefined) {
      const wanted = new Set(exts);
      const before = items.length;
      items = items.filter((e) => isRegularFile(e) && wanted.has(extensionOf(e.name)));
      excluded.extension = before - items.length;
    }

    if (items.length === 0) {
      return this.fail(
        'empty_scope',
        selector.extensions !== undefined
          ? `Tidak ada file ${extLabel || 'dengan ekstensi itu'} di ${where}. Tidak ada yang dijalankan.`
          : `Tidak ada item yang cocok di ${where}. Tidak ada yang dijalankan.`,
      );
    }

    let folders = 0;
    for (const e of items) if (e.isDir) folders++;
    const files = items.length - folders;
    const filterActive = probe.filter.mode !== 'all' || probe.filter.query.trim() !== '';
    const filterLabel = [
      probe.filter.mode !== 'all'
        ? `${probe.filter.mode}${probe.filter.pattern ? ` ${probe.filter.pattern}` : ''}`
        : '',
      probe.filter.query.trim(),
    ]
      .filter(Boolean)
      .join(' + ');

    if (selector.kind === 'selected') {
      text = `Item yang dipilih di ${where}: ${plural(files, folders)}.`;
    } else if (selector.kind === 'allVisible') {
      text = `Semua item yang tampil di ${where}: ${plural(files, folders)}${selector.entries === 'filesOnly' ? ' (hanya file)' : ''}.`;
      if (excluded.nonFiles) text += ` ${excluded.nonFiles} folder/tautan dilewati.`;
      if (excluded.hidden) text += ` ${excluded.hidden} item tersembunyi tidak ikut.`;
      if (filterActive)
        text += ` Filter aktif (${filterLabel}) membatasi ${excluded.filtered} item.`;
    } else {
      text = `Seluruh isi folder ${where} tanpa filter: ${plural(files, folders)}${selector.entries === 'filesOnly' ? ' (hanya file)' : ''}.`;
      text += selector.includeHidden
        ? ' Item tersembunyi ikut.'
        : ` ${excluded.hidden} item tersembunyi tidak ikut.`;
      if (excluded.nonFiles) text += ` ${excluded.nonFiles} folder/tautan dilewati.`;
    }

    if (selector.extensions !== undefined) {
      text += ` Hanya file berekstensi ${extLabel}; ${excluded.extension} item lain dilewati.`;
    }

    const manifest: ManifestEntry[] = items.map((e) => ({
      name: e.name,
      path: e.path,
      isDir: e.isDir,
    }));
    const handle = `scope_${(++this.seq).toString(36)}`;
    this.manifests.set(handle, Object.freeze(manifest));
    return {
      ok: true,
      handle,
      pane,
      requestId: snapshot.requestId,
      tabId: state.tabId,
      contextRevision: state.contextRevision,
      selectionRevision: state.selectionRevision,
      sourceDirectory: probe.path,
      selector,
      counts: { files, folders, total: items.length },
      excluded,
      filterActive,
      showHidden: probe.showHidden,
      scopeText: text,
    };
  }

  /**
   * FER-433 (ACT-08 R7 v2): selector `entity` -> manifest satu item dari lokasi lokal yang sudah dibaca
   * (`activity_chat_locate`). Panel tidak menjadi sumber: panel hanya dipakai sebagai TUJUAN ZIP, jadi
   * konteksnya divalidasi seperti selector lain. `sourceDirectory` = folder induk item.
   */
  resolveEntity(
    snapshot: PaneSnapshot,
    ref: PaneRef,
    entityId: string,
    location: ActivityLocation,
  ): ScopeResult {
    const pane: PaneName = ref === 'activeAtSubmit' ? snapshot.activePaneAtSubmit : ref;
    const state = snapshot.panes[pane === 'left' ? 0 : 1];
    const stale: CheckResult = this.ctx.check(snapshot, pane);
    if (!stale.ok)
      return this.fail(
        'stale_context',
        `${stale.message} Susun ulang rencana dengan konteks terbaru.`,
      );
    const probe: PaneProbe | undefined = this.ctx.host.probe(pane);
    if (!probe) return this.fail('pane_unavailable', `Panel ${label(pane)} tidak tersedia.`);
    if (probe.viewKind !== 'directory') {
      return this.fail(
        'unsupported_view',
        `Panel ${label(pane)} sedang menampilkan ${VIEW_LABEL[probe.viewKind] ?? probe.viewKind}; ` +
          'ZIP dari riwayat disimpan di folder panel aktif. Buka folder biasa lalu ulangi.',
      );
    }
    const name = entityBaseName(location);
    if (!name)
      return this.fail(
        'empty_scope',
        'Lokasi item riwayat tidak valid. Tidak ada yang dijalankan.',
      );
    const isDir = location.kind === 'folder';
    const manifest: ManifestEntry[] = [{ name, path: location.path, isDir }];
    const handle = `scope_${(++this.seq).toString(36)}`;
    this.manifests.set(handle, Object.freeze(manifest));
    const counts = isDir ? { files: 0, folders: 1, total: 1 } : { files: 1, folders: 0, total: 1 };
    return {
      ok: true,
      handle,
      pane,
      requestId: snapshot.requestId,
      tabId: state.tabId,
      contextRevision: state.contextRevision,
      selectionRevision: state.selectionRevision,
      sourceDirectory: location.parentPath,
      selector: { kind: 'entity', entityId },
      counts,
      excluded: { hidden: 0, filtered: 0, nonFiles: 0, extension: 0 },
      filterActive: false,
      showHidden: probe.showHidden,
      scopeText: `Satu item dari riwayat aktivitas: ${isDir ? 'folder' : 'file'} ${name} di ${location.parentPath}.`,
    };
  }

  /** Item beku hasil resolusi (lokal; hanya untuk executor, bukan untuk model). */
  entries(handle: string): readonly ManifestEntry[] {
    const m = this.manifests.get(handle);
    if (!m) throw new Error(`Unknown scope handle: ${handle}`);
    return m;
  }

  release(handle: string): void {
    this.manifests.delete(handle);
  }

  /** Jumlah manifest yang masih tertahan (untuk tes kebocoran). */
  manifestCount(): number {
    return this.manifests.size;
  }

  /** Cek ulang sebelum eksekusi: panel/tab/selection masih sama dengan saat resolusi? */
  revalidate(snapshot: PaneSnapshot, scope: ScopeOk): CheckResult {
    return this.ctx.check(snapshot, scope.pane, { selection: scope.selector.kind === 'selected' });
  }
}
