// FER-478 file icon themes (handoff "design_handoff_file_icon_themes"):
// one 14x16 geometry, one glyph set, four themes. Pure data so the
// component stays one <svg> with <=4 children per row.
import type { Kind } from './kind';

export type IconTheme = 'Line' | 'Duotone' | 'Label' | 'Tile';
export const ICON_THEMES: IconTheme[] = ['Line', 'Duotone', 'Label', 'Tile'];
export const DEFAULT_ICON_THEME: IconTheme = 'Tile';

export type GlyphKind =
  'code' | 'doc' | 'pdf' | 'image' | 'video' | 'audio' | 'archive' | 'data' | 'config';

const DOC_BODY = 'M2.5 1h5.8L12 4.7v9.3a1 1 0 0 1-1 1H2.5a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1z';
const DOC_FOLD = 'M8.3 1v2.7a1 1 0 0 0 1 1H12z';
const LINE_FOLD = 'M8.3 1.3v2.4a1 1 0 0 0 1 1h2.4';
const TILE_BODY =
  'M4 1.5h6a3.5 3.5 0 0 1 3.5 3.5v6a3.5 3.5 0 0 1-3.5 3.5H4A3.5 3.5 0 0 1 .5 11V5A3.5 3.5 0 0 1 4 1.5z';

export const GLYPHS: Record<GlyphKind, string> = {
  code: 'M5.2 8.3 3.7 10.4l1.5 2.1M8.6 8.3l1.5 2.1-1.5 2.1',
  image: 'M3.7 12.6l2.2-2.7 1.6 1.7 1-1 1.7 2M8.7 7.9v.01',
  video: 'M5.4 8.2v4.4l3.5-2.2z',
  audio: 'M8 12.2V7.9l2 .8M8 12.2a1.2 1.2 0 1 1-2.4 0 1.2 1.2 0 0 1 2.4 0z',
  archive: 'M6.9 1.9v.9M6.9 4v.9M6.9 6.1v.9M5.9 8.4h2v2.9h-2z',
  data: 'M3.8 8.2h6.4v4.4H3.8zM3.8 10.4h6.4M7 8.2v4.4',
  doc: 'M3.8 8.6h6.4M3.8 10.6h6.4M3.8 12.6h3.8',
  pdf: 'M4.1 12.8V8.4h1.5a1.3 1.3 0 0 1 0 2.6H4.1M8.1 8.6h2M8.1 10.6h2M8.1 12.6h2',
  config: 'M3.8 9h6.4M3.8 12h6.4M5.6 8v2M8.4 11v2',
};
const TILE_ARCHIVE = 'M6.9 6.3v.9M6.9 8.2v.9M5.9 10h2v2.6h-2z';

/** Port of the prototype's typeOf(name) -> kind (suffix map, first match wins). */
const SUFFIX: [string, GlyphKind][] = [
  ['.jsonl.gz', 'archive'],
  ['.rs', 'code'],
  ['.ts', 'code'],
  ['.js', 'code'],
  ['.css', 'code'],
  ['.html', 'code'],
  ['.json', 'config'],
  ['.toml', 'config'],
  ['.yaml', 'config'],
  ['.lock', 'config'],
  ['.code-workspace', 'config'],
  ['.md', 'doc'],
  ['.pdf', 'pdf'],
  ['.txt', 'doc'],
  ['.log', 'doc'],
  ['.xlsx', 'data'],
  ['.csv', 'data'],
  ['.parquet', 'data'],
  ['.sql', 'data'],
  ['.qcow2', 'archive'],
  ['.vhdx', 'archive'],
  ['.iso', 'archive'],
  ['.dmg', 'archive'],
  ['.zip', 'archive'],
  ['.png', 'image'],
  ['.jpg', 'image'],
  ['.exr', 'image'],
  ['.svg', 'image'],
  ['.mp4', 'video'],
  ['.mov', 'video'],
  ['.braw', 'video'],
  ['.flac', 'audio'],
  ['.wav', 'audio'],
];

