// Parser Markdown minimal -> pohon token. Dirender oleh MarkdownText.svelte lewat elemen
// Svelte (teks selalu di-escape); tidak ada HTML mentah, tidak ada link/gambar aktif.
export type Inline =
  | { t: 'text'; v: string }
  | { t: 'strong'; v: string }
  | { t: 'em'; v: string }
  | { t: 'code'; v: string };
export type Block =
  { t: 'p'; inline: Inline[] } | { t: 'ul' | 'ol'; items: Inline[][] } | { t: 'pre'; v: string };

// Batas agar masukan sangat besar tidak membekukan UI.
const MAX_CHARS = 20_000;
const MAX_BLOCKS = 200;

const INLINE = /(\*\*[^*\n]+\*\*|\*[^*\n]+\*|`[^`\n]+`)/g;

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  for (const part of src.split(INLINE)) {
    if (!part) continue;
    if (part.length > 4 && part.startsWith('**') && part.endsWith('**'))
      out.push({ t: 'strong', v: part.slice(2, -2) });
    else if (part.length > 2 && part.startsWith('*') && part.endsWith('*'))
      out.push({ t: 'em', v: part.slice(1, -1) });
    else if (part.length > 2 && part.startsWith('`') && part.endsWith('`'))
      out.push({ t: 'code', v: part.slice(1, -1) });
    else out.push({ t: 'text', v: part });
  }
  return out;
}

export function parseMarkdown(input: string): { blocks: Block[]; truncated: boolean } {
  const truncated = input.length > MAX_CHARS;
  const lines = (truncated ? input.slice(0, MAX_CHARS) : input).replace(/\r\n?/g, '\n').split('\n');
  const blocks: Block[] = [];
  let para: string[] = [];
  let list: { t: 'ul' | 'ol'; items: Inline[][] } | null = null;
  let fence: string[] | null = null;
  const flushPara = () => {
    if (para.length) blocks.push({ t: 'p', inline: parseInline(para.join('\n')) });
    para = [];
  };
  const flushList = () => {
    if (list) blocks.push(list);
    list = null;
  };
  for (const line of lines) {
    if (blocks.length >= MAX_BLOCKS) break;
    if (fence) {
      if (line.trimStart().startsWith('```')) {
        blocks.push({ t: 'pre', v: fence.join('\n') });
        fence = null;
      } else fence.push(line);
      continue;
    }
    if (line.trimStart().startsWith('```')) {
      flushPara();
      flushList();
      fence = [];
      continue;
    }
    const li = /^\s*(?:([-*])|(\d+)\.)\s+(.*)$/.exec(line);
    if (li) {
      flushPara();
      const kind = li[1] ? 'ul' : 'ol';
      if (list && list.t !== kind) flushList();
      list ??= { t: kind, items: [] };
      list.items.push(parseInline(li[3]));
      continue;
    }
    if (!line.trim()) {
      flushPara();
      flushList();
      continue;
    }
    flushList();
    para.push(line);
  }
  if (fence) blocks.push({ t: 'pre', v: fence.join('\n') });
  flushPara();
  flushList();
  return { blocks, truncated };
}
