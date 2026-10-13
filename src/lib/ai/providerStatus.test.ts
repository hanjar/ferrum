import { describe, expect, it, vi } from 'vitest';
import { AUTO_CHECK_INTERVAL_MS, ProviderStatus } from './providerStatus.svelte';

function make(result: { ok: true } | { ok: false; kind: string } = { ok: true }) {
  let clock = 1_000_000;
  const test = vi.fn(async () => result);
  const status = new ProviderStatus({
    setup: () => ({ endpoint: 'http://127.0.0.1:20128/v1' }),
    test,
    now: () => clock,
  });
  return { status, test, advance: (ms: number) => (clock += ms) };
}

describe('ProviderStatus', () => {
  it('turns red with the safe kind and host:port when the test fails', async () => {
    const { status } = make({ ok: false, kind: 'unreachable' });
    await status.check();
    expect(status.state).toBe('down');
    expect(status.kind).toBe('unreachable');
    expect(status.hostPort).toBe('127.0.0.1:20128');
  });

  it('turns green when the test succeeds', async () => {
    const { status } = make();
    await status.check();
    expect(status.state).toBe('ok');
    expect(status.kind).toBeNull();
  });

  it('auto checks at most once per minute; force always checks', async () => {
    const { status, test, advance } = make();
    await status.check();
    await status.check();
    expect(test).toHaveBeenCalledTimes(1);
    advance(AUTO_CHECK_INTERVAL_MS - 1);
    await status.check();
    expect(test).toHaveBeenCalledTimes(1);
    await status.check(true);
    expect(test).toHaveBeenCalledTimes(2);
    advance(AUTO_CHECK_INTERVAL_MS + 1);
    await status.check();
    expect(test).toHaveBeenCalledTimes(3);
  });

  it('shares one in-flight check', async () => {
    const { status, test } = make();
    await Promise.all([status.check(true), status.check(true)]);
    expect(test).toHaveBeenCalledTimes(1);
  });

  it('stays unknown when nothing is configured or the bridge throws', async () => {
    const none = new ProviderStatus({ setup: () => undefined, test: vi.fn(), now: () => 0 });
    await none.check();
    expect(none.state).toBe('unknown');
    const boom = new ProviderStatus({
      setup: () => ({ endpoint: 'http://127.0.0.1:1/v1' }),
      test: async () => {
        throw new Error('ipc');
      },
      now: () => 0,
    });
    await boom.check();
    expect(boom.state).toBe('unknown');
  });

  it('noteFailure marks down but ignores cancellation', () => {
    const { status } = make();
    status.noteFailure('cancelled');
    expect(status.state).toBe('unknown');
    status.noteFailure('unreachable');
    expect(status.state).toBe('down');
  });
});
