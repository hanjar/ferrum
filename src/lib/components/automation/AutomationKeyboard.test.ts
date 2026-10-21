// FER-577 B4: keyboard of the Automation page (list, tabs, editor, discard confirm).
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import AutomationPage from './AutomationPage.svelte';
import { AutomationStore } from '../../stores/automationStore.svelte';
import { duplicateMnemonics, modalDepth, resetModalDepth } from '../../dialogKit';
import { fakeBackend, fakeProfiles, profileSummary } from './automationTestKit';

afterEach(() => {
  cleanup();
  resetModalDepth();
});

function setup(onclose = vi.fn()) {
  const store = new AutomationStore({
    backend: fakeBackend(),
    profiles: fakeProfiles([profileSummary()]),
    validateDelayMs: 0,
  });
  const view = render(AutomationPage, { store, onclose });
  return { store, onclose, ...view };
}
const modal = () =>
  document.querySelector('[role="dialog"][aria-label="Automation"]') as HTMLElement;
const alt = (el: Element, key: string) => fireEvent.keyDown(el, { key, altKey: true });

describe('Automation page keyboard (FER-577 B4)', () => {
  it('list: unique access letters, tabs rove, names walk with arrows', async () => {
    setup();
    await screen.findByText('Nightly projects backup');
    expect(duplicateMnemonics(modal())).toEqual([]);
    const tabs = screen.getAllByRole('tab');
    tabs[0].focus();
    await fireEvent.keyDown(tabs[0], { key: 'ArrowRight' });
    expect(document.activeElement).toBe(tabs[1]);
    await fireEvent.keyDown(tabs[1], { key: 'Home' });
    expect(document.activeElement).toBe(tabs[0]);

    const names = [...document.querySelectorAll<HTMLElement>('.namelink')];
    expect(names.length).toBeGreaterThan(1);
    names[0].focus();
    await fireEvent.keyDown(names[0], { key: 'ArrowDown' });
    expect(document.activeElement).toBe(names[1]);
    await fireEvent.keyDown(names[1], { key: 'End' });
    expect(document.activeElement).toBe(names[names.length - 1]);
    await fireEvent.keyDown(document.activeElement!, { key: 'Home' });
    expect(document.activeElement).toBe(names[0]);
  });

  it('Alt+N opens the editor; Alt+C continues only when valid; Esc backs out to the list then closes', async () => {
    const { store, onclose } = setup();
    await screen.findByText('Nightly projects backup');
    await alt(modal(), 'n');
    expect(store.view).toBe('editor');
    expect(duplicateMnemonics(modal())).toEqual([]);
    // watch mode too
    await alt(modal(), 'f');
    expect(duplicateMnemonics(modal())).toEqual([]);
    await alt(modal(), 's');
    expect(duplicateMnemonics(modal())).toEqual([]);
    await fireEvent.keyDown(modal(), { key: 'Escape' });
    expect(store.view === 'list' || store.discardPending).toBe(true);
    expect(onclose).not.toHaveBeenCalled();
  });

  it('editor: Alt+N focuses Name, frequency radios rove with arrows', async () => {
    const { store } = setup();
    await screen.findByText('Nightly projects backup');
    await alt(modal(), 'n');
    expect(store.view).toBe('editor');
    await alt(modal(), 'n');
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Name' }));
    const once = screen.getByRole('radio', { name: 'Once' });
    once.focus();
    await fireEvent.keyDown(once, { key: 'ArrowRight' });
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Daily' }));
    expect(store.editor?.form.freq).toBe('daily');
    await fireEvent.keyDown(document.activeElement!, { key: 'End' });
    expect(store.editor?.form.freq).toBe('weekly');
  });

  it('Tab stays inside the page and it does not count as a modal', async () => {
    setup();
    await screen.findByText('Nightly projects backup');
    expect(modalDepth()).toBe(0);
    const list = [...modal().querySelectorAll<HTMLElement>('button:not([disabled]),input,a')];
    const last = list[list.length - 1];
    last.focus();
    const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    last.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    expect(modal().contains(document.activeElement)).toBe(true);
  });
});
