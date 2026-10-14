import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
  collectNpmComponents,
  collectRustComponents,
  isAllowedExpression,
  normalizeText,
  parseSpdx,
  renderNotices,
  validateComponents,
  withSynthesizedLicenseFiles,
} from './third-party-notices.mjs';

const dirs = [];
function tmp() {
  const d = mkdtempSync(join(tmpdir(), 'ferrum-notices-'));
  dirs.push(d);
  return d;
}
afterEach(() => {
  while (dirs.length) rmSync(dirs.pop(), { recursive: true, force: true });
});

const comp = (over) => ({
  ecosystem: 'crate',
  name: 'x',
  version: '1.0.0',
  license: 'MIT',
  repository: 'https://example.test/x',
  authors: [],
  files: [{ name: 'LICENSE', text: 'MIT text' }],
  extraFiles: [],
  ...over,
});

describe('SPDX expressions', () => {
  it('accepts allowed single, OR, AND and legacy slash forms', () => {
    expect(isAllowedExpression('MIT')).toBe(true);
    expect(isAllowedExpression('MIT OR Apache-2.0')).toBe(true);
    expect(isAllowedExpression('MIT/Apache-2.0')).toBe(true);
    expect(isAllowedExpression('Apache-2.0 / MIT')).toBe(true);
    expect(isAllowedExpression('(MIT OR Apache-2.0) AND Unicode-3.0')).toBe(true);
    expect(isAllowedExpression('Apache-2.0 WITH LLVM-exception')).toBe(true);
  });

  it('rejects copyleft and unknown licenses, and AND needs every side allowed', () => {
    expect(isAllowedExpression('GPL-3.0-only')).toBe(false);
    expect(isAllowedExpression('AGPL-3.0-or-later')).toBe(false);
    expect(isAllowedExpression('LGPL-2.1')).toBe(false);
    expect(isAllowedExpression('SSPL-1.0')).toBe(false);
    expect(isAllowedExpression('BUSL-1.1')).toBe(false);
    expect(isAllowedExpression('MIT AND GPL-3.0-only')).toBe(false);
    // OR: one allowed alternative is enough
    expect(isAllowedExpression('GPL-3.0-only OR MIT')).toBe(true);
  });

  it('gives AND higher precedence than OR and reports bad syntax', () => {
    expect(parseSpdx('A OR B AND C')).toEqual({
      op: 'or',
      items: [
        { op: 'id', id: 'A' },
        {
          op: 'and',
          items: [
            { op: 'id', id: 'B' },
            { op: 'id', id: 'C' },
          ],
        },
      ],
    });
    expect(() => parseSpdx('MIT OR')).toThrow();
    expect(() => parseSpdx('(MIT')).toThrow();
  });
});

describe('validation', () => {
  it('passes a clean component', () => {
    expect(validateComponents([comp({})])).toEqual([]);
  });

  it('fails a component without a license file', () => {
    const problems = validateComponents([comp({ name: 'nofile', files: [] })]);
    expect(problems).toHaveLength(1);
    expect(problems[0]).toContain('nofile@1.0.0');
    expect(problems[0]).toContain('no license file');
  });

  it('fails an undeclared or disallowed license', () => {
    const problems = validateComponents([
      comp({ name: 'nolic', license: null }),
      comp({ name: 'gpl', license: 'GPL-3.0-only' }),
    ]);
    expect(problems.join('\n')).toContain('nolic@1.0.0: no license declared');
    expect(problems.join('\n')).toContain(
      'gpl@1.0.0: license "GPL-3.0-only" is not in the allowlist',
    );
  });
});

describe('synthesized license files', () => {
  const templates = {
    MIT: 'Copyright (c) {{copyright}}\nMIT body',
    'BSD-3-Clause': 'BSD {{copyright}}',
  };

  it('fills a template from package metadata only when the package has no file', () => {
    const [a, b] = withSynthesizedLicenseFiles(
      [
        comp({ name: 'a', files: [], authors: ['Jane Doe <jane@example.test>'] }),
        comp({ name: 'b' }),
      ],
      templates,
    );
    expect(a.files[0].text).toBe('Copyright (c) Jane Doe\nMIT body');
    expect(b.files[0].text).toBe('MIT text');
  });

  it('falls back to the repository when there is no author and picks MIT from a dual license', () => {
    const [a] = withSynthesizedLicenseFiles(
      [comp({ files: [], license: 'MIT OR Apache-2.0' })],
      templates,
    );
    expect(a.files[0].text).toContain('the authors of https://example.test/x');
  });

  it('leaves licenses without a template empty so validation fails', () => {
    const [a] = withSynthesizedLicenseFiles([comp({ files: [], license: 'ISC' })], templates);
    expect(a.files).toEqual([]);
    expect(validateComponents([a])).toHaveLength(1);
  });
});

