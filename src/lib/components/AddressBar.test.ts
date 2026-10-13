import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import AddressBar from './AddressBar.svelte';
import { activity } from '../stores/activityStore.svelte';
import { FsError } from '../ipc/types';

const ipc = vi.hoisted(() => ({
  pathSuggest: vi.fn(),
  pathResolve: vi.fn(),
}));
vi.mock('../ipc/pathSuggest', () => ipc);

const crumbs = [
  { label: 'C:', path: 'C:\\' },
  { label: 'Users', path: 'C:\\Users' },
];

function res(items: ReturnType<typeof suggestion>[], extra: Record<string, unknown> = {}) {
  return {
    items,
    truncated: false,
    timedOut: false,
    needsConfirm: false,
    host: null,
    ...extra,
  };
}

function suggestion(name: string, kind: 'dir' | 'archive' | 'drive' = 'dir') {
  return { name, path: `C:\\Users\\${name}`, kind, link: false };
}

async function openBar(props: Record<string, unknown> = {}) {
  const onnavigate = vi.fn();
  const view = render(AddressBar, {
    crumbs,
    path: 'C:\\Users',
    onnavigate,
    oncrumb: vi.fn(),
    ...props,
  });
  await fireEvent.click(screen.getByLabelText('Edit path'));
  await tick();
  const input = screen.getByRole('combobox') as HTMLInputElement;
  return { ...view, input, onnavigate };
}

async function type(input: HTMLInputElement, value: string) {
  await fireEvent.input(input, { target: { value } });
  await vi.advanceTimersByTimeAsync(130);
  await tick();
}

beforeEach(() => {
  vi.useFakeTimers();
  ipc.pathSuggest.mockReset();
  ipc.pathResolve.mockReset();
  ipc.pathSuggest.mockResolvedValue(res([]));
});
afterEach(() => {
  vi.useRealTimers();
  activity.recent = [];
});

