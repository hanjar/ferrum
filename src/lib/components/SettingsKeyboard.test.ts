/**
 * FER-577 (B3): Settings keyboard - category mnemonics and arrows, Ctrl+PageUp/Down, Esc, segmented
 * groups. (The privacy confirmation is covered in ActivityPrivacyKeyboard.test.ts.)
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import SettingsDialog from './SettingsDialog.svelte';
import { duplicateMnemonics, modalDepth, resetModalDepth } from '../dialogKit';
import { ui } from '../uiState.svelte';

let opener: HTMLButtonElement;

beforeEach(() => {
  resetModalDepth();
  opener = document.createElement('button');
  document.body.append(opener);
  opener.focus();
});
afterEach(() => {
  cleanup();
  opener.remove();
  resetModalDepth();
});

function open() {
  const onclose = vi.fn();
  render(SettingsDialog, { onclose });
  return { onclose, modal: screen.getByRole('dialog', { name: 'Settings' }) };
}
const cat = (name: string) => screen.getByRole('button', { name });
const current = () => document.querySelector('.pHead h3')?.textContent;

describe('Settings categories', () => {
  it('has no access letter used twice', () => {
    const { modal } = open();
    expect(duplicateMnemonics(modal)).toEqual([]);
    for (const [name, key] of [
      ['Appearance', 'a'],
      ['Tabs', 't'],
      ['File Operations', 'f'],
      ['Preview', 'p'],
      ['Hotlist', 'h'],
      ['Indexer', 'x'],
      ['Archive', 'r'],
      ['AI', 'i'],
    ]) {
      expect(cat(name).dataset.mnemonic, name).toBe(key);
    }
    // Placeholders have none.
    expect(cat('Network').dataset.mnemonic).toBeUndefined();
  });

  it('Alt+letter jumps to the category', async () => {
    const { modal } = open();
    await fireEvent.keyDown(modal, { key: 't', altKey: true });
    expect(document.activeElement).toBe(cat('Tabs'));
    expect(current()).toBe('Tabs');
  });

  it('is one tab stop; arrows move through the active categories and skip the disabled ones', async () => {
    open();
    expect(cat('Appearance').tabIndex).toBe(0);
    expect(cat('Tabs').tabIndex).toBe(-1);
    cat('Appearance').focus();
    await fireEvent.keyDown(cat('Appearance'), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(cat('Tabs'));
    await fireEvent.keyDown(cat('Tabs'), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(cat('File Operations'));
    // Keyboard Shortcuts is a disabled placeholder: skipped.
    await fireEvent.keyDown(cat('File Operations'), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(cat('Preview'));
    await fireEvent.keyDown(cat('Preview'), { key: 'End' });
    expect(document.activeElement).toBe(cat('AI'));
    await fireEvent.keyDown(cat('AI'), { key: 'Home' });
    expect(document.activeElement).toBe(cat('Appearance'));
  });

  it('ArrowRight on a category enters its first control', async () => {
    open();
    cat('Tabs').focus();
    await fireEvent.keyDown(cat('Tabs'), { key: 'ArrowRight' });
    expect(current()).toBe('Tabs');
    expect(document.activeElement).toBe(screen.getByLabelText('Open new tabs next to current'));
  });

  it('Ctrl+PageDown / Ctrl+PageUp step through categories and wrap, focusing the first control', async () => {
    const { modal } = open();
    cat('Appearance').focus();
    await fireEvent.keyDown(modal, { key: 'PageDown', ctrlKey: true });
    expect(current()).toBe('Tabs');
    expect(document.activeElement).toBe(screen.getByLabelText('Open new tabs next to current'));
    await fireEvent.keyDown(modal, { key: 'PageUp', ctrlKey: true });
    expect(current()).toBe('Appearance');
    await fireEvent.keyDown(modal, { key: 'PageUp', ctrlKey: true });
    expect(current()).toBe('AI'); // wraps to the last active category
  });
});

describe('Settings dialog frame', () => {
  it('Esc closes; focus starts inside and returns to the opener; Tab stays in', async () => {
    const { onclose, modal } = open();
    expect(modal.contains(document.activeElement)).toBe(true);
    expect(modalDepth()).toBe(1);
    await fireEvent.keyDown(modal, { key: 'Escape' });
    expect(onclose).toHaveBeenCalledTimes(1);
    cleanup();
    expect(document.activeElement).toBe(opener);
  });

  it('segmented groups move AND select with the arrows (theme)', async () => {
    open();
    const dark = screen.getByRole('radio', { name: 'Dark' });
    const light = screen.getByRole('radio', { name: 'Light' });
    dark.focus();
    ui.setTheme('dark');
    await fireEvent.keyDown(dark, { key: 'ArrowRight' });
    expect(ui.theme).toBe('light');
    expect(document.activeElement).toBe(light);
    await fireEvent.keyDown(light, { key: 'Home' });
    expect(ui.theme).toBe('dark');
  });
});
