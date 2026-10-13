import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import SplitDividerHost from './SplitDivider.testhost.svelte';

// jsdom has no layout: the divider measures its [data-pane-grid] parent at
// pointerdown, so the grid gets a fixed 200x200 rect for the math below.
const GRID = 200;

function stubGridRect(host: { container: HTMLElement }): void {
  const grid = host.container.querySelector('[data-pane-grid]') as HTMLElement;
  vi.spyOn(grid, 'getBoundingClientRect').mockReturnValue({
    left: 0,
    top: 0,
    width: GRID,
    height: GRID,
    right: GRID,
    bottom: GRID,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  } as DOMRect);
}

function setup(orientation: 'columns' | 'rows', ratio = 0.5) {
  const host = render(SplitDividerHost, { orientation, ratio });
  stubGridRect(host);
  const divider = host.getByRole('separator');
  // jsdom lacks the pointer-capture API — stub it on the divider element.
  divider.setPointerCapture = vi.fn();
  divider.hasPointerCapture = vi.fn(() => true);
  divider.releasePointerCapture = vi.fn();
  return { host, divider };
}

const ratioOf = (host: { container: HTMLElement }) =>
  host.container.querySelector('[data-testid="ratio"]')?.textContent;

/** Real KeyboardEvent dispatch — lets the test read defaultPrevented (the
 *  @testing-library/svelte fireEvent wrapper returns a Promise instead). */
function pressKey(divider: HTMLElement, key: string): boolean {
  const e = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
  divider.dispatchEvent(e);
  return e.defaultPrevented;
}

/** Full pointer gesture on the divider ending at (x, y) in grid space. */
function drag(divider: HTMLElement, x: number, y = 0): void {
  fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientX: 0, clientY: 0 });
  fireEvent.pointerMove(divider, { pointerId: 1, clientX: x, clientY: y });
  fireEvent.pointerUp(divider, { pointerId: 1, clientX: x, clientY: y });
}

beforeEach(() => {
  document.body.style.cursor = '';
  document.body.style.userSelect = '';
});

afterEach(() => {
  cleanup();
});

