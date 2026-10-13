// Test-only helper (FER-375 APP-04): resolves app.css custom properties for a
// given palette x mode by emulating the cascade (specificity, then source
// order) of the :root / html[data-theme] / html[data-palette] rule blocks.
// Only hex literals and var() aliases are resolved; color-mix() values are
// returned verbatim (callers resolve them via mix()).
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

export const PALETTE_IDS = ['violet', 'electric', 'aqua', 'green', 'yellow', 'orange'] as const;
export const MODES = ['dark', 'light'] as const;
export type PaletteId = (typeof PALETTE_IDS)[number];
export type Mode = (typeof MODES)[number];

interface Rule {
  palette?: string;
  theme?: string;
  spec: number;
  order: number;
  decls: Record<string, string>;
}

export function readSource(rel: string): string {
  return readFileSync(resolve(process.cwd(), rel), 'utf8');
}

function parseRules(css: string): Rule[] {
  const rules: Rule[] = [];
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, '');
  let order = 0;
  for (const m of stripped.matchAll(/([^{}]+?)\s*\{([^{}]*)\}/g)) {
    const decls: Record<string, string> = {};
    for (const d of m[2].matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) decls[d[1]] = d[2].trim();
    for (const rawSel of m[1].split(',')) {
      const sel = rawSel.trim();
      let palette: string | undefined;
      let theme: string | undefined;
      let spec: number;
      if (sel === ':root') spec = 10;
      else if (/^html(\[data-(palette|theme)='\w+'\])+$/.test(sel)) {
        palette = /data-palette='(\w+)'/.exec(sel)?.[1];
        theme = /data-theme='(\w+)'/.exec(sel)?.[1];
        spec = 11 + (palette && theme ? 10 : 0);
      } else continue;
      rules.push({ palette, theme, spec, order: order++, decls });
    }
  }
  return rules;
}

const RULES = parseRules(readSource('src/app.css'));

export function resolveTokens(palette: PaletteId, mode: Mode): Record<string, string> {
  const applicable = RULES.filter(
    (r) => (!r.palette || r.palette === palette) && (!r.theme || r.theme === mode),
  ).sort((a, b) => a.spec - b.spec || a.order - b.order);
  const out: Record<string, string> = {};
  for (const r of applicable) Object.assign(out, r.decls);
  return out;
}

/** Resolve `--x` (following var() aliases) to a #rrggbb literal, or null. */
export function hex(tokens: Record<string, string>, name: string, depth = 0): string | null {
  const v = tokens[name.startsWith('--') ? name : `--${name}`];
  if (!v || depth > 6) return null;
  const lit = /^#([0-9a-fA-F]{6})$/.exec(v);
  if (lit) return `#${lit[1].toLowerCase()}`;
  const alias = /^var\((--[\w-]+)\)$/.exec(v);
  return alias ? hex(tokens, alias[1], depth + 1) : null;
}

function lum(h: string): number {
  const c = [1, 3, 5]
    .map((i) => parseInt(h.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}

export function contrast(a: string, b: string): number {
  const [l1, l2] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (l1 + 0.05) / (l2 + 0.05);
}

/** sRGB-space mix of fg over bg at `pct`% (close enough to color-mix for a
 *  conservative contrast check; oklch mixing differs by a few percent). */
export function mix(fg: string, bg: string, pct: number): string {
  const ch = (h: string, i: number) => parseInt(h.slice(i, i + 2), 16);
  const m = [1, 3, 5].map((i) => {
    const v = Math.round((ch(fg, i) * pct + ch(bg, i) * (100 - pct)) / 100);
    return v.toString(16).padStart(2, '0');
  });
  return `#${m.join('')}`;
}
