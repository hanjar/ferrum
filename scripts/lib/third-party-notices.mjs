/**
 * Pure helpers for scripts/generate-third-party-notices.mjs (FER-554).
 *
 * Nothing here touches the network and nothing writes an absolute local path into the output:
 * components are described by name, version, SPDX expression, repository URL and license text.
 */
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

/**
 * SPDX identifiers Ferrum accepts in what it ships. Anything else (GPL, AGPL, LGPL, SSPL, BUSL,
 * unknown) fails the generator, so a new dependency with an unreviewed license is a deliberate
 * decision and never an accident.
 */
export const ALLOWED_LICENSES = [
  '0BSD',
  'Apache-2.0',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'BlueOak-1.0.0',
  'CC0-1.0',
  'CDLA-Permissive-2.0',
  'ISC',
  'MIT',
  'MIT-0',
  'MPL-2.0',
  'OFL-1.1',
  'Unicode-3.0',
  'Unlicense',
  'Zlib',
];

// ── SPDX expressions ─────────────────────────────────────────────────────

/** Split an SPDX expression into tokens. The legacy "A/B" form (old crates) means "A OR B". */
export function tokenizeSpdx(expr) {
  const tokens = [];
  const re = /\s*(\(|\)|\/|[A-Za-z0-9.+-]+)/gy;
  let pos = 0;
  while (pos < expr.length) {
    re.lastIndex = pos;
    const m = re.exec(expr);
    if (!m) {
      if (expr.slice(pos).trim() === '') break;
      throw new Error(`Unparseable license expression: ${JSON.stringify(expr)}`);
    }
    pos = re.lastIndex;
    const t = m[1];
    const upper = t.toUpperCase();
    if (t === '/') tokens.push('OR');
    else if (upper === 'AND' || upper === 'OR' || upper === 'WITH') tokens.push(upper);
    else tokens.push(t);
  }
  return tokens;
}

/**
 * Parse an SPDX expression into a tree:
 * { op: 'id', id } | { op: 'and' | 'or', items: [...] }. `X WITH exception` is treated as `X`.
 * AND binds tighter than OR, as in the SPDX specification.
 */
export function parseSpdx(expr) {
  const tokens = tokenizeSpdx(expr);
  let i = 0;
  const peek = () => tokens[i];
  const next = () => tokens[i++];

  function parseOr() {
    const items = [parseAnd()];
    while (peek() === 'OR') {
      next();
      items.push(parseAnd());
    }
    return items.length === 1 ? items[0] : { op: 'or', items };
  }
  function parseAnd() {
    const items = [parseAtom()];
    while (peek() === 'AND') {
      next();
      items.push(parseAtom());
    }
    return items.length === 1 ? items[0] : { op: 'and', items };
  }
  function parseAtom() {
    const t = next();
    if (t === undefined)
      throw new Error(`Unexpected end of license expression: ${JSON.stringify(expr)}`);
    if (t === '(') {
      const inner = parseOr();
      if (next() !== ')')
        throw new Error(`Missing ")" in license expression: ${JSON.stringify(expr)}`);
      return inner;
    }
    if (t === ')' || t === 'AND' || t === 'OR' || t === 'WITH') {
      throw new Error(`Unexpected "${t}" in license expression: ${JSON.stringify(expr)}`);
    }
    if (peek() === 'WITH') {
      next();
      if (next() === undefined)
        throw new Error(`Missing exception after WITH: ${JSON.stringify(expr)}`);
    }
    return { op: 'id', id: t.replace(/\+$/, '') };
  }

  const tree = parseOr();
  if (i !== tokens.length)
    throw new Error(`Trailing tokens in license expression: ${JSON.stringify(expr)}`);
  return tree;
}

/** True when the expression can be satisfied using only allowed licenses (OR: pick any, AND: all). */
export function isAllowedExpression(expr, allowed = ALLOWED_LICENSES) {
  const set = new Set(allowed);
  const ok = (n) =>
    n.op === 'id' ? set.has(n.id) : n.op === 'and' ? n.items.every(ok) : n.items.some(ok);
  return ok(parseSpdx(expr));
}

