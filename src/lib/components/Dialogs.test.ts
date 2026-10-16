import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import App from '../../App.svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { ui } from '../uiState.svelte';
import { prefs } from '../stores/prefsStore.svelte';
import { activity } from '../stores/activityStore.svelte';

// FER-368: the archive IPC is mocked so the extractConflicts component flow
// can assert the exact policy that reaches the engine without a Tauri host.
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
}));

// DSK-06: the mutation IPC is mocked so delete/rename/move confirmations can
// be driven to SUCCESS here; the browser-mode default stays a rejection so
// the existing failure tests keep their behavior.
const mutMocks = vi.hoisted(() => ({
  planCopy: vi.fn(),
  createEntry: vi.fn(),
  deleteEntries: vi.fn().mockRejectedValue(new Error('unsupported')),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  renameEntry: vi.fn().mockRejectedValue(new Error('unsupported')),
}));
vi.mock('../ipc/mutations', () => mutMocks);

// DSK-06.3: the invalidation module is SPIED (not the real one — its markStale
// behavior is proven in diskUsage/invalidation.test.ts); here we assert the
// dialogs actually CALL it on success, i.e. the wiring exists.
const invMocks = vi.hoisted(() => ({
  noteScopeMutation: vi.fn(),
  recordQueuedMove: vi.fn(),
}));
vi.mock('../diskUsage/invalidation', () => invMocks);

const fsChangeSpy = vi.spyOn(activity, 'noteFsChange').mockImplementation(() => undefined);

afterEach(() => {
  fsChangeSpy.mockClear();
  dialogs.close();
  ui.activePane = 0;
  invMocks.noteScopeMutation.mockClear();
  invMocks.recordQueuedMove.mockClear();
  prefs.fileOperations.confirmBeforeDeleting = true;
  prefs.fileOperations.conflictPolicy = 'ask';
});

