import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';

vi.mock('../ipc/activity', () => ({
  activityAckDisclosure: vi.fn(),
  activityAddFavorite: vi.fn(),
  activityFavorites: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(async () => []),
  activityRemoveFavorite: vi.fn(async () => true),
  activitySetHidden: vi.fn(async () => null),
  activityState: vi.fn(async () => null),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: vi.fn(async () => true),
}));
vi.mock('../stores/panesRegistry.svelte', () => ({
  activePane: () => ({ path: '/work', openPath: vi.fn() }),
}));

const flag = vi.hoisted(() => ({ on: false }));
vi.mock('../featureFlags', () => ({
  get automationUiEnabled() {
    return flag.on;
  },
  flagOn: () => flag.on,
}));

import Sidebar from './Sidebar.svelte';
import { ui } from '../uiState.svelte';
import { automation } from '../stores/automationStore.svelte';

afterEach(() => {
  cleanup();
  flag.on = false;
  automation.rows = [];
});

function mountSidebar(on: boolean) {
  flag.on = on;
  render(Sidebar);
  return { ui, automation };
}

describe('Sidebar Automation entry (feature flag)', () => {
  it('flag OFF (the release default): the entry stays disabled and cannot open the page', async () => {
    const { ui } = mountSidebar(false);
    const btn = screen.getByRole('button', { name: /Automation/ }) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    await fireEvent.click(btn);
    expect(ui.automationOpen).toBe(false);
  });

  it('flag ON: the entry opens the Automation page and shows the attention badge', async () => {
    const { ui, automation } = mountSidebar(true);
    const btn = screen.getByRole('button', { name: /Automation/ }) as HTMLButtonElement;
    expect(btn.disabled).toBe(false);
    expect(screen.queryByTestId('automation-badge')).toBeNull();
    automation.rows = [
      {
        id: 'a',
        name: 'A',
        showReview: true,
      } as never,
      {
        id: 'b',
        name: 'B',
        showReview: false,
        attention: { kind: 'failed', runId: 1, text: 'x' },
      } as never,
    ];
    expect((await screen.findByTestId('automation-badge')).textContent?.trim()).toBe('2');
    await fireEvent.click(btn);
    expect(ui.automationOpen).toBe(true);
    ui.automationOpen = false;
  });
});