/** Legacy coarse Kind (kind.ts) -> glyph kind, used when no name is known. */
const FROM_KIND: Record<Kind, GlyphKind> = {
  dir: 'config',
  code: 'code',
  image: 'image',
  media: 'video',
  archive: 'archive',
  data: 'data',
  docs: 'doc',
  text: 'config',
};

// Extensions the app's Kind map knows but the prototype list does not;
// resolved through the existing Kind so nothing regresses to "config".
const AUDIO_EXT = new Set(['mp3', 'flac', 'wav', 'ogg', 'm4a']);
const PDF_EXT = new Set(['pdf']);
const SHEET_EXT = new Set(['xls', 'xlsx', 'csv', 'tsv']);

export function glyphKindOf(name: string | undefined, kind: Kind): GlyphKind {
  if (name) {
    const l = name.toLowerCase();
    for (const [e, k] of SUFFIX) if (l.endsWith(e)) return k;
    const dot = l.lastIndexOf('.');
    const ext = dot > 0 ? l.slice(dot + 1) : '';
    if (PDF_EXT.has(ext)) return 'pdf';
    if (SHEET_EXT.has(ext)) return 'data';
    if (AUDIO_EXT.has(ext)) return 'audio';
    if (l === 'license') return 'doc';
    if (dot === 0) return 'config';
  }
  return FROM_KIND[kind];
}

const KIND_VAR: Record<GlyphKind, string> = {
  code: 'var(--ftCode)',
  doc: 'var(--ftDocs)',
  pdf: 'var(--ftPdf)',
  image: 'var(--ftImage)',
  video: 'var(--ftMedia)',
  audio: 'var(--ftMedia)',
  archive: 'var(--ftArchive)',
  data: 'var(--ftData)',
  config: 'var(--ftConfig, #7d8aa8)',
};

export interface IconAttrs {
  body: string;
  bf: string;
  bo: number;
  bs: string;
  fold: string;
  ff: string;
  fs: string;
  glyph: string;
  gs: string;
  gt: string;
  badge: boolean;
  bw: number;
  bc: string;
}

const cache = new Map<string, IconAttrs>();

/** Per-theme attribute object, memoized per (theme, glyph kind, mono). The
 *  palette is resolved by the --ft* CSS variables, so it needs no key. */
export function iconAttrs(theme: IconTheme, k: GlyphKind, mono: boolean): IconAttrs {
  const key = `${theme}|${k}|${mono ? 1 : 0}`;
  const hit = cache.get(key);
  if (hit) return hit;
  const c = mono ? 'var(--text3)' : KIND_VAR[k];
  const o: IconAttrs = {
    body: DOC_BODY,
    bf: 'none',
    bo: 1,
    bs: 'none',
    fold: DOC_FOLD,
    ff: 'none',
    fs: 'none',
    glyph: GLYPHS[k],
    gs: c,
    gt: 'translate(0 0)',
    badge: false,
    bw: 0,
    bc: 'none',
  };
  if (theme === 'Line') Object.assign(o, { bs: c, fold: LINE_FOLD, fs: c });
  else if (theme === 'Duotone') Object.assign(o, { bf: c, bo: 0.24, ff: c });
  else if (theme === 'Label')
    Object.assign(o, {
      bf: 'color-mix(in oklch, var(--text3) 26%, transparent)',
      ff: 'color-mix(in oklch, var(--text3) 55%, transparent)',
      glyph: '',
      badge: true,
      bw: 10.2,
      bc: c,
    });
  else
    Object.assign(o, {
      body: TILE_BODY,
      bf: c,
      fold: '',
      gs: '#fff',
      gt: 'translate(0 -2.4)',
      glyph: k === 'archive' ? TILE_ARCHIVE : GLYPHS[k],
    });
  cache.set(key, o);
  return o;
}

/** Label theme text: extension uppercased, max 4 chars, "FILE" if none. */
export function labelOf(name: string | undefined): string {
  const m = (name ?? '').match(/\.([a-z0-9]+)$/i);
  return (m?.[1] || 'file').slice(0, 4).toUpperCase();
}

export function normalizeIconTheme(v: unknown): IconTheme {
  return ICON_THEMES.includes(v as IconTheme) ? (v as IconTheme) : DEFAULT_ICON_THEME;
}
