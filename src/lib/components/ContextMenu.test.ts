import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import ContextMenu from './ContextMenu.svelte';
import { contextMenu, type ContextMenuItem } from '../stores/contextMenuStore.svelte';

const runOpen = vi.fn();
const runCopy = vi.fn();
const runDisabled = vi.fn();
const runDelete = vi.fn();

const items: ContextMenuItem[] = [
  { label: 'Open', kbd: '↵', run: runOpen },
  { separator: true },
  { label: 'Copy to other pane', kbd: 'F5', run: runCopy },
  { label: 'Unavailable', disabled: true, run: runDisabled },
  { separator: true },
  { label: 'Delete', danger: true, run: runDelete },
];

afterEach(() => {
  cleanup();
  contextMenu.close();
  runOpen.mockClear();
  runCopy.mockClear();
  runDisabled.mockClear();
  runDelete.mockClear();
});

async function openAt(x = 40, y = 60) {
  render(ContextMenu);
  contextMenu.open(x, y, items);
  await tick();
  const menu = document.querySelector('[role="menu"]') as HTMLElement;
  expect(menu).toBeTruthy();
  return menu;
}

function itemByLabel(label: string): HTMLButtonElement {
  const el = Array.from(document.querySelectorAll('[role="menuitem"]')).find((b) =>
    b.textContent?.includes(label),
  );
  expect(el, `menu item "${label}"`).toBeTruthy();
  return el as HTMLButtonElement;
}

describe('ContextMenu rendering', () => {
  it('renders items, separators, kbd hints and position from the store', async () => {
    const menu = await openAt(40, 60);
    expect(menu.style.left).toBe('40px');
    expect(menu.style.top).toBe('60px');
    expect(document.querySelectorAll('[role="separator"]').length).toBe(2);
    expect(document.querySelectorAll('[role="menuitem"]').length).toBe(4);
    expect(itemByLabel('Copy to other pane').textContent).toContain('F5');
    expect(itemByLabel('Open').textContent).toContain('↵');
    expect(itemByLabel('Unavailable').disabled).toBe(true);
    expect(itemByLabel('Delete').className).toContain('danger');
  });

  it('renders nothing while the store is closed', () => {
    render(ContextMenu);
    expect(document.querySelector('[role="menu"]')).toBeNull();
  });

  it('does not render when the store was opened by another owner', async () => {
    render(ContextMenu, { owner: { mine: true } });
    contextMenu.open(0, 0, items, { other: true });
    await tick();
    expect(document.querySelector('[role="menu"]')).toBeNull();
  });
});

describe('ContextMenu interaction', () => {
  it('runs the clicked item exactly once and closes', async () => {
    await openAt();
    fireEvent.click(itemByLabel('Copy to other pane'));
    await tick();
    expect(runCopy).toHaveBeenCalledTimes(1);
    expect(runOpen).not.toHaveBeenCalled();
    expect(contextMenu.isOpen).toBe(false);
    expect(document.querySelector('[role="menu"]')).toBeNull();
  });

  it('never runs a disabled item and keeps the menu open', async () => {
    await openAt();
    fireEvent.click(itemByLabel('Unavailable'));
    await tick();
    expect(runDisabled).not.toHaveBeenCalled();
    expect(contextMenu.isOpen).toBe(true);
  });

  it('closes on Escape and on a mousedown outside the menu', async () => {
    await openAt();
    fireEvent.keyDown(window, { key: 'Escape' });
    await tick();
    expect(contextMenu.isOpen).toBe(false);

    await openAt();
    fireEvent.mouseDown(document.body);
    await tick();
    expect(contextMenu.isOpen).toBe(false);
  });

  it('stays open on a mousedown on the menu body, runs items on mousedown', async () => {
    const menu = await openAt();
    fireEvent.mouseDown(menu); // menu body itself, not an item
    await tick();
    expect(contextMenu.isOpen).toBe(true);
    expect(runOpen).not.toHaveBeenCalled();

    // Items activate on mousedown so a late dismissal can never swallow them.
    fireEvent.mouseDown(itemByLabel('Open'));
    await tick();
    expect(runOpen).toHaveBeenCalledTimes(1);
    expect(contextMenu.isOpen).toBe(false);
  });

  it('closes on window resize and scroll', async () => {
    await openAt();
    fireEvent(window, new Event('resize'));
    await tick();
    expect(contextMenu.isOpen).toBe(false);

    await openAt();
    fireEvent(window, new Event('scroll'));
    await tick();
    expect(contextMenu.isOpen).toBe(false);
  });
});

describe('ContextMenu keyboard', () => {
  it('focuses the first enabled item on open and moves with arrow keys (skipping disabled)', async () => {
    const menu = await openAt();
    await vi.waitFor(() => expect(document.activeElement).toBe(itemByLabel('Open')));

    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(itemByLabel('Copy to other pane'));

    fireEvent.keyDown(menu, { key: 'ArrowDown' });
    // 'Unavailable' is disabled → focus lands on 'Delete'.
    expect(document.activeElement).toBe(itemByLabel('Delete'));

    fireEvent.keyDown(menu, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(itemByLabel('Copy to other pane'));
  });

  it('wraps focus from the last item back to the first', async () => {
    const menu = await openAt();
    await vi.waitFor(() => expect(document.activeElement).toBe(itemByLabel('Open')));
    fireEvent.keyDown(menu, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(itemByLabel('Delete'));
  });
});
