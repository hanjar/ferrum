// FER-466: the `diskUsage.query` answer card in the chat panel.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import type { DiskUsageAnswer } from '../../ai/diskUsageQuery';

afterEach(cleanup);

const item = (name: string, bytes: number, rel = `Projects\\${name}`) => ({
  path: `D:\\Drive\\${rel}`,
  name,
  rel,
  kind: 'file' as const,
  bytes,
  percent: 12.5,
  category: 'video' as const,
});

const FILES: DiskUsageAnswer = {
  status: 'files',
  items: [item('big.mkv', 3 * 1024 ** 3), item('<img src=x onerror=alert(1)>.bin', 2048)],
  metric: 'logical',
  scanRoot: 'D:\\Drive',
  scopeLabel: 'Drive',
  scope: 'view',
  totalMatching: 5000,
  truncated: true,
  notes: [
    'Scan hanya sebagian (ada folder yang tidak terbaca atau dibatasi), jadi angka ini belum lengkap.',
  ],
};

function mount() {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  const reveal = vi.fn<(path: string) => void>();
  const copyPath = vi.fn<(path: string) => Promise<boolean>>(async () => true);
  const openDiskUsage = vi.fn();
  const r = render(ChatPanel, {
    store,
    onclose: vi.fn(),
    revealDiskUsagePath: reveal,
    copyDiskUsagePath: copyPath,
    openDiskUsage,
  });
  return { ex, store, reveal, copyPath, openDiskUsage, ...r };
}

async function answer(
  store: ChatStore,
  ex: FakeExecutor,
  diskUsage: DiskUsageAnswer,
  query = 'file terbesar, tampilan sekarang',
) {
  store.send('paling gede itu apa ya');
  await tick();
  const runId = store.runs.at(-1)!.runId;
  const requestId = ex.submitted.at(-1)!.requestId;
  ex.emit({ runId, requestId, state: 'answered', diskUsage, diskUsageQuery: query });
  await tick();
  return runId;
}

describe('DiskUsageAnswerCard', () => {
  it('lists the files with size and root-relative path, labels the cut list and the partial scan', async () => {
    const { ex, store } = mount();
    await answer(store, ex, FILES);
    expect(screen.getByTestId('du-answer').getAttribute('data-status')).toBe('files');
    expect(screen.getByTestId('du-query').textContent).toContain('dari model');
    const rows = screen.getAllByTestId('du-item');
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain('big.mkv');
    expect(rows[0].textContent).toContain('3.00 GB');
    expect(rows[0].textContent).toContain('Projects\\big.mkv');
    expect(screen.getByTestId('du-message').textContent).toContain('dari 5,000');
    expect(screen.getByTestId('du-message').textContent).toContain('bukan semua file');
    expect(screen.getByTestId('du-notes').textContent).toContain('sebagian');
    expect(store.busy).toBe(false);
  });

  it('renders names as text, never as markup', async () => {
    const { ex, store, container } = mount();
    await answer(store, ex, FILES);
    expect(container.querySelector('img')).toBeNull();
    expect(screen.getAllByTestId('du-item')[1].textContent).toContain('<img src=x');
  });

  it('Reveal and Copy path use the full local path of that row only', async () => {
    const { ex, store, reveal, copyPath } = mount();
    await answer(store, ex, FILES);
    await fireEvent.click(screen.getAllByTestId('du-reveal')[0]);
    expect(reveal).toHaveBeenCalledWith('D:\\Drive\\Projects\\big.mkv');
    await fireEvent.click(screen.getAllByTestId('du-copy')[0]);
    expect(copyPath).toHaveBeenCalledWith('D:\\Drive\\Projects\\big.mkv');
    await waitFor(() => expect(screen.getByTestId('du-note').textContent).toBe('Path disalin.'));
  });

  it('a failed copy says so', async () => {
    const { ex, store, copyPath } = mount();
    copyPath.mockResolvedValue(false);
    await answer(store, ex, FILES);
    await fireEvent.click(screen.getAllByTestId('du-copy')[0]);
    await waitFor(() => expect(screen.getByTestId('du-note').textContent).toContain('tidak dapat'));
  });

  it('no scan: honest text and an Open Disk Usage button, no rows', async () => {
    const { ex, store, openDiskUsage } = mount();
    await answer(store, ex, { status: 'noScan', reason: 'none' });
    expect(screen.getByTestId('du-message').textContent).toBe(
      'Belum ada hasil Disk Usage. Jalankan scan dulu.',
    );
    expect(screen.queryByTestId('du-items')).toBeNull();
    await fireEvent.click(screen.getByTestId('du-open'));
    expect(openDiskUsage).toHaveBeenCalledTimes(1);
  });

  it('summary shows the scan totals', async () => {
    const { ex, store } = mount();
    await answer(
      store,
      ex,
      {
        status: 'summary',
        scanRoot: 'D:\\Drive',
        metric: 'logical',
        notes: [],
        summary: {
          files: 1200,
          folders: 80,
          symlinks: 0,
          logicalBytes: 5 * 1024 ** 3,
          allocatedBytes: null,
          errors: 3,
          durationMs: 10,
        },
      },
      'ringkasan hasil scan',
    );
    const s = screen.getByTestId('du-summary').textContent ?? '';
    expect(s).toContain('1,200');
    expect(s).toContain('5.00 GB');
    expect(s).toContain('tidak tersedia');
    expect(s).toContain('3 entri');
  });

  it('an unavailable (evicted) answer suggests a rescan', async () => {
    const { ex, store } = mount();
    await answer(store, ex, { status: 'unavailable', reason: 'evicted' });
    expect(screen.getByTestId('du-message').textContent).toContain('scan ulang');
    expect(screen.getByTestId('du-open')).toBeTruthy();
  });

  it('other answered runs still render the activity card, not this one', async () => {
    const { ex, store } = mount();
    store.send('folder apa');
    await tick();
    const runId = store.runs.at(-1)!.runId;
    ex.emit({
      runId,
      requestId: ex.submitted.at(-1)!.requestId,
      state: 'answered',
      activity: { status: 'notFound' } as never,
      activityQuery: 'x',
    });
    await tick();
    expect(screen.queryByTestId('du-answer')).toBeNull();
    expect(screen.getByTestId('activity-answer')).toBeTruthy();
  });

  it('bidi / zero-width characters are shown as visible markers; Reveal and Copy keep the original path', async () => {
    const { ex, store, reveal, copyPath, container } = mount();
    const evil = item('invoice\u202Efdp.exe', 10, 'Dir\\invoice\u202Efdp.exe\u200B');
    await answer(store, ex, {
      ...FILES,
      items: [evil],
      truncated: false,
      totalMatching: 1,
      notes: [],
    } as DiskUsageAnswer);
    const row = screen.getByTestId('du-item');
    expect(row.textContent).toContain('invoice\u27E8U+202E\u27E9fdp.exe');
    expect(row.textContent).toContain('\u27E8U+200B\u27E9');
    expect(row.textContent).not.toContain('\u202E');
    expect(row.textContent).not.toContain('\u200B');
    expect(container.querySelectorAll('[data-testid="du-item"] bdi').length).toBeGreaterThanOrEqual(
      2,
    );
    await fireEvent.click(screen.getByTestId('du-reveal'));
    expect(reveal).toHaveBeenCalledWith(evil.path);
    await fireEvent.click(screen.getByTestId('du-copy'));
    expect(copyPath).toHaveBeenCalledWith(evil.path);
    expect(evil.path).toContain('\u202E');
  });
});
