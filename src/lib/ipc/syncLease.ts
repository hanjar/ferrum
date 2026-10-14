/**
 * Root lease IPC (Automation P0.4, design §3.4 / §7.1): typed wrappers over
 * `sync_lease_acquire` / `sync_lease_release` in `src-tauri/src/automation`. A lease is the
 * cross-process lock on a folder pair: while a manual Synchronize Dirs run (or the AI chat sync)
 * holds one, no automation or second Ferrum process may touch overlapping folders, and the
 * backend refuses `sync_baseline_save` without one.
 *
 * The backend keeps the lease alive (heartbeat) and only hands out an unguessable token; the
 * webview never sees or sends paths of other leases. Errors arrive as `{ code, message }` with a
 * closed-set code and never carry paths. Outside the desktop app there is no second process, so
 * a local stand-in lease is returned and nothing is persisted.
 */
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';

/** Who is in the way of a refused lease: the kind of holder only. */
export interface LeaseHolder {
  kind: 'manual' | 'app' | 'runner' | 'unknown';
  runId?: number;
}

export type LeaseAcquire = { leaseId: string } | { status: 'busy'; holder: LeaseHolder };

/** Rejection data from the lease commands: a closed-set code + a fixed message, never a path. */
export class SyncLeaseError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = 'SyncLeaseError';
    this.code = code;
  }

  static fromIPC(raw: unknown): SyncLeaseError {
    if (typeof raw === 'object' && raw !== null && 'code' in raw) {
      const r = raw as { code?: unknown; message?: unknown };
      if (typeof r.code === 'string') {
        return new SyncLeaseError(r.code, typeof r.message === 'string' ? r.message : r.code);
      }
    }
    return new SyncLeaseError('unknown', 'The folder lease could not be taken');
  }
}

const HOLDER_KINDS = new Set(['manual', 'app', 'runner']);

function asHolder(raw: unknown): LeaseHolder {
  const r = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  const kind = typeof r.kind === 'string' && HOLDER_KINDS.has(r.kind) ? r.kind : 'unknown';
  return {
    kind: kind as LeaseHolder['kind'],
    ...(typeof r.runId === 'number' ? { runId: r.runId } : {}),
  };
}

/**
 * Take a lease on the folder pair. `{ status: 'busy' }` is a normal answer (the folders are in
 * use); everything else that goes wrong throws `SyncLeaseError`.
 */
export async function syncLeaseAcquire(left: string, right: string): Promise<LeaseAcquire> {
  if (!isInTauri()) return { leaseId: 'local' };
  let raw: unknown;
  try {
    raw = await invoke('sync_lease_acquire', { left, right });
  } catch (e) {
    throw SyncLeaseError.fromIPC(e);
  }
  const r = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  if (typeof r.leaseId === 'string' && r.leaseId.length > 0) return { leaseId: r.leaseId };
  if (r.status === 'busy') return { status: 'busy', holder: asHolder(r.holder) };
  throw new SyncLeaseError('unknown', 'The folder lease answer was not understood');
}

/** Release a lease by its token. Idempotent; an unknown token does nothing. */
export async function syncLeaseRelease(leaseId: string): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('sync_lease_release', { leaseId });
  } catch (e) {
    throw SyncLeaseError.fromIPC(e);
  }
}
