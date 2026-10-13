import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { getAction, runAction } from '../actions';
import { aiChatHost } from './aiChatHost.svelte';
import { createAiRuntime, loadProviderSetup } from './aiRuntime';

const SETUP = {
  endpoint: 'http://127.0.0.1:20128/v1',
  model: 'm',
  timeoutMs: 5000,
  credentialRef: 'Ferrum/AI/custom',
  structured: 'none',
};

beforeEach(() => {
  // A developer's .env.local must not leak into these tests.
  vi.stubEnv('VITE_FERRUM_AI_ENDPOINT', '');
  vi.stubEnv('VITE_FERRUM_AI_MODEL', '');
  invokeMock.mockReset();
  invokeMock.mockResolvedValue([]);
  localStorage.clear();
  aiChatHost.close();
});
afterEach(() => {
  localStorage.clear();
  vi.unstubAllEnvs();
});

describe('loadProviderSetup', () => {
  it('is undefined when nothing is configured', () => {
    expect(loadProviderSetup()).toBeUndefined();
  });

  it('reads and validates a stored non-secret setup', () => {
    localStorage.setItem('ferrum.ai.setup', JSON.stringify(SETUP));
    expect(loadProviderSetup()).toMatchObject({ endpoint: SETUP.endpoint, model: 'm' });
  });

  it('ignores an invalid or unsafe stored setup instead of using it', () => {
    localStorage.setItem(
      'ferrum.ai.setup',
      JSON.stringify({ ...SETUP, endpoint: 'http://evil.example/v1' }),
    );
    expect(loadProviderSetup()).toBeUndefined();
    localStorage.setItem('ferrum.ai.setup', '{not json');
    expect(loadProviderSetup()).toBeUndefined();
  });

  it('rejects a setup that carries a secret-looking extra field', () => {
    localStorage.setItem('ferrum.ai.setup', JSON.stringify({ ...SETUP, apiKey: 'sk-test' }));
    expect(loadProviderSetup()).toBeUndefined();
  });
});

describe('environment setup', () => {
  it('falls back to VITE_FERRUM_AI_* when nothing is stored, with the fixed credential ref', () => {
    vi.stubEnv('VITE_FERRUM_AI_ENDPOINT', 'http://127.0.0.1:20128/v1');
    vi.stubEnv('VITE_FERRUM_AI_MODEL', 'm');
    expect(loadProviderSetup()).toMatchObject({
      endpoint: 'http://127.0.0.1:20128/v1',
      model: 'm',
      credentialRef: 'Ferrum/AI/custom',
      structured: 'none',
    });
  });
});

describe('createAiRuntime', () => {
  it('builds a chat store and reports whether a provider is configured', () => {
    expect(createAiRuntime(undefined).configured).toBe(false);
    const runtime = createAiRuntime(SETUP as never);
    expect(runtime.configured).toBe(true);
    expect(runtime.chat).toBeDefined();
  });

  it('asks the backend ledger for unresolved executions at startup (restart recovery)', () => {
    createAiRuntime(undefined);
    expect(invokeMock).toHaveBeenCalledWith('ai_exec_unresolved');
  });
});

describe('view.aiChat action', () => {
  it('is registered with Ctrl+Shift+A and toggles the panel flag', () => {
    const action = getAction('view.aiChat');
    expect(action?.shortcut).toEqual({ key: 'a', ctrlOrCmd: true, shift: true });
    expect(aiChatHost.open).toBe(false);
    expect(runAction(action!)).toBe(true);
    expect(aiChatHost.open).toBe(true);
    expect(runAction(action!)).toBe(true);
    expect(aiChatHost.open).toBe(false);
  });
});
