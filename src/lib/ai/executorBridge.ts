// AI-06 / FER-196: jembatan tipis ke ledger backend (src-tauri/src/ai/executor.rs).
// Command Tauri `ai_exec_*` BELUM terdaftar (lib.rs/ipc.rs milik integrator): lihat docs/ai/executor.md.
// Tidak ada kredensial atau path melewati batas ini; hanya ID opak dan digest.
import { invoke } from '@tauri-apps/api/core';
import type { ExecutionLedgerPort, LedgerEntry, Reservation, Resolution } from './executorLedger';

export const backendExecutionLedger: ExecutionLedgerPort = {
  reserve: (executionId, planId, planDigest) =>
    invoke<Reservation>('ai_exec_reserve', { executionId, planId, planDigest }),
  recordEnqueued: (key, jobIds) =>
    invoke<void>('ai_exec_record_enqueued', { key, jobIds: [...jobIds] }),
  recordNotStarted: (key) => invoke<void>('ai_exec_record_not_started', { key }),
  markUnknown: (key) => invoke<void>('ai_exec_mark_unknown', { key }),
  resolveUnknown: (key: string, resolution: Resolution) =>
    invoke<LedgerEntry>('ai_exec_resolve_unknown', { key, resolution }),
  byExecution: async (executionId) =>
    (await invoke<LedgerEntry | null>('ai_exec_by_execution', { executionId })) ?? undefined,
  unresolved: () => invoke<LedgerEntry[]>('ai_exec_unresolved'),
};
