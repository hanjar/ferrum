// AI-03 / FER-185: resolusi lokal target dan nama output ZIP.
// Model hanya boleh memberi `outputName` (basename) dan referensi panel; direktori
// tujuan selalu berasal dari snapshot panel, bukan dari teks model.
import type { ArchiveIntentFormat, ArchiveIntentLevel, PaneRef, PaneSnapshot } from './contracts';
import type { ContextService, PaneName } from './context';
import type { ManifestEntry, ScopeOk } from './contextResolver';

export type TargetErrorCode =
  'invalid_name' | 'stale_context' | 'target_read_only' | 'pane_unavailable' | 'unsupported_view';

export interface TargetOk {
  ok: true;
  targetPane: PaneName;
  targetDirectory: string;
  requestedName: string;
  finalName: string;
  outputPath: string;
  /** Nama diminta sudah dipakai di tujuan (tanpa memandang besar-kecil huruf). */
  collision: boolean;
  renamed: boolean;
  /** Identitas yang tidak boleh menjadi input job ini (output; staging ditambah engine). */
  excludeFromInput: string[];
  /** True bila direktori tujuan berada di dalam folder sumber yang direkursi. */
  outputInsideSource: boolean;
  excludeSubtreeOf: string[];
  warnings: string[];
  previewText: string;
}
export type TargetResult =
  TargetOk | { ok: false; error: { code: TargetErrorCode; message: string } };

