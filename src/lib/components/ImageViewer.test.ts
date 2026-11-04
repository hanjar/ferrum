/**
 * FER-593: F3 image viewer. IPC is mocked at the previewImage / media boundary; a fake pane gives
 * the cursor row and the listing. App is rendered for the keyboard-routing tests.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import App from '../../App.svelte';
import { imageViewer } from '../stores/imageViewerStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { getAction, runAction } from '../actions';
import { resetModalDepth } from '../dialogKit';
import { dialogs } from '../stores/dialogStore.svelte';
import { previewStore } from '../stores/previewStore.svelte';
import { ui } from '../uiState.svelte';

const readImagePreview = vi.fn();
vi.mock('../ipc/previewImage', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  readImagePreview: (...a: unknown[]) => readImagePreview(...a),
}));
const mediaRevoke = vi.fn<(scope: string) => Promise<void>>(async () => {});
vi.mock('../ipc/media', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  mediaRevoke: (scope: string) => mediaRevoke(scope),
}));

const LIST = [
  { name: 'docs', path: 'C:\\d\\docs', isDir: true, size: 0, modifiedMs: 1 },
  { name: 'a.png', path: 'C:\\d\\a.png', isDir: false, size: 1, modifiedMs: 1 },
  { name: 'notes.txt', path: 'C:\\d\\notes.txt', isDir: false, size: 1, modifiedMs: 1 },
  { name: 'b.svg', path: 'C:\\d\\b.svg', isDir: false, size: 1, modifiedMs: 1 },
  { name: 'c.JPG', path: 'C:\\d\\c.JPG', isDir: false, size: 1, modifiedMs: 1 },
];
/** An image inside an archive (a listing of its own; only reachable as the cursor entry here). */
const IN_ARCHIVE = {
  name: 'm.png',
  path: 'C:\\d\\x.zip\\m.png',
  isDir: false,
  size: 1,
  modifiedMs: 1,
};

let cursorPath: string | undefined;
const setCursor = vi.fn((p: string) => {
  cursorPath = p;
});
const focusList = vi.fn();

function fakePane(): PaneApi {
  return {
    path: 'C:\\d',
    selectedPaths: () => ['C:\\d\\a.png'],
    cursorEntry: () => [...LIST, IN_ARCHIVE].find((e) => e.path === cursorPath),
    visibleEntries: () => LIST,
    setCursor,
    focusList,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}

function preview(path: string) {
  return path.endsWith('.svg')
    ? {
        downsampled: false,
        inline: true,
        mime: 'image/svg+xml',
        data: 'PHN2Zy8+',
        width: 0,
        height: 0,
        originalWidth: 0,
        originalHeight: 0,
      }
    : {
        downsampled: true,
        mime: 'image/png',
        data: 'iVBORw0KGgo=',
        width: 2048,
        height: 1024,
        originalWidth: 4000,
        originalHeight: 2000,
      };
}

function registerFakePanes() {
  registerPane(0, fakePane());
  registerPane(1, fakePane());
}

const settle = async () => {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
    await tick();
  }
};

const viewerEl = (c: HTMLElement) => c.querySelector('[aria-label="Image viewer"]') as HTMLElement;
const zoomLabel = (c: HTMLElement) =>
  (c.querySelector('[data-testid="zoom-label"]') as HTMLElement).textContent;
const key = (el: EventTarget, k: string, init: KeyboardEventInit = {}) =>
  el.dispatchEvent(
    new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init }),
  );

beforeEach(() => {
  readImagePreview.mockReset();
  readImagePreview.mockImplementation(async (p: string) => preview(p));
  mediaRevoke.mockClear();
  setCursor.mockClear();
  focusList.mockClear();
  cursorPath = undefined;
  registerFakePanes();
  ui.activePane = 0;
  ui.quickViewOpen = false;
  previewStore.reset();
  imageViewer.reset();
  resetModalDepth();
  dialogs.close();
});

afterEach(() => {
  cleanup();
  imageViewer.reset();
  resetModalDepth();
  ui.quickViewOpen = false;
  previewStore.reset();
});

async function openOn(path: string) {
  cursorPath = path;
  const r = render(App);
  await settle();
  registerFakePanes(); // App's own FilePanes registered themselves: replace them
  key(window, 'F3');
  await settle();
  return r;
}

