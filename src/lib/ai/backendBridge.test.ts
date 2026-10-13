import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { backendConnectionTransport, backendIntentTransport } from './backendBridge';

const config = {
  endpoint: 'http://127.0.0.1:20128/v1',
  model: 'm',
  timeoutMs: 1000,
  credentialRef: 'Ferrum/AI/custom',
  structured: 'json_object' as const,
};
const input = {
  config,
  requestId: 'req_1',
  userText: 'zip left',
  context: {
    activePane: 'left',
    leftLocation: 'disk',
    rightLocation: 'disk',
    leftCount: 1,
    rightCount: 2,
    showHidden: false,
    filterActive: false,
  },
} as never;

describe('AI backend bridge', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('plans through ai_plan_intent with only config, id, text and context', async () => {
    invokeMock.mockResolvedValue({ kind: 'question', text: 'which pane?' });
    const result = await backendIntentTransport(input, new AbortController().signal);
    expect(result).toEqual({ kind: 'question', text: 'which pane?' });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    const [name, args] = invokeMock.mock.calls[0] as [string, { request: Record<string, unknown> }];
    expect(name).toBe('ai_plan_intent');
    expect(Object.keys(args.request).sort()).toEqual(['context', 'requestId', 'userText']);
  });

  it('forwards the offered capability ids when present', async () => {
    invokeMock.mockResolvedValue({ kind: 'question', text: 'x' });
    await backendIntentTransport(
      { ...(input as object), capabilities: ['archive.create'] } as never,
      new AbortController().signal,
    );
    const [, args] = invokeMock.mock.calls[0] as [string, { request: Record<string, unknown> }];
    expect(args.request.capabilities).toEqual(['archive.create']);
  });

  it('cancels the backend request on abort', async () => {
    let finish: (value: unknown) => void = () => undefined;
    invokeMock.mockImplementation((name: string) =>
      name === 'ai_cancel_plan'
        ? Promise.resolve()
        : new Promise((resolve) => {
            finish = resolve;
          }),
    );
    const controller = new AbortController();
    const pending = backendIntentTransport(input, controller.signal);
    controller.abort();
    expect(invokeMock).toHaveBeenCalledWith('ai_cancel_plan', { requestId: 'req_1' });
    finish({ kind: 'question', text: 'late' });
    await pending;
  });

  it('passes backend rejections through unchanged', async () => {
    invokeMock.mockRejectedValue({ kind: 'auth', attempts: 1 });
    await expect(backendIntentTransport(input, new AbortController().signal)).rejects.toEqual({
      kind: 'auth',
      attempts: 1,
    });
  });

  it('tests the connection through ai_test_connection with the setup only', async () => {
    invokeMock.mockResolvedValue({ ok: true, kind: null });
    await backendConnectionTransport(config, new AbortController().signal);
    expect(invokeMock).toHaveBeenCalledWith('ai_test_connection', { config });
  });
});
