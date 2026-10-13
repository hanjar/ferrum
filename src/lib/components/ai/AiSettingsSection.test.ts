import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import { tick } from 'svelte';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { getAction, runAction, searchActions } from '../../actions';
import { aiChatHost } from '../../ai/aiChatHost.svelte';
import { aiRuntime, reloadAiRuntime } from '../../ai/aiRuntime';
import { ui } from '../../uiState.svelte';
import SettingsDialog from '../SettingsDialog.svelte';
import AiSettingsSection from './AiSettingsSection.svelte';
import RuntimeHost from './ChatPanel.runtimehost.svelte';
import { saveProviderSetup } from '../../ai/providerSettings';

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue({ configured: false });
  vi.stubEnv('VITE_FERRUM_AI_ENDPOINT', '');
  vi.stubEnv('VITE_FERRUM_AI_MODEL', '');
  localStorage.clear();
  aiChatHost.revealSettings = false;
  reloadAiRuntime();
});

afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  localStorage.clear();
  reloadAiRuntime();
});

describe('AI settings in the Settings dialog (FER-429)', () => {
  it('opens Settings at the AI section from the menu action and the palette', () => {
    const action = getAction('view.aiSettings');
    expect(action).toBeDefined();
    ui.settingsOpen = false;
    expect(runAction(action!)).toBe(true);
    expect(ui.settingsOpen).toBe(true);
    expect(aiChatHost.revealSettings).toBe(true);
    expect(searchActions('AI assistant settings').map((a) => a.id)).toContain('view.aiSettings');
    ui.settingsOpen = false;
  });

  it('is rendered inside the Settings dialog with an AI category', async () => {
    render(SettingsDialog, { onclose: vi.fn() });
    await tick();
    const dialog = screen.getByRole('dialog', { name: 'Settings' });
    expect(within(dialog).getByRole('button', { name: 'AI' })).toBeTruthy();
    expect(within(dialog).getByRole('form', { name: 'AI provider setup' })).toBeTruthy();
  });

  it('focuses Endpoint once when opened through openSettings', async () => {
    aiChatHost.revealSettings = true;
    render(AiSettingsSection, {});
    await tick();
    await Promise.resolve();
    expect(document.activeElement).toBe(screen.getByLabelText('Endpoint'));
    expect(aiChatHost.revealSettings).toBe(false);
  });

  it('does not steal focus when Settings is opened normally', async () => {
    render(AiSettingsSection, {});
    await tick();
    await Promise.resolve();
    expect(document.activeElement).not.toBe(screen.getByLabelText('Endpoint'));
  });
});

describe('reloading the AI runtime', () => {
  it('stops the polling timer of the runtime it replaces', () => {
    const first = aiRuntime();
    const dispose = vi.spyOn(first, 'dispose');
    reloadAiRuntime();
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(aiRuntime()).not.toBe(first);
  });

  it('does nothing when no runtime was built yet', () => {
    reloadAiRuntime();
    expect(() => reloadAiRuntime()).not.toThrow();
  });
});

describe('AI settings section runtime (FER-382 I)', () => {
  it('bumps the runtime version when the setup is saved so the open chat panel is rebuilt', async () => {
    const before = aiChatHost.runtimeVersion;
    render(AiSettingsSection, {});
    await tick();
    await fireEvent.input(screen.getByLabelText('Endpoint'), {
      target: { value: 'http://127.0.0.1:20128/v1' },
    });
    await fireEvent.input(screen.getByLabelText('Model'), { target: { value: 'dummy-model' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(aiChatHost.runtimeVersion).toBeGreaterThan(before);
  });
});

describe('chat history survives a provider change (owner decision)', () => {
  it('keeps the history and drops the Open settings banner once the setup is saved', async () => {
    const r = render(RuntimeHost, {});
    await tick();
    const chat1 = aiRuntime().chat;
    chat1.notice('catatan lama');
    await tick();
    expect(screen.getByText('catatan lama')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Open settings' })).toBeTruthy();

    // What the dialog does after a saved setup: the save drops the runtime, then the version is bumped.
    expect(
      (
        await saveProviderSetup({
          endpoint: 'http://127.0.0.1:20128/v1',
          model: 'dummy-model',
          timeoutMs: 45000,
          structured: 'none',
        })
      ).ok,
    ).toBe(true);
    aiChatHost.runtimeVersion++;
    await tick();

    const chat2 = aiRuntime().chat;
    expect(chat2).not.toBe(chat1);
    expect(aiRuntime().configured).toBe(true);
    expect(screen.getByText('catatan lama')).toBeTruthy(); // same panel instance, history carried over
    expect(screen.queryByRole('button', { name: 'Open settings' })).toBeNull();
    r.unmount();
  });
});
