import { describe, expect, it, vi, beforeEach } from 'vitest';
import { fireEvent, render, waitFor } from '@testing-library/svelte';
import FilePane from './FilePane.svelte';

// Desktop (Tauri) boot: the real home dir must be the default for new tabs,
// never the browser-dev mock home (C:\Projects).
const fetchSpy = vi.hoisted(() => vi.fn<(path: string) => Promise<unknown[]>>(async () => []));
vi.mock('../stores/archiveFetcher', async (orig) => ({
  ...(await orig<typeof import('../stores/archiveFetcher')>()),
  createArchiveAwareFetcher: () => fetchSpy,
}));
const homeDirMock = vi.hoisted(() => vi.fn(async (): Promise<string> => 'C:\\Users\\tester'));
vi.mock('@tauri-apps/api/path', () => ({
  homeDir: homeDirMock,
}));
vi.mock('../ipc/tauri', async (orig) => ({
  ...(await orig<typeof import('../ipc/tauri')>()),
  isInTauri: () => true,
}));
vi.mock('../ipc/activity', async (orig) => ({
  ...(await orig<typeof import('../ipc/activity')>()),
  activityState: vi.fn(async () => null),
  recordActivityVisit: vi.fn(),
  onActivityChanged: vi.fn(async () => () => undefined),
}));

describe('FilePane new tab in Tauri', () => {
  beforeEach(() => {
    fetchSpy.mockClear();
    homeDirMock.mockReset();
    homeDirMock.mockImplementation(async () => 'C:\\Users\\tester');
  });

  it('when homeDir rejects, the new tab falls back to C:\\ and never to an empty path', async () => {
    homeDirMock.mockRejectedValue(new Error('no home'));
    const m = render(FilePane, { paneId: 0 });
    await waitFor(() => expect(fetchSpy).toHaveBeenCalledWith('C:\\'));

    const newTab = m.container.querySelector('button[title="New tab"]');
    expect(newTab).not.toBeNull();
    await fireEvent.click(newTab!);

    await waitFor(() =>
      expect(fetchSpy.mock.calls.filter((c) => c[0] === 'C:\\').length).toBeGreaterThanOrEqual(2),
    );
    expect(fetchSpy.mock.calls.map((c) => c[0])).not.toContain('');
    expect(fetchSpy.mock.calls.map((c) => c[0])).not.toContain('C:\\Projects');
    m.unmount();
  });

  it('new tab opens the real home directory and never the mock C:\\Projects', async () => {
    const m = render(FilePane, { paneId: 0 });
    await waitFor(() => expect(fetchSpy).toHaveBeenCalledWith('C:\\Users\\tester'));

    const newTab = m.container.querySelector('button[title="New tab"]');
    expect(newTab).not.toBeNull();
    await fireEvent.click(newTab!);

    await waitFor(() =>
      expect(
        fetchSpy.mock.calls.filter((c) => c[0] === 'C:\\Users\\tester').length,
      ).toBeGreaterThanOrEqual(2),
    );
    expect(fetchSpy.mock.calls.map((c) => c[0])).not.toContain('C:\\Projects');
    m.unmount();
  });
});
