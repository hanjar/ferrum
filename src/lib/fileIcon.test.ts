import { describe, expect, it } from 'vitest';
import { glyphKindOf, iconAttrs, labelOf, normalizeIconTheme, GLYPHS } from './fileIcon';

describe('FER-478 file icon themes', () => {
  it('falls back to Tile for missing or legacy values', () => {
    expect(normalizeIconTheme(undefined)).toBe('Tile');
    expect(normalizeIconTheme('Neon')).toBe('Tile');
    expect(normalizeIconTheme('Tile')).toBe('Tile');
  });

  it('maps names to glyph kinds like the prototype typeOf', () => {
    expect(glyphKindOf('a.pdf', 'docs')).toBe('pdf');
    expect(glyphKindOf('a.json', 'data')).toBe('config');
    expect(glyphKindOf('a.xlsx', 'docs')).toBe('data');
    expect(glyphKindOf('a.mp3', 'media')).toBe('audio');
    expect(glyphKindOf('a.mp4', 'media')).toBe('video');
    expect(glyphKindOf('x.jsonl.gz', 'archive')).toBe('archive');
    expect(glyphKindOf('.env', 'text')).toBe('config');
    expect(glyphKindOf('noext', 'text')).toBe('config');
    expect(glyphKindOf(undefined, 'docs')).toBe('doc');
  });

  it('builds the per-theme attributes from the spec', () => {
    const line = iconAttrs('Line', 'pdf', false);
    expect(line).toMatchObject({ bf: 'none', bs: 'var(--ftPdf)', fs: 'var(--ftPdf)' });
    expect(line.fold).toBe('M8.3 1.3v2.4a1 1 0 0 0 1 1h2.4');
    const duo = iconAttrs('Duotone', 'code', false);
    expect(duo).toMatchObject({ bf: 'var(--ftCode)', bo: 0.24, bs: 'none', ff: 'var(--ftCode)' });
    const label = iconAttrs('Label', 'code', false);
    expect(label).toMatchObject({ badge: true, bw: 10.2, bc: 'var(--ftCode)', glyph: '' });
    expect(label.bf).toContain('color-mix(in oklch, var(--text3) 26%');
    const tile = iconAttrs('Tile', 'archive', false);
    expect(tile).toMatchObject({ gs: '#fff', gt: 'translate(0 -2.4)', fold: '' });
    expect(tile.glyph).toBe('M6.9 6.3v.9M6.9 8.2v.9M5.9 10h2v2.6h-2z');
    expect(iconAttrs('Tile', 'code', false).glyph).toBe(GLYPHS.code);
  });

  it('monochrome swaps every kind color for --text3 in every theme', () => {
    for (const t of ['Line', 'Duotone', 'Label', 'Tile'] as const) {
      const a = iconAttrs(t, 'pdf', true);
      expect(JSON.stringify(a)).not.toContain('--ftPdf');
    }
    expect(iconAttrs('Duotone', 'pdf', true).bf).toBe('var(--text3)');
  });

  it('memoizes attribute objects', () => {
    expect(iconAttrs('Line', 'doc', false)).toBe(iconAttrs('Line', 'doc', false));
  });

  it('label text is the uppercased extension, max 4, FILE otherwise', () => {
    expect(labelOf('report.pdf')).toBe('PDF');
    expect(labelOf('a.parquet')).toBe('PARQ');
    expect(labelOf('Makefile')).toBe('FILE');
  });
});
