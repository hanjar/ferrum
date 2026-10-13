// ACT-08 R6/R7 v1: `answered` run state in the store and the answer card in the chat panel.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore, isTerminal, statusLabel } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import type { ActivityLocation } from '../../ai/activityQuery';

afterEach(cleanup);

const item = (over: Record<string, unknown> = {}) => ({
  id: 'l1',
  displayName: 'Invoice 2026',
  parentHint: '\u2026\\Clients\\Acme',
  kind: 'folder',
  availability: 'available',
  reasons: ['recent'],
  ...over,
});

class LocatingExecutor extends FakeExecutor {
  located: Array<[string, string]> = [];
  location: ActivityLocation | null = {
    path: 'C:\\Clients\\Acme\\Invoice 2026',
    parentPath: 'C:\\Clients\\Acme',
    kind: 'folder',
  };
  async locateActivityItem(runId: string, entityId: string) {
    this.located.push([runId, entityId]);
    return this.location;
  }
}

function mount(ex: FakeExecutor = new LocatingExecutor()) {
  const store = new ChatStore(ex);
  const place = vi.fn<(location: ActivityLocation) => boolean>(() => true);
  const r = render(ChatPanel, { store, onclose: vi.fn(), placeActivityItem: place });
  return { ex, store, place, ...r };
}
async function answer(
  store: ChatStore,
  ex: FakeExecutor,
  activity: unknown,
  query = 'folder terakhir dibuka (7 hari)',
) {
  store.send('folder apa yang saya buka minggu ini');
  await tick();
  const runId = store.runs.at(-1)!.runId;
  const requestId = ex.submitted.at(-1)!.requestId;
  ex.emit({
    runId,
    requestId,
    state: 'answered',
    activity: activity as never,
    activityQuery: query,
  });
  await tick();
  return runId;
}

describe('ChatStore: answered', () => {
  it('is terminal, labelled, re-validates the answer and frees the composer', async () => {
    const ex = new LocatingExecutor();
    const store = new ChatStore(ex);
    expect(isTerminal('answered')).toBe(true);
    expect(statusLabel('answered')).toBe('Answered from local history');
    const runId = await answer(store, ex, {
      status: 'entities',
      entries: [item()],
      truncated: false,
    });
    const run = store.run(runId)!;
    expect(run.state).toBe('answered');
    expect(run.activity?.status).toBe('entities');
    expect(store.busy).toBe(false);
    expect(store.activeRun).toBeUndefined();
    // Late updates after the terminal state are dropped.
    ex.emit({ runId, requestId: run.requestId, state: 'failed', errorMessage: 'late' });
    expect(store.run(runId)!.state).toBe('answered');
  });

  it('an answer with a locator (or any extra field) fails the run instead of being shown', async () => {
    const ex = new LocatingExecutor();
    const store = new ChatStore(ex);
    const runId = await answer(store, ex, {
      status: 'resolved',
      entry: item({ locator: 'C:\\x' }),
    });
    expect(store.run(runId)!.state).toBe('failed');
    expect(store.run(runId)!.activity).toBeUndefined();
  });

  it('chooseCandidate accepts only a listed candidate of an ambiguous answer', async () => {
    const ex = new LocatingExecutor();
    const store = new ChatStore(ex);
    const runId = await answer(store, ex, {
      status: 'ambiguous',
      candidates: [
        item({ id: 'l1', parentHint: 'C:\\A' }),
        item({ id: 'l2', parentHint: 'C:\\B' }),
      ],
    });
    store.chooseCandidate(runId, 'l9');
    expect(store.run(runId)!.chosenId).toBeUndefined();
    store.chooseCandidate(runId, 'l2');
    expect(store.run(runId)!.chosenId).toBe('l2');
  });

  it('openActivityItem: locate -> pane hand-off; a refused location leaves a note, places nothing', async () => {
    const ex = new LocatingExecutor();
    const store = new ChatStore(ex);
    const runId = await answer(store, ex, { status: 'resolved', entry: item() });
    const place = vi.fn(() => true);
    expect(await store.openActivityItem(runId, 'l1', place)).toBe(true);
    expect(place).toHaveBeenCalledWith(ex.location);
    expect(await store.openActivityItem(runId, 'other', place)).toBe(false); // not in the answer
    expect(ex.located).toEqual([[runId, 'l1']]);
    ex.location = null;
    expect(await store.openActivityItem(runId, 'l1', place)).toBe(false);
    expect(place).toHaveBeenCalledTimes(1);
    expect(store.run(runId)!.openNote).toContain('tidak dapat dibuka');
  });
});