describe('AddressBar', () => {
  it('shows breadcrumbs and switches to a selected input on empty-area click', async () => {
    const { input } = await openBar();
    expect(input.value).toBe('C:\\Users');
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe('C:\\Users'.length);
    expect(document.activeElement).toBe(input);
  });

  it('debounces typing into one backend call with the last text', async () => {
    const { input } = await openBar();
    await fireEvent.input(input, { target: { value: 'C:\\Users\\h' } });
    await fireEvent.input(input, { target: { value: 'C:\\Users\\ha' } });
    expect(ipc.pathSuggest).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(130);
    expect(ipc.pathSuggest).toHaveBeenCalledTimes(1);
    expect(ipc.pathSuggest).toHaveBeenCalledWith('C:\\Users\\ha', 'C:\\Users', undefined, []);
  });

  it('ignores a stale reply when a newer request is in flight', async () => {
    const { input } = await openBar();
    let first!: (v: unknown) => void;
    ipc.pathSuggest.mockImplementationOnce(() => new Promise((r) => (first = r)));
    await type(input, 'C:\\Users\\a');
    ipc.pathSuggest.mockResolvedValueOnce({
      items: [suggestion('beta')],
      truncated: false,
      timedOut: false,
    });
    await type(input, 'C:\\Users\\b');
    first(res([suggestion('alpha')]));
    await tick();
    const opts = screen.getAllByRole('option').map((o) => o.textContent?.trim());
    expect(opts.join(' ')).toContain('beta');
    expect(opts.join(' ')).not.toContain('alpha');
  });

  it('renders ARIA combobox/listbox and moves aria-activedescendant with arrows', async () => {
    ipc.pathSuggest.mockResolvedValue({
      items: [suggestion('alpha'), suggestion('alps')],
      truncated: false,
      timedOut: false,
    });
    const { input } = await openBar();
    await type(input, 'C:\\Users\\al');
    expect(input.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByRole('listbox')).toBeTruthy();
    await fireEvent.keyDown(input, { key: 'ArrowDown' });
    const opts = screen.getAllByRole('option');
    expect(input.getAttribute('aria-activedescendant')).toBe(opts[0].id);
    expect(opts[0].getAttribute('aria-selected')).toBe('true');
    await fireEvent.keyDown(input, { key: 'ArrowDown' });
    expect(input.getAttribute('aria-activedescendant')).toBe(opts[1].id);
    await fireEvent.keyDown(input, { key: 'ArrowUp' });
    expect(input.getAttribute('aria-activedescendant')).toBe(opts[0].id);
  });

  it('Tab completes the first suggestion (folders get a separator)', async () => {
    ipc.pathSuggest.mockResolvedValue({
      items: [suggestion('Alpha')],
      truncated: false,
      timedOut: false,
    });
    const { input } = await openBar();
    await type(input, 'C:\\Users\\al');
    await fireEvent.keyDown(input, { key: 'Tab' });
    expect(input.value).toBe('C:\\Users\\Alpha\\');
  });

  it('Tab completes an archive without a separator and labels it', async () => {
    ipc.pathSuggest.mockResolvedValue({
      items: [suggestion('pack.zip', 'archive')],
      truncated: false,
      timedOut: false,
    });
    const { input } = await openBar();
    await type(input, 'C:\\Users\\pa');
    expect(screen.getByRole('option').textContent).toContain('arsip');
    await fireEvent.keyDown(input, { key: 'Tab' });
    expect(input.value).toBe('C:\\Users\\pack.zip');
  });

  it('Enter resolves the typed text and navigates to a folder', async () => {
    ipc.pathResolve.mockResolvedValue({ path: 'C:\\Windows', kind: 'dir' });
    const { input, onnavigate } = await openBar();
    await type(input, 'C:\\Windows');
    await fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    expect(ipc.pathResolve).toHaveBeenCalledWith('C:\\Windows', 'C:\\Users');
    expect(onnavigate).toHaveBeenCalledWith('C:\\Windows');
    expect(screen.queryByRole('combobox')).toBeNull();
  });

  it('Enter on a highlighted suggestion opens that suggestion', async () => {
    ipc.pathSuggest.mockResolvedValue({
      items: [suggestion('Alpha')],
      truncated: false,
      timedOut: false,
    });
    ipc.pathResolve.mockResolvedValue({ path: 'C:\\Users\\Alpha', kind: 'dir' });
    const { input, onnavigate } = await openBar();
    await type(input, 'C:\\Users\\al');
    await fireEvent.keyDown(input, { key: 'ArrowDown' });
    await fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    expect(ipc.pathResolve).toHaveBeenCalledWith('C:\\Users\\Alpha', 'C:\\Users');
    expect(onnavigate).toHaveBeenCalledWith('C:\\Users\\Alpha');
  });

  it('keeps the input open with an inline message for a missing path', async () => {
    ipc.pathResolve.mockResolvedValue({ path: 'C:\\nope', kind: 'missing' });
    const { input, onnavigate } = await openBar();
    await type(input, 'C:\\nope');
    await fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('not found');
    expect(screen.getByRole('combobox')).toBeTruthy();
    expect(onnavigate).not.toHaveBeenCalled();
  });

  it('reports a file target and a backend error inline', async () => {
    ipc.pathResolve.mockResolvedValueOnce({ path: 'C:\\a.txt', kind: 'file' });
    const { input } = await openBar();
    await type(input, 'C:\\a.txt');
    await fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('Not a folder');
    ipc.pathResolve.mockRejectedValueOnce(
      new FsError({ code: 'invalidPath', message: 'Device paths are not supported' }),
    );
    await fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('Device paths');
  });

  it('Escape returns to the breadcrumb without navigating', async () => {
    const { input, onnavigate } = await openBar();
    await fireEvent.keyDown(input, { key: 'Escape' });
    await tick();
    expect(screen.queryByRole('combobox')).toBeNull();
    expect(screen.getByRole('navigation', { name: 'Path' })).toBeTruthy();
    expect(onnavigate).not.toHaveBeenCalled();
  });

  it('clicking outside (focus leaving) cancels', async () => {
    const { input } = await openBar();
    await fireEvent.focusOut(input, { relatedTarget: document.body });
    await tick();
    expect(screen.queryByRole('combobox')).toBeNull();
  });

  it('shows Recent only while activity recording is active', async () => {
    activity.recent = [
      {
        entity: {
          id: 'e1',
          kind: 'folder',
          provider: 'fs',
          volumeId: 'v',
          locator: 'C:\\Projects\\alpha-old',
          parentPath: 'C:\\Projects',
          displayName: 'alpha-old',
          availability: 'ok',
        },
        lastUtcMs: 1,
        episodeCount: 1,
        activeDays: 1,
        score: null,
      },
    ];
    activity.recording = true;
    activity.privateSession = false;
    const { input } = await openBar();
    await type(input, 'alpha');
    expect(screen.getAllByRole('option').some((o) => o.textContent?.includes('recent'))).toBe(true);
    activity.privateSession = true;
    await tick();
    expect(screen.queryAllByRole('option')).toHaveLength(0);
    activity.privateSession = false;
  });

  it('filters a longer prefix locally without asking the backend again', async () => {
    ipc.pathSuggest.mockResolvedValue(res([suggestion('Alpha'), suggestion('alps')]));
    const { input } = await openBar();
    await type(input, 'C:\\Users\\al');
    expect(ipc.pathSuggest).toHaveBeenCalledTimes(1);
    await fireEvent.input(input, { target: { value: 'C:\\Users\\alph' } });
    await tick();
    expect(ipc.pathSuggest).toHaveBeenCalledTimes(1);
    expect(screen.getAllByRole('option')).toHaveLength(1);
    // A shorter prefix needs the backend again.
    await type(input, 'C:\\Users\\a');
    expect(ipc.pathSuggest).toHaveBeenCalledTimes(2);
  });

  it('does not trust a truncated result for local filtering', async () => {
    ipc.pathSuggest.mockResolvedValue(res([suggestion('Alpha')], { truncated: true }));
    const { input } = await openBar();
    await type(input, 'C:\\Users\\a');
    await type(input, 'C:\\Users\\al');
    expect(ipc.pathSuggest).toHaveBeenCalledTimes(2);
  });

  it('asks for Enter on an unknown UNC host and shows no suggestions', async () => {
    ipc.pathSuggest.mockResolvedValue(res([], { needsConfirm: true, host: 'fileserver' }));
    const { input } = await openBar();
    await type(input, '\\\\fileserver\\share\\a');
    expect(document.body.textContent).toContain('Tekan Enter untuk menghubungi \\\\fileserver');
    expect(screen.queryAllByRole('option')).toHaveLength(0);
  });

  it('passes UNC locations from the current path as known hosts', async () => {
    const { input } = await openBar({ path: '\\\\srv\\share\\docs' });
    await type(input, '\\\\srv\\share\\d');
    expect(ipc.pathSuggest).toHaveBeenCalledWith(
      '\\\\srv\\share\\d',
      '\\\\srv\\share\\docs',
      undefined,
      ['\\\\srv\\share\\docs'],
    );
  });

  it('error messages quote what was typed, never the expanded value', async () => {
    ipc.pathResolve.mockResolvedValue({ path: 'C:\\Users\\secret-expanded', kind: 'missing' });
    const { input } = await openBar();
    await type(input, '%USERPROFILE%\\nope');
    await fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    const msg = screen.getByRole('alert').textContent ?? '';
    expect(msg).toContain('%USERPROFILE%\\nope');
    expect(msg).not.toContain('secret-expanded');
  });

  it('aria-controls only points at the list while it exists; hints are hidden from AT', async () => {
    ipc.pathSuggest.mockResolvedValue(res([suggestion('Alpha', 'archive')]));
    const { input } = await openBar();
    expect(input.getAttribute('aria-controls')).toBeNull();
    await type(input, 'C:\\Users\\al');
    const id = input.getAttribute('aria-controls');
    expect(id).toBeTruthy();
    expect(document.getElementById(id as string)).not.toBeNull();
    const hidden = document.querySelectorAll('[role=option] [aria-hidden=true]');
    expect(hidden.length).toBeGreaterThan(0);
  });

  it('hotlist slot calls its handler', async () => {
    const onhotlist = vi.fn();
    render(AddressBar, { crumbs, path: 'C:\\Users', onhotlist });
    await fireEvent.click(screen.getByTitle(/Directory Hotlist/));
    expect(onhotlist).toHaveBeenCalledTimes(1);
  });

  it('FER-53: renders archive crumbs with archive markers and handles oncrumb navigation', async () => {
    const oncrumb = vi.fn();
    const archiveCrumbs = [
      { label: 'C:', path: 'C:\\' },
      { label: 'data', path: 'C:\\data' },
      { label: 'backup.zip', path: 'C:\\data\\backup.zip', isArchive: true, inArchive: true },
      { label: 'docs', path: 'C:\\data\\backup.zip\\docs', inArchive: true },
    ];
    render(AddressBar, {
      crumbs: archiveCrumbs,
      path: 'C:\\data\\backup.zip\\docs',
      oncrumb,
    });
    const buttons = screen.getAllByRole('button');
    const zipBtn = buttons.find((b) => b.textContent?.trim() === 'backup.zip');
    expect(zipBtn).toBeDefined();
    expect(zipBtn?.getAttribute('data-archive')).toBe('true');
    expect(zipBtn?.classList.contains('archive')).toBe(true);

    const docsBtn = buttons.find((b) => b.textContent?.trim() === 'docs');
    expect(docsBtn).toBeDefined();
    expect(docsBtn?.classList.contains('in-archive')).toBe(true);

    // Clicking a crumb navigates
    await fireEvent.click(zipBtn!);
    expect(oncrumb).toHaveBeenCalledWith('C:\\data\\backup.zip');
  });
});
