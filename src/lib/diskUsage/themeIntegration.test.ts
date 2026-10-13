// FER-375 (APP-04): Disk Usage UI x global theme tokens.
// Static source + resolved-token checks across all 6 palettes x light/dark
// (12 combinations). This proves the DU stylesheet is wired to the global
// tokens and that the token values it relies on are legible; it does not
// render the app (no screenshot proof) — see the live-switch test below and
// docs/decisions notes for what remains manual.
import { describe, expect, it } from 'vitest';
import {
  MODES,
  PALETTE_IDS,
  contrast,
  hex,
  mix,
  readSource,
  resolveTokens,
} from './themeTokens.testutil';

const DU_FILES = [
  'DiskUsageWorkspace.svelte',
  'DiskUsageTree.svelte',
  'DiskUsageFiles.svelte',
  'Treemap.svelte',
].map((f) => ({ f, src: readSource(`src/lib/diskUsage/${f}`) }));

const COMBOS = PALETTE_IDS.flatMap((p) => MODES.map((m) => [p, m] as const));

function styleOf(src: string): string {
  return /<style[^>]*>([\s\S]*)<\/style>/.exec(src)?.[1] ?? '';
}
function h(t: Record<string, string>, n: string): string {
  const v = hex(t, n);
  if (!v) throw new Error(`token --${n} does not resolve to a hex colour`);
  return v;
}

