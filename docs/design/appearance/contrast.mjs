// Contrast audit for the Appearance tokens (FER-370 APP-05).
// Usage: node docs/design/appearance/contrast.mjs > docs/design/appearance/CONTRAST.md
// Parses src/app.css palette blocks and computes WCAG 2.x ratios:
// normal text >= 4.5:1; focus/UI accent >= 3:1. text3 (tertiary meta) is
// reported unjudged — its design role is de-emphasized metadata.
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../../../src/app.css', import.meta.url), 'utf8');
const lum = (h) => {
  const c = [1, 3, 5]
    .map((i) => parseInt(h.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
};
const ratio = (a, b) => {
  const [l1, l2] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (l1 + 0.05) / (l2 + 0.05);
};

const dark = {};
const light = {};
// Merge every block per palette: palettes and their per-palette --ft* blocks
// share the same selector prefix, so later blocks must not clobber earlier ones.
for (const m of css.matchAll(/html\[data-palette='(\w+)'\]\s*\{([^}]+)\}/g)) {
  const v = {};
  for (const t of m[2].matchAll(/--(\w+):\s*(#[0-9a-fA-F]{6})/g)) v[t[1]] = t[2];
  dark[m[1]] = { ...(dark[m[1]] ?? {}), ...v };
}
for (const m of css.matchAll(/html\[data-palette='(\w+)'\]\[data-theme='light'\]\s*\{([^}]+)\}/g)) {
  const v = {};
  for (const t of m[2].matchAll(/--(\w+):\s*(#[0-9a-fA-F]{6})/g)) v[t[1]] = t[2];
  light[m[1]] = { ...(light[m[1]] ?? {}), ...v };
}

const f = (r, t) => r.toFixed(2) + (r >= t ? ' PASS' : ' FAIL');
console.log('# Kontras WCAG — token Appearance (dihitung dari src/app.css)');
console.log('');
console.log(
  'Target: teks normal >= 4.5:1; accent/fokus >= 3:1. `text3` = metadata tersier (dilaporkan tanpa vonis — peran desainnya memang diredam).',
);
console.log('');
console.log(
  '| palet | mode | text/bg (4.5) | text2/pane (4.5) | text3/pane (info) | accentInk/bg (3.0) |',
);
console.log('|---|---|---|---|---|---|');
let missing = 0;
for (const pal of ['violet', 'electric', 'aqua', 'green', 'yellow', 'orange']) {
  for (const [mode, set] of [
    ['dark', dark],
    ['light', light],
  ]) {
    const s = set[pal];
    if (!s || !s.text || !s.bg || !s.pane || !s.text2 || !s.text3 || !s.accentInk) {
      console.log(`| ${pal} | ${mode} | MISSING (${Object.keys(s ?? {}).join(',')}) | | | |`);
      missing++;
      continue;
    }
    console.log(
      `| ${pal} | ${mode} | ${f(ratio(s.text, s.bg), 4.5)} | ${f(ratio(s.text2, s.pane), 4.5)} | ${ratio(s.text3, s.pane).toFixed(2)} | ${f(ratio(s.accentInk, s.bg), 3.0)} |`,
    );
  }
}
if (missing) process.exitCode = 1;