// ── License text discovery and normalisation ─────────────────────────────

const LICENSE_FILE_RE = /^(licen[cs]e|licen[cs]es|copying|copyright|notice|unlicense)([-_. ].*)?$/i;

/** Names of the license-like files directly inside `dir`, in stable order. */
export function findLicenseFiles(dir, extra = []) {
  const names = new Set();
  for (const name of readdirSync(dir)) {
    if (!LICENSE_FILE_RE.test(name)) continue;
    if (!statSync(join(dir, name)).isFile()) continue;
    names.add(name);
  }
  for (const rel of extra) {
    if (!rel) continue;
    try {
      if (statSync(join(dir, rel)).isFile()) names.add(rel.replace(/\\/g, '/'));
    } catch {
      /* declared but absent: ignored, the "no license file" check reports it */
    }
  }
  return [...names].sort(compareStrings);
}

/** LF line endings, no trailing blanks on lines, no leading/trailing blank lines, no BOM. */
export function normalizeText(text) {
  return text
    .replace(/^\uFEFF/, '')
    .replace(/\r\n?/g, '\n')
    .split('\n')
    .map((l) => l.replace(/[ \t]+$/, ''))
    .join('\n')
    .replace(/^\n+/, '')
    .replace(/\n+$/, '');
}

export function textKey(text) {
  return createHash('sha256').update(text).digest('hex');
}