describe('rendering', () => {
  const header = 'HEADER';
  const a = comp({ name: 'zeta', files: [{ name: 'LICENSE', text: 'same' }] });
  const b = comp({ name: 'alpha', files: [{ name: 'LICENSE-MIT', text: 'same' }] });
  const c = comp({ name: 'mid', files: [{ name: 'LICENSE', text: 'other' }], license: 'ISC' });

  it('writes identical texts once and references them', () => {
    const out = renderNotices({ components: [a, b, c], header, special: '' });
    expect(out.match(/^same$/gm)).toHaveLength(1);
    expect(out).toContain(
      'alpha 1.0.0\n  License: MIT\n  Source:  https://example.test/x\n  Text:    [L1] (LICENSE-MIT)',
    );
    expect(out).toContain('zeta 1.0.0');
    expect(out).toContain('[L1] used by 2 license files');
  });

  it('is stable regardless of input order', () => {
    const one = renderNotices({ components: [a, b, c], header, special: '' });
    const two = renderNotices({ components: [c, b, a], header, special: '' });
    expect(two).toBe(one);
    expect(one.indexOf('alpha 1.0.0')).toBeLessThan(one.indexOf('mid 1.0.0'));
    expect(one.indexOf('mid 1.0.0')).toBeLessThan(one.indexOf('zeta 1.0.0'));
  });

  it('keeps special notices verbatim and normalizes line endings', () => {
    const out = renderNotices({ components: [a], header, special: 'SPECIAL TEXT' });
    expect(out).toContain('SPECIAL TEXT');
    expect(normalizeText('﻿\r\nx  \r\ny\r\n\r\n')).toBe('x\ny');
  });
});

describe('collectors', () => {
  it('collects only normal-dependency crates from cargo metadata and skips local crates', () => {
    const root = tmp();
    const crateDir = (name) => {
      const d = join(root, name);
      mkdirSync(d, { recursive: true });
      writeFileSync(join(d, 'LICENSE-MIT'), `${name} license\r\n`);
      return join(d, 'Cargo.toml');
    };
    const pkg = (name, source = 'registry+x') => ({
      id: name,
      name,
      version: '1.0.0',
      source,
      license: 'MIT',
      manifest_path: crateDir(name),
    });
    const metadata = {
      packages: [pkg('app', null), pkg('normal'), pkg('devonly'), pkg('buildonly')],
      resolve: {
        root: 'app',
        nodes: [
          {
            id: 'app',
            deps: [
              { pkg: 'normal', dep_kinds: [{ kind: null }] },
              { pkg: 'devonly', dep_kinds: [{ kind: 'dev' }] },
              { pkg: 'buildonly', dep_kinds: [{ kind: 'build' }] },
            ],
          },
          { id: 'normal', deps: [] },
          { id: 'devonly', deps: [] },
          { id: 'buildonly', deps: [] },
        ],
      },
    };
    const found = collectRustComponents(metadata);
    expect(found.map((c) => c.name)).toEqual(['normal']);
    expect(found[0].files[0].text).toBe('normal license');
    expect(JSON.stringify(found)).not.toContain(root);
  });

  it('collects npm packages reachable from dependencies and extra roots, not dev-only ones', () => {
    const entry = (extra = {}) => ({ version: '1.0.0', license: 'MIT', ...extra });
    const lock = {
      packages: {
        '': { dependencies: { a: '1' } },
        'node_modules/a': entry({ dependencies: { b: '1' } }),
        'node_modules/b': entry(),
        'node_modules/rt': entry({ dependencies: { c: '1' } }),
        'node_modules/rt/node_modules/c': entry({ version: '2.0.0' }),
        'node_modules/c': entry({ dev: true }),
        'node_modules/devtool': entry({ dev: true }),
      },
    };
    const read = () => ({ manifest: {}, files: [{ name: 'LICENSE', text: 't' }] });
    const found = collectNpmComponents(lock, read, ['rt']);
    expect(found.map((c) => `${c.name}@${c.version}`).sort()).toEqual([
      'a@1.0.0',
      'b@1.0.0',
      'c@2.0.0',
      'rt@1.0.0',
    ]);
  });
});