describe('SplitDivider (FER-406)', () => {
  it('exposes separator semantics for the columns orientation', () => {
    const { host, divider } = setup('columns', 0.42);
    expect(divider.getAttribute('aria-orientation')).toBe('vertical');
    expect(divider.getAttribute('aria-valuemin')).toBe('20');
    expect(divider.getAttribute('aria-valuemax')).toBe('80');
    expect(divider.getAttribute('aria-valuenow')).toBe('42');
    expect(divider.getAttribute('aria-valuetext')).toBe('42% / 58%');
    expect(divider.getAttribute('tabindex')).toBe('0');
    expect(ratioOf(host)).toBe('0.42');
  });

  it('exposes horizontal separator semantics for the rows orientation', () => {
    const { divider } = setup('rows', 0.5);
    expect(divider.getAttribute('aria-orientation')).toBe('horizontal');
    expect(divider.getAttribute('aria-valuenow')).toBe('50');
    expect(divider.getAttribute('aria-valuetext')).toBe('50% / 50%');
  });

  it('drags in columns mode: pointer x maps to the left-pane fraction', async () => {
    const { host, divider } = setup('columns', 0.5);
    drag(divider, 140); // 140/200 = 0.7
    await tick();
    expect(ratioOf(host)).toBe('0.7');
    expect(divider.hasPointerCapture(1)).toBe(true);
    expect(divider.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it('drags in rows mode: pointer y maps to the top-pane fraction', async () => {
    const { host, divider } = setup('rows', 0.5);
    drag(divider, 0, 30); // 30/200 = 0.15
    await tick();
    expect(ratioOf(host)).toBe('0.2'); // clamped to the minimum
  });

  it('clamps drag positions to [0.2, 0.8]', async () => {
    const { host, divider } = setup('columns', 0.5);
    drag(divider, 500); // far past the right edge
    await tick();
    expect(ratioOf(host)).toBe('0.8');

    drag(divider, -100); // far past the left edge
    await tick();
    expect(ratioOf(host)).toBe('0.2');
  });

  it('ignores non-primary button presses', async () => {
    const { host, divider } = setup('columns', 0.5);
    fireEvent.pointerDown(divider, { button: 2, pointerId: 1, clientX: 140, clientY: 0 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 140, clientY: 0 });
    await tick();
    expect(ratioOf(host)).toBe('0.5');
    expect(divider.setPointerCapture).not.toHaveBeenCalled();
  });

  it('stops cleanly on pointercancel — later moves never move the layout', async () => {
    const { host, divider } = setup('columns', 0.5);
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientX: 0, clientY: 0 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 140, clientY: 0 });
    fireEvent.pointerCancel(divider, { pointerId: 1 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 20, clientY: 0 });
    await tick();
    expect(ratioOf(host)).toBe('0.7'); // last value before the cancel sticks
    expect(divider.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it('stops cleanly on lostpointercapture', async () => {
    const { host, divider } = setup('columns', 0.5);
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientX: 0, clientY: 0 });
    fireEvent.lostPointerCapture(divider, { pointerId: 1 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 140, clientY: 0 });
    await tick();
    expect(ratioOf(host)).toBe('0.5');
  });

  it('double-click resets to 50/50', async () => {
    const { host, divider } = setup('columns', 0.72);
    fireEvent.dblClick(divider);
    await tick();
    expect(ratioOf(host)).toBe('0.5');
  });

  it('keyboard adjusts by 2% with clamp (columns: Left/Right, Home/End)', async () => {
    const { host, divider } = setup('columns', 0.5);

    // Handled arrows are preventDefault'ed so app-level shortcuts stay out.
    expect(pressKey(divider, 'ArrowRight')).toBe(true);
    await tick();
    expect(ratioOf(host)).toBe('0.52');
    expect(pressKey(divider, 'ArrowLeft')).toBe(true);
    await tick();
    expect(ratioOf(host)).toBe('0.5');

    expect(pressKey(divider, 'Home')).toBe(true);
    await tick();
    expect(ratioOf(host)).toBe('0.2');
    expect(pressKey(divider, 'End')).toBe(true);
    await tick();
    expect(ratioOf(host)).toBe('0.8');
    expect(pressKey(divider, 'ArrowRight')); // clamped at the max
    await tick();
    expect(ratioOf(host)).toBe('0.8');
  });

  it('keyboard adjusts by 2% with clamp (rows: Up/Down)', async () => {
    const { host, divider } = setup('rows', 0.5);
    expect(pressKey(divider, 'ArrowDown')).toBe(true);
    await tick();
    expect(ratioOf(host)).toBe('0.52');
    expect(pressKey(divider, 'ArrowUp')).toBe(true);
    await tick();
    expect(ratioOf(host)).toBe('0.5');
    // Horizontal arrows do nothing in rows mode.
    expect(pressKey(divider, 'ArrowRight')).toBe(false);
    await tick();
    expect(ratioOf(host)).toBe('0.5');
  });

  it('unrelated keys are ignored (no preventDefault, no ratio change)', async () => {
    const { host, divider } = setup('columns', 0.5);
    expect(pressKey(divider, 'a')).toBe(false);
    await tick();
    expect(ratioOf(host)).toBe('0.5');
  });

  it('suppresses text selection and swaps the cursor only while dragging', async () => {
    const { divider } = setup('columns', 0.5);
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientX: 0, clientY: 0 });
    await tick();
    expect(document.body.style.userSelect).toBe('none');
    expect(document.body.style.cursor).toBe('col-resize');

    fireEvent.pointerUp(divider, { pointerId: 1, clientX: 100, clientY: 0 });
    await tick();
    expect(document.body.style.userSelect).toBe('');
    expect(document.body.style.cursor).toBe('');
  });
});
