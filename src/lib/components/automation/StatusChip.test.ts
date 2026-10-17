import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { render } from '@testing-library/svelte';
import StatusChip from './StatusChip.svelte';
import { STATUS_KINDS, STATUS_TONE } from './automationModel';

describe('StatusChip', () => {
  for (const theme of ['light', 'dark'] as const) {
    it(`renders every kind with an icon, its label and a tint in the ${theme} theme`, () => {
      document.documentElement.setAttribute('data-theme', theme);
      const icons = new Set<string>();
      for (const kind of STATUS_KINDS) {
        const { container, unmount } = render(StatusChip, {
          status: { kind, label: `Label ${kind}` },
        });
        const chip = container.querySelector('.chip') as HTMLElement;
        expect(chip.dataset.kind).toBe(kind);
        expect(chip.dataset.tone).toBe(STATUS_TONE[kind]);
        expect(chip.className).toContain(`tone-${STATUS_TONE[kind]}`);
        expect(chip.textContent?.trim()).toBe(`Label ${kind}`);
        const svg = chip.querySelector('svg') as SVGElement;
        expect(svg).not.toBeNull();
        expect(svg.getAttribute('aria-hidden')).toBe('true');
        expect(svg.children.length).toBeGreaterThan(0);
        icons.add(svg.innerHTML);
        unmount();
      }
      // Colour is never the only signal: all 18 glyphs are different.
      expect(icons.size).toBe(STATUS_KINDS.length);
      document.documentElement.removeAttribute('data-theme');
    });
  }

  it('draws the sub-line only when asked to', () => {
    const status = { kind: 'running' as const, label: 'Running 3/6', sub: 'Syncing' };
    const a = render(StatusChip, { status });
    expect(a.queryByText('Syncing')).toBeNull();
    a.unmount();
    const b = render(StatusChip, { status, showSub: true });
    expect(b.getByText('Syncing')).toBeTruthy();
  });

  it('takes its tints from theme tokens only (they follow light, dark and every palette)', () => {
    const src = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), 'StatusChip.svelte'),
      'utf-8',
    );
    const css = src.slice(src.indexOf('<style>'));
    expect(css).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(css).not.toMatch(/\brgba?\(/);
    for (const token of ['--greenSoft', '--accentSoft', '--amberSoft', '--redSoft', '--hover2']) {
      expect(css).toContain(token);
    }
  });
});
