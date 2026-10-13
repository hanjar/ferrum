/**
 * F2-17 — Quick View panel rendering and its Ctrl+Q registry integration.
 * The preview store is mocked at the IPC boundary (readPreview) and driven
 * directly, so the panel renders without Tauri.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import QuickView from './QuickView.svelte';
import App from '../../App.svelte';
import { previewStore } from '../stores/previewStore.svelte';
import { ui } from '../uiState.svelte';
import { getAction, labelOf, runAction } from '../actions';
import type { PreviewData } from '../ipc/preview';

vi.mock('../ipc/preview', () => ({
  readPreview: (...args: unknown[]) => readPreviewMock(...args),
  readArchiveMemberPreview: (...args: unknown[]) => readArchiveMock(...args),
  cancelArchiveRequest: vi.fn(),
}));

// Disk images render through `ferrum-media`; mock the grant so the test
// asserts the derivation without a webview.
vi.mock('../ipc/media', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  mediaGrantOne: async (path: string) =>
    `http://ferrum-media.localhost/tok-${encodeURIComponent(path)}`,
  mediaRevoke: vi.fn(async () => {}),
}));

const readPreviewMock = vi.fn();
const readArchiveMock = vi.fn();

function data(kind: string, base64 = '', truncated = false): PreviewData {
  return {
    kind: kind as PreviewData['kind'],
    data: base64,
    truncated,
    tooLarge: false,
    size: 12,
    modifiedMs: 1,
  };
}

beforeEach(() => {
  ui.preview.maxTextBytes = 2_000_000;
  readPreviewMock.mockReset();
  previewStore.reset();
  ui.quickViewOpen = false;
  ui.activePane = 0;
});

afterEach(() => {
  cleanup();
  previewStore.reset();
  ui.quickViewOpen = false;
});

describe('QuickView panel', () => {
  it('renders only while open; empty state guides the keyboard user', async () => {
    const r = render(QuickView);
    expect(r.container.querySelector('aside')).toBeNull();
    ui.quickViewOpen = true;
    await tick();
    expect(r.container.querySelector('aside[aria-label="Quick View"]')).toBeTruthy();
    expect(r.container.textContent).toContain('Move the cursor onto a file');
  });

  it('renders text content as text (never HTML) and notes truncation', async () => {
    // base64 of "<img src=x onerror=alert(1)>" — must appear as text, not an element
    const evil = btoa('<img src=x onerror=alert(1)>');
    readPreviewMock.mockResolvedValue(data('text', evil, true));
    ui.quickViewOpen = true;
    previewStore.setTarget({
      name: 'evil.txt',
      path: 'C:\\d\\evil.txt',
      isDir: false,
      size: 40,
      modifiedMs: 1,
    });
    await previewStore.flush();
    render(QuickView);
    await tick();
    const pre = document.querySelector('pre.text');
    expect(pre).toBeTruthy();
    expect(pre?.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(document.querySelector('pre.text img')).toBeNull();
    expect(document.querySelector('.truncated')).toBeTruthy();
  });

  it('renders images via the ferrum-media protocol and binary as an explicit message', async () => {
    ui.quickViewOpen = true;
    // Images ship no base64: the src is a ferrum-media grant for target.path.
    readPreviewMock.mockResolvedValue(data('image'));
    previewStore.setTarget({
      name: 'p.png',
      path: 'C:\\d\\p.png',
      isDir: false,
      size: 3,
      modifiedMs: 1,
    });
    await previewStore.flush();
    const r = render(QuickView);
    await tick();
    await new Promise((res) => setTimeout(res, 0));
    await tick();
    const img = r.container.querySelector('img.image') as HTMLImageElement;
    expect(img?.src).toBe('http://ferrum-media.localhost/tok-C%3A%5Cd%5Cp.png');
    expect(img?.src).not.toContain('data:image');
    // XSS safety: the media src is inert — no inline handlers, ever.
    expect(img?.getAttribute('onerror')).toBeNull();
    expect(img?.getAttribute('onload')).toBeNull();

    readPreviewMock.mockResolvedValue(data('binary', 'AAE='));
    previewStore.setTarget({
      name: 'b.bin',
      path: 'C:\\d\\b.bin',
      isDir: false,
      size: 2,
      modifiedMs: 1,
    });
    await previewStore.flush();
    await tick();
    expect(r.container.textContent).toContain('Binary file — no preview.');
  });

  it('renders archive members from memory: image as data URL, text as text, no metadata IPC', async () => {
    ui.quickViewOpen = true;
    readArchiveMock.mockResolvedValue(data('image', 'AAAA'));
    previewStore.setTarget({
      name: 'p.png',
      path: 'C:\\d\\a.zip\\img/p.png',
      isDir: false,
      size: 3,
      modifiedMs: 1,
    });
    await previewStore.flush();
    const r = render(QuickView);
    await tick();
    expect(readArchiveMock).toHaveBeenCalledWith(
      'C:\\d\\a.zip',
      'img/p.png',
      2_000_000,
      expect.stringMatching(/^pv-[0-9a-f-]{36}-\d+$/),
    );
    expect(readPreviewMock).not.toHaveBeenCalled();
    const img = r.container.querySelector('img.image') as HTMLImageElement;
    expect(img?.src).toBe('data:image/png;base64,AAAA');

    readArchiveMock.mockResolvedValue(data('text', btoa('<b>x</b>')));
    previewStore.setTarget({
      name: 'n.txt',
      path: 'C:\\d\\a.zip\\n.txt',
      isDir: false,
      size: 8,
      modifiedMs: 1,
    });
    await previewStore.flush();
    await tick();
    expect(r.container.textContent).toContain('<b>x</b>');
    expect(r.container.querySelector('b')).toBeNull();
  });

  it('over-limit files show metadata only and render no payload', async () => {
    ui.quickViewOpen = true;
    ui.preview.maxTextBytes = 1_000_000;
    previewStore.setTarget({
      name: 'log.txt',
      path: 'C:\\d\\log.txt',
      isDir: false,
      size: 5_000_000,
      modifiedMs: 1,
    });
    await previewStore.flush();
    const r = render(QuickView);
    await tick();
    expect(readPreviewMock).not.toHaveBeenCalled();
    expect(r.container.textContent).toContain(
      'File larger than the text preview limit (1 MB) — showing metadata only',
    );
    expect(r.container.textContent).toContain('log.txt');
    expect(r.container.querySelector('pre.text')).toBeNull();
    expect(r.container.querySelector('img')).toBeNull();
  });

  it('video renders through MediaPlayer, no autoplay by default', async () => {
    ui.quickViewOpen = true;
    ui.preview.autoplayVideo = false;
    const play = vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue();
    readPreviewMock.mockResolvedValue(data('video'));
    previewStore.setTarget({
      name: 'c.mp4',
      path: 'C:/d/c.mp4',
      isDir: false,
      size: 9,
      modifiedMs: 1,
    });
    await previewStore.flush();
    const r = render(QuickView);
    await tick();
    expect(r.container.querySelector('video')).not.toBeNull();
    expect(play).not.toHaveBeenCalled();
    play.mockRestore();
  });

  it('syntax highlighting follows the setting (plain text when OFF)', async () => {
    ui.quickViewOpen = true;
    readPreviewMock.mockResolvedValue(data('json', btoa('{"a": 1}')));
    previewStore.setTarget({
      name: 'a.json',
      path: 'C:/d/a.json',
      isDir: false,
      size: 8,
      modifiedMs: 1,
    });
    await previewStore.flush();
    ui.preview.syntaxHighlight = true;
    const r = render(QuickView);
    await tick();
    expect(r.container.querySelector('[class*="tok-"]')).not.toBeNull();
    expect(r.container.querySelector('pre')?.textContent).toBe('{"a": 1}');
    ui.preview.syntaxHighlight = false;
    await tick();
    expect(r.container.querySelector('[class*="tok-"]')).toBeNull();
    expect(r.container.querySelector('pre')?.textContent).toBe('{"a": 1}');
    ui.preview.syntaxHighlight = true;
  });

  it('shows the honest error state and folder guidance', async () => {
    ui.quickViewOpen = true;
    readPreviewMock.mockRejectedValue(new Error('Access is denied.'));
    previewStore.setTarget({ name: 'x', path: 'C:\\d\\x', isDir: false, size: 1, modifiedMs: 1 });
    await previewStore.flush();
    const r = render(QuickView);
    await tick();
    expect(r.container.querySelector('.error')?.textContent).toContain('Access is denied.');

    previewStore.setTarget({
      name: 'sub',
      path: 'C:\\d\\sub',
      isDir: true,
      size: 0,
      modifiedMs: 1,
    });
    await tick();
    expect(r.container.textContent).toContain('Folders have no preview.');
  });

  it('close button clears the panel through the shell state', async () => {
    ui.quickViewOpen = true;
    const r = render(QuickView);
    await tick();
    fireEvent.click(r.container.querySelector('button.close') as HTMLElement);
    expect(ui.quickViewOpen).toBe(false);
  });
});

describe('Ctrl+Q registry integration', () => {
  it('toggles quick view through the action registry with a stateful label', () => {
    const action = getAction('show.quickView')!;
    expect(action.shortcut).toEqual({ key: 'q', ctrlOrCmd: true });
    expect(labelOf(action)).toBe('Quick View Panel');
    expect(runAction(action)).toBe(true);
    expect(ui.quickViewOpen).toBe(true);
    expect(labelOf(action)).toBe('Close Quick View Panel');
    runAction(action);
    expect(ui.quickViewOpen).toBe(false);
    expect(previewStore.target).toBeNull(); // closing clears the preview target
  });

  it('renders inside the app shell next to the panes', async () => {
    const r = render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    ui.quickViewOpen = true;
    await tick();
    expect(r.container.querySelector('aside[aria-label="Quick View"]')).toBeTruthy();
  });
});
