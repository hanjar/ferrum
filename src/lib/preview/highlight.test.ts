import { describe, expect, it } from 'vitest';
import { detectLang, tokenize, type HighlightLang } from './highlight';

const join = (s: string, l: HighlightLang) =>
  tokenize(s, l)
    .map((t) => t.text)
    .join('');

const SAMPLES: Record<HighlightLang, string> = {
  json: '{\n "a": [1, -2.5e+3, true, null], "b": "x\\"y", "c": "unterminated\n}',
  markdown: '# Title\n\n- item `code` [link](http://x)\n```js\nlet a = 1;\n```\n> quote **b**',
  js: 'const a = `t${1}` // c\n/* b */ function f(x) { return "s" + 1.5; }',
  rust: "fn main<'a>() { let c = 'x'; let s = \"s\"; /* c */ } // end",
  python: '# c\ndef f(x):\n  """doc\nstring"""\n  return \'a\' + 1 if True else None',
  css: 'a { color: #fff; margin: 0 auto !important; } /* c */',
  html: '<!-- c --><div class="a" id=\'b\' hidden>héllo 😀</div><br/>',
  shell: '# c\nif [ "$a" ]; then echo \'x\'; fi <# b #>',
  config: '[sec]\nkey = "v" # c\nother: 12\n; semi\n',
  plain: 'anything at all',
};

describe('tokenize lossless', () => {
  for (const [lang, src] of Object.entries(SAMPLES)) {
    it(`round-trips ${lang} (LF and CRLF)`, () => {
      expect(join(src, lang as HighlightLang)).toBe(src);
      const crlf = src.replace(/\n/g, '\r\n');
      expect(join(crlf, lang as HighlightLang)).toBe(crlf);
    });
  }
  it('handles empty and Unicode', () => {
    expect(tokenize('', 'js')).toEqual([]);
    const u = '日本語 "文字列" // コメント\n😀';
    expect(join(u, 'js')).toBe(u);
  });
});

describe('token classification', () => {
  const find = (s: string, l: HighlightLang, type: string) =>
    tokenize(s, l)
      .filter((t) => t.type === type)
      .map((t) => t.text);

  it('json key vs string', () => {
    expect(find('{"k": "v"}', 'json', 'key')).toEqual(['"k"']);
    expect(find('{"k": "v"}', 'json', 'string')).toEqual(['"v"']);
    expect(find('{"k": true, "n": -1}', 'json', 'bool')).toEqual(['true']);
    expect(find('{"n": -1.5e3}', 'json', 'number')).toEqual(['-1.5e3']);
  });
  it('comments', () => {
    expect(find('a // x\nb', 'js', 'comment')).toEqual(['// x']);
    expect(find('a /* x\ny */ b', 'js', 'comment')).toEqual(['/* x\ny */']);
    expect(find('x = 1 # c', 'python', 'comment')).toEqual(['# c']);
  });
  it('unterminated string stops at line end', () => {
    const t = tokenize('"abc\nlet', 'js');
    expect(t[0]).toEqual({ type: 'string', text: '"abc' });
    expect(t.some((x) => x.type === 'keyword' && x.text === 'let')).toBe(true);
  });
  it('markdown heading and link', () => {
    expect(find('# Hi\ntext', 'markdown', 'heading')).toEqual(['# Hi']);
    expect(find('see [a](b)', 'markdown', 'link')).toEqual(['[a](b)']);
  });
  it('config keys', () => {
    expect(find('a = 1\nb: 2', 'config', 'key')).toEqual(['a', 'b']);
  });
  it('detectLang', () => {
    expect(detectLang('x.rs', 'text')).toBe('rust');
    expect(detectLang('X.TSX', 'text')).toBe('js');
    expect(detectLang('Makefile', 'text')).toBe('plain');
    expect(detectLang('a.txt', 'json')).toBe('json');
    expect(detectLang('a.txt', 'markdown')).toBe('markdown');
  });
  it('html script is only text tokens', () => {
    const s = '<script>alert(1)</script>';
    expect(join(s, 'html')).toBe(s);
    expect(tokenize(s, 'html').every((t) => typeof t.text === 'string')).toBe(true);
  });
});

describe('safety', () => {
  it('falls back to a single plain token for huge input', () => {
    const big = 'a'.repeat(200_001);
    expect(tokenize(big, 'js')).toEqual([{ type: 'plain', text: big }]);
    const lines = 'x\n'.repeat(5001);
    expect(tokenize(lines, 'js')).toEqual([{ type: 'plain', text: lines }]);
  });
  const patho = ['"', '/*', '[', '`', '<', "'", '#', '[a](', '<a "', '"a" ', '*', '\\'];
  for (const p of patho) {
    for (const lang of Object.keys(SAMPLES) as HighlightLang[]) {
      it(`pathological ${JSON.stringify(p)} in ${lang} is fast and lossless`, () => {
        const s = p.repeat(Math.floor(100_000 / p.length));
        const t0 = performance.now();
        expect(join(s, lang)).toBe(s);
        expect(performance.now() - t0).toBeLessThan(1500);
      });
    }
  }
  it('never throws on non-string', () => {
    expect(() => tokenize(undefined as unknown as string, 'js')).not.toThrow();
  });
});
