import { ContractError } from './contracts';
import {
  parseHint,
  parseModelReply,
  type IntentProvider,
  type ProviderEvent,
  type ProviderPlanRequest,
} from './provider';

export interface RemoteProviderConfig {
  endpoint: string;
  model: string;
  timeoutMs: number;
  credentialRef: string;
  // 'json_object' asks the provider for JSON mode; 'none' for endpoints that reject response_format.
  structured?: 'json_object' | 'none';
}

export interface BackendPlanInput {
  config: RemoteProviderConfig;
  requestId: string;
  userText: string;
  context: ProviderPlanRequest['context'];
  capabilities?: readonly string[];
}

// The transport must invoke a backend command; browser fetch would expose provider credentials.
export type BackendIntentTransport = (
  input: BackendPlanInput,
  signal: AbortSignal,
) => Promise<unknown>;

// Stable ids emitted by the Rust backend (ProviderErrorKind, snake_case) plus local ones.
export const BACKEND_ERROR_KINDS = [
  'invalid_config',
  'unsafe_endpoint',
  // FER-512: the request endpoint differs from the saved AI setup (or there is none).
  'endpoint_mismatch',
  'credential',
  'auth',
  'rate_limited',
  'quota_exceeded',
  'model_unavailable',
  'bad_request',
  'server_error',
  'network',
  'unreachable',
  'timeout',
  'cancelled',
  'malformed',
  'refused',
] as const;
export type BackendErrorKind = (typeof BACKEND_ERROR_KINDS)[number];
export type RemoteProviderErrorCode = BackendErrorKind | 'backend_failure';
export class RemoteProviderError extends Error {
  constructor(
    public readonly code: RemoteProviderErrorCode,
    message: string,
    /** FER-573: sanitized summary of the provider's own error message (display only). */
    public readonly detail?: string,
  ) {
    super(message);
    this.name = 'RemoteProviderError';
  }
}

const MAX_DETAIL_CHARS = 300;

/**
 * FER-573: the backend already sanitizes (and redacts the user's actual key verbatim; this layer
 * does not know the key, so it only applies the generic patterns) `detail`; this is a second, independent pass. Only a string
 * is accepted; control characters (including ANSI) are removed, whitespace collapsed, key-like
 * runs hidden and the length bounded. Rendered as text only, never as HTML.
 */
export function sanitizeProviderDetail(raw: unknown): string | undefined {
  if (typeof raw !== 'string') return undefined;
  const text = raw
    // eslint-disable-next-line no-control-regex
    .replace(/\u001b\[[0-9;?]*[A-Za-z]/g, ' ')
    .replace(/\p{Cf}/gu, '')
    .replace(/\p{Cc}/gu, ' ')
    .replace(/\bBearer\s+\S+/gi, 'Bearer [redacted]')
    .replace(/\bsk-[A-Za-z0-9_-]{6,}/g, '[redacted]')
    .replace(/[A-Za-z0-9+/=_]{40,}/g, '[redacted]')
    .replace(/\s+/g, ' ')
    .trim();
  return text ? Array.from(text).slice(0, MAX_DETAIL_CHARS).join('') : undefined;
}

/** Backend rejections may carry `{ kind, detail }`; only the sanitized detail string is read. */
export function backendErrorDetail(error: unknown): string | undefined {
  if (!error || typeof error !== 'object' || error instanceof Error) return undefined;
  return sanitizeProviderDetail((error as { detail?: unknown }).detail);
}

// Backend rejections are data objects like { kind }; only known kinds pass, never their messages.
export function backendErrorKind(error: unknown): BackendErrorKind | undefined {
  if (!error || typeof error !== 'object' || error instanceof Error) return undefined;
  const kind = (error as { kind?: unknown }).kind;
  return BACKEND_ERROR_KINDS.find((known) => known === kind);
}

const SAFE_MESSAGE = 'Provider request failed';

export function validateProviderConfig(config: RemoteProviderConfig): void {
  let url: URL;
  try {
    url = new URL(config.endpoint);
  } catch {
    throw new RemoteProviderError('unsafe_endpoint', 'Invalid provider endpoint');
  }
  const local = url.hostname === 'localhost' || url.hostname === '127.0.0.1';
  if (
    (url.protocol !== 'https:' && !(url.protocol === 'http:' && local && url.port)) ||
    url.username ||
    url.password ||
    url.hash ||
    url.search ||
    !/^(\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,31}){1,4}$/.test(url.pathname) ||
    config.endpoint.split('/').some((segment) => segment === '..' || segment === '.')
  ) {
    throw new RemoteProviderError(
      'unsafe_endpoint',
      'Provider endpoint must use HTTPS or explicit localhost HTTP with a plain base path',
    );
  }
  if (
    !config.model.trim() ||
    config.model.length > 128 ||
    !/^[A-Za-z0-9._:/-]+$/.test(config.model) ||
    config.credentialRef !== 'Ferrum/AI/custom' ||
    !Number.isSafeInteger(config.timeoutMs) ||
    config.timeoutMs < 100 ||
    config.timeoutMs > 120_000 ||
    (config.structured !== undefined &&
      config.structured !== 'json_object' &&
      config.structured !== 'none')
  ) {
    throw new RemoteProviderError('invalid_config', 'Invalid provider configuration');
  }
}

