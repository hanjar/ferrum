// FER-465: status provider AI untuk header chat dan tombol "Test connection" di kartu galat.
// Hasil Test connection terakhir; pengecekan otomatis saat panel dibuka dibatasi sekali per menit.
// Hanya kode aman dan host:port yang disimpan; tidak ada key atau isi respons.
import { backendConnectionTransport } from './backendBridge';
import { endpointHostPort } from './providerErrors';
import { testConnection, type ProviderSetup } from './provider-setup';
import { currentSetup } from './providerSetupStore';
import type { RemoteProviderErrorCode } from './remote-provider';

export type ProviderState = 'unknown' | 'checking' | 'ok' | 'down';

export const AUTO_CHECK_INTERVAL_MS = 60_000;

export interface ProviderStatusDeps {
  setup: () => unknown;
  test(setup: unknown, signal: AbortSignal): Promise<{ ok: true } | { ok: false; kind: string }>;
  now(): number;
}

export class ProviderStatus {
  state = $state<ProviderState>('unknown');
  /** Kode aman galat terakhir (null bila ok / belum dicek). */
  kind = $state<RemoteProviderErrorCode | 'backend_failure' | null>(null);
  hostPort = $state<string | undefined>(undefined);
  checkedAt = $state<number | null>(null);
  private inflight: Promise<void> | null = null;

  constructor(private readonly deps: ProviderStatusDeps) {}

  /** The composition root (aiRuntime) points this at its own setup loader (stored setup, then env). */
  useSetupSource(source: () => unknown): void {
    this.deps.setup = source;
  }

  /** `force`: tombol Test connection (selalu mengecek); tanpa force: dibatasi sekali per menit. */
  check(force = false): Promise<void> {
    if (this.inflight) return this.inflight;
    const now = this.deps.now();
    if (!force && this.checkedAt !== null && now - this.checkedAt < AUTO_CHECK_INTERVAL_MS) {
      return Promise.resolve();
    }
    const setup = this.deps.setup() as ProviderSetup | undefined;
    this.hostPort = endpointHostPort(setup?.endpoint);
    if (!setup) {
      this.state = 'unknown';
      this.kind = null;
      return Promise.resolve();
    }
    this.state = 'checking';
    this.inflight = (async () => {
      try {
        const result = await this.deps.test(setup, new AbortController().signal);
        this.state = result.ok ? 'ok' : 'down';
        this.kind = result.ok ? null : (result.kind as RemoteProviderErrorCode);
      } catch {
        this.state = 'unknown';
        this.kind = 'backend_failure';
      } finally {
        this.checkedAt = this.deps.now();
        this.inflight = null;
      }
    })();
    return this.inflight;
  }

  /** Dipakai bila galat nyata dari plan sudah menunjukkan provider mati (tanpa ping tambahan). */
  noteFailure(kind: RemoteProviderErrorCode | 'backend_failure'): void {
    if (kind === 'cancelled') return;
    this.state = 'down';
    this.kind = kind;
    this.checkedAt = this.deps.now();
  }
}

export const providerStatus = new ProviderStatus({
  setup: () => currentSetup(),
  test: (setup, signal) => testConnection(setup, backendConnectionTransport, signal),
  now: () => Date.now(),
});
