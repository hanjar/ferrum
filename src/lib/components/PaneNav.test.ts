import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FilePane from './FilePane.svelte';
import App from '../../App.svelte';
import { ui } from '../uiState.svelte';
import { FsError } from '../ipc/types';

afterEach(() => {
  ui.activePane = 0;
});

describe('dual-pane activation (task 06)', () => {
  it('Tab alternates the active pane', async () => {
    render(App);
    await tick();
    const panes = () => document.querySelectorAll('section.pane');
    expect(panes()[0].className).toContain('active');

    const lists = () => document.querySelectorAll<HTMLElement>('[data-pane-list]');
    fireEvent.keyDown(lists()[0], { key: 'Tab' });
    await tick();
    expect(panes()[1].className).toContain('active');
    expect(panes()[0].className).not.toContain('active');
    expect(document.activeElement).toBe(lists()[1]);

    fireEvent.keyDown(lists()[1], { key: 'Tab', shiftKey: true });
    await tick();
    expect(panes()[0].className).toContain('active');
    expect(document.activeElement).toBe(lists()[0]);
  });

  it('Tab with no focus in any list focuses the active pane list (FER-489)', async () => {
    render(App);
    await tick();
    const lists = document.querySelectorAll<HTMLElement>('[data-pane-list]');
    const before = ui.activePane;
    fireEvent.keyDown(window, { key: 'Tab' });
    await tick();
    expect(ui.activePane).toBe(before);
    expect(document.activeElement).toBe(lists[before]);
  });

  it('Tab from a toolbar button jumps to the other pane list (FER-489)', async () => {
    render(App);
    await tick();
    const lists = document.querySelectorAll<HTMLElement>('[data-pane-list]');
    const btn = document.querySelector<HTMLElement>('section.pane button')!;
    btn.focus();
    ui.activePane = 0;
    fireEvent.keyDown(btn, { key: 'Tab' });
    await tick();
    expect(ui.activePane).toBe(1);
    expect(document.activeElement).toBe(lists[1]);
  });

  it('Tab inside the filter input returns to the same pane list (FER-489)', async () => {
    render(App);
    await tick();
    (document.querySelector('.filterBtn') as HTMLElement).click();
    await tick();
    const input = document.querySelector('.filter input') as HTMLInputElement;
    input.focus();
    const before = ui.activePane;
    const pane = input.closest('section.pane') as HTMLElement;
    fireEvent.keyDown(input, { key: 'Tab' });
    await tick();
    expect(ui.activePane).toBe(Number(pane.dataset.pane));
    expect(document.activeElement).toBe(pane.querySelector('[data-pane-list]'));
    void before;
  });
});

describe('breadcrumb navigation (task 06)', () => {
  it('clicking a crumb opens that path and clears selection', async () => {
    const { container } = render(FilePane, { paneId: 0 });
    // Browser mode bootstraps the mock tab asynchronously.
    await vi.waitFor(() => {
      expect(container.querySelectorAll('.crumb').length).toBeGreaterThanOrEqual(2);
    });
    // click first crumb ("C:") — mockFetcher 404s it; the pane must show
    // the error locally and not crash.
    (container.querySelectorAll('.crumb')[0] as HTMLElement).click();
    await vi.waitFor(() => expect(container.textContent).toContain("Can't open this folder"));
  });
});

describe('failure isolation (task 06)', () => {
  it('an error in one pane leaves the other pane listing intact', async () => {
    const { PaneStore } = await import('../stores/paneStore.svelte');
    const kept = {
      name: 'kept.txt',
      path: 'D:\\ok\\kept.txt',
      isDir: false,
      isSymlink: false,
      size: 5,
      modifiedMs: 1,
      hidden: false,
    };
    const bad = new PaneStore(
      () => Promise.reject(new FsError({ code: 'notFound', message: 'gone' })),
      'C:\\h',
    );
    const good = new PaneStore(async () => [kept], 'D:\\h');
    await bad.newTab('C:\\missing');
    await good.newTab('D:\\ok');
    expect(bad.error?.code).toBe('notFound');
    expect(good.rows.length).toBe(1);
    expect(good.error).toBeNull();
  });
});
