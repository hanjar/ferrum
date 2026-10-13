import { describe, expect, it, vi } from 'vitest';
import { ContractError } from './contracts';
import type { ProviderPlanRequest } from './provider';
import {
  RemoteIntentProvider,
  RemoteProviderError,
  type BackendIntentTransport,
} from './remote-provider';

const intent = {
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'directories.sync',
    leftPane: 'left',
    rightPane: 'right',
    mode: 'bidirectional',
  },
};
const request = (signal = new AbortController().signal): ProviderPlanRequest => ({
  requestId: 'req_1',
  userText: 'Sync both panes',
  context: {
    activePane: 'left',
    leftLocation: 'disk',
    rightLocation: 'disk',
    leftCount: 3,
    rightCount: 4,
    showHidden: false,
    filterActive: false,
  },
  signal,
});
const config = {
  endpoint: 'https://router.example/v1',
  model: 'configured-model',
  timeoutMs: 5000,
  credentialRef: 'Ferrum/AI/custom',
};
async function read(provider: RemoteIntentProvider, input = request()) {
  const result = [];
  for await (const event of provider.plan(input)) result.push(event);
  return result;
}

describe('backend-only remote provider bridge', () => {
  it('rejects HTTP non-local endpoints before invoking backend', async () => {
    const transport = vi.fn();
    expect(
      () => new RemoteIntentProvider({ ...config, endpoint: 'http://custom.test/v1' }, transport),
    ).toThrowError(expect.objectContaining({ code: 'unsafe_endpoint' }));
    expect(transport).not.toHaveBeenCalled();
  });

  it('rejects credential refs other than backend-owned target', () => {
    expect(
      () => new RemoteIntentProvider({ ...config, credentialRef: 'other' }, vi.fn()),
    ).toThrowError(expect.objectContaining({ code: 'invalid_config' }));
  });

  it('accepts explicit localhost HTTP but rejects userinfo, fragments, and non-v1 URLs', () => {
    const transport = vi.fn();
    expect(
      () =>
        new RemoteIntentProvider({ ...config, endpoint: 'http://127.0.0.1:20128/v1' }, transport),
    ).not.toThrow();
    for (const endpoint of [
      'http://localhost/v1',
      'http://127.0.0.1/v1',
      'http://localhost.evil.test/v1',
      'https://key@router.example/v1',
      'https://router.example/v1#secret',
      'https://router.example',
      'https://router.example/',
      'https://router.example/v1/../x',
      'https://router.example/v1?x=1',
      'http://[::1]:20128/v1',
    ]) {
      expect(() => new RemoteIntentProvider({ ...config, endpoint }, transport)).toThrow();
    }
  });

  it('accepts documented provider base paths such as Z.ai /api/paas/v4', () => {
    for (const endpoint of ['https://api.z.ai/api/paas/v4', 'https://router.example/v1']) {
      expect(() => new RemoteIntentProvider({ ...config, endpoint }, vi.fn())).not.toThrow();
    }
    expect(
      () => new RemoteIntentProvider({ ...config, structured: 'none' }, vi.fn()),
    ).not.toThrow();
    expect(
      () => new RemoteIntentProvider({ ...config, structured: 'x' as 'none' }, vi.fn()),
    ).toThrowError(expect.objectContaining({ code: 'invalid_config' }));
  });

  it('maps stable backend error kinds without exposing backend messages', async () => {
    const kinds = [
      'auth',
      'rate_limited',
      'quota_exceeded',
      'model_unavailable',
      'timeout',
      'cancelled',
      'malformed',
      'refused',
      'network',
      'server_error',
      'credential',
    ] as const;
    for (const kind of kinds) {
      const transport: BackendIntentTransport = vi.fn(async () => {
        throw { kind, attempts: 2, message: 'Bearer private-token' };
      });
      const error = await read(new RemoteIntentProvider(config, transport)).catch((e) => e);
      expect(error).toBeInstanceOf(RemoteProviderError);
      expect(error.code).toBe(kind);
      expect(error.message).not.toContain('private-token');
    }
    const transport: BackendIntentTransport = vi.fn(async () => {
      throw { kind: 'shell_exec' };
    });
    await expect(read(new RemoteIntentProvider(config, transport))).rejects.toMatchObject({
      code: 'backend_failure',
    });
  });

  it('accepts a strict question result and rejects malformed ones', async () => {
    const ok: BackendIntentTransport = vi.fn(async () => ({
      kind: 'question',
      text: 'Which pane?',
    }));
    expect(await read(new RemoteIntentProvider(config, ok))).toEqual([
      { kind: 'question', text: 'Which pane?' },
    ]);
    for (const value of [
      { kind: 'question', text: '' },
      { kind: 'question', text: 'x', extra: 1 },
      { kind: 'question', text: 'a'.repeat(513) },
      { kind: 'other', text: 'x' },
    ]) {
      const bad: BackendIntentTransport = vi.fn(async () => value);
      await expect(read(new RemoteIntentProvider(config, bad))).rejects.toBeInstanceOf(
        ContractError,
      );
    }
  });

  it('sends minimal opaque request to backend transport, no credential value or file listing', async () => {
    const transport: BackendIntentTransport = vi.fn(async () => intent);
    expect(await read(new RemoteIntentProvider(config, transport))).toEqual([
      { kind: 'intent', intent },
    ]);
    expect(transport).toHaveBeenCalledOnce();
    const sent = vi.mocked(transport).mock.calls[0][0];
    expect(sent).toEqual({
      config,
      requestId: 'req_1',
      userText: 'Sync both panes',
      context: request().context,
    });
    expect(JSON.stringify(sent)).not.toContain('apiKey');
  });

  it('validates output, refuses mismatched IDs and injected authority', async () => {
    for (const value of [
      { ...intent, requestId: 'other' },
      { ...intent, intent: { ...intent.intent, shell: 'delete' } },
    ]) {
      const transport: BackendIntentTransport = vi.fn(async () => value);
      await expect(read(new RemoteIntentProvider(config, transport))).rejects.toBeInstanceOf(
        ContractError,
      );
    }
  });

  it('rejects timeout and abort without late intent', async () => {
    const transport: BackendIntentTransport = vi.fn(
      (_input, signal) =>
        new Promise((_resolve, reject) => {
          signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true });
        }),
    );
    await expect(
      read(new RemoteIntentProvider({ ...config, timeoutMs: 100 }, transport)),
    ).rejects.toMatchObject({ code: 'timeout' });
    const controller = new AbortController();
    controller.abort();
    await expect(
      read(new RemoteIntentProvider(config, transport), request(controller.signal)),
    ).rejects.toMatchObject({ code: 'cancelled' });
  });

  it('normalizes backend errors, even ones posing as contract errors', async () => {
    for (const error of [
      new Error('Bearer private-token must never be displayed'),
      Object.assign(new Error('Bearer private-token'), { name: 'ContractError' }),
      new RemoteProviderError('unsafe_endpoint', 'Bearer private-token'),
    ]) {
      const transport: BackendIntentTransport = vi.fn(async () => {
        throw error;
      });
      await expect(read(new RemoteIntentProvider(config, transport))).rejects.toMatchObject({
        code: 'backend_failure',
        message: 'Provider request failed',
      });
    }
  });

  it('limits user text and strips unexpected context fields before transport', async () => {
    const transport: BackendIntentTransport = vi.fn(async () => intent);
    const provider = new RemoteIntentProvider(config, transport);
    const oversized = request();
    oversized.userText = 'a'.repeat(8193);
    await expect(read(provider, oversized)).rejects.toMatchObject({ code: 'invalid_config' });
    expect(transport).not.toHaveBeenCalled();
    const contaminated = request();
    (contaminated.context as unknown as Record<string, unknown>).paths = ['C:\\secret'];
    expect(await read(provider, contaminated)).toHaveLength(1);
    expect(vi.mocked(transport).mock.calls[0][0].context).toEqual(request().context);
  });

  it('rejects invalid context and request IDs before transport', async () => {
    const transport: BackendIntentTransport = vi.fn(async () => intent);
    const provider = new RemoteIntentProvider(config, transport);
    const wrongCount = request();
    wrongCount.context.leftCount = -1;
    const wrongId = request();
    wrongId.requestId = '../escape';
    const wrongLocation = request();
    wrongLocation.context.rightLocation = 'secret' as 'disk';
    for (const input of [wrongCount, wrongId, wrongLocation]) {
      await expect(read(provider, input)).rejects.toMatchObject({ code: 'invalid_config' });
    }
    expect(transport).not.toHaveBeenCalled();
  });

  it('does not invoke transport when request aborts during listener setup', async () => {
    const transport: BackendIntentTransport = vi.fn(async () => intent);
    const controller = new AbortController();
    const signal = controller.signal;
    const add = signal.addEventListener.bind(signal);
    vi.spyOn(signal, 'addEventListener').mockImplementation((...args) => {
      add(...args);
      controller.abort();
    });
    await expect(
      read(new RemoteIntentProvider(config, transport), request(signal)),
    ).rejects.toMatchObject({ code: 'cancelled' });
    expect(transport).not.toHaveBeenCalled();
  });
});

