import {
  ACTIVITY_QUERY_CAPABILITY,
  COPY_ADJUST_CAPABILITY,
  DISK_USAGE_QUERY_CAPABILITY,
  INDEXER_SET_MODE_CAPABILITY,
  INDEXER_STATUS_CAPABILITY,
  ITEMS_DELETE_CAPABILITY,
  PANE_NAVIGATE_CAPABILITY,
  parseChain,
  ContractError,
  parseIntent,
  parseOfferedIntent,
  type ActivityQuery,
  type ChainEnvelope,
  type PaneRef,
  type CopyAdjustRequest,
  type DiskUsageQueryKind,
  type IndexerSetModeRequest,
  type IntentEnvelope,
} from './contracts';
import { userAskedToDelete } from './paneNavigate';

export interface ProviderPlanRequest {
  requestId: string;
  userText: string;
  context: {
    activePane: 'left' | 'right';
    leftLocation: 'disk' | 'archive';
    rightLocation: 'disk' | 'archive';
    leftCount: number;
    rightCount: number;
    showHidden: boolean;
    filterActive: boolean;
  };
  /** Capability ids the app offers now (ready + enabled); the model may use only these. */
  capabilities?: readonly string[];
  signal: AbortSignal;
}

export type ProviderEvent =
  | { kind: 'intent'; intent: IntentEnvelope }
  | { kind: 'question'; text: string }
  /** ACT-08: read intent (only when `activity.query` was offered). Answered locally, never planned. */
  | { kind: 'activityQuery'; request: ActivityQuery }
  /** FER-471: tune the running copy job. Never planned; the user confirms on a card first. */
  | { kind: 'copyAdjust'; request: CopyAdjustRequest }
  /** FER-476: read-only FerrumIndexer status, answered by the app on a card. */
  | { kind: 'indexerStatus' }
  /** FER-476: switch the indexer privacy mode. Never planned; the user confirms on a card first. */
  | { kind: 'indexerSetMode'; request: IndexerSetModeRequest }
  /** FER-466: read-only Disk Usage question. Answered locally on the snapshot; no scan data leaves. */
  | { kind: 'diskUsageQuery'; query: DiskUsageQueryKind }
  /** FER-576: open a path the user typed (validated against the request text and the disk locally). */
  | { kind: 'paneNavigate'; pane: PaneRef; path: string }
  /** FER-576: an ordered multi-step plan (navigate? -> archive? -> delete?), run one step at a time. */
  | { kind: 'chain'; requestId: string; steps: ChainEnvelope['steps'] };

type OfferedEvent = Extract<
  ProviderEvent,
  {
    kind:
      | 'intent'
      | 'activityQuery'
      | 'copyAdjust'
      | 'indexerStatus'
      | 'indexerSetMode'
      | 'diskUsageQuery'
      | 'paneNavigate'
      | 'chain';
  }
>;

/**
 * Model reply -> event. A reply naming `activity.query` is accepted only when the request offered it
 * (`parseOfferedIntent`); every other reply goes through the unchanged `parseIntent`.
 */
