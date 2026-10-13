// FER-466: the read-only `diskUsage.query` answer path.
//
// Rules kept here:
//   - The model only CHOOSES a strictly typed question (`parseDiskUsageQueryKind`). The app runs it
//     locally through `DiskUsageQueryPort` against the snapshot of the Disk Usage workspace and
//     renders the answer itself.
//   - Nothing from the scan (path, name, size, count) is ever sent to the provider: no second turn,
//     nothing in `OutboundContext`, and the offer text only describes the capability.
//   - Read-only. "Reveal" / "Copy path" on the card are the existing pane navigation and clipboard;
//     deleting or moving something still goes through the normal prepared plan + confirm card.
//   - Honest limits: a Top-N is labelled "N dari M", a partial / stale / cancelled scan says so, and
//     "no scan on screen" is an answer of its own (never a guess).
import type { DiskUsageQueryKind, DiskUsageScope } from './contracts';
import { MAX_DISK_USAGE_LIMIT } from './contracts';
import { formatSize } from '../format';
import type {
  DiskUsageChildItem,
  DiskUsageChildrenPage,
  DiskUsageCoverage,
  DiskUsageFileCategory,
  DiskUsageFileItem,
  DiskUsageFilter,
  DiskUsageScanEngine,
  DiskUsageSizeMetric,
  DiskUsageStats,
  DiskUsageTopFiles,
} from '../ipc/types';

export const DEFAULT_DISK_USAGE_LIMIT = 10;
/** Children fetched to find the largest FOLDERS (the page mixes files and folders). */
const FOLDER_PAGE = 200;
const MAX_PATH_CHARS = 4096;
const CONTROL = /\p{Cc}/u;

/**
 * Display-only form of untrusted text: format characters (bidi overrides such as U+202E, zero-width
 * characters) are replaced by a visible marker so a name cannot reorder or hide itself. The data
 * (and Reveal / Copy path) keep the original string.
 */
export function visibleText(text: string): string {
  return text.replace(
    /\p{Cf}/gu,
    (c) => `⟨U+${(c.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0')}⟩`,
  );
}

/** Phases of the workspace store that matter here (subset of `DiskUsagePhase`). */
export type DuPhase =
  'idle' | 'scanning' | 'complete' | 'partial' | 'cancelled' | 'stale' | 'empty' | 'error';

/** The scan the workspace holds right now, as far as a question needs it. */
export interface DiskUsageActiveScan {
  scanId: string;
  /** Scanned root (absolute path). */
  root: string;
  /** Node ids (absolute paths) for each scope. */
  rootId: string;
  viewId: string;
  viewLabel: string;
  metric: DiskUsageSizeMetric;
  phase: DuPhase;
  engine: DiskUsageScanEngine;
  scannedAtMs: number;
  stats: DiskUsageStats | null;
}

/** Why there is nothing to answer from. */
export type NoScanReason = 'none' | 'scanning' | 'unusable';

/**
 * Port to the Disk Usage workspace. `available()` gates the OFFER (workspace open or still holding
 * results); `current()` is the live check right before answering.
 */
export interface DiskUsageQueryPort {
  available(): boolean;
  current(): DiskUsageActiveScan | { none: NoScanReason };
  topFiles(
    scanId: string,
    nodeId: string,
    limit: number,
    filter: DiskUsageFilter,
  ): Promise<DiskUsageTopFiles>;
  children(
    scanId: string,
    nodeId: string,
    limit: number,
    filter: DiskUsageFilter,
  ): Promise<DiskUsageChildrenPage>;
}

/** One listed entry. `path` is local-only (Reveal / Copy path); `rel` is what the card shows. */
export interface DiskUsageAnswerItem {
  path: string;
  name: string;
  /** Path relative to the scan root, `\`-separated; the name alone when it cannot be derived. */
  rel: string;
  kind: 'file' | 'folder';
  /** Bytes under the metric of the card; null = unknown (allocated not supported). Never guessed. */
  bytes: number | null;
  /** Share of the scan root, 0..100; null = unknown. */
  percent: number | null;
  category?: DiskUsageFileCategory;
}

export interface DiskUsageSummary {
  files: number;
  folders: number;
  symlinks: number;
  logicalBytes: number;
  allocatedBytes: number | null;
  errors: number;
  durationMs: number;
}

