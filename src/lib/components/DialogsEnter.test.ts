/**
 * FER-577 (X2): Enter in Dialogs.svelte. A focused button / link / select / textarea keeps
 * its native Enter behaviour; the dialog's primary action only runs from the dialog itself or
 * from a text field / radio. Enter on Cancel in the Delete dialog must NEVER delete.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { prefs } from '../stores/prefsStore.svelte';

const mutMocks = vi.hoisted(() => ({
  planCopy: vi.fn(),
  createEntry: vi.fn(),
  deleteEntries: vi.fn().mockResolvedValue({ requested: 1, failed: [] }),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  renameEntry: vi.fn(),
}));
vi.mock('../ipc/mutations', () => mutMocks);
vi.mock('../ipc/clipboardFiles', () => ({ clipboardFinishCut: vi.fn() }));
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
  newArchiveRequestId: () => 'req-enter',
  listenArchiveProgress: vi.fn(async () => () => {}),
}));

afterEach(() => {
  dialogs.close();
  prefs.fileOperations.confirmBeforeDeleting = true;
  mutMocks.deleteEntries.mockClear();
  mutMocks.enqueueCopy.mockClear();
});

const modal = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const button = (label: string) =>
  Array.from(modal().querySelectorAll('button')).find((b) => b.textContent?.trim() === label)!;

/** Dispatch a real, cancelable Enter keydown from `el`; true = the dialog did NOT swallow it. */
function enterOn(el: Element): boolean {
  const ev = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
  el.dispatchEvent(ev);
  return !ev.defaultPrevented;
}

describe('FER-577 X2: Enter in the Delete dialog', () => {
  async function openDelete() {
    render(Dialogs);
    dialogs.open({ kind: 'delete', paths: ['C:/x/a.txt'], permanent: false });
    await tick();
  }

  it('Enter on the focused Cancel button does NOT delete (native button activation)', async () => {
    await openDelete();
    const cancel = button('Cancel');
    cancel.focus();
    expect(enterOn(cancel)).toBe(true); // left to the button itself
    await tick();
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
    expect(dialogs.current?.kind).toBe('delete');
  });

  it('Enter on the "permanently" checkbox does NOT delete either', async () => {
    await openDelete();
    const box = modal().querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    box.focus();
    enterOn(box);
    await tick();
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
    expect(box.checked).toBe(false);
  });

  it('a bare Enter on the dialog itself still confirms (keyboard F8 + Enter)', async () => {
    await openDelete();
    await vi.waitFor(() => expect(document.activeElement).toBe(modal()));
    fireEvent.keyDown(modal(), { key: 'Enter' });
    await vi.waitFor(() =>
      expect(mutMocks.deleteEntries).toHaveBeenCalledExactlyOnceWith(['C:/x/a.txt'], false),
    );
  });
});

describe('FER-577 X2: buttons keep native Enter in the other dialogs', () => {
  it('Pack: Enter on Cancel does not start the pack', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'pack', paths: ['C:/x/a.txt'], target: 'C:/x/a.zip', format: 'zip' });
    await tick();
    const cancel = button('Cancel');
    cancel.focus();
    expect(enterOn(cancel)).toBe(true);
    await tick();
    expect(dialogs.current?.kind).toBe('pack'); // not replaced by archiveProgress
  });

  it('error dialog: bare Enter closes, Enter on OK is native', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'error', title: 'Oops', message: 'boom' });
    await tick();
    expect(enterOn(button('OK'))).toBe(true);
    expect(dialogs.current?.kind).toBe('error');
    fireEvent.keyDown(modal(), { key: 'Enter' });
    await tick();
    expect(dialogs.current).toBeNull();
  });

  it('quitConfirm: a bare Enter does not quit', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'quitConfirm', operations: 2 });
    await tick();
    fireEvent.keyDown(modal(), { key: 'Enter' });
    await tick();
    expect(dialogs.current?.kind).toBe('quitConfirm');
  });

  it('copy conflict dialog: Enter submits the chosen policy', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'copy',
      paths: ['C:/x/a.txt'],
      target: 'D:/t',
      totalFiles: 1,
      totalBytes: 1,
      conflicts: [{ dest: 'D:/t/a.txt' }],
    } as never);
    await tick();
    const radio = modal().querySelector<HTMLInputElement>('input[type="radio"][value="skip"]')!;
    fireEvent.click(radio);
    await tick();
    fireEvent.keyDown(radio, { key: 'Enter' });
    await vi.waitFor(() => expect(mutMocks.enqueueCopy).toHaveBeenCalled());
    expect(mutMocks.enqueueCopy.mock.calls[0][2]).toBe('skip');
  });
});