describe('mutation dialogs (task 10)', () => {
  it('F7 opens the New-folder dialog bound to the active pane', async () => {
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    fireEvent.keyDown(window, { key: 'F7' });
    await tick();
    const modal = document.querySelector('[role="dialog"]');
    expect(modal?.textContent).toContain('New folder');
    expect(modal?.querySelector('input')).toBeTruthy();
    // Escape closes.
    fireEvent.keyDown(modal!, { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
  });

  it('F2 acts on the cursor row and prefills the rename dialog', async () => {
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    // Nothing marked: F2 acts on the cursor row (FER-492); the first row holds it at boot.
    fireEvent.keyDown(window, { key: 'F2' });
    await tick();
    expect(dialogs.current?.kind).toBe('rename');

    dialogs.close();
    // Move the cursor onto README.md.
    const row = Array.from(document.querySelectorAll('.row[data-path]')).find((el) =>
      el.textContent?.includes('README.md'),
    ) as HTMLElement;
    fireEvent.click(row);
    await tick();
    fireEvent.keyDown(window, { key: 'F2' });
    await tick();
    expect(dialogs.current?.kind).toBe('rename');
    const input = document.querySelector('[role="dialog"] input') as HTMLInputElement;
    expect(input.value).toBe('README.md');
  });

  it('F8 with selection opens a delete confirm; failures keep the dialog and show the error', async () => {
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    const row = Array.from(document.querySelectorAll('.row[data-path]')).find((el) =>
      el.textContent?.includes('README.md'),
    ) as HTMLElement;
    fireEvent.click(row);
    await tick();
    fireEvent.keyDown(window, { key: 'F8' });
    await tick();
    expect(dialogs.current?.kind).toBe('delete');

    // Browser mode: deleteEntries rejects with 'unsupported' → dialog stays,
    // error shown, no state corrupted.
    const btn = Array.from(document.querySelectorAll('[role="dialog"] button')).find(
      (b) => b.textContent === 'Delete',
    ) as HTMLElement;
    fireEvent.click(btn);
    await vi.waitFor(() => {
      expect(document.querySelector('[role="dialog"] .error')).toBeTruthy();
    });
    expect(dialogs.current?.kind).toBe('delete'); // still open: no silent loss
  });
});

describe('FER-364: delete dialog initial permanent state', () => {
  async function openDelete(shift: boolean) {
    prefs.fileOperations.confirmBeforeDeleting = true;
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    const row = Array.from(document.querySelectorAll('.row[data-path]')).find((el) =>
      el.textContent?.includes('README.md'),
    ) as HTMLElement;
    fireEvent.click(row);
    await tick();
    fireEvent.keyDown(window, { key: 'F8', shiftKey: shift });
    await tick();
  }

  const checkbox = () =>
    document.querySelector('[role="dialog"] input[type="checkbox"]') as HTMLInputElement;

  it('F8 opens the trash delete with an unchecked permanent confirm (D05 default)', async () => {
    await openDelete(false);
    expect(dialogs.current?.kind).toBe('delete');
    expect(checkbox().checked).toBe(false);
    const heading = document.querySelector('[role="dialog"] h2');
    expect(heading?.textContent).not.toContain('permanently');
  });

  it('Shift+F8 opens the permanent delete with the confirm pre-checked', async () => {
    await openDelete(true);
    expect(dialogs.current).toMatchObject({ kind: 'delete', permanent: true });
    await vi.waitFor(() => expect(checkbox().checked).toBe(true));
    expect(document.querySelector('[role="dialog"] h2')?.textContent).toContain('permanently');
  });

  it('the checkbox stays editable and the initial state re-applies on the next open', async () => {
    await openDelete(false);
    fireEvent.click(checkbox());
    await tick();
    expect(checkbox().checked).toBe(true); // user can opt into permanent from trash delete
    // …but a NEW dialog open re-initializes from the carried flag, not from
    // leftover component state.
    dialogs.open({ kind: 'delete', paths: ['C:\\x\\a.txt'], permanent: false });
    await tick();
    expect(checkbox().checked).toBe(false);
  });
});

describe('FER-368: extractConflicts dialog (D21 one-ask)', () => {
  function openConflictDialog(run: (policy: 'skip' | 'overwrite' | 'rename') => void) {
    dialogs.open({
      kind: 'extractConflicts',
      target: 'D:\\Backup',
      conflicts: Array.from({ length: 25 }, (_, i) => ({
        memberPath: `m${i}.txt`,
        destinationPath: `D:\\Backup\\m${i}.txt`,
      })),
      run,
    });
  }

  const modal = () => document.querySelector('[role="dialog"]')!;
  const radios = () =>
    Array.from(modal().querySelectorAll<HTMLInputElement>('input[type="radio"]'));

  it('lists conflicts (capped) with Skip pre-selected as the safe default', async () => {
    render(Dialogs);
    openConflictDialog(() => {});
    await tick();
    expect(modal().querySelector('h2')?.textContent).toContain('Extract conflicts');
    const items = modal().querySelectorAll('ul.list li');
    expect(items.length).toBe(21); // 20 shown + the "… and 5 more" row
    expect(items[items.length - 1].textContent).toContain('and 5 more');
    expect(radios()[0].checked).toBe(true); // Skip
    expect(radios()[1].checked).toBe(false);
  });

  it('choosing Overwrite and submitting hands exactly that policy to the batch runner', async () => {
    render(Dialogs);
    const run = vi.fn();
    openConflictDialog(run);
    await tick();
    fireEvent.click(radios()[1]); // Overwrite
    await tick();
    fireEvent.click(
      Array.from(modal().querySelectorAll('button')).find(
        (b) => b.textContent === 'Extract',
      ) as HTMLElement,
    );
    await tick();
    expect(run).toHaveBeenCalledExactlyOnceWith('overwrite');
  });

  it('Escape/Cancel closes without running any extract (clean abort before staging)', async () => {
    render(Dialogs);
    const run = vi.fn();
    openConflictDialog(run);
    await tick();
    fireEvent.keyDown(modal(), { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
    expect(run).not.toHaveBeenCalled();
  });

  it('Enter submits the currently selected policy', async () => {
    render(Dialogs);
    const run = vi.fn();
    openConflictDialog(run);
    await tick();
    fireEvent.click(radios()[2]); // Rename automatically
    await tick();
    fireEvent.keyDown(modal(), { key: 'Enter' });
    await tick();
    expect(run).toHaveBeenCalledExactlyOnceWith('rename');
  });
});

describe('FER-549 R2-P3-3: Escape during a running delete', () => {
  it('does not close (settle) the delete dialog until deleteEntries has finished', async () => {
    render(Dialogs);
    let finish!: (r: { requested: number; deleted: number; failed: [] }) => void;
    mutMocks.deleteEntries.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const onSettled = vi.fn();
    dialogs.open({ kind: 'delete', paths: ['C://x/a.txt'], permanent: false, onSettled });
    await tick();
    const del = Array.from(document.querySelectorAll('[role="dialog"] button')).find(
      (b) => b.textContent === 'Delete',
    ) as HTMLElement;
    fireEvent.click(del);
    await tick();
    expect(mutMocks.deleteEntries).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(document.querySelector('[role="dialog"]') as HTMLElement, { key: 'Escape' });
    await tick();
    expect(dialogs.current?.kind).toBe('delete');
    expect(onSettled).not.toHaveBeenCalled();
    finish({ requested: 1, deleted: 1, failed: [] });
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(onSettled).toHaveBeenCalledTimes(1);
  });

  function startDelete(withLease: boolean) {
    let finish!: (r: { requested: number; deleted: number; failed: [] }) => void;
    mutMocks.deleteEntries.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const onSettled = vi.fn();
    dialogs.open({
      kind: 'delete',
      paths: ['C://x/a.txt'],
      permanent: false,
      ...(withLease ? { onSettled } : {}),
    });
    return { onSettled, finish: (r = { requested: 1, deleted: 1, failed: [] as [] }) => finish(r) };
  }

  async function confirmDelete() {
    await tick();
    const del = Array.from(document.querySelectorAll('[role="dialog"] button')).find(
      (b) => b.textContent === 'Delete',
    ) as HTMLElement;
    fireEvent.click(del);
    await tick();
  }

  it('a delete dialog WITHOUT a lease hook still closes on Escape while running', async () => {
    render(Dialogs);
    startDelete(false);
    await confirmDelete();
    fireEvent.keyDown(document.querySelector('[role="dialog"]') as HTMLElement, { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
  });

  it('another dialog still closes on Escape while busy', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'mkdir', parent: 'C://x' });
    await tick();
    fireEvent.keyDown(document.querySelector('[role="dialog"]') as HTMLElement, { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
  });

  it('a dialog opened mid-delete is queued; the lease settles only when the delete ends (N3)', async () => {
    render(Dialogs);
    const run = startDelete(true);
    await confirmDelete();
    dialogs.open({ kind: 'error', title: 'T', message: 'M' });
    await tick();
    expect(dialogs.current?.kind).toBe('delete');
    expect(run.onSettled).not.toHaveBeenCalled();
    run.finish();
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
    expect(run.onSettled).toHaveBeenCalledTimes(1);
  });
});

describe('DSK-06.3: completed mutations invalidate a touching scan', () => {
  async function selectReadme() {
    render(App);
    await vi.waitFor(() =>
      expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
    );
    const row = Array.from(document.querySelectorAll('.row[data-path]')).find((el) =>
      el.textContent?.includes('README.md'),
    ) as HTMLElement;
    fireEvent.click(row);
    await tick();
  }

  function primaryButton(label: string): HTMLElement {
    return Array.from(document.querySelectorAll('[role="dialog"] button')).find(
      (b) => b.textContent === label,
    ) as HTMLElement;
  }

  it('a successful delete through the EXISTING confirm calls the invalidation hook', async () => {
    await selectReadme();
    mutMocks.deleteEntries.mockResolvedValueOnce({ requested: 1, deleted: 1, failed: [] });
    fireEvent.keyDown(window, { key: 'F8' });
    await tick();
    expect(dialogs.current?.kind).toBe('delete');
    fireEvent.click(primaryButton('Delete'));
    await vi.waitFor(() => expect(invMocks.noteScopeMutation).toHaveBeenCalled());
    expect(invMocks.noteScopeMutation).toHaveBeenCalledWith(['C:\\Projects\\README.md']);
    expect(fsChangeSpy).toHaveBeenCalled();
    expect(dialogs.current).toBeNull(); // success closes the dialog as before
  });

  it('a successful rename reports the old AND the new path', async () => {
    await selectReadme();
    mutMocks.renameEntry.mockResolvedValueOnce({
      name: 'renamed.md',
      path: 'C:\\Projects\\renamed.md',
      isDir: false,
      isSymlink: false,
      size: 10,
      modifiedMs: 1,
      hidden: false,
    });
    fireEvent.keyDown(window, { key: 'F2' });
    await tick();
    fireEvent.click(primaryButton('Rename'));
    await vi.waitFor(() => expect(invMocks.noteScopeMutation).toHaveBeenCalled());
    expect(invMocks.noteScopeMutation).toHaveBeenCalledWith([
      'C:\\Projects\\README.md',
      'C:\\Projects\\renamed.md',
    ]);
    expect(fsChangeSpy).toHaveBeenCalled(); // sidebar Recent/Frequent labels refresh
  });

  it('a failed delete (nothing removed) never invalidates', async () => {
    await selectReadme();
    mutMocks.deleteEntries.mockResolvedValueOnce({
      requested: 1,
      deleted: 0,
      failed: [{ path: 'C:\\Projects\\README.md', code: 'permissionDenied', message: 'locked' }],
    });
    fireEvent.keyDown(window, { key: 'F8' });
    await tick();
    fireEvent.click(primaryButton('Delete'));
    await vi.waitFor(() => expect(document.querySelector('[role="dialog"] .error')).toBeTruthy());
    expect(invMocks.noteScopeMutation).not.toHaveBeenCalled();
  });

  it('an accepted move records the queued op for terminal-state observation', async () => {
    await selectReadme();
    mutMocks.planCopy.mockResolvedValueOnce({ totalFiles: 1, totalBytes: 10, conflicts: [] });
    mutMocks.enqueueMove.mockResolvedValueOnce('op-42');
    fireEvent.keyDown(window, { key: 'F6' });
    await tick();
    expect(dialogs.current?.kind).toBe('move');
    fireEvent.click(primaryButton('Move'));
    await vi.waitFor(() => expect(invMocks.recordQueuedMove).toHaveBeenCalled());
    expect(invMocks.recordQueuedMove).toHaveBeenCalledWith(
      'op-42',
      ['C:\\Projects\\README.md'],
      'D:\\Backup',
    );
  });
});

describe('FER-460: fast scan offer dialog', () => {
  function open() {
    const choose = vi.fn();
    render(Dialogs);
    dialogs.open({ kind: 'fastScanOffer', root: 'C:/Users', volume: 'C', choose });
    return choose;
  }
  const button = (label: string) =>
    Array.from(document.querySelectorAll('[role="dialog"] button')).find((b) =>
      b.textContent?.includes(label),
    ) as HTMLButtonElement;

  it('offers fast + regular scan; the checkbox rides along with the choice', async () => {
    const choose = open();
    await tick();
    expect(document.querySelector('[role="dialog"] h2')?.textContent).toContain('Scan lebih cepat');
    const box = document.querySelector(
      '[role="dialog"] input[type="checkbox"]',
    ) as HTMLInputElement;
    expect(box.checked).toBe(false);
    fireEvent.click(box);
    await tick();
    fireEvent.click(button('Scan biasa'));
    expect(choose).toHaveBeenCalledWith('normal', true);
  });

  it('Scan cepat needs an explicit click; a bare Enter is the safe regular scan (L7)', async () => {
    const choose = open();
    await tick();
    await vi.waitFor(() => expect(document.activeElement?.textContent).toContain('Scan biasa'));
    fireEvent.keyDown(document.querySelector('[role="dialog"]') as HTMLElement, { key: 'Enter' });
    expect(choose).toHaveBeenLastCalledWith('normal', false);
    // Enter on a focused button is left to that button (not intercepted).
    const fast = button('Scan cepat');
    const ev = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    fast.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
    fireEvent.click(fast);
    expect(choose).toHaveBeenLastCalledWith('fast', false);
  });

  it('Cancel/Escape settles the offer as dismissed (no scan)', async () => {
    const choose = open();
    await tick();
    fireEvent.keyDown(document.querySelector('[role="dialog"]') as HTMLElement, { key: 'Escape' });
    expect(choose).toHaveBeenCalledWith(null, false);
    expect(dialogs.current).toBeNull();
  });
});