describe('F3 opens the viewer', () => {
  it('on an image: shows the name, position among images only, and the image as a data URL', async () => {
    const { container } = await openOn('C:\\d\\b.svg');
    const v = viewerEl(container);
    expect(v).toBeTruthy();
    expect(v.textContent).toContain('b.svg');
    expect(v.textContent).toContain('2 / 3'); // a.png, b.svg, c.JPG (txt and folder skipped)
    expect(readImagePreview).toHaveBeenCalledWith('C:\\d\\b.svg');
    const img = v.querySelector('img') as HTMLImageElement;
    expect(img.getAttribute('src')).toBe('data:image/svg+xml;base64,PHN2Zy8+');
    expect(v.querySelector('object,embed,iframe')).toBeNull();
  });

  it('on a folder does nothing', async () => {
    const { container } = await openOn('C:\\d\\docs');
    expect(viewerEl(container)).toBeNull();
    expect(ui.quickViewOpen).toBe(false);
    expect(readImagePreview).not.toHaveBeenCalled();
  });

  it('on a non-image toggles Quick View instead', async () => {
    const { container } = await openOn('C:\\d\\notes.txt');
    expect(viewerEl(container)).toBeNull();
    expect(ui.quickViewOpen).toBe(true);
  });

  it('on an image inside an archive opens Quick View instead (the viewer reads disk only)', async () => {
    const { container } = await openOn('C:\\d\\x.zip\\m.png');
    expect(viewerEl(container)).toBeNull();
    expect(ui.quickViewOpen).toBe(true);
    expect(readImagePreview).not.toHaveBeenCalled();
  });

  it('shows the downsampled note for a big raster', async () => {
    const { container } = await openOn('C:\\d\\a.png');
    expect(viewerEl(container).textContent).toContain('downsampled from 4000×2000');
  });

  it('shows an error line when the image cannot be read, and Loading… meanwhile', async () => {
    let fail!: (e: Error) => void;
    readImagePreview.mockImplementation(
      () =>
        new Promise((_, rej) => {
          fail = rej;
        }),
    );
    const { container } = await openOn('C:\\d\\b.svg');
    expect(viewerEl(container).textContent).toContain('Loading…');
    fail(new Error('SVG file too large to preview'));
    await settle();
    expect(viewerEl(container).querySelector('[role="alert"]')?.textContent).toContain('too large');
    expect(viewerEl(container).querySelector('img')).toBeNull();
  });
});

describe('zoom', () => {
  it('steps from Fit through the fixed levels, clamps at 25 and 400, 0/1 keys', async () => {
    const { container } = await openOn('C:\\d\\a.png');
    const v = viewerEl(container);
    expect(zoomLabel(container)).toBe('Fit');
    key(v, '+');
    await settle();
    expect(zoomLabel(container)).toBe('150%'); // Fit is 100% here (no layout in jsdom)
    for (const expected of ['200%', '300%', '400%', '400%']) {
      key(v, '=');
      await settle();
      expect(zoomLabel(container)).toBe(expected);
    }
    key(v, '1');
    await settle();
    expect(zoomLabel(container)).toBe('100%');
    for (const expected of ['75%', '50%', '25%', '25%']) {
      key(v, '-');
      await settle();
      expect(zoomLabel(container)).toBe(expected);
    }
    key(v, '0');
    await settle();
    expect(zoomLabel(container)).toBe('Fit');
    key(v, '1');
    key(v, 'f');
    await settle();
    expect(zoomLabel(container)).toBe('Fit');
  });

  it('Ctrl+wheel and the toolbar buttons zoom; a new image starts at Fit', async () => {
    const { container, getByLabelText } = await openOn('C:\\d\\a.png');
    const stage = viewerEl(container).querySelector('.stage') as HTMLElement;
    const wheel = new WheelEvent('wheel', { deltaY: -100, ctrlKey: true, cancelable: true });
    stage.dispatchEvent(wheel);
    await settle();
    expect(wheel.defaultPrevented).toBe(true);
    expect(zoomLabel(container)).toBe('150%');
    await fireEvent.click(getByLabelText('Zoom out'));
    await fireEvent.click(getByLabelText('Zoom out'));
    expect(zoomLabel(container)).toBe('75%');
    key(viewerEl(container), 'ArrowRight');
    await settle();
    expect(zoomLabel(container)).toBe('Fit');
  });
});

