// FER-576: the delete card is a WARNING card, and a chain step card says which step it is.
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import PlanCard from './PlanCard.svelte';
import { ChatStore, type RunView } from '../../ai/chatStore.svelte';
import { FakeExecutor, samplePlan } from '../../ai/fixtures/chatFixtures';

function deletePlan(count = 3) {
  return samplePlan({
    capability: 'items.delete',
    operation: { kind: 'items.delete', sourceManifest: 'man-del', sourceDirectory: 'D:\\Work' },
    preview: { itemCount: count, conflictCount: 0, totalBytes: 0 },
    steps: [{ id: 's1', capability: 'items.delete', dependsOn: [] }],
  });
}

async function mount(
  extra: Record<string, unknown> = {},
  rows = [{ ref: 'a', label: 'pc_202508014.csv', detail: 'file' }],
) {
  const ex = new FakeExecutor();
  ex.itemTotal = 0;
  ex.listItems = async () => ({ rows, total: rows.length + (extra.moreItems ? 5 : 0) });
  const store = new ChatStore(ex);
  store.send('hapus yang terpilih');
  await tick();
  const runId = store.runs.at(-1)!.runId;
  ex.emit({
    runId,
    requestId: ex.submitted.at(-1)!.requestId,
    state: 'awaitingApproval',
    plan: deletePlan(),
    warnings: ['Item akan dipindahkan ke Recycle Bin (bukan hapus permanen).'],
    ...extra,
  });
  await tick();
  const run = store.runs.at(-1) as RunView;
  const view = render(PlanCard, { run, store });
  await tick();
  await tick();
  return { ex, store, run, ...view };
}

afterEach(cleanup);

describe('PlanCard: items.delete warning card', () => {
  it('shows a clear warning, the count, the item names and "Recycle Bin"', async () => {
    await mount();
    const warning = screen.getByTestId('delete-warning');
    expect(warning.getAttribute('role')).toBe('alert');
    expect(warning.textContent).toContain('Peringatan');
    expect(warning.textContent).toContain('3 item akan dipindahkan ke Recycle Bin');
    expect(warning.textContent?.replace(/\s+/g, ' ')).toContain('bukan hapus permanen');
    expect(screen.getByTestId('delete-items').textContent).toContain('pc_202508014.csv');
    expect(screen.getByText('Recycle Bin')).toBeTruthy();
  });

  it('has an explicit confirm button that is not the default focus, and Cancel comes first', async () => {
    const { ex } = await mount();
    const confirm = screen.getByTestId('confirm-delete');
    expect(confirm.textContent).toContain('Pindahkan ke Recycle Bin');
    expect(confirm.className).toContain('danger');
    expect(document.activeElement).not.toBe(confirm);
    expect(confirm.hasAttribute('autofocus')).toBe(false);
    const buttons = screen.getAllByRole('button').map((b) => b.textContent?.trim());
    expect(buttons.indexOf('Cancel')).toBeLessThan(buttons.indexOf('Pindahkan ke Recycle Bin'));
    expect(screen.queryByRole('button', { name: 'Run' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    await fireEvent.click(confirm);
    expect(ex.approved).toHaveLength(1);
  });

  it('does not run on its own: nothing is approved just by showing the card', async () => {
    const { ex } = await mount();
    expect(ex.approved).toEqual([]);
  });

  it('says how many more items there are beyond the names shown', async () => {
    await mount({ moreItems: true });
    expect(screen.getByTestId('delete-warning').textContent).toContain('dan 5 item lainnya');
  });

  it('shows which step of the chain it is and the verified-archive note', async () => {
    await mount({
      chain: { index: 2, total: 3, note: 'Arsip x.7z sudah dibuat dan lolos uji arsip (1 entri).' },
    });
    expect(screen.getByTestId('chain-step').textContent).toContain('Langkah 3 dari 3');
    expect(screen.getByTestId('chain-note').textContent).toContain('lolos uji arsip');
  });
});