describe('ActivityAnswerCard in ChatPanel', () => {
  it('lists items with name, parent hint, kind, availability and reasons; Open in panel uses the pane API only', async () => {
    const { ex, store, place, container } = mount();
    await answer(store, ex, {
      status: 'entities',
      entries: [
        item(),
        item({
          id: 'l2',
          displayName: 'old.pdf',
          kind: 'file',
          availability: 'missing',
          reasons: ['frequent'],
        }),
      ],
      truncated: true,
    });
    const card = screen.getByTestId('activity-answer');
    expect(card.getAttribute('aria-label')).toContain('Riwayat aktivitas');
    expect(card.querySelector('[role="status"]')?.textContent).toContain(
      'Answered from local history',
    );
    expect(screen.getByTestId('answer-query').textContent).toContain(
      'folder terakhir dibuka (7 hari)',
    );
    // FER-432: the query line is marked as the model's request, not app-verified text.
    expect(screen.getByTestId('answer-query').textContent).toMatch(/^Permintaan \(dari model\)/);
    expect(screen.getByTestId('answer-message').textContent).toContain('Masih ada item lain');
    const rows = screen.getAllByTestId('answer-item');
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain('Invoice 2026');
    expect(rows[0].textContent).toContain('\u2026\\Clients\\Acme');
    expect(rows[0].textContent).toContain('Folder');
    expect(rows[0].textContent).toContain('Tersedia');
    expect(rows[0].textContent).toContain('Baru dibuka');
    // Missing item: no open button.
    expect(rows[1].textContent).toContain('Sudah tidak ada');
    expect(rows[1].querySelector('[data-testid="open-item"]')).toBeNull();
    expect(screen.getAllByTestId('open-item')).toHaveLength(1);
    // No plan controls on an answer (Run/Edit/Cancel belong to prepared plans only).
    expect(container.querySelector('button.primary:not([type="submit"])')).toBeNull();
    expect(screen.queryByText('Cancel')).toBeNull();
    const open = screen.getByRole('button', { name: 'Buka Invoice 2026 di panel aktif' });
    await fireEvent.click(open);
    await waitFor(() => expect(place).toHaveBeenCalledTimes(1));
    expect(place).toHaveBeenCalledWith({
      path: 'C:\\Clients\\Acme\\Invoice 2026',
      parentPath: 'C:\\Clients\\Acme',
      kind: 'folder',
    });
    await waitFor(() =>
      expect(screen.getByTestId('open-note').textContent).toContain('Dibuka di panel aktif'),
    );
    expect(screen.getByTestId('answer-privacy').textContent).toContain('tidak dikirim ke provider');
    expect(screen.getByTestId('answer-privacy').textContent).toMatch(
      /pertanyaan\s+Anda tetap dikirim/,
    );
  });

  it.each([
    [{ status: 'withheld' }, 'dijeda'],
    [{ status: 'stale' }, 'Tanyakan lagi'],
    [{ status: 'rejected', reason: 'days must be 7, 30 or 90' }, 'tidak valid'],
    [{ status: 'unavailable' }, 'tidak tersedia'],
    [{ status: 'notFound' }, 'Tidak ditemukan'],
  ])('status %j shows a clear message', async (activity, text) => {
    const { ex, store } = mount();
    await answer(store, ex, activity);
    const msg = screen.getByTestId('answer-message');
    expect(msg.textContent).toContain(text);
    expect(msg.textContent).not.toContain('days must');
    if (activity.status !== 'notFound') expect(msg.getAttribute('role')).toBe('alert');
    expect(screen.queryByTestId('open-item')).toBeNull();
  });

  it('ambiguous: candidates with parent hints and a Pick button; picking focuses Open in panel', async () => {
    const { ex, store, place } = mount();
    const runId = await answer(store, ex, {
      status: 'ambiguous',
      candidates: [
        item({ id: 'l1', parentHint: '\u2026\\Acme\\Reports' }),
        item({ id: 'l2', parentHint: '\u2026\\Globex\\Reports' }),
      ],
    });
    expect(screen.getByTestId('answer-message').textContent).toContain('Pilih yang Anda maksud');
    const list = screen.getByTestId('candidates');
    expect(list.getAttribute('aria-label')).toBe('Pilih item yang dimaksud');
    expect(list.textContent).toContain('\u2026\\Acme\\Reports');
    expect(list.textContent).toContain('\u2026\\Globex\\Reports');
    expect(screen.queryByTestId('open-item')).toBeNull(); // nothing opens before a choice
    const pick = screen.getByRole('button', {
      name: 'Pilih Invoice 2026 di \u2026\\Globex\\Reports',
    });
    await fireEvent.click(pick);
    await tick();
    await tick();
    expect(store.run(runId)!.chosenId).toBe('l2');
    expect(screen.queryByTestId('candidates')).toBeNull();
    const rows = screen.getAllByTestId('answer-item');
    expect(rows).toHaveLength(1);
    expect(rows[0].textContent).toContain('Globex');
    const open = screen.getByTestId('open-item');
    await waitFor(() => expect(document.activeElement).toBe(open));
    await fireEvent.click(open);
    await waitFor(() => expect(place).toHaveBeenCalledTimes(1));
    expect((ex as LocatingExecutor).located).toEqual([[runId, 'l2']]);
  });

  it('a refused location shows a note and places nothing', async () => {
    const ex = new LocatingExecutor();
    ex.location = null;
    const { store, place } = mount(ex);
    await answer(store, ex, { status: 'resolved', entry: item() });
    await fireEvent.click(screen.getByTestId('open-item'));
    await waitFor(() =>
      expect(screen.getByTestId('open-note').textContent).toContain('tidak dapat dibuka'),
    );
    expect(place).not.toHaveBeenCalled();
  });

  it('without a locating executor there is no Open in panel button', async () => {
    const ex = new FakeExecutor();
    const { store } = mount(ex);
    await answer(store, ex, { status: 'resolved', entry: item() });
    expect(screen.getAllByTestId('answer-item')).toHaveLength(1);
    expect(screen.queryByTestId('open-item')).toBeNull();
  });

  it('keyboard inside the card does not leak to file manager shortcuts', async () => {
    const { ex, store } = mount();
    await answer(store, ex, { status: 'resolved', entry: item() });
    const spy = vi.fn();
    window.addEventListener('keydown', spy);
    await fireEvent.keyDown(screen.getByTestId('open-item'), { key: 'Delete' });
    window.removeEventListener('keydown', spy);
    expect(spy).not.toHaveBeenCalled();
  });
});

