// AI-06 / FER-196: port ledger idempotensi (backend: src-tauri/src/ai/executor.rs) +
// implementasi memori yang meniru semantik backend persis (untuk tes dan mode tanpa Tauri).
import { digestOf } from './commands';

export type LedgerState = 'reserved' | 'enqueued' | 'unknownOutcome' | 'notStarted';

export interface LedgerEntry {
  key: string;
  executionId: string;
  planId: string;
  planDigest: string;
  state: LedgerState;
  jobIds: string[];
}

export type Reservation = { kind: 'fresh'; key: string } | { kind: 'existing'; entry: LedgerEntry };
export type Resolution = { kind: 'found'; jobIds: string[] } | { kind: 'notFound' };

/** Semua panggilan async: implementasi nyata adalah IPC ke backend (ledger durable). */
export interface ExecutionLedgerPort {
  /** Catat niat SEBELUM enqueue. `existing` berarti JANGAN enqueue lagi. */
  reserve(executionId: string, planId: string, planDigest: string): Promise<Reservation>;
  recordEnqueued(key: string, jobIds: readonly string[]): Promise<void>;
  /** Gagal sebelum menulis apa pun; plan boleh disusun ulang. */
  recordNotStarted(key: string): Promise<void>;
  /** Hasil enqueue tidak jelas; jangan replay. */
  markUnknown(key: string): Promise<void>;
  resolveUnknown(key: string, resolution: Resolution): Promise<LedgerEntry>;
  byExecution(executionId: string): Promise<LedgerEntry | undefined>;
  unresolved(): Promise<LedgerEntry[]>;
}

const REF = /^[A-Za-z0-9_-]{1,128}$/;

export function idempotencyKey(executionId: string, planDigest: string): string {
  return `idem_${digestOf([executionId, planDigest])}`;
}

export class LedgerError extends Error {
  constructor(public readonly code: 'invalid_reference' | 'unknown_key' | 'bad_state') {
    super(code);
    this.name = 'LedgerError';
  }
}

/** Persistensi tiruan: `persisted()` = isi journal; `MemoryExecutionLedger.restart(p)` = proses baru. */
export interface PersistedLedger {
  entries: LedgerEntry[];
}

export class MemoryExecutionLedger implements ExecutionLedgerPort {
  private readonly entries = new Map<string, LedgerEntry>();

  /** Restart: entri `reserved` tanpa konfirmasi enqueue menjadi `unknownOutcome` (batas crash). */
  static restart(persisted: PersistedLedger): MemoryExecutionLedger {
    const ledger = new MemoryExecutionLedger();
    for (const e of persisted.entries) {
      ledger.entries.set(e.key, {
        ...e,
        jobIds: [...e.jobIds],
        state: e.state === 'reserved' ? 'unknownOutcome' : e.state,
      });
    }
    return ledger;
  }

  persisted(): PersistedLedger {
    return { entries: [...this.entries.values()].map((e) => ({ ...e, jobIds: [...e.jobIds] })) };
  }

  async reserve(executionId: string, planId: string, planDigest: string): Promise<Reservation> {
    if (![executionId, planId, planDigest].every((v) => REF.test(v)))
      throw new LedgerError('invalid_reference');
    const key = idempotencyKey(executionId, planDigest);
    const same = this.entries.get(key);
    if (same && same.state !== 'notStarted') return { kind: 'existing', entry: { ...same } };
    for (const e of this.entries.values()) {
      if (e.planId === planId && e.state !== 'notStarted')
        return { kind: 'existing', entry: { ...e } };
    }
    this.entries.set(key, { key, executionId, planId, planDigest, state: 'reserved', jobIds: [] });
    return { kind: 'fresh', key };
  }

  private get(key: string): LedgerEntry {
    const e = this.entries.get(key);
    if (!e) throw new LedgerError('unknown_key');
    return e;
  }

  async recordEnqueued(key: string, jobIds: readonly string[]): Promise<void> {
    if (!jobIds.every((j) => REF.test(j))) throw new LedgerError('invalid_reference');
    const e = this.get(key);
    if (e.state !== 'reserved' && e.state !== 'unknownOutcome') throw new LedgerError('bad_state');
    e.state = 'enqueued';
    e.jobIds = [...jobIds];
  }

  async recordNotStarted(key: string): Promise<void> {
    const e = this.get(key);
    if (e.state !== 'reserved' && e.state !== 'unknownOutcome') throw new LedgerError('bad_state');
    e.state = 'notStarted';
  }

  async markUnknown(key: string): Promise<void> {
    const e = this.get(key);
    if (e.state === 'reserved') e.state = 'unknownOutcome';
  }

  async resolveUnknown(key: string, resolution: Resolution): Promise<LedgerEntry> {
    const e = this.get(key);
    if (e.state !== 'unknownOutcome') throw new LedgerError('bad_state');
    if (resolution.kind === 'found') await this.recordEnqueued(key, resolution.jobIds);
    else await this.recordNotStarted(key);
    return { ...this.get(key) };
  }

  async byExecution(executionId: string): Promise<LedgerEntry | undefined> {
    for (const e of this.entries.values())
      if (e.executionId === executionId) return { ...e, jobIds: [...e.jobIds] };
    return undefined;
  }

  async unresolved(): Promise<LedgerEntry[]> {
    return [...this.entries.values()]
      .filter((e) => e.state === 'unknownOutcome')
      .map((e) => ({ ...e }));
  }
}

/** Job ID yang boleh dibatalkan/dipantau untuk execution ini (hanya dari ledger yang enqueued). */
export async function ownedJobIds(
  ledger: ExecutionLedgerPort,
  executionId: string,
): Promise<string[]> {
  const entry = await ledger.byExecution(executionId);
  return entry && entry.state === 'enqueued' ? [...entry.jobIds] : [];
}
