/**
 * Pure, dependency-free, linear-time tokenizer for the text preview (PRV-04).
 * Lossless: concatenating every token.text yields the input exactly.
 * Never throws; oversized input falls back to a single plain token.
 */

export type TokenType =
  | 'plain'
  | 'keyword'
  | 'string'
  | 'number'
  | 'comment'
  | 'punct'
  | 'key'
  | 'heading'
  | 'link'
  | 'bool';

export interface Token {
  type: TokenType;
  text: string;
}

export type HighlightLang =
  'json' | 'markdown' | 'js' | 'rust' | 'python' | 'css' | 'html' | 'shell' | 'config' | 'plain';

export const MAX_HIGHLIGHT_CHARS = 200_000;
export const MAX_HIGHLIGHT_LINES = 5000;
const LOOKAHEAD_CAP = 500;

const EXT: Record<string, HighlightLang> = {
  json: 'json',
  jsonc: 'json',
  json5: 'json',
  md: 'markdown',
  markdown: 'markdown',
  js: 'js',
  mjs: 'js',
  cjs: 'js',
  jsx: 'js',
  ts: 'js',
  tsx: 'js',
  mts: 'js',
  cts: 'js',
  svelte: 'html',
  rs: 'rust',
  py: 'python',
  pyw: 'python',
  css: 'css',
  scss: 'css',
  html: 'html',
  htm: 'html',
  xml: 'html',
  svg: 'html',
  xaml: 'html',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  ps1: 'shell',
  psm1: 'shell',
  toml: 'config',
  yaml: 'config',
  yml: 'config',
  ini: 'config',
  cfg: 'config',
  conf: 'config',
  properties: 'config',
};

export function detectLang(fileName: string, kind: 'text' | 'json' | 'markdown'): HighlightLang {
  if (kind === 'json') return 'json';
  if (kind === 'markdown') return 'markdown';
  const name = (fileName ?? '').toLowerCase();
  const dot = name.lastIndexOf('.');
  if (dot < 0) return 'plain';
  return EXT[name.slice(dot + 1)] ?? 'plain';
}

interface LangCfg {
  line: string[];
  block?: [string, string];
  quotes: string;
  keywords: Set<string>;
  bools: Set<string>;
  identDash?: boolean;
  config?: boolean;
  rustChars?: boolean;
  tripleQuotes?: boolean;
  cssColors?: boolean;
}

const set = (s: string) => new Set(s.split(' '));
const JS_KW = set(
  'break case catch class const continue debugger default delete do else enum export extends finally for function if import in instanceof let new of return static super switch this throw try typeof var void while with yield async await as from type interface implements public private protected readonly declare namespace abstract',
);
const RS_KW = set(
  'as break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while async await',
);
const PY_KW = set(
  'and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield self',
);
const SH_KW = set(
  'if then else elif fi for while until do done case esac in function return exit local export echo set unset param foreach switch break continue try catch finally',
);
const CSS_KW = set('important media import charset keyframes font-face supports');

const CFG: Partial<Record<HighlightLang, LangCfg>> = {
  js: {
    line: ['//'],
    block: ['/*', '*/'],
    quotes: '"\'`',
    keywords: JS_KW,
    bools: set('true false null undefined NaN'),
  },
  rust: {
    line: ['//'],
    block: ['/*', '*/'],
    quotes: '"',
    keywords: RS_KW,
    bools: set('true false'),
    rustChars: true,
  },
  python: {
    line: ['#'],
    quotes: '"\'',
    keywords: PY_KW,
    bools: set('True False None'),
    tripleQuotes: true,
  },
  css: {
    line: [],
    block: ['/*', '*/'],
    quotes: '"\'',
    keywords: CSS_KW,
    bools: new Set(),
    identDash: true,
    cssColors: true,
  },
  html: {
    line: [],
    block: ['<!--', '-->'],
    quotes: '',
    keywords: new Set(),
    bools: new Set(),
    identDash: true,
  },
  shell: {
    line: ['#'],
    block: ['<#', '#>'],
    quotes: '"\'',
    keywords: SH_KW,
    bools: set('true false'),
    identDash: true,
  },
  config: {
    line: ['#', ';'],
    quotes: '"\'',
    keywords: new Set(),
    bools: set('true false yes no on off null True False'),
    config: true,
  },
  json: {
    line: ['//'],
    block: ['/*', '*/'],
    quotes: '"',
    keywords: new Set(),
    bools: set('true false null'),
  },
};

