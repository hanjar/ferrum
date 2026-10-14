/**
 * F2-19 — SyncPanel component: mandatory dry-run preview, direction
 * controls, stale banner, and disabled execute without a plan.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import SyncPanel from './SyncPanel.svelte';
import { compareStore } from '../stores/compareStore.svelte';
import { getAction, runAction } from '../actions';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../ipc/compare';

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...args: unknown[]) => compareDirsMock(...args),
  compareFiles: vi.fn(),
}));
vi.mock('../ipc/mutations', () => ({
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
}));

const compareDirsMock = vi.fn();

function diff(name: string, state: DirDiffState): DirDiffEntry {
  return { name, state, leftSize: 1, leftModifiedMs: 1, rightSize: 1, rightModifiedMs: 1 };
}

function report(entries: DirDiffEntry[]): CompareDirsReport {
  return { leftRoot: 'C:\\L', rightRoot: 'D:\\R', entries, truncated: false };
}

function fakePane(path: string): PaneApi {
  return {
    path,
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
  compareDirsMock.mockReset();
  compareStore.reset();
  registerPane(0, fakePane('C:\\L'));
  registerPane(1, fakePane('D:\\R'));
});

afterEach(() => {
  cleanup();
  compareStore.reset();
});

describe('SyncPanel', () => {
  it('renders only while open and leads with the dry-run notice', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'leftOnly')]));
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    expect(r.container.querySelector('aside[aria-label="Synchronize Directories"]')).toBeTruthy();
    expect(r.container.textContent).toContain('Dry run — nothing executed yet');
    expect(r.container.textContent).toContain('a.txt');
    expect(r.container.textContent).toContain('copy →');
  });

  it('shows which scan served the preview and why a fast scan was skipped (FER-472)', async () => {
    compareDirsMock.mockResolvedValue({
      ...report([diff('a.txt', 'leftOnly')]),
      fastScan: { left: true, right: false, note: 'Scan cepat dilewati: izin admin ditolak' },
    });
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    const text = (r.container.textContent ?? '').replace(/\s+/g, ' ');
    expect(text).toContain('Scan cepat (MFT) dipakai untuk folder kiri');
    expect(text).toContain('izin admin ditolak');
  });

  it('FER-477: shows how many content reads the journal skipped, or why not', async () => {
    compareDirsMock.mockResolvedValue({
      ...report([diff('a.txt', 'same')]),
      incremental: { used: true, contentSkipped: 42 },
    });
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    expect(r.getByTestId('sync-journal-skip').textContent).toContain('isi 42 file dilewati');
    cleanup();
    compareStore.reset();
    compareDirsMock.mockResolvedValue({
      ...report([diff('a.txt', 'same')]),
      incremental: { used: false, reason: 'marks_expired', contentSkipped: 0 },
    });
    await compareStore.openSync();
    const r2 = render(SyncPanel);
    await tick();
    const t = r2.getByTestId('sync-journal-skip').textContent ?? '';
    expect(t).toContain('tidak dipakai (marks_expired)');
    expect(t).toContain('semua isi file dibaca');
  });

  it('execute is disabled without a plan and close clears the panel', async () => {
    compareStore.syncOpen = true;
    const r = render(SyncPanel);
    await tick();
    const run = Array.from(r.container.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('Run Synchronization'),
    ) as HTMLButtonElement;
    expect(run.disabled).toBe(true);
    (r.container.querySelector('button.close') as HTMLElement).click();
    expect(compareStore.syncOpen).toBe(false);
  });

  it('direction radios regenerate the preview; stale banner shows', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'leftOnly')]));
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    const rtl = r.container.querySelector('input[value="rightToLeft"]') as HTMLInputElement;
    fireEvent.click(rtl);
    await tick();
    expect(compareStore.syncDirection).toBe('rightToLeft');
    expect(r.container.textContent).toContain('1 already equal'); // a.txt now skipped

    compareStore.syncStale = true;
    await tick();
    expect(r.container.textContent).toContain('Sources changed since this preview');
  });

  it('opens through the Commands menu action (both panes required)', async () => {
    const action = getAction('commands.synchronize')!;
    expect(action.shortcut).toBeUndefined(); // TC has no default binding
    compareDirsMock.mockResolvedValue(report([]));
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() => expect(compareStore.syncPlan).not.toBeNull());
    expect(compareStore.syncOpen).toBe(true);

    // without a second pane the action is unavailable
    registerPane(1, fakePane('D:\\R'))();
    expect(runAction(getAction('commands.synchronize')!)).toBe(false);
  });
});

describe('SyncPanel FER-111/112 controls', () => {
  const btn = (c: HTMLElement, label: string) =>
    Array.from(c.querySelectorAll('button')).find((b) =>
      b.textContent?.trim().includes(label),
    ) as HTMLButtonElement;

  it('paginates the preview instead of silently truncating it', async () => {
    const many = Array.from({ length: 450 }, (_, i) => diff(`f${i}.txt`, 'leftOnly'));
    compareDirsMock.mockResolvedValue(report(many));
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    expect(
      r.container.querySelectorAll('ul[aria-label="Synchronization plan"] li.entry'),
    ).toHaveLength(200);
    fireEvent.click(btn(r.container, 'Show all 450'));
    await tick();
    expect(
      r.container.querySelectorAll('ul[aria-label="Synchronization plan"] li.entry'),
    ).toHaveLength(450);
  });

  it('shows why a partial report produced no plan and disables Run', async () => {
    compareDirsMock.mockResolvedValue({
      ...report([diff('a', 'leftOnly')]),
      errors: [{ name: 'a', message: 'Access is denied' }],
    });
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    expect(r.container.textContent).toContain('Access is denied');
    expect(btn(r.container, 'Run Synchronization').disabled).toBe(true);
  });

  it('asks for overwrite confirmation and per-item conflict decisions', async () => {
    compareDirsMock.mockResolvedValue(
      report([diff('c.txt', 'contentDifferent'), diff('n.txt', 'newerLeft')]),
    );
    await compareStore.openSync();
    compareStore.setSyncDirection('leftToRight');
    const r = render(SyncPanel);
    await tick();
    fireEvent.click(btn(r.container, 'Run Synchronization'));
    await tick();
    expect(r.container.textContent).toContain('2 file(s) will be overwritten');
  });

  it('conflict buttons record decisions; apply-to-all is offered', async () => {
    compareDirsMock.mockResolvedValue(report([diff('c.txt', 'contentDifferent')]));
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    expect(r.container.textContent).toContain('1 conflict(s) — apply to all');
    fireEvent.click(btn(r.container.querySelector('.decide') as HTMLElement, 'Keep left'));
    await tick();
    expect(compareStore.syncPlan?.[0]).toMatchObject({
      action: 'updateRight',
      decision: 'keepLeft',
    });
  });

  it('has a Compare contents option that recompares by content', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a', 'same')]));
    await compareStore.openSync();
    const r = render(SyncPanel);
    await tick();
    const cb = Array.from(r.container.querySelectorAll('input[type="checkbox"]')).find((i) =>
      i.closest('label')?.textContent?.includes('Compare contents'),
    ) as HTMLInputElement;
    fireEvent.click(cb);
    await vi.waitFor(() =>
      expect(compareDirsMock).toHaveBeenLastCalledWith('C:\\L', 'D:\\R', true, expect.anything()),
    );
  });
  it('shows the busy message when the folders are locked by an automation and runs nothing (P0.4)', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'leftOnly')]));
    await compareStore.openSync();
    compareStore.syncLeaseApi = {
      acquire: vi.fn(async () => ({
        status: 'busy' as const,
        holder: { kind: 'runner' as const, runId: 3 },
      })),
      release: vi.fn(async () => {}),
    };
    const r = render(SyncPanel);
    await tick();
    expect(r.container.querySelector('[data-testid="sync-lease-busy"]')).toBeNull();
    const run = Array.from(r.container.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('Run Synchronization'),
    ) as HTMLButtonElement;
    fireEvent.click(run);
    await vi.waitFor(() =>
      expect(r.container.querySelector('[data-testid="sync-lease-busy"]')).toBeTruthy(),
    );
    const msg = r.container.querySelector('[data-testid="sync-lease-busy"]') as HTMLElement;
    expect(msg.getAttribute('role')).toBe('alert');
    expect(msg.textContent).toContain('in use by an automation');
    expect(msg.textContent).toContain('Nothing was changed');
    // Only the preview compare ran; the plan is still there for a retry.
    expect(compareDirsMock).toHaveBeenCalledTimes(1);
    expect(run.disabled).toBe(false);
    // A fresh preview clears the message.
    compareStore.syncLeaseApi = {
      acquire: vi.fn(async () => ({ leaseId: 'x' })),
      release: vi.fn(async () => {}),
    };
    await compareStore.openSync();
    await tick();
    expect(r.container.querySelector('[data-testid="sync-lease-busy"]')).toBeNull();
  });
});
