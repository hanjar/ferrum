import { describe, expect, it, vi } from 'vitest';
import {
  OUTBOUND_DISCLOSURE,
  testConnection,
  validateProviderSetup,
  type BackendConnectionTransport,
} from './provider-setup';
import { RemoteProviderError } from './remote-provider';

const setup = {
  endpoint: 'https://api.z.ai/api/paas/v4',
  model: 'configured-model',
  timeoutMs: 20_000,
  credentialRef: 'Ferrum/AI/custom',
  structured: 'json_object' as const,
};

describe('provider setup and explicit connection test', () => {
  it('validates endpoint, model and timeout before anything is stored or sent', () => {
    expect(validateProviderSetup(setup)).toEqual(setup);
    for (const bad of [
      { ...setup, endpoint: 'http://router.test/v1' },
      { ...setup, model: '' },
      { ...setup, model: 'has space' },
      { ...setup, timeoutMs: 5 },
      { ...setup, credentialRef: 'x' },
      { ...setup, apiKey: 'sk-secret' },
      { ...setup, structured: 'strict' },
      null,
      'x',
    ]) {
      expect(() => validateProviderSetup(bad)).toThrow(RemoteProviderError);
    }
  });

  it('never carries credential values and defaults structured output to json_object', () => {
    const without: Record<string, unknown> = { ...setup };
    delete without.structured;
    expect(validateProviderSetup(without).structured).toBe('json_object');
    expect(JSON.stringify(validateProviderSetup(setup))).not.toMatch(/key|token|secret/i);
  });

  it('runs the connection test only through the backend and reports ok', async () => {
    const transport: BackendConnectionTransport = vi.fn(async () => ({ ok: true, kind: null }));
    expect(await testConnection(setup, transport, new AbortController().signal)).toEqual({
      ok: true,
    });
    expect(transport).toHaveBeenCalledOnce();
    expect(vi.mocked(transport).mock.calls[0][0]).toEqual(setup);
  });

  it('FER-573: carries the sanitized provider message of a failed test, never for unknown kinds', async () => {
    const detailed: BackendConnectionTransport = async () => ({
      ok: false,
      kind: 'model_unavailable',
      detail: '[antigravity/x] [404]: Requested entity was not found. sk-abcdef0123456789',
    });
    expect(await testConnection(setup, detailed, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'model_unavailable',
      detail: '[antigravity/x] [404]: Requested entity was not found. [redacted]',
    });
    const thrown: BackendConnectionTransport = async () => {
      throw { kind: 'server_error', attempts: 3, detail: 'upstream overloaded' };
    };
    expect(await testConnection(setup, thrown, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'server_error',
      detail: 'upstream overloaded',
    });
    const unknown: BackendConnectionTransport = async () => ({
      ok: false,
      kind: 'rm -rf',
      detail: 'x',
    });
    expect(await testConnection(setup, unknown, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'backend_failure',
    });
  });

  it('reports stable failure kinds and normalizes unknown or thrown failures', async () => {
    const auth: BackendConnectionTransport = async () => ({ ok: false, kind: 'auth' });
    expect(await testConnection(setup, auth, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'auth',
    });
    const odd: BackendConnectionTransport = async () => ({ ok: false, kind: 'rm -rf' });
    expect(await testConnection(setup, odd, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'backend_failure',
    });
    const thrown: BackendConnectionTransport = async () => {
      throw { kind: 'timeout', message: 'Bearer abc' };
    };
    expect(await testConnection(setup, thrown, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'timeout',
    });
    const garbage: BackendConnectionTransport = async () => 42;
    expect(await testConnection(setup, garbage, new AbortController().signal)).toEqual({
      ok: false,
      kind: 'backend_failure',
    });
  });

  it('does not call the backend with an invalid setup or an aborted signal', async () => {
    const transport: BackendConnectionTransport = vi.fn(async () => ({ ok: true, kind: null }));
    expect(
      await testConnection(
        { ...setup, endpoint: 'http://x.test/v1' },
        transport,
        new AbortController().signal,
      ),
    ).toEqual({ ok: false, kind: 'unsafe_endpoint' });
    const controller = new AbortController();
    controller.abort();
    expect(await testConnection(setup, transport, controller.signal)).toEqual({
      ok: false,
      kind: 'cancelled',
    });
    expect(transport).not.toHaveBeenCalled();
  });

  it('abort during the test returns cancelled without waiting for the backend', async () => {
    const controller = new AbortController();
    const transport: BackendConnectionTransport = () => new Promise(() => {});
    const pending = testConnection(setup, transport, controller.signal);
    controller.abort();
    expect(await pending).toEqual({ ok: false, kind: 'cancelled' });
  });

  it('discloses what leaves the machine', () => {
    expect(OUTBOUND_DISCLOSURE.join(' ')).toMatch(/typed request/i);
    expect(OUTBOUND_DISCLOSURE.join(' ')).toMatch(/never.*file (names|contents)/i);
  });
});
