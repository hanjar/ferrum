// AI-03 / FER-185: resolusi lokal target dan nama output ZIP.
// Model hanya boleh memberi `outputName` (basename) dan referensi panel; direktori
// tujuan selalu berasal dari snapshot panel, bukan dari teks model.
import type { PaneRef, PaneSnapshot } from './contracts';
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
const EXT = '.zip';

export type NameResult = { ok: true; name: string } | { ok: false; message: string };

/** Validasi basename dari pengguna/model; menambah `.zip` bila belum ada. */
export function sanitizeOutputName(raw: string): NameResult {
  const trimmed = raw.trim();
  if (!trimmed) return { ok: false, message: 'Nama file ZIP kosong.' };
  if (FORBIDDEN.test(trimmed)) {
    return {
      ok: false,
      message: 'Nama file ZIP tidak boleh memuat pemisah folder, ":" atau karakter terlarang.',
    };
  }
  if (/[. ]$/.test(trimmed))
    return { ok: false, message: 'Nama file ZIP tidak boleh berakhir dengan titik atau spasi.' };
  const name = trimmed.toLowerCase().endsWith(EXT) ? trimmed : trimmed + EXT;
  const stem = name.slice(0, -EXT.length);
  if (!stem.trim() || stem === '.' || stem === '..')
    return { ok: false, message: 'Nama file ZIP tidak valid.' };
  if (RESERVED.test(stem.split('.')[0].trimEnd())) {
    return { ok: false, message: 'Nama tersebut dicadangkan oleh sistem operasi.' };
  }
  if (name.length > MAX_NAME) return { ok: false, message: 'Nama file ZIP terlalu panjang.' };
  return { ok: true, name };
}

function lastSegment(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  return i === -1 ? norm : norm.slice(i + 1);
}

/** Nama default: basename folder sumber + .zip; root drive memakai `archive`. */
function defaultName(sourcePath: string): string {
  const base = lastSegment(sourcePath);
  if (!base || /^[A-Za-z]:$/.test(base)) return `archive${EXT}`;
  const safe = base.replace(FORBIDDEN, '_').replace(/[. ]+$/, '');
  const res = safe ? sanitizeOutputName(safe) : undefined;
  return res && res.ok ? res.name : `archive${EXT}`;
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

function uniqueName(wanted: string, taken: ReadonlySet<string>): string {
  if (!taken.has(wanted.toLowerCase())) return wanted;
  const stem = wanted.slice(0, -EXT.length);
  const ext = wanted.slice(-EXT.length);
  for (let n = 2; ; n++) {
    const candidate = `${stem} (${n})${ext}`;
    if (!taken.has(candidate.toLowerCase())) return candidate;
  }
}

export interface ArchiveTargetArgs {
  sourcePane: PaneRef;
  target: { kind: 'sourceDirectory' } | { kind: 'pane'; pane: PaneRef };
  outputName?: string;
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
    const n = sanitizeOutputName(args.outputName);
    if (!n.ok) return fail('invalid_name', n.message);
    requested = n.name;
  } else {
    requested = defaultName(args.scope.sourceDirectory);
  }

  const taken = new Set<string>();
  for (const e of probe.allEntries()) taken.add(e.name.toLowerCase());
  const finalName = uniqueName(requested, taken);
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
  if (collision)
    previewText += ` Nama ${requested} sudah ada di tujuan; memakai ${finalName}. File lama tidak diubah.`;
  if (outputInsideSource)
    previewText +=
      ' Folder tujuan berada di dalam folder sumber; hasil ZIP dan file sementara tidak ikut diarsipkan.';

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