const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i;
// eslint-disable-next-line no-control-regex -- karakter kontrol memang harus ditolak di nama file
const FORBIDDEN = /[<>:"/\\|?*\u0000-\u001f]/;
const MAX_NAME = 255;
const KNOWN_EXTS = ['.zip', '.7z'] as const;
const extOf = (format: ArchiveIntentFormat): string => (format === '7z' ? '.7z' : '.zip');
const labelOf = (format: ArchiveIntentFormat): string => (format === '7z' ? '7z' : 'ZIP');

export type NameResult = { ok: true; name: string } | { ok: false; message: string };

/**
 * Validasi basename dari pengguna/model; menambah ekstensi format (`.zip`/`.7z`) bila belum ada.
 * Ekstensi arsip lain yang dikenal (`.zip` untuk 7z, `.7z` untuk ZIP) diganti, bukan ditumpuk.
 */
export function sanitizeOutputName(raw: string, format: ArchiveIntentFormat = 'zip'): NameResult {
  const ext = extOf(format);
  const label = labelOf(format);
  const trimmed = raw.trim();
  if (!trimmed) return { ok: false, message: `Nama file ${label} kosong.` };
  if (FORBIDDEN.test(trimmed)) {
    return {
      ok: false,
      message: `Nama file ${label} tidak boleh memuat pemisah folder, ":" atau karakter terlarang.`,
    };
  }
  if (/[. ]$/.test(trimmed))
    return {
      ok: false,
      message: `Nama file ${label} tidak boleh berakhir dengan titik atau spasi.`,
    };
  const lower = trimmed.toLowerCase();
  const other = KNOWN_EXTS.find((e) => e !== ext && lower.endsWith(e));
  const base = other ? trimmed.slice(0, -other.length) : trimmed;
  const name = base.toLowerCase().endsWith(ext) ? base : base + ext;
  const stem = name.slice(0, -ext.length);
  if (!stem.trim() || stem === '.' || stem === '..')
    return { ok: false, message: `Nama file ${label} tidak valid.` };
  if (RESERVED.test(stem.split('.')[0].trimEnd())) {
    return { ok: false, message: 'Nama tersebut dicadangkan oleh sistem operasi.' };
  }
  if (name.length > MAX_NAME) return { ok: false, message: `Nama file ${label} terlalu panjang.` };
  return { ok: true, name };
}

function lastSegment(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  return i === -1 ? norm : norm.slice(i + 1);
}

/** Nama default: basename folder sumber + ekstensi format; root drive memakai `archive`. */
function defaultName(sourcePath: string, format: ArchiveIntentFormat): string {
  const base = lastSegment(sourcePath);
  if (!base || /^[A-Za-z]:$/.test(base)) return `archive${extOf(format)}`;
  const safe = base.replace(FORBIDDEN, '_').replace(/[. ]+$/, '');
  const res = safe ? sanitizeOutputName(safe, format) : undefined;
  return res && res.ok ? res.name : `archive${extOf(format)}`;
}

function join(dir: string, name: string): string {
  const trimmed = dir.replace(/[\\/]+$/, '');
  if (trimmed === '') return `/${name}`;
  if (/^[A-Za-z]:$/.test(trimmed)) return `${trimmed}\\${name}`;
  return `${trimmed}${dir.includes('\\') ? '\\' : '/'}${name}`;
}

const norm = (p: string): string => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();

function contains(dir: string, target: string): boolean {
  const d = norm(dir);
  const t = norm(target);
  return t === d || t.startsWith(`${d}/`);
}

function uniqueName(wanted: string, taken: ReadonlySet<string>, ext: string): string {
  if (!taken.has(wanted.toLowerCase())) return wanted;
  const stem = wanted.slice(0, -ext.length);
  for (let n = 2; ; n++) {
    const candidate = `${stem} (${n})${ext}`;
    if (!taken.has(candidate.toLowerCase())) return candidate;
  }
}

export function levelText(level: ArchiveIntentLevel): string {
  switch (level) {
    case 'store':
      return 'tanpa kompresi (simpan saja)';
    case 'fast':
      return 'cepat';
    case 'normal':
      return 'normal';
    case 'maximum':
      return 'maksimum';
    case 'ultra':
      return 'ultra (ukuran paling kecil)';
  }
}

export interface ArchiveTargetArgs {
  sourcePane: PaneRef;
  target: { kind: 'sourceDirectory' } | { kind: 'pane'; pane: PaneRef };
  outputName?: string;
  /** FER-576: format arsip (bawaan zip) dan level kompresi untuk teks pratinjau. */
  format?: ArchiveIntentFormat;
  level?: ArchiveIntentLevel;
  scope: ScopeOk;
  /** Item beku hasil ScopeResolver.entries(scope.handle). */
  manifest: readonly ManifestEntry[];
}

export function resolveArchiveTarget(
  ctx: ContextService,
  snapshot: PaneSnapshot,
  args: ArchiveTargetArgs,
): TargetResult {
  const fail = (code: TargetErrorCode, message: string): TargetResult => ({
    ok: false,
    error: { code, message },
  });
  const pick = (ref: PaneRef): PaneName =>
    ref === 'activeAtSubmit' ? snapshot.activePaneAtSubmit : ref;
  const format = args.format ?? 'zip';
  const ext = extOf(format);
  const label = labelOf(format);
  const sourcePane = args.scope.pane;
  const targetPane = args.target.kind === 'sourceDirectory' ? sourcePane : pick(args.target.pane);

  for (const pane of new Set<PaneName>([sourcePane, targetPane])) {
    const c = ctx.check(snapshot, pane);
    if (!c.ok)
      return fail('stale_context', `${c.message} Susun ulang rencana dengan konteks terbaru.`);
  }
  const probe = ctx.host.probe(targetPane);
  if (!probe) return fail('pane_unavailable', `Panel ${targetPane} tidak tersedia.`);
  if (probe.viewKind === 'archive') {
    return fail(
      'target_read_only',
      'Tujuan berada di dalam arsip (hanya baca); pilih folder biasa sebagai tujuan.',
    );
  }

  let requested: string;
  if (args.outputName !== undefined) {
    const n = sanitizeOutputName(args.outputName, format);
    if (!n.ok) return fail('invalid_name', n.message);
    requested = n.name;
  } else {
    requested = defaultName(args.scope.sourceDirectory, format);
  }

  const taken = new Set<string>();
  for (const e of probe.allEntries()) taken.add(e.name.toLowerCase());
  const finalName = uniqueName(requested, taken, ext);
  const collision = finalName !== requested;
  const targetDirectory = probe.path;
  const outputPath = join(targetDirectory, finalName);

  const excludeSubtreeOf = args.manifest
    .filter((e) => e.isDir && contains(e.path, targetDirectory))
    .map((e) => e.path);
  const outputInsideSource = excludeSubtreeOf.length > 0;

  const warnings: string[] = [];
  const { files, folders } = args.scope.counts;
  if (args.scope.excluded.hidden > 0)
    warnings.push(`${args.scope.excluded.hidden} item tersembunyi tidak ikut.`);
  if (args.scope.filterActive)
    warnings.push('Filter panel aktif; hanya item yang lolos filter ikut diarsipkan.');

  let previewText = `Buat ${finalName} di ${targetDirectory} dari ${files} file dan ${folders} folder. Isi subfolder ikut. ${args.scope.scopeText}`;
  if (format !== 'zip' || args.level !== undefined)
    previewText += ` Format ${label}${args.level !== undefined ? `, kompresi ${levelText(args.level)}` : ''}.`;
  if (collision)
    previewText += ` Nama ${requested} sudah ada di tujuan; memakai ${finalName}. File lama tidak diubah.`;
  if (outputInsideSource)
    previewText += ` Folder tujuan berada di dalam folder sumber; hasil ${label} dan file sementara tidak ikut diarsipkan.`;

  return {
    ok: true,
    targetPane,
    targetDirectory,
    requestedName: requested,
    finalName,
    outputPath,
    collision,
    renamed: collision,
    excludeFromInput: [outputPath],
    outputInsideSource,
    excludeSubtreeOf,
    warnings,
    previewText,
  };
}
