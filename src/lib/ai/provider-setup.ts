import {
  BACKEND_ERROR_KINDS,
  backendErrorDetail,
  backendErrorKind,
  RemoteProviderError,
  validateProviderConfig,
  type BackendErrorKind,
  type RemoteProviderConfig,
} from './remote-provider';

// Explicit provider setup (endpoint + model) and user-triggered connection test. The setup holds
// no secret: the credential stays in the OS store and is read only by the Rust backend.
export type ProviderSetup = Required<RemoteProviderConfig>;

const SETUP_KEYS = ['endpoint', 'model', 'timeoutMs', 'credentialRef', 'structured'] as const;

export function validateProviderSetup(raw: unknown): ProviderSetup {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new RemoteProviderError('invalid_config', 'Invalid provider configuration');
  }
  const record = raw as Record<string, unknown>;
  if (Object.keys(record).some((key) => !(SETUP_KEYS as readonly string[]).includes(key))) {
    throw new RemoteProviderError('invalid_config', 'Invalid provider configuration');
  }
  const setup = {
    ...record,
    structured: record.structured ?? 'json_object',
  } as unknown as ProviderSetup;
  validateProviderConfig(setup);
  return {
    endpoint: setup.endpoint,
    model: setup.model,
    timeoutMs: setup.timeoutMs,
    credentialRef: setup.credentialRef,
    structured: setup.structured,
  };
}

export type ConnectionTestResult =
  | { ok: true }
  | {
      ok: false;
      kind: BackendErrorKind | 'backend_failure';
      /** FER-573: sanitized summary of the provider's own error message (display only). */
      detail?: string;
    };

// Must invoke a backend command; it resolves { ok, kind } and never receives user text or paths.
export type BackendConnectionTransport = (
  setup: ProviderSetup,
  signal: AbortSignal,
) => Promise<unknown>;

export async function testConnection(
  raw: unknown,
  transport: BackendConnectionTransport,
  signal: AbortSignal,
): Promise<ConnectionTestResult> {
  let setup: ProviderSetup;
  try {
    setup = validateProviderSetup(raw);
  } catch (error) {
    return {
      ok: false,
      kind:
        error instanceof RemoteProviderError && error.code !== 'backend_failure'
          ? error.code
          : 'invalid_config',
    };
  }
  if (signal.aborted) return { ok: false, kind: 'cancelled' };
  const cancelled = new Promise<ConnectionTestResult>((resolve) => {
    signal.addEventListener('abort', () => resolve({ ok: false, kind: 'cancelled' }), {
      once: true,
    });
  });
  const run = (async (): Promise<ConnectionTestResult> => {
    try {
      const result = await transport(setup, signal);
      if (result && typeof result === 'object' && (result as { ok?: unknown }).ok === true)
        return { ok: true };
      const kind = backendErrorKind(result);
      const detail = kind ? backendErrorDetail(result) : undefined;
      return { ok: false, kind: kind ?? 'backend_failure', ...(detail ? { detail } : {}) };
    } catch (error) {
      const kind = backendErrorKind(error);
      const detail = kind ? backendErrorDetail(error) : undefined;
      return { ok: false, kind: kind ?? 'backend_failure', ...(detail ? { detail } : {}) };
    }
  })();
  const outcome = await Promise.race([run, cancelled]);
  return signal.aborted ? { ok: false, kind: 'cancelled' } : outcome;
}

// Shown at setup time; keep in sync with the backend OutboundContext (counts and flags only).
export const OUTBOUND_DISCLOSURE: readonly string[] = [
  'Your typed request is sent to the configured provider endpoint, which may be a cloud service.',
  'Together with it Ferrum sends only: active pane, location kind (disk or archive), item counts, and whether hidden items or a filter are active.',
  'Ferrum never sends file contents, file names, folder listings, paths or archive passwords for ZIP or sync planning.',
  'The API key stays in the operating system credential store and is read only by the Ferrum backend.',
];

export { BACKEND_ERROR_KINDS };