export function parseModelReply(
  value: unknown,
  requestId: string,
  offered: readonly string[] | undefined,
  userText = '',
): OfferedEvent {
  if (value && typeof value === 'object' && !Array.isArray(value) && 'steps' in value) {
    const chain = parseChain(value, requestId, offered ?? [], userText);
    return { kind: 'chain', requestId: chain.requestId, steps: chain.steps };
  }
  const capability = (value as { intent?: { capability?: unknown } } | null)?.intent?.capability;
  if (capability === PANE_NAVIGATE_CAPABILITY && offered?.includes(PANE_NAVIGATE_CAPABILITY)) {
    const envelope = parseOfferedIntent(value, requestId, offered);
    if (envelope.intent.capability === PANE_NAVIGATE_CAPABILITY) {
      return { kind: 'paneNavigate', pane: envelope.intent.pane, path: envelope.intent.path };
    }
  }
  if (capability === ITEMS_DELETE_CAPABILITY && !offered?.includes(ITEMS_DELETE_CAPABILITY)) {
    throw new ContractError('invalid_payload', 'Capability not offered');
  }
  // FER-576 (review R1, S5): a delete is only accepted when the user asked to delete.
  if (capability === ITEMS_DELETE_CAPABILITY && !userAskedToDelete(userText)) {
    throw new ContractError('invalid_payload', 'The request does not ask to delete');
  }
  if (
    (capability === INDEXER_STATUS_CAPABILITY || capability === INDEXER_SET_MODE_CAPABILITY) &&
    offered?.includes(capability)
  ) {
    const envelope = parseOfferedIntent(value, requestId, offered);
    if (envelope.intent.capability === INDEXER_STATUS_CAPABILITY) return { kind: 'indexerStatus' };
    if (envelope.intent.capability === INDEXER_SET_MODE_CAPABILITY) {
      return { kind: 'indexerSetMode', request: { mode: envelope.intent.mode } };
    }
  }
  if (
    capability === DISK_USAGE_QUERY_CAPABILITY &&
    offered?.includes(DISK_USAGE_QUERY_CAPABILITY)
  ) {
    const envelope = parseOfferedIntent(value, requestId, offered);
    if (envelope.intent.capability === DISK_USAGE_QUERY_CAPABILITY) {
      return { kind: 'diskUsageQuery', query: envelope.intent.query };
    }
  }
  if (capability === COPY_ADJUST_CAPABILITY && offered?.includes(COPY_ADJUST_CAPABILITY)) {
    const envelope = parseOfferedIntent(value, requestId, offered);
    if (envelope.intent.capability === COPY_ADJUST_CAPABILITY) {
      const request: Record<string, unknown> = { ...envelope.intent };
      delete request.capability;
      return { kind: 'copyAdjust', request: request as CopyAdjustRequest };
    }
  }
  if (capability === ACTIVITY_QUERY_CAPABILITY && offered?.includes(ACTIVITY_QUERY_CAPABILITY)) {
    const envelope = parseOfferedIntent(value, requestId, offered);
    if (envelope.intent.capability === ACTIVITY_QUERY_CAPABILITY) {
      const { query, limit } = envelope.intent;
      return { kind: 'activityQuery', request: limit === undefined ? { query } : { query, limit } };
    }
  }
  return { kind: 'intent', intent: parseIntent(value, requestId) };
}

export interface IntentProvider {
  plan(request: ProviderPlanRequest): AsyncIterable<ProviderEvent>;
}

export type ProviderErrorCode = 'cancelled' | 'no_fixture' | 'invalid_fixture';

export class ProviderError extends Error {
  constructor(
    public readonly code: ProviderErrorCode,
    message: string,
  ) {
    super(message);
    this.name = 'ProviderError';
  }
}

type FakeResponse = unknown;

function ensureActive(signal: AbortSignal): void {
  if (signal.aborted) throw new ProviderError('cancelled', 'Request cancelled');
}

function wait(delayMs: number, signal: AbortSignal): Promise<void> {
  if (!delayMs) return Promise.resolve();
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new ProviderError('cancelled', 'Request cancelled'));
      return;
    }
    const abort = () => {
      clearTimeout(timer);
      reject(new ProviderError('cancelled', 'Request cancelled'));
    };
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', abort);
      resolve();
    }, delayMs);
    signal.addEventListener('abort', abort, { once: true });
  });
}

export class FakeIntentProvider implements IntentProvider {
  constructor(
    private readonly fixtures: Readonly<Record<string, FakeResponse>>,
    private readonly options: Readonly<{ delayMs?: number }> = {},
  ) {
    const delay = options.delayMs ?? 0;
    if (!Number.isSafeInteger(delay) || delay < 0 || delay > 30_000) {
      throw new ProviderError('invalid_fixture', 'Invalid fake delay');
    }
  }

  async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
    ensureActive(request.signal);
    if (!Object.hasOwn(this.fixtures, request.requestId)) {
      throw new ProviderError('no_fixture', 'No fake response for request');
    }
    await wait(this.options.delayMs ?? 0, request.signal);
    ensureActive(request.signal);
    const response = this.fixtures[request.requestId];
    if (
      response &&
      typeof response === 'object' &&
      !Array.isArray(response) &&
      Object.hasOwn(response, 'kind')
    ) {
      const question = response as Record<string, unknown>;
      if (
        question.kind !== 'question' ||
        Object.keys(question).length !== 2 ||
        typeof question.text !== 'string' ||
        !question.text.trim() ||
        new TextEncoder().encode(question.text).length > 512
      ) {
        throw new ProviderError('invalid_fixture', 'Invalid fake question');
      }
      ensureActive(request.signal);
      yield { kind: 'question', text: question.text };
      return;
    }
    const event = parseModelReply(
      response,
      request.requestId,
      request.capabilities,
      request.userText,
    );
    ensureActive(request.signal);
    yield structuredClone(event);
  }
}
