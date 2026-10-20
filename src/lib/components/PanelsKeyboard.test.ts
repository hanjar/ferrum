/**
 * FER-577 (B3): keyboard conventions of the docked Search / Compare / Sync panels and the
 * overwrite confirmation: initial focus, Esc, focus return, no modal side effects.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import SearchPanel from './SearchPanel.svelte';
import ComparePanel from './ComparePanel.svelte';
import SyncPanel from './SyncPanel.svelte';
import { searchStore } from '../stores/searchStore.svelte';
import { compareStore } from '../stores/compareStore.svelte';
import { modalDepth, resetModalDepth } from '../dialogKit';

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: vi.fn(),
  compareFiles: vi.fn(),
}));

let opener: HTMLButtonElement;

beforeEach(() => {
  resetModalDepth();
  compareStore.reset();
  searchStore.panelOpen = false;
  opener = document.createElement('button');
  opener.textContent = 'opener';
  document.body.append(opener);
  opener.focus();
});

afterEach(() => {
  cleanup();
  compareStore.reset();
  searchStore.panelOpen = false;
  opener.remove();
  resetModalDepth();
});

describe('SearchPanel', () => {
  it('puts the cursor in the pattern field and returns focus on close', async () => {
    render(SearchPanel);
    searchStore.panelOpen = true;
    await tick();
    expect(document.activeElement).toBe(screen.getByLabelText('Search pattern'));
    expect(modalDepth()).toBe(0); // docked: the global shortcuts keep working
    searchStore.panelOpen = false;
    await tick();
    expect(document.activeElement).toBe(opener);
  });

  it('Esc from the pattern field closes the panel; while searching it stops the search first', async () => {
    render(SearchPanel);
    searchStore.panelOpen = true;
    await tick();
    const input = screen.getByLabelText('Search pattern');
    const cancel = vi.spyOn(searchStore, 'cancel').mockResolvedValue();
    searchStore.running = true;
    await fireEvent.keyDown(input, { key: 'Escape' });
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(searchStore.panelOpen).toBe(true);
    searchStore.running = false;
    await fireEvent.keyDown(input, { key: 'Escape' });
    expect(searchStore.panelOpen).toBe(false);
    cancel.mockRestore();
  });

  it('Tab is not trapped (a docked panel)', async () => {
    render(SearchPanel);
    searchStore.panelOpen = true;
    await tick();
    const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    document.activeElement!.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
  });
});

describe('ComparePanel', () => {
  it('starts on Compare Directories, Esc closes, focus returns', async () => {
    render(ComparePanel);
    compareStore.panelOpen = true;
    await tick();
    const btn = screen.getByRole('button', { name: 'Compare Directories' });
    expect(document.activeElement).toBe(btn);
    expect(modalDepth()).toBe(0);
    await fireEvent.keyDown(btn, { key: 'Escape' });
    expect(compareStore.panelOpen).toBe(false);
    await tick();
    expect(document.activeElement).toBe(opener);
  });

  it('Esc while a compare runs cancels it instead of closing', async () => {
    render(ComparePanel);
    compareStore.panelOpen = true;
    await tick();
    const cancel = vi.spyOn(compareStore, 'cancelRun').mockResolvedValue();
    compareStore.busy = true;
    await fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' });
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(compareStore.panelOpen).toBe(true);
    cancel.mockRestore();
  });
});

describe('SyncPanel', () => {
  async function openSync() {
    render(SyncPanel);
    compareStore.syncOpen = true;
    await tick();
  }

  it('starts on the checked direction; Esc closes; focus returns', async () => {
    await openSync();
    const checked = document.querySelector<HTMLInputElement>(
      'input[name="sync-direction"]:checked',
    );
    expect(document.activeElement).toBe(checked);
    await fireEvent.keyDown(checked!, { key: 'Escape' });
    expect(compareStore.syncOpen).toBe(false);
    await tick();
    expect(document.activeElement).toBe(opener);
  });

  it('Esc does not close the panel while a synchronization runs', async () => {
    await openSync();
    compareStore.syncBusy = true;
    await tick();
    await fireEvent.keyDown(screen.getByLabelText('Compare contents (slower, byte-exact)'), {
      key: 'Escape',
    });
    expect(compareStore.syncOpen).toBe(true);
    compareStore.syncBusy = false;
  });

  it('the overwrite confirmation takes focus on Cancel, traps Tab, and Esc dismisses only it', async () => {
    await openSync();
    compareStore.syncConfirm = { overwrites: 2, names: ['a', 'b'] };
    await tick();
    const confirm = screen.getByRole('alertdialog', { name: 'Confirm overwrite' });
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    const overwrite = screen.getByRole('button', { name: /Overwrite 2 file/ });
    expect(document.activeElement).toBe(cancel);
    expect(modalDepth()).toBe(1);
    // Tab from the last control wraps to the first, never out of the confirmation.
    overwrite.focus();
    await fireEvent.keyDown(overwrite, { key: 'Tab' });
    expect(document.activeElement).toBe(cancel);
    await fireEvent.keyDown(cancel, { key: 'Shift' });
    await fireEvent.keyDown(cancel, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(overwrite);
    expect(confirm.contains(document.activeElement)).toBe(true);
    // Esc dismisses the confirmation and leaves the panel open.
    await fireEvent.keyDown(overwrite, { key: 'Escape' });
    expect(compareStore.syncConfirm).toBeNull();
    expect(compareStore.syncOpen).toBe(true);
    expect(modalDepth()).toBe(0);
  });

  it('Enter on the focused Cancel dismisses and does not overwrite', async () => {
    await openSync();
    const exec = vi.spyOn(compareStore, 'executeSync').mockResolvedValue(undefined as never);
    compareStore.syncConfirm = { overwrites: 1, names: ['a'] };
    await tick();
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(document.activeElement).toBe(cancel);
    // A bare Enter keydown is native button activation: no dialog handler runs it as "Overwrite".
    const ev = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    cancel.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
    expect(exec).not.toHaveBeenCalled();
    exec.mockRestore();
  });
});