describe('capability offering', () => {
  it('forwards the capabilities the app offers to the backend', async () => {
    const transport = vi.fn<BackendIntentTransport>(async () => intent);
    const provider = new RemoteIntentProvider(config, transport);
    await read(provider, { ...request(), capabilities: ['directories.sync'] });
    expect(transport.mock.calls[0]![0]).toMatchObject({ capabilities: ['directories.sync'] });
  });

  it('FER-466: forwards a camelCase prefix such as diskUsage.query', async () => {
    const transport = vi.fn<BackendIntentTransport>(async () => intent);
    const provider = new RemoteIntentProvider(config, transport);
    await read(provider, { ...request(), capabilities: ['directories.sync', 'diskUsage.query'] });
    expect(transport.mock.calls[0]![0]).toMatchObject({
      capabilities: ['directories.sync', 'diskUsage.query'],
    });
  });

  it('omits the field for callers that do not pass a list', async () => {
    const transport = vi.fn<BackendIntentTransport>(async () => intent);
    await read(new RemoteIntentProvider(config, transport));
    expect(transport.mock.calls[0]![0]).not.toHaveProperty('capabilities');
  });

  it('rejects malformed capability lists before invoking the backend', async () => {
    const transport = vi.fn<BackendIntentTransport>(async () => intent);
    const provider = new RemoteIntentProvider(config, transport);
    for (const capabilities of [
      ['shell exec'],
      ['archive.create', 'archive.create'],
      [1 as never],
    ]) {
      await expect(read(provider, { ...request(), capabilities })).rejects.toMatchObject({
        code: 'invalid_config',
      });
    }
    expect(transport).not.toHaveBeenCalled();
  });
});
