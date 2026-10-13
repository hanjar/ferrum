/**
 * FER-432 #1: every renderer path that opens a FILE through the OS passes the
 * window's `activity.recordingActive` explicitly, so a private session or a
 * global pause never leaves an OpenAccepted receipt (the backend default is
 * fail-closed too; see ipc.rs `shell_open_records`).
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';

const ipc = vi.hoisted(() => ({
  state: { recordingEnabled: true, privacyEpoch: 1, disclosureAcked: true },
  shellOpen: vi.fn<(path: string, record: boolean) => Promise<boolean>>(async () => true),
}));

vi.mock('../ipc/activity', async (orig) => ({
  ...(await orig<typeof import('../ipc/activity')>()),
  activityState: vi.fn(async () => ({ ...ipc.state })),
  activityRecent: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityFavorites: vi.fn(async () => [
    { path: 'C:\\Pin\\book.xlsx', label: 'book.xlsx', kind: 'file' },
  ]),
  activitySearch: vi.fn(async () => []),
  activityOverview: vi.fn(async () => ({
    days: 30,
    sort: 'recent',
    items: [
      {
        entity: {
          id: 'f1',
          kind: 'file',
          provider: 'local',
          volumeId: 'v',
          locator: 'C:\\Work\\notes.txt',
          parentPath: 'C:\\Work',
          displayName: 'notes.txt',
          availability: 'available',
        },
        lastUtcMs: 1,
        episodeCount: 1,
        activeDays: 1,
        score: null,
        hidden: false,
      },
    ],
    daily: [],
    totalItems: 1,
    totalEpisodes: 1,
    oldestUtcMs: null,
    truncated: false,
    retentionDays: 90,
    privacyEpoch: 1,
  })),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: ipc.shellOpen,
}));

import CommandPalette from './CommandPalette.svelte';
import Sidebar from './Sidebar.svelte';
import ActivityPage from './ActivityPage.svelte';
import { ui } from '../uiState.svelte';
import { activity } from '../stores/activityStore.svelte';
import { activityPage } from '../stores/activityPageStore.svelte';

type Mode = 'recording' | 'private' | 'paused';
const MODES: [Mode, boolean][] = [
  ['recording', true],
  ['private', false],
  ['paused', false],
];

function setMode(mode: Mode) {
  ipc.state = { recordingEnabled: mode !== 'paused', privacyEpoch: 1, disclosureAcked: true };
  activity.recording = mode !== 'paused';
  activity.privateSession = mode === 'private';
}

beforeEach(() => {
  ipc.shellOpen.mockClear();
  activity.available = true;
  activity.privacyEpoch = 1;
  activity.favorites = [{ path: 'C:\\Pin\\book.xlsx', label: 'book.xlsx', kind: 'file' }];
  activity.recent = [];
  activity.frequent = [];
  activityPage.reset();
  ui.paletteOpen = false;
});

describe('shellOpen record flag follows recordingActive (FER-432)', () => {
  it.each(MODES)('palette empty-query file favorite (%s) -> record=%s', async (mode, record) => {
    setMode(mode);
    render(CommandPalette);
    ui.paletteOpen = true;
    await tick();
    const opt = await screen.findByRole('option', { name: /book\.xlsx/ });
    await fireEvent.click(opt);
    await waitFor(() => expect(ipc.shellOpen).toHaveBeenCalledTimes(1));
    expect(ipc.shellOpen).toHaveBeenCalledWith('C:\\Pin\\book.xlsx', record);
  });

  it.each(MODES)('sidebar file favorite (%s) -> record=%s', async (mode, record) => {
    setMode(mode);
    render(Sidebar);
    const fav = await screen.findByRole('button', { name: 'book.xlsx' });
    await fireEvent.click(fav);
    await waitFor(() => expect(ipc.shellOpen).toHaveBeenCalledTimes(1));
    expect(ipc.shellOpen).toHaveBeenCalledWith('C:\\Pin\\book.xlsx', record);
  });

  it.each(MODES)('Activity page file open (%s) -> record=%s', async (mode, record) => {
    setMode(mode);
    render(ActivityPage);
    const row = await screen.findByRole('option', { name: /notes\.txt/ });
    await fireEvent.dblClick(row);
    await waitFor(() => expect(ipc.shellOpen).toHaveBeenCalledTimes(1));
    expect(ipc.shellOpen).toHaveBeenCalledWith('C:\\Work\\notes.txt', record);
  });
});
