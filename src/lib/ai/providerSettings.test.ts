import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn<(command: string, args?: unknown) => Promise<unknown>>();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (c: string, a?: unknown) => invokeMock(c, a) }));
const reloadMock = vi.fn<() => void>();
vi.mock('./aiRuntime', async () => {
  const actual = await vi.importActual<typeof import('./aiRuntime')>('./aiRuntime');
  return { ...actual, reloadAiRuntime: () => reloadMock() };
});

import { aiChatHost } from './aiChatHost.svelte';
import { loadProviderSetup } from './aiRuntime';
import { resetSetupStoreForTests } from './providerSetupStore';
import {
  clearProviderSetup,
  credentialClear,
  credentialSave,
  credentialStatus,
  DEFAULT_SETUP_FORM,
  initProviderSetup,
  loadStoredSetup,
  saveProviderSetup,
  SETUP_STORAGE_KEY,
} from './providerSettings';

const form = {
  endpoint: 'http://127.0.0.1:20128/v1',
  model: 'glm-4.6',
  timeoutMs: 30000,
  structured: 'json_object' as const,
};

beforeEach(() => {
  invokeMock.mockReset();
  reloadMock.mockReset();
  localStorage.clear();
  resetSetupStoreForTests();
});

describe('provider setup storage (browser fallback outside Tauri)', () => {
  it('saves a validated non-secret setup under the key loadProviderSetup reads', async () => {
    const result = await saveProviderSetup(form);
    expect(result).toEqual({
      ok: true,
      setup: { ...form, credentialRef: 'Ferrum/AI/custom' },
    });
    expect(loadProviderSetup()).toEqual({ ...form, credentialRef: 'Ferrum/AI/custom' });
    expect(loadStoredSetup()?.model).toBe('glm-4.6');
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it('stores only the five setup fields (never anything like a key)', async () => {
    await saveProviderSetup({ ...form, apiKey: 'sk-should-not-persist-1234' } as never);
    const raw = localStorage.getItem(SETUP_STORAGE_KEY) ?? '';
    expect(Object.keys(JSON.parse(raw)).sort()).toEqual([
      'credentialRef',
      'endpoint',
      'model',
      'structured',
      'timeoutMs',
    ]);
    expect(raw).not.toContain('sk-should-not-persist');
  });

  it('rejects an invalid setup with a safe kind and writes nothing', async () => {
    expect(await saveProviderSetup({ ...form, endpoint: 'http://example.com/v1' })).toEqual({
      ok: false,
      kind: 'unsafe_endpoint',
    });
    expect((await saveProviderSetup({ ...form, model: 'bad model!' })).ok).toBe(false);
    expect((await saveProviderSetup({ ...form, timeoutMs: 5 })).ok).toBe(false);
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
    expect(reloadMock).not.toHaveBeenCalled();
  });

  it('reports storage failure without throwing', async () => {
    const spy = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    expect(await saveProviderSetup(form)).toEqual({ ok: false, kind: 'storage_unavailable' });
    spy.mockRestore();
  });

  it('ignores an invalid or corrupt stored value', () => {
    localStorage.setItem(SETUP_STORAGE_KEY, '{not json');
    expect(loadStoredSetup()).toBeUndefined();
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify({ endpoint: 'ftp://x', model: 'm' }));
    expect(loadStoredSetup()).toBeUndefined();
  });

  it('clears the setup and reloads the runtime', async () => {
    await saveProviderSetup(form);
    reloadMock.mockClear();
    expect(await clearProviderSetup()).toEqual({ ok: true });
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
    expect(loadStoredSetup()).toBeUndefined();
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it('exposes sane defaults for the form', () => {
    expect(DEFAULT_SETUP_FORM.timeoutMs).toBeGreaterThanOrEqual(100);
    expect(DEFAULT_SETUP_FORM.structured).toBe('json_object');
  });
});

describe('provider setup storage in the desktop app (FER-430)', () => {
  const stored = { ...form, credentialRef: 'Ferrum/AI/custom' };
  // Fake backend: one slot, the same contract as ai_setup_load / ai_setup_save / ai_setup_clear.
  let slot: unknown;
  let failSave: { kind: string } | undefined;

  beforeEach(() => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    resetSetupStoreForTests();
    slot = undefined;
    failSave = undefined;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === 'ai_setup_load') return slot ?? null;
      if (command === 'ai_setup_save') {
        if (failSave) throw failSave;
        slot = (args as { config: unknown }).config;
        return null;
      }
      if (command === 'ai_setup_clear') {
        slot = undefined;
        return null;
      }
      throw new Error('unexpected ' + command);
    });
  });
  afterEach(() => {
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    resetSetupStoreForTests();
  });

  it('saves through the backend, not browser storage, and loadProviderSetup reads the cache', async () => {
    expect(await saveProviderSetup(form)).toEqual({ ok: true, setup: stored });
    expect(invokeMock).toHaveBeenCalledWith('ai_setup_save', { config: stored });
    expect(slot).toEqual(stored);
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
    expect(loadProviderSetup()).toEqual(stored);
    expect(loadStoredSetup()).toEqual(stored);
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it('shows a setup saved by another build (same backend) without re-entering it', async () => {
    slot = stored;
    expect(loadProviderSetup()).toBeUndefined(); // before the backend load finished
    const before = aiChatHost.runtimeVersion;
    expect(await initProviderSetup()).toEqual(stored);
    expect(loadProviderSetup()).toEqual(stored);
    expect(reloadMock).toHaveBeenCalledTimes(1);
    expect(aiChatHost.runtimeVersion).toBe(before + 1);
    await initProviderSetup(); // single flight / unchanged: no second rebuild
    expect(invokeMock.mock.calls.filter((c) => c[0] === 'ai_setup_load')).toHaveLength(1);
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it('migrates a legacy browser copy to the backend exactly once, then drops it', async () => {
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify(stored));
    expect(await initProviderSetup()).toEqual(stored);
    expect(slot).toEqual(stored);
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
    resetSetupStoreForTests();
    await initProviderSetup();
    expect(invokeMock.mock.calls.filter((c) => c[0] === 'ai_setup_save')).toHaveLength(1);
  });

  it('keeps the legacy copy when the backend refuses the migration, and still uses it this session', async () => {
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify(stored));
    failSave = { kind: 'storage_failure' };
    expect(await initProviderSetup()).toEqual(stored);
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).not.toBeNull();
    expect(loadProviderSetup()).toEqual(stored);
  });

  it('prefers the backend setup over a stale browser copy and discards the copy', async () => {
    slot = { ...stored, model: 'from-backend' };
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify({ ...stored, model: 'stale' }));
    expect((await initProviderSetup())?.model).toBe('from-backend');
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
  });

  it('ignores an invalid backend value instead of crashing', async () => {
    slot = { ...stored, endpoint: 'http://evil.example/v1' };
    expect(await initProviderSetup()).toBeUndefined();
    expect(loadProviderSetup()).toBeUndefined();
  });

  it('maps backend rejections to safe kinds and keeps the previous setup', async () => {
    await saveProviderSetup(form);
    failSave = { kind: 'unsafe_endpoint' };
    expect(await saveProviderSetup({ ...form, model: 'other' })).toEqual({
      ok: false,
      kind: 'unsafe_endpoint',
    });
    failSave = { kind: 'invalid_config' };
    expect(await saveProviderSetup({ ...form, model: 'other' })).toEqual({
      ok: false,
      kind: 'invalid_config',
    });
    // FER-512: the native confirmation for a new host was answered with No.
    failSave = { kind: 'not_confirmed' };
    expect(await saveProviderSetup({ ...form, model: 'other' })).toEqual({
      ok: false,
      kind: 'not_confirmed',
    });
    failSave = { kind: 'storage_failure' };
    expect(await saveProviderSetup({ ...form, model: 'other' })).toEqual({
      ok: false,
      kind: 'storage_unavailable',
    });
    failSave = { kind: 'weird value with a path' };
    expect(await saveProviderSetup({ ...form, model: 'other' })).toEqual({
      ok: false,
      kind: 'storage_unavailable',
    });
    expect(loadStoredSetup()?.model).toBe('glm-4.6');
  });

  it('never sends a key to the setup commands and validates before invoking', async () => {
    expect((await saveProviderSetup({ ...form, endpoint: 'http://example.com/v1' })).ok).toBe(
      false,
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === 'ai_setup_save')).toBe(false);
    await saveProviderSetup({ ...form, apiKey: 'sk-should-not-persist-1234' } as never);
    const call = invokeMock.mock.calls.find((c) => c[0] === 'ai_setup_save')!;
    expect(Object.keys((call[1] as { config: object }).config).sort()).toEqual([
      'credentialRef',
      'endpoint',
      'model',
      'structured',
      'timeoutMs',
    ]);
    expect(JSON.stringify(call)).not.toContain('sk-should-not-persist');
  });

  it('clears through the backend and reloads the runtime', async () => {
    await saveProviderSetup(form);
    reloadMock.mockClear();
    expect(await clearProviderSetup()).toEqual({ ok: true });
    expect(slot).toBeUndefined();
    expect(loadStoredSetup()).toBeUndefined();
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });
});

