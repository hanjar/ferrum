import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import PlanCard from './PlanCard.svelte';
import { ChatStore, type RunView } from '../../ai/chatStore.svelte';
import { FakeExecutor, samplePlan } from '../../ai/fixtures/chatFixtures';
import { compareStore } from '../../stores/compareStore.svelte';
import { registerPane, type PaneApi } from '../../stores/panesRegistry.svelte';

const compareDirsMock = vi.fn();
vi.mock('../../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...args: unknown[]) => compareDirsMock(...args),
  compareFiles: vi.fn(),
}));
vi.mock('../../ipc/mutations', () => ({
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
}));

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

const LEFT = 'C:\\Runs\\Left';
const RIGHT = 'D:\\Runs\\Right';
const PANE_A = ['Z:', 'Other', 'A'].join(String.fromCharCode(92));
const PANE_B = ['Z:', 'Other', 'B'].join(String.fromCharCode(92));

function syncPlan(conflictCount: number) {
  const base = samplePlan();
  return samplePlan({
    capability: 'directories.sync',
    sourceSnapshot: {
      ...base.sourceSnapshot,
      panes: [
        { ...base.sourceSnapshot.panes[0], location: { kind: 'disk', path: LEFT } },
        { ...base.sourceSnapshot.panes[1], location: { kind: 'disk', path: RIGHT } },
      ],
    },
    operation: {
      kind: 'directories.sync',
      leftRoot: LEFT,
      rightRoot: RIGHT,
      leftManifest: 'lm-1',
      rightManifest: 'rm-1',
    },
    preview: { itemCount: 3, conflictCount, totalBytes: 10 },
    steps: [{ id: 's1', capability: 'directories.sync', dependsOn: [] }],
  });
}

async function mountCard(plan: ReturnType<typeof syncPlan>, conflicts: number) {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  store.send('sync both');
  await tick();
  const runId = store.runs.at(-1)!.runId;
  ex.emit({
    runId,
    requestId: ex.submitted.at(-1)!.requestId,
    state: 'awaitingApproval',
    plan,
    conflicts: Array.from({ length: conflicts }, (_, i) => ({
      itemRef: `c${i}`,
      label: `file${i}.txt`,
      kind: 'both changed',
    })),
  });
  await tick();
  const run = store.runs.at(-1) as RunView;
  return render(PlanCard, { run, store });
}

beforeEach(() => {
  compareDirsMock.mockReset();
  compareStore.reset();
  registerPane(0, fakePane(PANE_A));
  registerPane(1, fakePane(PANE_B));
});
afterEach(() => {
  cleanup();
  compareStore.reset();
});

describe('PlanCard: Resolve in Sync panel', () => {
  it('shows the button for a sync run with conflicts and opens SyncPanel for the run folder pair', async () => {
    compareDirsMock.mockResolvedValue({
      leftRoot: LEFT,
      rightRoot: RIGHT,
      entries: [],
      truncated: false,
    });
    await mountCard(syncPlan(2), 2);
    await fireEvent.click(screen.getByRole('button', { name: 'Resolve in Sync panel' }));
    await tick();
    expect(compareStore.syncOpen).toBe(true);
    // FER-472: opening the panel first settles the (here: not offered) fast-scan question.
    await vi.waitFor(() => expect(compareDirsMock).toHaveBeenCalledTimes(1));
    expect(compareDirsMock.mock.calls[0][0]).toBe(LEFT);
    expect(compareDirsMock.mock.calls[0][1]).toBe(RIGHT);
    // Refresh keeps the pinned pair, not the panes.
    await compareStore.openSync();
    expect(compareDirsMock.mock.calls[1].slice(0, 2)).toEqual([LEFT, RIGHT]);
    // The Synchronize Dirs command goes back to the panes.
    await compareStore.openSync(null);
    expect(compareDirsMock.mock.calls[2].slice(0, 2)).toEqual([PANE_A, PANE_B]);
  });

  it('does not offer the button when the sync run has no conflicts', async () => {
    await mountCard(syncPlan(0), 0);
    expect(screen.queryByRole('button', { name: 'Resolve in Sync panel' })).toBeNull();
  });

  it('does not offer the button for a non-sync run even with conflicts', async () => {
    await mountCard(samplePlan({ preview: { itemCount: 2, conflictCount: 1, totalBytes: 1 } }), 1);
    expect(screen.queryByRole('button', { name: 'Resolve in Sync panel' })).toBeNull();
    expect(compareStore.syncOpen).toBe(false);
  });
});