describe('navigation', () => {
  it('Left/Right and PageUp/PageDown skip non-images and stop at the ends; Home/End jump', async () => {
    const { container } = await openOn('C:\\d\\a.png');
    const v = viewerEl(container);
    const pos = async () => {
      await tick();
      return viewerEl(container).querySelector('.pos')?.textContent;
    };
    expect(await pos()).toBe('1 / 3');
    key(v, 'ArrowLeft');
    expect(await pos()).toBe('1 / 3'); // no wrap-around
    key(v, 'ArrowRight');
    expect(imageViewer.path).toBe('C:\\d\\b.svg'); // notes.txt skipped
    key(v, 'PageDown');
    expect(imageViewer.path).toBe('C:\\d\\c.JPG');
    key(v, 'PageDown');
    expect(await pos()).toBe('3 / 3'); // no wrap-around
    key(v, 'Home');
    expect(imageViewer.path).toBe('C:\\d\\a.png');
    key(v, 'End');
    expect(imageViewer.path).toBe('C:\\d\\c.JPG');
    key(v, 'PageUp');
    expect(imageViewer.path).toBe('C:\\d\\b.svg');
  });

  it('revokes the scope of the image it leaves and of the one open on close', async () => {
    const { container } = await openOn('C:\\d\\a.png');
    await settle();
    expect(mediaRevoke).not.toHaveBeenCalled();
    key(viewerEl(container), 'ArrowRight');
    await settle();
    expect(mediaRevoke).toHaveBeenCalledTimes(1);
    expect(mediaRevoke.mock.calls[0][0]).toMatch(/^viewer:/);
    key(viewerEl(container), 'Escape');
    await settle();
    expect(mediaRevoke).toHaveBeenCalledTimes(2);
    expect(mediaRevoke.mock.calls[1][0]).toMatch(/^viewer:/);
    expect(mediaRevoke.mock.calls[1][0]).not.toBe(mediaRevoke.mock.calls[0][0]);
  });
});

describe('closing and keyboard ownership', () => {
  it('Esc, F3 and Enter close it and put the pane cursor on the image shown last', async () => {
    for (const k of ['Escape', 'F3', 'Enter']) {
      cleanup();
      imageViewer.reset();
      resetModalDepth();
      setCursor.mockClear();
      const { container } = await openOn('C:\\d\\a.png');
      key(viewerEl(container), 'ArrowRight');
      await settle();
      key(viewerEl(container), k);
      await settle();
      expect(viewerEl(container), k).toBeNull();
      expect(setCursor, k).toHaveBeenCalledWith('C:\\d\\b.svg');
      expect(focusList, k).toHaveBeenCalled();
    }
  });

  it('pane shortcuts (F8 delete, F5 copy) do not run while it is open, but do after', async () => {
    const del = getAction('file.delete')!;
    const spy = vi.spyOn(del, 'run').mockImplementation(() => {});
    try {
      const { container } = render(App);
      await settle();
      registerFakePanes();
      key(window, 'F8');
      expect(spy).toHaveBeenCalledTimes(1); // sanity: it works with the viewer closed
      cursorPath = 'C:\\d\\a.png';
      key(window, 'F3');
      await settle();
      expect(viewerEl(container)).toBeTruthy();
      key(viewerEl(container), 'F8');
      key(window, 'F8');
      key(document.body, 'F8');
      expect(spy).toHaveBeenCalledTimes(1);
      key(viewerEl(container), 'Escape');
      await settle();
      key(window, 'F8');
      expect(spy).toHaveBeenCalledTimes(2);
    } finally {
      spy.mockRestore();
    }
  });

  it('the view.image action registers plain F3 without clashing with Ctrl+F3', () => {
    expect(getAction('view.image')!.shortcut).toEqual({ key: 'F3' });
    expect(getAction('show.sortName')!.shortcut).toEqual({ key: 'F3', ctrlOrCmd: true });
    expect(runAction(getAction('view.image')!)).toBe(true); // no cursor: harmless
  });
});
