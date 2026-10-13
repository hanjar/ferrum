/**
 * Typo-tolerant matcher shared by the Command Palette providers (commands, hotlist; FER-485).
 *
 * Behaviour mirrors the Activity location matcher in the backend (exactName / prefix / token /
 * initialism / fuzzy) so the palette feels the same for every kind of result. Pure and
 * allocation-light: normalised strings are cached, and the edit distance reuses module buffers.
 *
 * Per query word, best class wins (higher is better):
 *   exact(1000) > label prefix(800) > word start(650..700) > initialism(600)
 *   > mid-word substring(500) > subsequence(350) > typo(250 - 50 per edit).
 * Typo budget: words of 4-7 letters allow 1 edit, 8+ letters allow 2, shorter words none.
 * Every query word must match; the sum of the word scores ranks the entry. Ties keep input order.
 */

export interface MatchField {
  text: string;
  /** Multiplier on the class score. Default 1. */
  weight?: number;
  /** Allow initialism / subsequence / typo classes. Off for long paths, where they only add noise. */
  fuzzy?: boolean;
}

interface Prep {
  text: string;
  words: string[];
  initials: string;
}

const NON_WORD = /[^\p{L}\p{N}]+/u;
const cache = new Map<string, Prep>();
const CACHE_MAX = 4000;

function prep(raw: string): Prep {
  let p = cache.get(raw);
  if (p) return p;
  const text = raw.toLowerCase();
  const words = text.split(NON_WORD).filter(Boolean);
  p = { text, words, initials: words.map((w) => w[0]).join('') };
  if (cache.size >= CACHE_MAX) cache.clear();
  cache.set(raw, p);
  return p;
}

// Optimal-string-alignment distance (Damerau-Levenshtein with adjacent transposition), bounded.
const MAXLEN = 40;
const W = MAXLEN + 1;
const rowA = new Uint8Array(W);
const rowB = new Uint8Array(W);
const rowC = new Uint8Array(W);

/** Returns the distance, or `max + 1` when it is certainly greater than `max`. */
export function boundedDamerauLevenshtein(a: string, b: string, max: number): number {
  const n = a.length;
  const m = b.length;
  if (Math.abs(n - m) > max) return max + 1;
  if (n > MAXLEN || m > MAXLEN) return max + 1;
  let prev2 = rowA;
  let prev = rowB;
  let cur = rowC;
  for (let j = 0; j <= m; j++) prev[j] = j;
  for (let i = 1; i <= n; i++) {
    cur[0] = i;
    let rowMin = i;
    const ai = a.charCodeAt(i - 1);
    for (let j = 1; j <= m; j++) {
      const bj = b.charCodeAt(j - 1);
      const cost = ai === bj ? 0 : 1;
      let v = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
      if (i > 1 && j > 1 && ai === b.charCodeAt(j - 2) && a.charCodeAt(i - 2) === bj) {
        v = Math.min(v, prev2[j - 2] + 1);
      }
      cur[j] = v;
      if (v < rowMin) rowMin = v;
    }
    if (rowMin > max) return max + 1;
    const t = prev2;
    prev2 = prev;
    prev = cur;
    cur = t;
  }
  return prev[m];
}

/** Edits allowed for a query word of this length (spec: <4 none, 4-7 one, 8+ two). */
export function typoBudget(len: number): number {
  return len >= 8 ? 2 : len >= 4 ? 1 : 0;
}

/** Characters of `w` in order inside `t`, starting at a word start, within a compact span. */
function subsequence(w: string, t: string): boolean {
  const maxSpan = w.length * 3;
  for (let s = 0; s < t.length; s++) {
    if (t[s] !== w[0]) continue;
    if (s > 0 && /[\p{L}\p{N}]/u.test(t[s - 1])) continue; // not a word start
    let k = 1;
    let last = s;
    for (let i = s + 1; i < t.length && k < w.length; i++) {
      if (t[i] === w[k]) {
        k++;
        last = i;
      }
    }
    if (k === w.length && last - s + 1 <= maxSpan) return true;
  }
  return false;
}

function wordScore(w: string, p: Prep, fuzzy: boolean): number {
  const t = p.text;
  if (t === w) return 1000;
  if (t.startsWith(w)) return 800;
  let best = 0;
  for (const word of p.words) {
    if (word === w) return 700;
    if (word.startsWith(w)) best = 650;
  }
  if (best) return best;
  // Initialism outranks a hit in the middle of a word: "nf" is "New folder", not "Configure".
  if (fuzzy && w.length >= 2 && p.initials.startsWith(w)) return 600;
  if (t.includes(w)) return 500;
  if (!fuzzy) return 0;
  if (w.length >= 3 && subsequence(w, t)) return 350;
  const max = typoBudget(w.length);
  if (max > 0) {
    let d = max + 1;
    for (const word of p.words) {
      if (word.length < 3) continue;
      let x = boundedDamerauLevenshtein(w, word, max);
      // A word typed halfway with a slip ("invoce" for "invoice-2024") still counts.
      if (x > 0 && word.length > w.length) {
        x = Math.min(x, boundedDamerauLevenshtein(w, word.slice(0, w.length), max));
      }
      if (x < d) d = x;
    }
    if (d <= max) return 250 - 50 * d;
  }
  return 0;
}

export interface ParsedQuery {
  /** Lower-cased, trimmed whole query. */
  q: string;
  words: string[];
}

export function parseQuery(query: string): ParsedQuery {
  const q = query.trim().toLowerCase();
  return { q, words: q.split(/\s+/).filter(Boolean) };
}

/** Score of one entry, or -1 when some query word matches none of its fields. */
export function scoreFields(pq: ParsedQuery, fields: readonly MatchField[]): number {
  let total = 0;
  for (const w of pq.words) {
    let best = 0;
    for (const f of fields) {
      const s = wordScore(w, prep(f.text), f.fuzzy !== false) * (f.weight ?? 1);
      if (s > best) best = s;
    }
    if (best === 0) return -1;
    total += best;
  }
  if (pq.words.length > 1) {
    let phrase = 0;
    for (const f of fields) {
      const t = prep(f.text).text;
      const s = t === pq.q ? 400 : t.startsWith(pq.q) ? 200 : t.includes(pq.q) ? 80 : 0;
      if (s > phrase) phrase = s;
    }
    total += phrase;
  }
  // Prefer the shorter primary label among equals; at most ~12 points, below any class gap.
  if (fields.length > 0) total -= Math.min(fields[0].text.length, 60) * 0.2;
  return total;
}

/**
 * Filter and order `items` for `query`. Empty query returns the input untouched. Equal scores keep
 * the input (registry) order.
 */
export function rankMatches<T>(
  items: readonly T[],
  query: string,
  fieldsOf: (item: T) => readonly MatchField[],
): T[] {
  const pq = parseQuery(query);
  if (pq.words.length === 0) return items as T[];
  const hits: { item: T; score: number; i: number }[] = [];
  items.forEach((item, i) => {
    const score = scoreFields(pq, fieldsOf(item));
    if (score >= 0) hits.push({ item, score, i });
  });
  hits.sort((a, b) => b.score - a.score || a.i - b.i);
  return hits.map((h) => h.item);
}