/** Codepoint order: identical on every machine and locale (no localeCompare). */
export function compareStrings(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Read and normalise the license files of one package directory. */
export function readLicenseFiles(dir, extra = []) {
  return findLicenseFiles(dir, extra).map((name) => ({
    name,
    text: normalizeText(readFileSync(join(dir, name), 'utf8')),
  }));
}

// ── Rust ─────────────────────────────────────────────────────────────────

/** Crates that vendor other software whose license file is not at the top of the package. */
export const EXTRA_LICENSE_FILES = { unrar_sys: ['vendor/unrar/license.txt'] };

function dirOfManifest(manifestPath) {
  return manifestPath.replace(/[\\/][^\\/]+$/, '');
}

/**
 * Crates that end up in the Windows binaries, from `cargo metadata` output.
 *
 * The walk follows normal dependency edges only (dev-dependencies are tests, build-dependencies run
 * on the build machine and are not linked into the binary). Proc-macro crates are normal edges and
 * stay in. Workspace-local crates (no registry source) are Ferrum itself and are not listed.
 */
export function collectRustComponents(metadata) {
  const byId = new Map(metadata.packages.map((p) => [p.id, p]));
  const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
  const roots = metadata.resolve.root ? [metadata.resolve.root] : metadata.workspace_members;
  const seen = new Set(roots);
  const stack = [...roots];
  while (stack.length) {
    const node = nodes.get(stack.pop());
    if (!node) continue;
    for (const dep of node.deps) {
      if (!dep.dep_kinds.some((k) => k.kind === null)) continue;
      if (seen.has(dep.pkg)) continue;
      seen.add(dep.pkg);
      stack.push(dep.pkg);
    }
  }
  const out = [];
  for (const id of seen) {
    const pkg = byId.get(id);
    if (!pkg || !pkg.source) continue;
    const dir = dirOfManifest(pkg.manifest_path);
    out.push({
      ecosystem: 'crate',
      name: pkg.name,
      version: pkg.version,
      license: pkg.license ?? null,
      repository: pkg.repository ?? pkg.homepage ?? null,
      authors: pkg.authors ?? [],
      files: readLicenseFiles(dir, [pkg.license_file]),
      // Vendored third-party sources that carry their own license (see EXTRA_LICENSE_FILES).
      extraFiles: (EXTRA_LICENSE_FILES[pkg.name] ?? []).map((rel) => ({
        name: rel,
        text: normalizeText(readFileSync(join(dir, rel), 'utf8')),
      })),
    });
  }
  return out;
}

// ── npm ──────────────────────────────────────────────────────────────────

/** Resolve `dep` as Node would from the package at lock key `fromKey` (nested node_modules first). */
function resolveLockKey(packages, fromKey, dep) {
  let base = fromKey;
  for (;;) {
    const candidate = base ? `${base}/node_modules/${dep}` : `node_modules/${dep}`;
    if (packages[candidate]) return candidate;
    if (!base) return null;
    const idx = base.lastIndexOf('/node_modules/');
    base = idx === -1 ? '' : base.slice(0, idx);
  }
}

/**
 * npm packages that reach the frontend bundle: the root `dependencies` plus `extraRoots` (build-time
 * packages whose runtime is compiled into the bundle, e.g. the Svelte runtime), following
 * `dependencies` only. Optional platform binaries are not part of the webview bundle and are skipped.
 */
export function collectNpmComponents(lock, readPackageDir, extraRoots = []) {
  const packages = lock.packages;
  const rootDeps = Object.keys(packages['']?.dependencies ?? {});
  const seen = new Set();
  const stack = [];
  for (const name of [...rootDeps, ...extraRoots]) {
    const key = resolveLockKey(packages, '', name);
    if (!key) throw new Error(`package-lock.json has no entry for "${name}"`);
    if (!seen.has(key)) {
      seen.add(key);
      stack.push(key);
    }
  }
  while (stack.length) {
    const key = stack.pop();
    for (const dep of Object.keys(packages[key].dependencies ?? {})) {
      const depKey = resolveLockKey(packages, key, dep);
      if (!depKey || seen.has(depKey)) continue;
      seen.add(depKey);
      stack.push(depKey);
    }
  }
  const out = [];
  for (const key of seen) {
    const entry = packages[key];
    const name = key.slice(key.lastIndexOf('node_modules/') + 'node_modules/'.length);
    const pkgDir = readPackageDir(key);
    const manifest = pkgDir.manifest ?? {};
    const lic =
      entry.license ?? manifest.license ?? manifest.licenses?.map((l) => l.type).join(' OR ');
    const repo =
      typeof manifest.repository === 'string' ? manifest.repository : manifest.repository?.url;
    out.push({
      ecosystem: 'npm',
      name,
      version: entry.version,
      license: typeof lic === 'object' && lic !== null ? (lic.type ?? null) : (lic ?? null),
      repository: normalizeRepoUrl(repo ?? manifest.homepage ?? null),
      authors: authorsOf(manifest),
      files: pkgDir.files,
      extraFiles: [],
    });
  }
  return out;
}

function authorsOf(manifest) {
  const a = manifest.author;
  if (!a) return [];
  return [typeof a === 'string' ? a : (a.name ?? '')].filter(Boolean);
}

export function normalizeRepoUrl(url) {
  if (!url) return null;
  return String(url)
    .replace(/^git\+/, '')
    .replace(/^git:\/\//, 'https://')
    .replace(/^github:/, 'https://github.com/')
    .replace(/\.git$/, '');
}

// ── Packages that publish no license file ────────────────────────────────

function idsToCover(node, templates) {
  if (node.op === 'id') return [node.id];
  if (node.op === 'and') return node.items.flatMap((n) => idsToCover(n, templates));
  // OR: any one alternative is enough; prefer the first one we hold a template for.
  const pick = node.items.find((n) => idsToCover(n, templates).every((id) => id in templates));
  return idsToCover(pick ?? node.items[0], templates);
}

function copyrightHolder(c) {
  const names = (c.authors ?? []).map((a) => a.replace(/\s*<[^>]*>/g, '').trim()).filter(Boolean);
  if (names.length) return names.join(', ');
  return `the authors of ${c.repository ?? c.name}`;
}

/**
 * Some published packages carry no license file (the repository has one, the package does not). For
 * those the standard text of the declared license is written with the copyright holder taken from
 * the package metadata, and the entry is labelled as generated. Licenses without a template are
 * left alone, so validation still fails loudly for them.
 */
export function withSynthesizedLicenseFiles(components, templates) {
  return components.map((c) => {
    if (c.files.length > 0 || !c.license) return c;
    let ids;
    try {
      ids = idsToCover(parseSpdx(c.license), templates);
    } catch {
      return c;
    }
    if (!ids.every((id) => id in templates)) return c;
    const files = ids.map((id) => ({
      name: `standard ${id} text, the published package has no license file`,
      text: normalizeText(templates[id].replaceAll('{{copyright}}', copyrightHolder(c))),
    }));
    return { ...c, files };
  });
}

// ── Validation ───────────────────────────────────────────────────────────

/** Human-readable problems (empty list = fine): missing/unknown/disallowed license, no license text. */
export function validateComponents(components, allowed = ALLOWED_LICENSES) {
  const problems = [];
  for (const c of components) {
    const id = `${c.ecosystem === 'crate' ? 'crate' : 'npm'} ${c.name}@${c.version}`;
    if (!c.license) {
      problems.push(`${id}: no license declared`);
    } else {
      try {
        if (!isAllowedExpression(c.license, allowed)) {
          problems.push(`${id}: license "${c.license}" is not in the allowlist`);
        }
      } catch (e) {
        problems.push(`${id}: ${e.message}`);
      }
    }
    if (c.files.length + (c.extraFiles?.length ?? 0) === 0)
      problems.push(`${id}: no license file found in the package`);
  }
  return problems;
}

// ── Rendering ────────────────────────────────────────────────────────────

const RULE = '='.repeat(78);
const THIN = '-'.repeat(78);

/**
 * Plain-text notice. Identical license texts are written once and referenced as [Ln] so the file
 * stays small; every ordering is codepoint order so the output is byte-stable.
 */
export function renderNotices({ components, header, special }) {
  const sorted = [...components].sort(
    (a, b) =>
      compareStrings(a.ecosystem, b.ecosystem) ||
      compareStrings(a.name, b.name) ||
      compareStrings(a.version, b.version),
  );

  const texts = new Map(); // key -> { id, text, users }
  const filesOf = (c) => [...c.files, ...(c.extraFiles ?? [])];
  for (const c of sorted) {
    for (const f of filesOf(c)) {
      const key = textKey(f.text);
      let entry = texts.get(key);
      if (!entry) {
        entry = { id: texts.size + 1, text: f.text, users: 0 };
        texts.set(key, entry);
      }
      entry.users += 1;
    }
  }

  const bySpdx = new Map();
  for (const c of sorted) bySpdx.set(c.license, (bySpdx.get(c.license) ?? 0) + 1);

  const lines = [];
  lines.push(header.trimEnd(), '');
  if (special.trim()) lines.push(RULE, 'SPECIAL NOTICES', RULE, '', special.trim(), '');

  const crates = sorted.filter((c) => c.ecosystem === 'crate').length;
  const npm = sorted.filter((c) => c.ecosystem === 'npm').length;
  lines.push(RULE, 'SUMMARY', RULE, '');
  lines.push(`Rust crates linked into the Windows binaries: ${crates}`);
  lines.push(`npm packages bundled into the frontend: ${npm}`, '');
  lines.push('Components per declared license (SPDX expression):');
  for (const [lic, n] of [...bySpdx].sort((a, b) => b[1] - a[1] || compareStrings(a[0], b[0]))) {
    lines.push(`  ${String(n).padStart(4)}  ${lic}`);
  }
  lines.push('');

  for (const [eco, title] of [
    ['crate', 'RUST CRATES'],
    ['npm', 'NPM PACKAGES (FRONTEND BUNDLE)'],
  ]) {
    lines.push(RULE, title, RULE, '');
    for (const c of sorted.filter((x) => x.ecosystem === eco)) {
      lines.push(`${c.name} ${c.version}`);
      lines.push(`  License: ${c.license}`);
      if (c.repository) lines.push(`  Source:  ${c.repository}`);
      const refs = filesOf(c).map((f) => `[L${texts.get(textKey(f.text)).id}] (${f.name})`);
      lines.push(`  Text:    ${refs.join(', ')}`, '');
    }
  }

  lines.push(RULE, 'LICENSE TEXTS', RULE, '');
  for (const e of [...texts.values()].sort((a, b) => a.id - b.id)) {
    lines.push(
      THIN,
      `[L${e.id}] used by ${e.users} license file${e.users === 1 ? '' : 's'}`,
      THIN,
      '',
    );
    lines.push(e.text, '');
  }
  return lines.join('\n').replace(/\n+$/, '\n');
}
