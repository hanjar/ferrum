/**
 * Sync baseline IPC (FER-414 / H2) — typed bridge over the local, versioned baseline store in
 * `src-tauri/src/sync_baseline.rs`. One baseline per (left, right) root pair, keyed by the
 * canonical identity of the pair on the Rust side. A baseline that is missing, corrupt, of an
 * unknown version, bound to other roots or on a detached volume comes back as `firstRun`; the
 * planner then stays in its most conservative mode.
 *
 * Only a `VerifiedBaseline` (the return type of `reconcileBaseline`) can be saved, so no other
 * code path can write facts that were not verified against a fresh compare.
 */
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';
import { isBaselineTrusted, type SyncBaseline, type VerifiedBaseline } from '../syncBaseline';

export interface RootPair {
  leftRoot: string;
  rightRoot: string;
}

export type FirstRunReason =
  'none' | 'corrupt' | 'unknownVersion' | 'rootsUnavailable' | 'mismatch' | 'tooLarge';

export type SyncBaselineLoad =
  { status: 'ok'; baseline: SyncBaseline } | { status: 'firstRun'; reason: FirstRunReason };

/** Rejection data from the store: a fixed code + message, never a path. */
export class SyncBaselineError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = 'SyncBaselineError';
    this.code = code;
  }

  static fromIPC(raw: unknown): SyncBaselineError {
    if (typeof raw === 'object' && raw !== null && 'code' in raw) {
      const r = raw as { code?: unknown; message?: unknown };
      if (typeof r.code === 'string') {
        return new SyncBaselineError(r.code, typeof r.message === 'string' ? r.message : r.code);
      }
    }
    return new SyncBaselineError('io', 'Baseline store failed');
  }
}

/** Load the stored baseline. Outside the desktop shell there is no store: always first-run. */
export async function syncBaselineLoad(pair: RootPair): Promise<SyncBaselineLoad> {
  if (!isInTauri()) return { status: 'firstRun', reason: 'none' };
  let raw: unknown;
  try {
    raw = await invoke('sync_baseline_load', { pair });
  } catch (e) {
    throw SyncBaselineError.fromIPC(e);
  }
  const r = raw as Partial<SyncBaselineLoad> | null;
  if (r && r.status === 'ok' && isBaselineTrusted(r.baseline, pair.leftRoot, pair.rightRoot)) {
    return { status: 'ok', baseline: r.baseline };
  }
  if (r && r.status === 'firstRun' && typeof r.reason === 'string') {
    return { status: 'firstRun', reason: r.reason };
  }
  // Anything else (wrong shape / roots) is not trusted.
  return { status: 'firstRun', reason: 'mismatch' };
}

/** Atomic save (Rust writes tmp + rename). Throws `SyncBaselineError` on refusal. */
export async function syncBaselineSave(pair: RootPair, baseline: VerifiedBaseline): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('sync_baseline_save', { pair, baseline });
  } catch (e) {
    throw SyncBaselineError.fromIPC(e);
  }
}

/** Forget the baseline of a pair (back to first-run). Idempotent. */
export async function syncBaselineClear(pair: RootPair): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('sync_baseline_clear', { pair });
  } catch (e) {
    throw SyncBaselineError.fromIPC(e);
  }
}
