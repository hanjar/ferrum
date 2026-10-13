import { describe, expect, it, beforeEach } from 'vitest';
import { render } from '@testing-library/svelte';
import FileIcon from './FileIcon.svelte';
import { ui } from '../uiState.svelte';

describe('FileIcon (FER-478)', () => {
  beforeEach(() => {
    ui.iconTheme = 'Duotone';
    ui.iconStyle = 'colored';
  });

  it('draws one 14x16 svg with at most 4 children and no label in list size', () => {
    ui.iconTheme = 'Label';
    const { container } = render(FileIcon, { kind: 'code', name: 'a.rs' });
    const svg = container.querySelector('svg')!;
    expect(svg.getAttribute('viewBox')).toBe('0 0 14 16');
    expect(svg.getAttribute('width')).toBe('14');
    expect(svg.children.length).toBeLessThanOrEqual(4);
    expect(container.querySelector('.lbl')).toBeNull();
  });

  it('shows the extension label only at grid and preview sizes', () => {
    ui.iconTheme = 'Label';
    const g = render(FileIcon, { kind: 'docs', name: 'a.pdf', variant: 'grid' });
    expect(g.container.querySelector('.lbl')?.textContent).toBe('PDF');
    expect(g.container.querySelector('svg')!.getAttribute('height')).toBe('36');
    const p = render(FileIcon, { kind: 'docs', name: 'a.markdown', variant: 'preview' });
    expect(p.container.querySelector('.lbl')?.textContent).toBe('MARK');
    expect(p.container.querySelector('svg')!.getAttribute('width')).toBe('44');
  });

  it('keeps folders unchanged in every theme', () => {
    ui.iconTheme = 'Tile';
    const { container } = render(FileIcon, { kind: 'dir' });
    expect(container.querySelector('svg')!.getAttribute('viewBox')).toBe('0 0 16 16');
  });
});