export interface DiskUsageListAnswer {
  status: 'files' | 'folders';
  items: DiskUsageAnswerItem[];
  metric: DiskUsageSizeMetric;
  scanRoot: string;
  scopeLabel: string;
  scope: DiskUsageScope;
  /** Matches before the cap, when the backend reports it (files). */
  totalMatching: number | null;
  truncated: boolean;
  category?: DiskUsageFileCategory;
  notes: string[];
}

export type DiskUsageAnswer =
  | DiskUsageListAnswer
  | {
      status: 'summary';
      summary: DiskUsageSummary | null;
      metric: DiskUsageSizeMetric;
      scanRoot: string;
      notes: string[];
    }
  | { status: 'noScan'; reason: NoScanReason }
  /** The query failed or the snapshot was replaced / evicted while it ran. */
  | { status: 'unavailable'; reason: 'evicted' | 'failed' };

// ───────────────────────── helpers ─────────────────────────

function plainPath(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= MAX_PATH_CHARS &&
    !CONTROL.test(value)
  );
}

const norm = (p: string) => p.replace(/\//g, '\\').replace(/\\+$/, '');

/** `path` relative to `root` (case-insensitive, Windows); falls back to the last segment. */
export function relativeToRoot(path: string, root: string): string {
  const p = norm(path);
  const r = norm(root);
  if (r && p.toLowerCase() === r.toLowerCase()) return '.';
  if (r && p.toLowerCase().startsWith(`${r.toLowerCase()}\\`)) return p.slice(r.length + 1);
  const i = p.lastIndexOf('\\');
  return i >= 0 ? p.slice(i + 1) : p;
}

function bytesOf(
  logical: number,
  allocated: number | null,
  metric: DiskUsageSizeMetric,
): number | null {
  if (metric === 'allocated') return allocated;
  return Number.isFinite(logical) && logical >= 0 ? logical : null;
}

function pct(value: number | null): number | null {
  return value !== null && Number.isFinite(value) && value >= 0 ? Math.min(100, value) : null;
}

function fileItem(f: DiskUsageFileItem, scan: DiskUsageActiveScan): DiskUsageAnswerItem | null {
  if (!plainPath(f.id) || typeof f.name !== 'string' || CONTROL.test(f.name)) return null;
  return {
    path: f.id,
    name: f.name,
    rel: relativeToRoot(f.id, scan.root),
    kind: 'file',
    bytes: bytesOf(f.logicalBytes, f.allocatedBytes, scan.metric),
    percent: pct(f.percentOfScanRoot),
    category: f.category,
  };
}

function folderItem(c: DiskUsageChildItem, scan: DiskUsageActiveScan): DiskUsageAnswerItem | null {
  if (!plainPath(c.id) || typeof c.name !== 'string' || CONTROL.test(c.name)) return null;
  return {
    path: c.id,
    name: c.name,
    rel: relativeToRoot(c.id, scan.root),
    kind: 'folder',
    bytes: bytesOf(c.logicalBytes, c.allocatedBytes, scan.metric),
    percent: pct(c.percentOfScanRoot),
  };
}

/** Honest caveats about the scan itself (app text, never from the model or the backend). */
export function scanNotes(
  scan: Pick<DiskUsageActiveScan, 'phase'>,
  coverage?: DiskUsageCoverage,
): string[] {
  const notes: string[] = [];
  if (scan.phase === 'stale' || coverage === 'stale') {
    notes.push(
      'Hasil scan ini sudah usang; isi folder mungkin sudah berubah. Scan ulang untuk data terbaru.',
    );
  }
  if (scan.phase === 'cancelled') {
    notes.push('Scan dibatalkan sebelum selesai, jadi angka ini belum lengkap.');
  } else if (scan.phase === 'partial' || coverage === 'partial') {
    notes.push(
      'Scan hanya sebagian (ada folder yang tidak terbaca atau dibatasi), jadi angka ini belum lengkap.',
    );
  }
  return notes;
}

export const CATEGORY_LABEL: Record<DiskUsageFileCategory, string> = {
  video: 'video',
  image: 'gambar',
  audio: 'audio',
  document: 'dokumen',
  archive: 'arsip',
  other: 'lainnya',
};

function scanLabel(root: string): string {
  const n = norm(root);
  const i = n.lastIndexOf('\\');
  return i >= 0 && i < n.length - 1 ? n.slice(i + 1) : n || root;
}

// ───────────────────────── query runner ─────────────────────────

/**
 * Run one validated question against the workspace. Never throws: every failure is an answer. The
 * result stays in the renderer (card); it is never handed to a provider.
 */
export async function runDiskUsageQuery(
  port: DiskUsageQueryPort,
  query: DiskUsageQueryKind,
): Promise<DiskUsageAnswer> {
  let current: ReturnType<DiskUsageQueryPort['current']>;
  try {
    current = port.current();
  } catch {
    return { status: 'noScan', reason: 'unusable' };
  }
  if ('none' in current) return { status: 'noScan', reason: current.none };
  const scan = current;

  try {
    if (query.kind === 'summary') {
      const s = scan.stats;
      return {
        status: 'summary',
        scanRoot: scan.root,
        metric: scan.metric,
        summary: s
          ? {
              files: s.scannedFiles,
              folders: s.scannedDirs,
              symlinks: s.scannedSymlinks,
              logicalBytes: s.logicalBytes,
              allocatedBytes: s.knownAllocatedBytes,
              errors: s.totalErrors,
              durationMs: s.durationMs,
            }
          : null,
        notes: scanNotes(scan, s?.coverage),
      };
    }

    const scope: DiskUsageScope = query.scope ?? 'view';
    const nodeId = scope === 'root' ? scan.rootId : scan.viewId;
    const scopeLabel = scope === 'root' ? scanLabel(scan.root) : scan.viewLabel;
    const limit = Math.min(query.limit ?? DEFAULT_DISK_USAGE_LIMIT, MAX_DISK_USAGE_LIMIT);

    if (query.kind === 'largestFiles') {
      const result = await port.topFiles(scan.scanId, nodeId, limit, {
        metric: scan.metric,
        categories: query.category ? [query.category] : null,
      });
      if (result.scanId !== scan.scanId) return { status: 'unavailable', reason: 'evicted' };
      const items = result.items
        .map((f) => fileItem(f, scan))
        .filter((i): i is DiskUsageAnswerItem => i !== null)
        .slice(0, limit);
      return {
        status: 'files',
        items,
        metric: scan.metric,
        scanRoot: scan.root,
        scopeLabel,
        scope,
        totalMatching: Number.isFinite(result.totalMatching) ? result.totalMatching : null,
        truncated: result.truncated === true || result.totalMatching > items.length,
        ...(query.category ? { category: query.category } : {}),
        notes: scanNotes(scan, result.coverage),
      };
    }

    // largestFolders: direct child folders of the scope, largest first.
    const page = await port.children(scan.scanId, nodeId, FOLDER_PAGE, { metric: scan.metric });
    if (page.scanId !== scan.scanId) return { status: 'unavailable', reason: 'evicted' };
    const all = page.items
      .filter((c) => c.kind === 'dir')
      .map((c) => folderItem(c, scan))
      .filter((i): i is DiskUsageAnswerItem => i !== null);
    const notes = scanNotes(scan, page.coverage);
    notes.push(
      'Hanya folder tepat di bawah folder yang dipilih, bukan semua subfolder di dalamnya.',
    );
    return {
      status: 'folders',
      items: all.slice(0, limit),
      metric: scan.metric,
      scanRoot: scan.root,
      scopeLabel,
      scope,
      totalMatching: null,
      truncated: all.length > limit || page.total > page.items.length,
      notes,
    };
  } catch (error) {
    const code = (error as { code?: unknown } | null)?.code;
    return { status: 'unavailable', reason: code === 'notFound' ? 'evicted' : 'failed' };
  }
}

// ───────────────────────── user-facing copy (Indonesian, app text) ─────────────────────────

/** One-line summary of the local question (from the parsed intent, never model prose). */
export function describeDiskUsageQuery(query: DiskUsageQueryKind): string {
  const scope = (s: DiskUsageScope | undefined) =>
    s === 'root' ? 'seluruh scan' : 'tampilan sekarang';
  switch (query.kind) {
    case 'largestFiles': {
      const cat = query.category ? ` (${CATEGORY_LABEL[query.category]})` : '';
      return `file terbesar${cat}, ${scope(query.scope)}`;
    }
    case 'largestFolders':
      return `folder terbesar, ${scope(query.scope)}`;
    case 'summary':
      return 'ringkasan hasil scan';
  }
}

export function describeDiskUsageAnswer(answer: DiskUsageAnswer): {
  text: string;
  tone: 'good' | 'warn' | 'bad' | 'info';
} {
  switch (answer.status) {
    case 'noScan':
      return {
        text:
          answer.reason === 'scanning'
            ? 'Scan Disk Usage masih berjalan. Tunggu sampai selesai, lalu tanyakan lagi.'
            : answer.reason === 'unusable'
              ? 'Hasil Disk Usage tidak bisa dibaca (scan gagal atau kosong). Jalankan scan ulang.'
              : 'Belum ada hasil Disk Usage. Jalankan scan dulu.',
        tone: 'info',
      };
    case 'unavailable':
      return {
        text:
          answer.reason === 'evicted'
            ? 'Hasil scan sudah diganti atau dibuang dari memori. Jalankan scan ulang, lalu tanyakan lagi.'
            : 'Hasil Disk Usage tidak dapat dibaca saat ini; tidak ada yang diubah.',
        tone: 'warn',
      };
    case 'summary':
      return answer.summary
        ? { text: 'Ringkasan hasil scan Disk Usage yang sedang tampil.', tone: 'good' }
        : { text: 'Ringkasan belum tersedia untuk scan ini.', tone: 'info' };
    case 'files':
    case 'folders': {
      const unit = answer.status === 'files' ? 'file' : 'folder';
      if (!answer.items.length) {
        return { text: `Tidak ada ${unit} yang cocok di hasil scan ini.`, tone: 'info' };
      }
      const total =
        answer.totalMatching !== null && answer.totalMatching > answer.items.length
          ? `${answer.items.length} ${unit} terbesar dari ${answer.totalMatching.toLocaleString('en-US')}`
          : `${answer.items.length} ${unit} terbesar`;
      return {
        text: answer.truncated ? `${total}. Ini daftar teratas, bukan semua ${unit}.` : `${total}.`,
        tone: answer.notes.some((n) => /usang|sebagian|dibatalkan|belum lengkap/.test(n))
          ? 'warn'
          : 'good',
      };
    }
  }
}

export function formatAnswerBytes(bytes: number | null): string {
  return bytes === null ? 'tidak tersedia' : formatSize(bytes);
}

// ───────────────────────── workspace port ─────────────────────────

/** The part of `DiskUsageStore` the port reads (structural, so tests need no Tauri). */
export interface DuStoreLike {
  phase: DuPhase;
  hasData: boolean;
  scanId: string | null;
  root: string;
  rootId: string;
  metric: DiskUsageSizeMetric;
  engine: DiskUsageScanEngine;
  scannedAtMs: number;
  stats: DiskUsageStats | null;
  /** Zoom node (the folder the treemap shows). */
  zoomNodeId: string;
  crumbs(): { id: string; label: string }[];
}

export interface DuWorkspaceLike {
  open: boolean;
  store: DuStoreLike;
}

export interface DuIo {
  topFiles: DiskUsageQueryPort['topFiles'];
  children: DiskUsageQueryPort['children'];
}

/**
 * Port over the app-level Disk Usage workspace. Offered while the workspace is open (it shows a
 * result or the empty state) or still holds a result; a running scan has no queryable snapshot yet
 * and an error/idle store has none at all, so those answer `noScan` instead of guessing.
 */
export function diskUsageWorkspacePort(workspace: DuWorkspaceLike, io: DuIo): DiskUsageQueryPort {
  return {
    available: () => workspace.open === true || workspace.store.hasData === true,
    current() {
      const s = workspace.store;
      if (s.phase === 'scanning') return { none: 'scanning' };
      if (!s.hasData || !s.scanId || s.phase === 'empty') {
        return { none: s.phase === 'idle' ? 'none' : 'unusable' };
      }
      const crumbs = s.crumbs();
      const view = s.zoomNodeId || s.rootId || s.root;
      return {
        scanId: s.scanId,
        root: s.root,
        rootId: s.rootId || s.root,
        viewId: view,
        viewLabel: crumbs[crumbs.length - 1]?.label ?? scanLabel(s.root),
        metric: s.metric,
        phase: s.phase,
        engine: s.engine,
        scannedAtMs: s.scannedAtMs,
        stats: s.stats,
      };
    },
    topFiles: io.topFiles,
    children: io.children,
  };
}
