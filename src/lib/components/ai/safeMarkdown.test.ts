import { describe, expect, it } from 'vitest';
import { parseInline, parseMarkdown } from './safeMarkdown';

describe('safeMarkdown', () => {
  it('parses inline emphasis, code and plain text', () => {
    expect(parseInline('a **b** *c* `d` e')).toEqual([
      { t: 'text', v: 'a ' },
      { t: 'strong', v: 'b' },
      { t: 'text', v: ' ' },
      { t: 'em', v: 'c' },
      { t: 'text', v: ' ' },
      { t: 'code', v: 'd' },
      { t: 'text', v: ' e' },
    ]);
  });

  it('keeps raw HTML as literal text', () => {
    const { blocks } = parseMarkdown('<img src=x onerror=alert(1)>');
    expect(blocks).toEqual([
      { t: 'p', inline: [{ t: 'text', v: '<img src=x onerror=alert(1)>' }] },
    ]);
  });

  it('parses paragraphs, lists and fenced code', () => {
    const { blocks } = parseMarkdown('hi\n\n- a\n- b\n\n1. x\n\n```\n**no**\n```');
    expect(blocks.map((b) => b.t)).toEqual(['p', 'ul', 'ol', 'pre']);
    expect(blocks[3]).toEqual({ t: 'pre', v: '**no**' });
  });

  it('closes an unterminated fence and bounds huge input', () => {
    expect(parseMarkdown('```\nx').blocks).toEqual([{ t: 'pre', v: 'x' }]);
    const big = parseMarkdown('a\n\n'.repeat(50_000));
    expect(big.truncated).toBe(true);
    expect(big.blocks.length).toBeLessThanOrEqual(200);
  });
});
