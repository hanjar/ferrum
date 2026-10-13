import { describe, expect, it } from 'vitest';
import { ContractError } from './contracts';
import { FakeIntentProvider, ProviderError, type ProviderPlanRequest } from './provider';

const archive = {
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'archive.create',
    sourcePane: 'left',
    selector: { kind: 'allVisible', entries: 'filesOnly' },
    format: 'zip',
    target: { kind: 'pane', pane: 'right' },
  },
};
const sync = {
  schemaVersion: 1,
  requestId: 'req_2',
  intent: {
    capability: 'directories.sync',
    leftPane: 'left',
    rightPane: 'right',
    mode: 'bidirectional',
  },
};
const request = (
  requestId: string,
  signal = new AbortController().signal,
): ProviderPlanRequest => ({
  requestId,
  userText: 'Archive visible files',
  context: {
    activePane: 'left',
    leftLocation: 'disk',
    rightLocation: 'disk',
    leftCount: 3,
    rightCount: 2,
    showHidden: false,
    filterActive: false,
  },
  signal,
});

async function events(provider: FakeIntentProvider, input: ProviderPlanRequest) {
  const received: unknown[] = [];
  for await (const event of provider.plan(input)) received.push(event);
  return received;
}

describe('AI-04.1 fake intent provider', () => {
  it('returns configured plans in request order without parsing user prose or enqueuing operations', async () => {
    const provider = new FakeIntentProvider({ req_1: archive, req_2: sync });
    expect(await events(provider, request('req_1'))).toEqual([{ kind: 'intent', intent: archive }]);
    expect(await events(provider, request('req_2'))).toEqual([{ kind: 'intent', intent: sync }]);
    expect(await events(provider, request('req_1'))).toEqual([{ kind: 'intent', intent: archive }]);
  });

  it('uses only its request ID and opaque context; rejects unknown or mismatched requests', async () => {
    const provider = new FakeIntentProvider({ req_1: archive });
    await expect(events(provider, request('missing'))).rejects.toMatchObject({
      name: 'ProviderError',
      code: 'no_fixture',
    });
    await expect(
      events(new FakeIntentProvider({ req_2: archive }), request('req_2')),
    ).rejects.toMatchObject({
      name: 'ContractError',
      code: 'request_mismatch',
    });
    expect(new ProviderError('no_fixture', 'Missing')).toBeInstanceOf(Error);
  });

  it('validates fixture output with intent schema before emission', async () => {
    const provider = new FakeIntentProvider({
      req_1: {
        ...archive,
        intent: {
          ...archive.intent,
          target: { kind: 'pane', pane: 'right' },
          command: 'shell',
        },
      },
    });
    await expect(events(provider, request('req_1'))).rejects.toBeInstanceOf(ContractError);
  });

  it('supports deterministic questions without giving question events mutation authority', async () => {
    const provider = new FakeIntentProvider({
      req_1: { kind: 'question', text: 'Which destination pane?' },
    });
    expect(await events(provider, request('req_1'))).toEqual([
      { kind: 'question', text: 'Which destination pane?' },
    ]);
    await expect(
      events(new FakeIntentProvider({ req_1: { kind: 'question', text: '' } }), request('req_1')),
    ).rejects.toMatchObject({ name: 'ProviderError', code: 'invalid_fixture' });
  });

  it('aborts before or between events without producing late intent', async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      events(new FakeIntentProvider({ req_1: archive }), request('req_1', controller.signal)),
    ).rejects.toMatchObject({
      name: 'ProviderError',
      code: 'cancelled',
    });
    const provider = new FakeIntentProvider({ req_1: archive }, { delayMs: 100 });
    const during = new AbortController();
    const pending = events(provider, request('req_1', during.signal));
    during.abort();
    await expect(pending).rejects.toMatchObject({ name: 'ProviderError', code: 'cancelled' });
  });

  it('returns isolated copies instead of exposing mutable fixture data', async () => {
    const provider = new FakeIntentProvider({ req_1: archive });
    const first = (await events(provider, request('req_1'))) as Array<{ intent: typeof archive }>;
    first[0].intent.intent.target = { kind: 'sourceDirectory', pane: 'left' };
    expect(await events(provider, request('req_1'))).toEqual([{ kind: 'intent', intent: archive }]);
  });
});
