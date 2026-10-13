import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import TabStrip from './TabStrip.svelte';

const tabs = [{ label: 'Alpha' }, { label: 'Beta' }, { label: 'Gamma' }];

function setup(overrides: Record<string, unknown> = {}) {
  const onselect = vi.fn();
  const onclose = vi.fn();
  const onnew = vi.fn();
  const host = render(TabStrip, { tabs, current: 1, onselect, onclose, onnew, ...overrides });
  const tab = (i: number) => host.getAllByRole('tab')[i] as HTMLElement;
  return { host, onselect, onclose, onnew, tab, newTab: host.getByTitle('New tab') };
}

/** Native MouseEvent dispatch so the test can read defaultPrevented. */
function mouseDown(el: HTMLElement, button: number): boolean {
  const e = new MouseEvent('mousedown', { button, bubbles: true, cancelable: true });
  el.dispatchEvent(e);
  return e.defaultPrevented;
}
function auxClick(el: HTMLElement, button: number): void {
  el.dispatchEvent(new MouseEvent('auxclick', { button, bubbles: true, cancelable: true }));
}

afterEach(() => {
  cleanup();
});

describe('TabStrip basics', () => {
  it('selects on click, closes via the close button, opens via the new button', async () => {
    const { onselect, onclose, onnew, tab, newTab } = setup();
    fireEvent.click(tab(0));
    expect(onselect).toHaveBeenCalledWith(0);
    fireEvent.click(newTab);
    expect(onnew).toHaveBeenCalledOnce();
    const close = tab(2).querySelector('.close') as HTMLElement;
    fireEvent.click(close);
    expect(onclose).toHaveBeenCalledWith(2);
  });

  it('marks the active tab and defaults to fit width (no fixed class)', () => {
    const { host, tab } = setup();
    expect(tab(1).getAttribute('aria-selected')).toBe('true');
    expect(tab(0).getAttribute('aria-selected')).toBe('false');
    expect(host.container.querySelector('.tabs')?.classList.contains('fixed')).toBe(false);
  });
});

describe('FER-395 fixed tab width', () => {
  it('binds the fixed class to the tabWidth prop', () => {
    const { host } = setup({ tabWidth: 'fixed' });
    expect(host.container.querySelector('.tabs')?.classList.contains('fixed')).toBe(true);
  });
});

describe('FER-395 middle-click close', () => {
  it('off by default: auxclick button 1 does nothing and mousedown is not prevented', () => {
    const { onclose, tab } = setup();
    auxClick(tab(0), 1);
    expect(onclose).not.toHaveBeenCalled();
    expect(mouseDown(tab(0), 1)).toBe(false);
  });

  it('on: auxclick button 1 closes the clicked (also inactive) tab', () => {
    const { onclose, tab } = setup({ middleClose: true });
    auxClick(tab(0), 1); // tab 0 is NOT the active one (current: 1)
    expect(onclose).toHaveBeenCalledOnce();
    expect(onclose).toHaveBeenCalledWith(0);
  });

  it('on: the button-1 mousedown is prevented (no autoscroll), other buttons ignored', () => {
    const { onclose, onselect, tab } = setup({ middleClose: true });
    expect(mouseDown(tab(0), 1)).toBe(true);
    expect(mouseDown(tab(0), 0)).toBe(false); // plain click keeps default focus behavior

    auxClick(tab(1), 0); // left auxclick must not close
    auxClick(tab(1), 2); // right click (context menu) must not close
    expect(onclose).not.toHaveBeenCalled();
    expect(onselect).not.toHaveBeenCalled();
  });

  it('on: the new-tab button never closes a tab', () => {
    const { onclose, newTab } = setup({ middleClose: true });
    auxClick(newTab, 1);
    expect(onclose).not.toHaveBeenCalled();
  });
});