// FER-433 / ACT-08 R7 v2: "Buat ZIP" starts an ordinary archive run for one listed item.
class ZippingExecutor extends LocatingExecutor {
  zips: Array<{ requestId: string; runId: string; sourceRunId: string; entityId: string }> = [];
  accept = true;
  submitEntityArchive(req: {
    requestId: string;
    runId: string;
    sourceRunId: string;
    entityId: string;
  }): boolean {
    this.zips.push(req);
    return this.accept;
  }
}

describe('Buat ZIP (selector entity)', () => {
  it('is absent when the executor cannot start it, and for missing/offline items', async () => {
    const plain = mount(new LocatingExecutor());
    await answer(plain.store, plain.ex, {
      status: 'entities',
      entries: [item()],
      truncated: false,
    });
    expect(screen.queryByTestId('zip-item')).toBeNull();
    cleanup();

    const ex = new ZippingExecutor();
    const m = mount(ex);
    await answer(m.store, ex, {
      status: 'entities',
      entries: [item(), item({ id: 'l2', availability: 'missing' })],
      truncated: false,
    });
    expect(screen.getAllByTestId('zip-item')).toHaveLength(1);
    expect(screen.getByTestId('not-openable')).toBeTruthy();
  });

  it('starts a NEW run bound to the listed item; nothing is approved or opened by the click', async () => {
    const ex = new ZippingExecutor();
    const m = mount(ex);
    const runId = await answer(m.store, ex, {
      status: 'entities',
      entries: [item()],
      truncated: false,
    });
    await fireEvent.click(screen.getByTestId('zip-item'));
    await tick();
    expect(ex.zips).toHaveLength(1);
    expect(ex.zips[0]).toMatchObject({ sourceRunId: runId, entityId: 'l1' });
    expect(ex.zips[0].runId).not.toBe(runId);
    expect(m.store.runs).toHaveLength(2);
    expect(m.store.run(ex.zips[0].runId)?.state).toBe('planning');
    expect(m.store.messages.at(-2)).toMatchObject({
      role: 'user',
      text: 'Buat ZIP dari riwayat: Invoice 2026',
    });
    expect(ex.approved).toEqual([]);
    expect(ex.located).toEqual([]);
    expect(m.place).not.toHaveBeenCalled();
    expect(ex.submitted).toHaveLength(1); // the model/provider turn is not used
  });

  it('is disabled while another plan is active, and refused ids leave no run behind', async () => {
    const ex = new ZippingExecutor();
    const m = mount(ex);
    const runId = await answer(m.store, ex, {
      status: 'entities',
      entries: [item()],
      truncated: false,
    });
    expect(m.store.zipActivityItem(runId, 'not-listed')).toBe(false);
    ex.accept = false;
    expect(m.store.zipActivityItem(runId, 'l1')).toBe(false);
    expect(m.store.runs).toHaveLength(1);
    expect(m.store.messages.filter((x) => x.role === 'run')).toHaveLength(1);

    ex.accept = true;
    expect(m.store.zipActivityItem(runId, 'l1')).toBe(true);
    await tick();
    expect((screen.getAllByTestId('zip-item')[0] as HTMLButtonElement).disabled).toBe(true);
    expect(m.store.zipActivityItem(runId, 'l1')).toBe(false); // busy: no second plan
  });

  it('an ambiguous answer offers it only for the chosen candidate', async () => {
    const ex = new ZippingExecutor();
    const m = mount(ex);
    await answer(m.store, ex, {
      status: 'ambiguous',
      candidates: [item({ id: 'l1' }), item({ id: 'l2', parentHint: '\u2026\\Other' })],
    });
    expect(screen.queryByTestId('zip-item')).toBeNull();
    await fireEvent.click(screen.getAllByTestId('choose-candidate')[1]);
    await waitFor(() => expect(screen.getAllByTestId('zip-item')).toHaveLength(1));
    await fireEvent.click(screen.getByTestId('zip-item'));
    expect(ex.zips[0].entityId).toBe('l2');
  });
});
