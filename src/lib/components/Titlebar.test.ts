import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Titlebar from './Titlebar.svelte';

// FER-470: the custom titlebar is the only window chrome (decorations: false), so its
// buttons must reach the Tauri window API and the maximize button must reflect state.
const win = vi.hoisted(() => ({
  minimize: vi.fn().mockResolvedValue(undefined),
  toggleMaximize: vi.fn().mockResolvedValue(undefined),
  close: vi.fn().mockResolvedValue(undefined),
  isMaximized: vi.fn().mockResolvedValue(false),
  onResized: vi.fn().mockResolvedValue(() => {}),
  // Titlebar decides at module load whether it runs inside Tauri.
  tauri: ((window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}),
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => win }));

async function mount() {
  const r = render(Titlebar);
  await tick();
  await tick();
  return r;
}

describe('Titlebar window controls', () => {
  beforeEach(() => {
    win.isMaximized.mockResolvedValue(false);
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('is a drag region', async () => {
    const { container } = await mount();
    expect(container.querySelector('header.titlebar')?.hasAttribute('data-tauri-drag-region')).toBe(
      true,
    );
  });

  it('wires minimize, maximize and close to the window', async () => {
    const { getByTitle } = await mount();
    await fireEvent.click(getByTitle('Minimize'));
    await fireEvent.click(getByTitle('Maximize'));
    await fireEvent.click(getByTitle('Close'));
    expect(win.minimize).toHaveBeenCalledTimes(1);
    expect(win.toggleMaximize).toHaveBeenCalledTimes(1);
    expect(win.close).toHaveBeenCalledTimes(1);
  });

  it('shows Restore while the window is maximized', async () => {
    win.isMaximized.mockResolvedValue(true);
    const { getByTitle } = await mount();
    expect(getByTitle('Restore')).toBeTruthy();
  });
});
