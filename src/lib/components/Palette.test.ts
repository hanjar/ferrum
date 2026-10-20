import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import App from '../../App.svelte';
import { ui } from '../uiState.svelte';

afterEach(() => {
  ui.paletteOpen = false;
  ui.activePane = 0;
});

async function boot() {
  const r = render(App);
  await vi.waitFor(() =>
    expect(document.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
  );
  return r;
}

function paletteInput(): HTMLInputElement {
  return document.querySelector('.palette input') as HTMLInputElement;
}

describe('command palette (task 13)', () => {
  it('Ctrl+K opens the palette; Escape closes it', async () => {
    await boot();
    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    await tick();
    expect(ui.paletteOpen).toBe(true);
    expect(paletteInput()).toBeTruthy();
    fireEvent.keyDown(document.querySelector('.palette')!, { key: 'Escape' });
    await tick();
    expect(ui.paletteOpen).toBe(false);
  });

  it('typing filters; Enter runs the highlighted command', async () => {
    await boot();
    ui.paletteOpen = true;
    await tick();
    const input = paletteInput();
    fireEvent.input(input, { target: { value: 'switch pane' } });
    await tick();
    const items = document.querySelectorAll('.palette .item');
    expect(items.length).toBe(1);
    const before = ui.activePane;
    fireEvent.keyDown(document.querySelector('.palette')!, { key: 'Enter' });
    await tick();
    expect(ui.paletteOpen).toBe(false);
    expect(ui.activePane).not.toBe(before); // Switch to other pane (Tab) ran
  });

  it('arrow keys move the highlight', async () => {
    await boot();
    ui.paletteOpen = true;
    await tick();
    const pal = document.querySelector('.palette')!;
    const active = () =>
      Array.from(pal.querySelectorAll('.item')).findIndex((el) =>
        (el as HTMLElement).classList.contains('active'),
      );
    expect(active()).toBe(0);
    fireEvent.keyDown(pal, { key: 'ArrowDown' });
    await tick();
    expect(active()).toBe(1);
    fireEvent.keyDown(pal, { key: 'ArrowUp' });
    await tick();
    expect(active()).toBe(0);
  });
  it('Enter runs filtered result after previous highlight exceeds new result count', async () => {
    await boot();
    ui.paletteOpen = true;
    await tick();
    const pal = document.querySelector('.palette')!;
    fireEvent.keyDown(pal, { key: 'ArrowDown' });
    fireEvent.keyDown(pal, { key: 'ArrowDown' });
    const input = paletteInput();
    fireEvent.input(input, { target: { value: 'switch pane' } });
    await tick();
    const before = ui.activePane;
    fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    expect(ui.activePane).not.toBe(before);
  });

  it('shortcuts do not steal typing inside the palette input', async () => {
    await boot();
    ui.paletteOpen = true;
    await tick();
    const input = paletteInput();
    // F7 inside the palette input must NOT open the mkdir dialog.
    fireEvent.keyDown(input, { key: 'F7' });
    await tick();
    expect(document.querySelector('[role="dialog"] #dlg-title')).toBeNull();
  });
});

describe('keyboard-only MVP flow (task 13 acceptance)', () => {
  it('browse → select → copy dialog can be driven without a mouse', async () => {
    await boot();
    // keyboard: move cursor + select via ArrowDown
    const body = document.querySelectorAll('[role="grid"]')[0] as HTMLElement;
    body.focus();
    fireEvent.keyDown(body, { key: 'ArrowDown' }); // list owns arrow keys
    await tick();
    // Space marks the cursor row (status bar); F5 then copies the marked row
    fireEvent.keyDown(body, { key: 'ArrowUp' });
    fireEvent.keyDown(body, { key: ' ' });
    await tick();
    const info = document.querySelectorAll('.status .info')[0]?.textContent;
    expect(info).toMatch(/1 of \d+ selected/);
    // F5 opens the copy dialog against the other pane
    fireEvent.keyDown(window, { key: 'F5' });
    await vi.waitFor(() => {
      expect(document.querySelector('[role="dialog"]')?.textContent).toContain('Copy');
    });
  });
});