describe('Disk Usage stylesheet uses only global tokens', () => {
  it('has 12 palette x mode combinations', () => {
    expect(COMBOS).toHaveLength(12);
  });

  it.each(DU_FILES)('$f: no hard-coded colour literals', ({ src }) => {
    const css = styleOf(src);
    // var(--x, #fallback) fallbacks count as literals too.
    expect(css.match(/#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/g) ?? []).toEqual([]);
  });

  it('every var(--token) used by the DU components is defined in app.css (or set by the component)', () => {
    const t = resolveTokens('violet', 'dark');
    const local = new Set(['--duRowMinW', '--duRowCols']);
    const missing: string[] = [];
    for (const { f, src } of DU_FILES) {
      for (const m of src.matchAll(/var\((--[\w-]+)/g)) {
        if (!(m[1] in t) && !local.has(m[1])) missing.push(`${f}:${m[1]}`);
      }
    }
    expect([...new Set(missing)]).toEqual([]);
  });

  it('text scales with the interface font level (--uiStep) like the rest of the app', () => {
    for (const { f, src } of DU_FILES) {
      const bare = styleOf(src).match(/font-size:\s*[\d.]+px/g) ?? [];
      expect(bare, f).toEqual([]);
    }
  });

  it('status tones are not accent: error -> red, partial/warn -> amber, success -> green', () => {
    const ws = styleOf(DU_FILES[0].src);
    const rule = (sel: string) => {
      const at = ws.indexOf(`${sel} {`);
      return at < 0 ? '' : ws.slice(at, ws.indexOf('}', at));
    };
    expect(rule('.chip.err')).toContain('var(--red)');
    expect(rule('.chip.warn')).toContain('var(--amber)');
    expect(rule('.chip.ok')).toContain('var(--green)');
    expect(rule('.note.err')).toContain('var(--red)');
    expect(rule('.note.warn')).toContain('var(--amber)');
    for (const s of ['.chip.err', '.chip.warn', '.chip.ok', '.note.err', '.note.warn']) {
      expect(rule(s), s).not.toMatch(/--accent/);
    }
    // error text in tree/files is the red ink, not amber
    expect(styleOf(DU_FILES[1].src)).toMatch(/\.submsg\.err\s*\{[^}]*--redInk/);
    expect(styleOf(DU_FILES[2].src)).toMatch(/\.err\s*\{[^}]*--redInk/);
  });
});

describe('treemap category mapping', () => {
  const tm = DU_FILES[3].src;
  const catBlock = /CATEGORY_COLOR[^{]*\{([^}]*)\}/.exec(tm)![1];
  const cats = [...catBlock.matchAll(/(\w+):\s*'var\((--[\w-]+)\)'/g)].map((m) => [m[1], m[2]]);

  it('maps the six DU categories to global file-type tokens', () => {
    expect(Object.fromEntries(cats)).toEqual({
      video: '--ftMedia',
      image: '--ftImage',
      audio: '--ftCode',
      document: '--ftDocs',
      archive: '--ftArchive',
      other: '--bar',
    });
  });

  it('matches the file-pane icon mapping for shared categories (image/archive/docs)', () => {
    const icon = readSource('src/lib/fileIcon.ts');
    for (const [cat, tok] of [
      ['image', '--ftImage'],
      ['archive', '--ftArchive'],
      ['doc', '--ftDocs'],
    ] as const) {
      expect(icon).toContain(`${cat}: 'var(${tok})'`);
    }
  });

  it.each(COMBOS)('%s/%s: category colours resolve and are pairwise distinct', (p, m) => {
    const t = resolveTokens(p, m);
    const vals = cats.map(([, tok]) => h(t, tok));
    expect(new Set(vals).size).toBe(vals.length);
  });

  it.each(COMBOS)('%s/%s: category tiles keep text and selection ring legible', (p, m) => {
    const t = resolveTokens(p, m);
    const elev = h(t, 'elev');
    for (const [, tok] of [...cats, ['dir', '--folder']]) {
      const tile = mix(h(t, tok), elev, 28);
      expect(contrast(h(t, 'text'), tile), `text on ${tok}`).toBeGreaterThanOrEqual(4.5);
      // selected/focus ring (--text) is a UI component: >= 3:1. (accentInk
      // was < 3:1 on some dark image/archive tiles, e.g. electric/dark.)
      expect(contrast(h(t, 'text'), tile), `ring on ${tok}`).toBeGreaterThanOrEqual(3);
    }
  });

  it('tile labels use the primary text token (secondary text2 is below 4.5:1 on some dark media tiles)', () => {
    expect(styleOf(tm)).toMatch(/\.tv\s*\{[^}]*color:\s*var\(--text\);/);
  });

  it('treemap selection and focus rings use the primary text token', () => {
    const css = styleOf(tm);
    expect(css).toMatch(/\.tile:focus-visible\s*\{[^}]*outline:\s*2px solid var\(--text\)/);
    expect(css).toMatch(/\.tile\.sel\s*\{[^}]*outline:\s*2px solid var\(--text\)/);
  });

  it('directory tiles carry a non-colour cue (folder vs category must not rely on hue)', () => {
    expect(styleOf(tm)).toMatch(/\.tile\.dir\s*\{[^}]*inset 3px 0 0 var\(--folder\)/);
  });
});

describe('status legibility across the 12 combinations', () => {
  it.each(COMBOS)('%s/%s: chips, notes and error text meet 4.5:1', (p, m) => {
    const t = resolveTokens(p, m);
    const elev = h(t, 'elev');
    const pane = h(t, 'pane');
    const text = h(t, 'text');
    const chipBgs = [
      mix(h(t, 'green'), elev, 24),
      mix(h(t, 'amber'), elev, 26),
      mix(h(t, 'red'), elev, 28),
      mix(h(t, 'accent'), elev, 22),
    ];
    for (const bg of chipBgs) expect(contrast(text, bg)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(h(t, 'text2'), elev)).toBeGreaterThanOrEqual(4.5); // idle/cancelled chip
    for (const bg of [mix(h(t, 'amber'), pane, 16), mix(h(t, 'red'), pane, 18)]) {
      expect(contrast(text, bg)).toBeGreaterThanOrEqual(4.5);
    }
    // standalone error text and danger button label
    // (DU renders redInk blended 75% toward --text, see the .err rules)
    const errInk = mix(h(t, 'redInk'), h(t, 'text'), 75);
    expect(contrast(errInk, pane)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(errInk, elev)).toBeGreaterThanOrEqual(4.5);
  });
});
