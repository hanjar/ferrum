/** FER-593: Thumbnail View size levels (per pane), their actions and old saved state. */
import { beforeEach, describe, expect, it } from 'vitest';
import { viewState, parseThumbSize, THUMB_SIZES } from './viewStateStore.svelte';
import { parseViewPresets } from './viewPresetsStore.svelte';
import { getAction, labelOf, runAction, searchActions } from '../actions';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import { ui } from '../uiState.svelte';

function fakePane(): PaneApi {
  return {
    path: 'C:\\dir',
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}

beforeEach(() => {
  registerPane(0, fakePane());
  registerPane(1, fakePane());
  ui.activePane = 0;
  viewState.setThumbSize(0, 'medium');
  viewState.setThumbSize(1, 'medium');
});

describe('thumbnail size state', () => {
  it('defaults to Medium and is independent per pane', () => {
    expect(viewState.thumbSize(0)).toBe('medium');
    viewState.setThumbSize(1, 'large');
    expect(viewState.thumbSize(0)).toBe('medium');
    expect(viewState.thumbSize(1)).toBe('large');
  });

  it('steps one level at a time and clamps at both ends', () => {
    expect(viewState.stepThumbSize(0, 1)).toBe('large');
    expect(viewState.stepThumbSize(0, 1)).toBe('xlarge');
    expect(viewState.stepThumbSize(0, 1)).toBe('xlarge');
    expect(viewState.stepThumbSize(0, -1)).toBe('large');
    for (let i = 0; i < 6; i++) viewState.stepThumbSize(0, -1);
    expect(viewState.thumbSize(0)).toBe('small');
  });

  it('an unknown or missing stored value is Medium', () => {
    expect(parseThumbSize(undefined)).toBe('medium');
    expect(parseThumbSize('huge')).toBe('medium');
    for (const s of THUMB_SIZES) expect(parseThumbSize(s)).toBe(s);
  });

  it('a saved view preset without the field loads as Medium; with it keeps the level', () => {
    const old = {
      name: 'Old',
      viewMode: 'thumbnail',
      sort: { key: 'name', dir: 1 },
      reversed: false,
      filterMode: 'all',
      filterPattern: '',
    };
    const [a, b] = parseViewPresets(
      JSON.stringify([old, { ...old, name: 'New', thumbSize: 'xlarge' }]),
    );
    expect(a.thumbSize).toBe('medium');
    expect(b.thumbSize).toBe('xlarge');
  });
});

describe('thumbnail size actions', () => {
  it('"Thumbnail size: X" sets the level of the active pane and marks the current one', () => {
    ui.activePane = 1;
    runAction(getAction('show.thumbSize.small')!);
    expect(viewState.thumbSize(1)).toBe('small');
    expect(viewState.thumbSize(0)).toBe('medium');
    expect(labelOf(getAction('show.thumbSize.small')!)).toContain('\u2713');
    expect(labelOf(getAction('show.thumbSize.xlarge')!)).toBe('Thumbnail size: Extra large');
  });

  it('Larger / Smaller thumbnails step and clamp', () => {
    runAction(getAction('show.thumbLarger')!);
    expect(viewState.thumbSize(0)).toBe('large');
    runAction(getAction('show.thumbLarger')!);
    runAction(getAction('show.thumbLarger')!);
    expect(viewState.thumbSize(0)).toBe('xlarge');
    for (let i = 0; i < 5; i++) runAction(getAction('show.thumbSmaller')!);
    expect(viewState.thumbSize(0)).toBe('small');
  });

  it('is found in the command palette', () => {
    expect(searchActions('larger thumbnails').map((a) => a.id)).toContain('show.thumbLarger');
  });
});