export function parseQuestion(value: unknown): string | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value) || !('kind' in value))
    return undefined;
  const record = value as Record<string, unknown>;
  const text = record.text;
  if (
    record.kind !== 'question' ||
    Object.keys(record).length !== 2 ||
    typeof text !== 'string' ||
    !text.trim() ||
    new TextEncoder().encode(text).length > 512 ||
    /\p{Cc}/u.test(text)
  ) {
    throw new ContractError('invalid_payload', 'Invalid provider question');
  }
  return text.trim();
}

export class RemoteIntentProvider implements IntentProvider {
  private readonly config: RemoteProviderConfig;
  constructor(
    config: RemoteProviderConfig,
    private readonly transport: BackendIntentTransport,
  ) {
    validateProviderConfig(config);
    this.config = { ...config };
  }

  async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
    if (request.signal.aborted) throw new RemoteProviderError('cancelled', 'Request cancelled');
    const context = request.context;
    if (
      typeof request.userText !== 'string' ||
      new TextEncoder().encode(request.userText).length > 8192 ||
      !/^[A-Za-z0-9_-]{1,128}$/.test(request.requestId) ||
      !context ||
      !['left', 'right'].includes(context.activePane) ||
      !['disk', 'archive'].includes(context.leftLocation) ||
      !['disk', 'archive'].includes(context.rightLocation) ||
      !Number.isSafeInteger(context.leftCount) ||
      context.leftCount < 0 ||
      context.leftCount > 1_000_000 ||
      !Number.isSafeInteger(context.rightCount) ||
      context.rightCount < 0 ||
      context.rightCount > 1_000_000 ||
      typeof context.showHidden !== 'boolean' ||
      typeof context.filterActive !== 'boolean'
    ) {
      throw new RemoteProviderError('invalid_config', 'Invalid provider request');
    }
    if (
      request.capabilities !== undefined &&
      (!Array.isArray(request.capabilities) ||
        request.capabilities.length > 12 ||
        new Set(request.capabilities).size !== request.capabilities.length ||
        request.capabilities.some(
          (id) => typeof id !== 'string' || !/^[a-zA-Z]+\.[a-zA-Z]+$/.test(id),
        ))
    ) {
      throw new RemoteProviderError('invalid_config', 'Invalid provider request');
    }
    const controller = new AbortController();
    let timedOut = false;
    const abort = () => controller.abort();
    request.signal.addEventListener('abort', abort, { once: true });
    if (request.signal.aborted) {
      request.signal.removeEventListener('abort', abort);
      throw new RemoteProviderError('cancelled', 'Request cancelled');
    }
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, this.config.timeoutMs);
    try {
      const {
        activePane,
        leftLocation,
        rightLocation,
        leftCount,
        rightCount,
        showHidden,
        filterActive,
      } = request.context;
      const input: BackendPlanInput = {
        config: { ...this.config },
        requestId: request.requestId,
        userText: request.userText,
        context: {
          activePane,
          leftLocation,
          rightLocation,
          leftCount,
          rightCount,
          showHidden,
          filterActive,
        },
        ...(request.capabilities ? { capabilities: [...request.capabilities] } : {}),
      };
      const cancelled = new Promise<never>((_resolve, reject) => {
        controller.signal.addEventListener(
          'abort',
          () =>
            reject(
              new RemoteProviderError(
                timedOut ? 'timeout' : 'cancelled',
                timedOut ? 'Provider timed out' : 'Request cancelled',
              ),
            ),
          { once: true },
        );
      });
      const result = await Promise.race([this.transport(input, controller.signal), cancelled]);
      if (request.signal.aborted) throw new RemoteProviderError('cancelled', 'Request cancelled');
      if (timedOut) throw new RemoteProviderError('timeout', 'Provider timed out');
      // FER-576: the backend sends a validated multi-step plan as `{kind:'chain', chain:{...}}`.
      const chainResult =
        result && typeof result === 'object' && (result as { kind?: unknown }).kind === 'chain'
          ? (result as { chain?: unknown }).chain
          : undefined;
      const hint = chainResult === undefined ? parseHint(result, request.capabilities) : undefined;
      const question =
        chainResult === undefined && hint === undefined ? parseQuestion(result) : undefined;
      const reply =
        question === undefined && hint === undefined
          ? parseModelReply(
              chainResult ?? result,
              request.requestId,
              request.capabilities,
              request.userText,
            )
          : undefined;
      if (request.signal.aborted) throw new RemoteProviderError('cancelled', 'Request cancelled');
      if (hint !== undefined) yield { kind: 'hint', hint };
      else if (question !== undefined) yield { kind: 'question', text: question };
      else if (reply) yield reply;
    } catch (error) {
      if (request.signal.aborted) throw new RemoteProviderError('cancelled', 'Request cancelled');
      if (timedOut) throw new RemoteProviderError('timeout', 'Provider timed out');
      if (error instanceof ContractError) throw error;
      const kind = backendErrorKind(error);
      throw new RemoteProviderError(
        kind ?? 'backend_failure',
        SAFE_MESSAGE,
        kind ? backendErrorDetail(error) : undefined,
      );
    } finally {
      clearTimeout(timer);
      request.signal.removeEventListener('abort', abort);
    }
  }
}