describe('credential commands', () => {
  it('reads status without receiving a value', async () => {
    invokeMock.mockResolvedValue({ configured: true });
    expect(await credentialStatus()).toEqual({ ok: true, configured: true });
    expect(invokeMock).toHaveBeenCalledWith('ai_credential_status', undefined);
  });

  it('treats an unreachable status check as not configured with a safe kind', async () => {
    invokeMock.mockRejectedValue(new Error('boom'));
    expect(await credentialStatus()).toEqual({ ok: false, kind: 'credential_failure' });
  });

  it('sends the key only as the value argument and reloads the runtime', async () => {
    invokeMock.mockResolvedValue({ configured: true });
    const result = await credentialSave('sk-test-value-1234');
    expect(result).toEqual({ ok: true, configured: true });
    expect(invokeMock).toHaveBeenCalledWith('ai_credential_save', { value: 'sk-test-value-1234' });
    expect(JSON.stringify(result)).not.toContain('sk-test');
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it('maps backend save errors to the kind only and never echoes the key', async () => {
    invokeMock.mockRejectedValue({ kind: 'invalid_value' });
    const result = await credentialSave('bad');
    expect(result).toEqual({ ok: false, kind: 'invalid_value' });
    invokeMock.mockRejectedValue({ kind: 'unsupported' });
    expect(await credentialSave('sk-test-value-1234')).toEqual({ ok: false, kind: 'unsupported' });
    invokeMock.mockRejectedValue(new Error('detail with sk-test-value-1234'));
    const generic = await credentialSave('sk-test-value-1234');
    expect(generic).toEqual({ ok: false, kind: 'credential_failure' });
    expect(JSON.stringify(generic)).not.toContain('sk-test');
  });

  it('clears the credential', async () => {
    invokeMock.mockResolvedValue({ configured: false });
    expect(await credentialClear()).toEqual({ ok: true, configured: false });
    expect(invokeMock).toHaveBeenCalledWith('ai_credential_clear', undefined);
    expect(reloadMock).toHaveBeenCalledTimes(1);
    invokeMock.mockRejectedValue({ kind: 'weird' });
    expect(await credentialClear()).toEqual({ ok: false, kind: 'credential_failure' });
  });
});
