import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import HighlightedText from './HighlightedText.svelte';

afterEach(cleanup);

describe('HighlightedText', () => {
  it('enabled=false renders plain pre.text without spans', () => {
    const { container } = render(HighlightedText, {
      text: 'const a = 1;',
      fileName: 'a.ts',
      kind: 'text',
      enabled: false,
    });
    const pre = container.querySelector('pre.text')!;
    expect(pre.textContent).toBe('const a = 1;');
    expect(pre.querySelector('span')).toBeNull();
  });

  it('enabled=true renders token spans with identical text', () => {
    const text = 'const a = "x"; // c\n{"k": 1}';
    const { container } = render(HighlightedText, {
      text,
      fileName: 'a.ts',
      kind: 'text',
      enabled: true,
    });
    const pre = container.querySelector('pre')!;
    expect(pre.textContent).toBe(text);
    expect(pre.querySelector('.tok-keyword')?.textContent).toBe('const');
    expect(pre.querySelector('.tok-string')).not.toBeNull();
    expect(pre.querySelector('.tok-comment')).not.toBeNull();
  });

  it('never creates script/img elements from injected markup', () => {
    const text = '<script>alert(1)</script><img src=x onerror=alert(1)>';
    for (const fileName of ['a.html', 'a.js', 'a.txt']) {
      for (const enabled of [true, false]) {
        const { container, unmount } = render(HighlightedText, {
          text,
          fileName,
          kind: 'text',
          enabled,
        });
        expect(container.querySelector('script')).toBeNull();
        expect(container.querySelector('img')).toBeNull();
        expect(container.querySelector('pre')!.textContent).toBe(text);
        unmount();
      }
    }
  });
});
