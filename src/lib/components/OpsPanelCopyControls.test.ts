/**
 * FER-471: live copy-thread controls in the Background Transfer Manager. The UI only asks the
 * backend (copy_control_adjust) and shows the state the backend returns.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import OpsPanel from './OpsPanel.svelte';
import { ops } from '../stores/opsStore.svelte';
import type { CopyAdjust, CopyControlState } from '../ipc/copyControl';
import type { OperationProgress } from '../ipc/types';

const adjustMock = vi.fn<(id: string, change: CopyAdjust) => Promise<CopyControlState>>();

vi.mock('../ipc/copyControl', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../ipc/copyControl')>()),
  copyControlAdjust: (id: string, change: CopyAdjust) => adjustMock(id, change),
  copyControlSnapshot: async () => [],
}));

const running = (over: Partial<OperationProgress> = {}): OperationProgress => ({
  operationId: 'op_1',
  kind: 'copy',
  state: 'running',
  processedBytes: 10,
  totalBytes: 100,
  processedFiles: 1,
  totalFiles: 10,
  ...over,
});

const control = (over: Partial<CopyControlState> = {}): CopyControlState => ({
  operationId: 'op_1',
  auto: true,
  threads: 4,
  activeWorkers: 4,
  autoMax: 16,
  media: 'ssd',
  priority: 'normal',
  running: true,
  ...over,
});

beforeEach(() => {
  ops.ops.clear();
  ops.copyControls.clear();
  ops.panelOpen = true;
  adjustMock.mockReset();
  ops.ops.set('op_1', running());
});
afterEach(() => {
  cleanup();
  ops.panelOpen = false;
  ops.ops.clear();
  ops.copyControls.clear();
});

describe('copy controls in the progress panel', () => {
  it('an open panel follows progress events (a plain Map inside $state is not reactive)', async () => {
    ops.ops.clear();
    render(OpsPanel);
    expect(screen.queryByText('Cancel')).toBeNull();
    ops.applyProgress(running({ operationId: 'op_live', processedBytes: 10, totalBytes: 100 }));
    await vi.waitFor(() => expect(screen.getByText('Cancel')).toBeTruthy());
    ops.applyProgress(running({ operationId: 'op_live', state: 'completed' }));
    await vi.waitFor(() => expect(screen.queryByText('Cancel')).toBeNull());
    expect(screen.getByText(/Completed/)).toBeTruthy();
  });

  it('shows nothing until the backend reports a running copy control', () => {
    render(OpsPanel);
    expect(screen.queryByLabelText('Kontrol thread copy')).toBeNull();
  });

  it('shows threads, Otomatis and priority from the backend state', async () => {
    ops.applyCopyControl(control());
    render(OpsPanel);
    await tick();
    expect(screen.getByTestId('copy-threads').textContent).toBe('4');
    expect((screen.getByLabelText('Otomatis') as HTMLInputElement).checked).toBe(true);
    expect((screen.getByLabelText('Prioritas copy') as HTMLSelectElement).value).toBe('normal');
    expect(screen.getByTestId('copy-hint').textContent).toContain('SSD');
  });

  it('[+] and [-] ask the backend and show the confirmed number', async () => {
    ops.applyCopyControl(control());
    adjustMock.mockImplementation(async (_id, change) =>
      control({
        auto: false,
        threads: change.kind === 'up' ? 5 : 3,
        activeWorkers: change.kind === 'up' ? 5 : 4,
      }),
    );
    render(OpsPanel);
    await fireEvent.click(screen.getByLabelText('Tambah thread'));
    expect(adjustMock).toHaveBeenLastCalledWith('op_1', { kind: 'up' });
    await vi.waitFor(() => expect(screen.getByTestId('copy-threads').textContent).toBe('5'));
    expect((screen.getByLabelText('Otomatis') as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(screen.getByLabelText('Kurangi thread'));
    expect(adjustMock).toHaveBeenLastCalledWith('op_1', { kind: 'down' });
    await vi.waitFor(() => expect(screen.getByTestId('copy-threads').textContent).toBe('3'));
    expect(screen.getByTestId('copy-hint').textContent).toContain('4 aktif dari 3');
  });

  it('changing priority and toggling Otomatis send the matching change', async () => {
    ops.applyCopyControl(control({ auto: false }));
    adjustMock.mockImplementation(async () => control({ auto: false, priority: 'background' }));
    render(OpsPanel);
    await fireEvent.change(screen.getByLabelText('Prioritas copy'), {
      target: { value: 'background' },
    });
    expect(adjustMock).toHaveBeenLastCalledWith('op_1', {
      kind: 'priority',
      priority: 'background',
    });
    await vi.waitFor(() =>
      expect((screen.getByLabelText('Prioritas copy') as HTMLSelectElement).value).toBe(
        'background',
      ),
    );
    await fireEvent.click(screen.getByLabelText('Otomatis'));
    expect(adjustMock).toHaveBeenLastCalledWith('op_1', { kind: 'auto', enabled: true });
  });

  it('disables [-] at one thread and [+] at sixteen', async () => {
    ops.applyCopyControl(control({ threads: 1, auto: false }));
    render(OpsPanel);
    expect((screen.getByLabelText('Kurangi thread') as HTMLButtonElement).disabled).toBe(true);
    cleanup();
    ops.applyCopyControl(control({ threads: 16, auto: false }));
    render(OpsPanel);
    expect((screen.getByLabelText('Tambah thread') as HTMLButtonElement).disabled).toBe(true);
  });

  it('a job that ended loses its controls (rejected adjust or running=false event)', async () => {
    ops.applyCopyControl(control());
    adjustMock.mockRejectedValue(new Error('No running copy job'));
    render(OpsPanel);
    await fireEvent.click(screen.getByLabelText('Tambah thread'));
    await vi.waitFor(() => expect(screen.queryByLabelText('Kontrol thread copy')).toBeNull());
    ops.applyCopyControl(control());
    ops.applyCopyControl(control({ running: false }));
    expect(ops.copyControlOf('op_1')).toBeUndefined();
  });

  it('ignores malformed control payloads', () => {
    ops.applyCopyControl({ operationId: 'op_1', threads: 'many' });
    ops.applyCopyControl(null);
    expect(ops.copyControls.size).toBe(0);
  });
});