const PUNCT = new Set('{}[]()<>,;:=+-*/%&|!?.~^@\\'.split(''));

const isDigit = (c: string) => c >= '0' && c <= '9';
const isIdStart = (c: string) =>
  (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c === '_' || c === '$';
const isIdPart = (c: string) => isIdStart(c) || isDigit(c);
const isWs = (c: string) => c === ' ' || c === '\t' || c === '\n' || c === '\r' || c === '\f';

class Out {
  tokens: Token[] = [];
  private lastType: TokenType | null = null;
  constructor(private src: string) {}
  push(type: TokenType, start: number, end: number) {
    if (end <= start) return;
    const text = this.src.slice(start, end);
    if (this.lastType === type && type !== 'key' && type !== 'heading') {
      this.tokens[this.tokens.length - 1].text += text;
    } else {
      this.tokens.push({ type, text });
      this.lastType = type;
    }
  }
}

function eolFrom(text: string, i: number): number {
  const n = text.length;
  let j = i;
  while (j < n && text[j] !== '\n') j++;
  return j;
}

function tokenizeGeneric(text: string, cfg: LangCfg, lang: HighlightLang): Token[] {
  const n = text.length;
  const out = new Out(text);
  const isJson = lang === 'json';
  const isHtml = lang === 'html';
  let i = 0;
  let lineStart = true;

  while (i < n) {
    const c = text[i];

    if (isWs(c)) {
      let j = i + 1;
      let nl = c === '\n';
      while (j < n && isWs(text[j])) {
        if (text[j] === '\n') nl = true;
        j++;
      }
      out.push('plain', i, j);
      if (nl) lineStart = true;
      i = j;
      continue;
    }
    const atLineStart = lineStart;
    lineStart = false;

    // comments
    if (cfg.block && text.startsWith(cfg.block[0], i)) {
      const k = text.indexOf(cfg.block[1], i + cfg.block[0].length);
      const j = k < 0 ? n : k + cfg.block[1].length;
      out.push('comment', i, j);
      i = j;
      continue;
    }
    let handled = false;
    for (const p of cfg.line) {
      if (text.startsWith(p, i)) {
        if (cfg.cssColors && p === '#') break;
        const j = eolFrom(text, i);
        out.push('comment', i, j);
        i = j;
        handled = true;
        break;
      }
    }
    if (handled) continue;

    // html tags
    if (isHtml && c === '<') {
      const nx = text[i + 1] ?? '';
      if (isIdStart(nx) || nx === '/' || nx === '!' || nx === '?') {
        let j = i + 1;
        if (nx === '/' || nx === '!' || nx === '?') j++;
        out.push('punct', i, j);
        let s = j;
        while (
          j < n &&
          (isIdPart(text[j]) || text[j] === '-' || text[j] === ':' || text[j] === '.')
        )
          j++;
        out.push('keyword', s, j);
        while (j < n) {
          const d = text[j];
          if (d === '>') {
            out.push('punct', j, j + 1);
            j++;
            break;
          }
          if (d === '"' || d === "'") {
            const k = text.indexOf(d, j + 1);
            const e = k < 0 ? n : k + 1;
            out.push('string', j, e);
            j = e;
          } else if (isWs(d)) {
            s = j;
            while (j < n && isWs(text[j])) j++;
            out.push('plain', s, j);
          } else if (d === '=' || d === '/' || d === '?') {
            out.push('punct', j, j + 1);
            j++;
          } else if (d === '<') {
            break; // malformed tag; let the outer loop handle it
          } else {
            s = j;
            while (j < n && !isWs(text[j]) && !'>="\'/<'.includes(text[j])) j++;
            if (j === s) j++;
            out.push('key', s, j);
          }
        }
        i = j;
        continue;
      }
    }

    // config sections / keys
    if (cfg.config && atLineStart) {
      if (c === '[') {
        const j = eolFrom(text, i);
        let e = j;
        if (text[e - 1] === '\r') e--;
        out.push('heading', i, e);
        i = e;
        continue;
      }
      const eol = eolFrom(text, i);
      const cap = Math.min(eol, i + LOOKAHEAD_CAP);
      let j = i;
      while (j < cap && text[j] !== '=' && text[j] !== ':' && text[j] !== '#') j++;
      if (j < cap && (text[j] === '=' || text[j] === ':')) {
        let e = j;
        while (e > i && isWs(text[e - 1])) e--;
        if (e > i) {
          out.push('key', i, e);
          i = e;
          continue;
        }
      }
    }

    // strings
    if (cfg.quotes.includes(c)) {
      let j = i + 1;
      if (cfg.tripleQuotes && text.startsWith(c + c + c, i)) {
        const k = text.indexOf(c + c + c, i + 3);
        j = k < 0 ? n : k + 3;
      } else if (c === '`') {
        while (j < n && text[j] !== '`') j += text[j] === '\\' ? 2 : 1;
        j = Math.min(n, j + 1);
      } else {
        while (j < n && text[j] !== c && text[j] !== '\n') {
          if (text[j] === '\\' && j + 1 < n && text[j + 1] !== '\n') j += 2;
          else j++;
        }
        if (j < n && text[j] === c) j++;
        j = Math.min(j, n);
        // keep a trailing CR of an unterminated string out of the token
        if (j > i + 1 && text[j - 1] === '\r') j--;
      }
      if (isJson) {
        let k = j;
        while (k < n && (text[k] === ' ' || text[k] === '\t')) k++;
        out.push(text[k] === ':' ? 'key' : 'string', i, j);
      } else {
        out.push('string', i, j);
      }
      i = j;
      continue;
    }
    if (cfg.rustChars && c === "'") {
      // char literal 'x' / '\n'; otherwise a lifetime or label
      let len = 0;
      if (text[i + 1] === '\\') {
        const k = text.indexOf("'", i + 3);
        if (k > 0 && k - i <= 12) len = k - i + 1;
      } else if (text[i + 2] === "'" && text[i + 1] !== undefined) {
        len = 3;
      }
      if (len) {
        out.push('string', i, i + len);
        i += len;
        continue;
      }
      let j = i + 1;
      while (j < n && isIdPart(text[j])) j++;
      out.push('keyword', i, j);
      i = j;
      continue;
    }

    // numbers
    const negNum = isJson && c === '-' && isDigit(text[i + 1] ?? '');
    if (isDigit(c) || negNum) {
      let j = i + 1;
      while (j < n) {
        const d = text[j];
        if (isIdPart(d) || d === '.') {
          j++;
        } else if (
          (d === '+' || d === '-') &&
          (text[j - 1] === 'e' || text[j - 1] === 'E') &&
          isDigit(text[j + 1] ?? '')
        ) {
          j++;
        } else break;
      }
      out.push('number', i, j);
      i = j;
      continue;
    }
    if (cfg.cssColors && c === '#') {
      let j = i + 1;
      while (j < n && isIdPart(text[j])) j++;
      out.push(j > i + 1 ? 'number' : 'punct', i, j);
      i = j;
      continue;
    }

    // identifiers
    if (isIdStart(c)) {
      let j = i + 1;
      while (j < n && (isIdPart(text[j]) || (cfg.identDash && text[j] === '-'))) j++;
      const w = text.slice(i, j);
      if (cfg.bools.has(w)) out.push('bool', i, j);
      else if (cfg.keywords.has(w)) out.push('keyword', i, j);
      else out.push('plain', i, j);
      i = j;
      continue;
    }

    out.push(PUNCT.has(c) ? 'punct' : 'plain', i, i + 1);
    i++;
  }
  return out.tokens;
}

function tokenizeMarkdown(text: string): Token[] {
  const n = text.length;
  const out = new Out(text);
  let i = 0;
  let inFence = false;

  while (i < n) {
    const eol = eolFrom(text, i);
    const next = Math.min(n, eol + 1);
    let end = eol;
    if (end > i && text[end - 1] === '\r') end--;
    // line is [i, end), terminator is [end, next)

    let p = i;
    while (p < end && p < i + 3 && text[p] === ' ') p++;
    const isFence = text.startsWith('```', p) || text.startsWith('~~~', p);

    if (isFence) {
      out.push('comment', i, end);
      inFence = !inFence;
    } else if (inFence) {
      out.push('string', i, end);
    } else if (text[p] === '#') {
      let h = p;
      while (h < end && text[h] === '#') h++;
      if (h - p <= 6 && (h === end || text[h] === ' ' || text[h] === '\t')) {
        out.push('heading', i, end);
      } else {
        inlineMd(text, i, end, out);
      }
    } else {
      let q = i;
      while (q < end && (text[q] === ' ' || text[q] === '\t')) q++;
      const m = text[q];
      if (m === '>' || ((m === '-' || m === '*' || m === '+') && text[q + 1] === ' ')) {
        out.push('plain', i, q);
        out.push('punct', q, q + 1);
        inlineMd(text, q + 1, end, out);
      } else {
        inlineMd(text, i, end, out);
      }
    }
    out.push('plain', end, next);
    i = next;
  }
  return out.tokens;
}

function inlineMd(text: string, from: number, end: number, out: Out) {
  let i = from;
  let bracketFailed = false;
  let plainStart = i;
  const flush = (upto: number) => out.push('plain', plainStart, upto);
  while (i < end) {
    const c = text[i];
    if (c === '`') {
      const cap = Math.min(end, i + 1 + LOOKAHEAD_CAP);
      let j = i + 1;
      while (j < cap && text[j] !== '`') j++;
      if (j < cap) {
        flush(i);
        out.push('string', i, j + 1);
        i = j + 1;
        plainStart = i;
        continue;
      }
    } else if (c === '[' && !bracketFailed) {
      const cap = Math.min(end, i + LOOKAHEAD_CAP);
      let j = i + 1;
      while (j < cap && text[j] !== ']') j++;
      if (j < cap && text[j + 1] === '(') {
        let k = j + 2;
        while (k < cap && text[k] !== ')') k++;
        if (k < cap) {
          flush(i);
          out.push('link', i, k + 1);
          i = k + 1;
          plainStart = i;
          continue;
        }
      }
      if (j >= cap) bracketFailed = true;
    } else if ((c === '*' || c === '_') && text[i + 1] === c) {
      flush(i);
      out.push('punct', i, i + 2);
      i += 2;
      plainStart = i;
      continue;
    }
    i++;
  }
  flush(end);
}

function plainFallback(text: string): Token[] {
  return text.length ? [{ type: 'plain', text }] : [];
}

function tooBig(text: string): boolean {
  if (text.length > MAX_HIGHLIGHT_CHARS) return true;
  let lines = 1;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10 && ++lines > MAX_HIGHLIGHT_LINES) return true;
  }
  return false;
}

export function tokenize(text: string, lang: HighlightLang): Token[] {
  try {
    if (typeof text !== 'string') return [];
    if (lang === 'plain' || tooBig(text)) return plainFallback(text);
    let tokens: Token[];
    if (lang === 'markdown') {
      tokens = tokenizeMarkdown(text);
    } else {
      const cfg = CFG[lang];
      if (!cfg) return plainFallback(text);
      tokens = tokenizeGeneric(text, cfg, lang);
    }
    // defensive lossless guard
    let len = 0;
    for (const t of tokens) len += t.text.length;
    if (len !== text.length) return plainFallback(text);
    return tokens;
  } catch {
    return plainFallback(text);
  }
}
