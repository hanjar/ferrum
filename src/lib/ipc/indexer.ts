/**
 * FerrumIndexer IPC (FER-474, IDX-S1): status of the optional Windows service and the privacy-mode
 * change. Every reply is validated; anything unexpected degrades to "not available" so Settings
 * never breaks when the service (or the desktop app) is absent.
 */
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';

export type IndexerMode = 'high_performance' | 'shared';
export const INDEXER_MODES: IndexerMode[] = ['high_performance', 'shared'];

export interface IndexerStatus {
  installed: boolean;
  /** SCM state: running, stopped, paused, start_pending, … */
  scmState: string | null;
  /** The service answered on its pipe. */
  reachable: boolean;
  version: string | null;
  mode: IndexerMode | null;
  /** Service-reported state: running | paused. */
  state: string | null;
  index: string | null;
  canLowerPrivacy: boolean;
  /** unsupported | location | scm | untrusted | squatted | unreachable | busy | protocol | ipc */
  problem: string | null;
  /** FER-480: PID of the process holding the pipe name when `problem === 'squatted'`. */
  squatterPid: number | null;
  /** FER-476: index work paused (service paused). */
  indexPaused: boolean;
  /** FER-476 review H1: the service token is not restricted, so queries are off (walker only). */
  degraded: boolean;
  /** FER-476: per-volume index state (empty when the service is not reachable). */
  volumes: IndexerVolume[];
}

export type IndexerVolumeState = 'building' | 'ready' | 'error';

/** One volume's index (FER-476). Codes only, never paths. */
export interface IndexerVolume {
  /** Drive letter `A`..`Z`. */
  letter: string;
  state: IndexerVolumeState;
  /** Error code when `state === 'error'` (`access_denied`, `memory_limit`, …). */
  error: string | null;
  /** Entries (names). Rounded down for non-admin clients in shared mode. */
  entries: number;
  /** Build progress 0..1000 (1000 = ready). */
  progressPermille: number;
  /** Kept current from the USN journal. */
  live: boolean;
}

/**
 * FER-480 review F1: another process holds the FerrumIndexer pipe name (its owner or server PID
 * is not the service's). Ferrum sent it nothing and uses its own scanning. Null otherwise.
 */
export function squattedNote(s: IndexerStatus): string | null {
  if (s.problem !== 'squatted') return null;
  const who = s.squatterPid !== null ? ` (PID ${s.squatterPid})` : '';
  return `Nama pipe FerrumIndexer dipakai proses lain${who} — indeks tidak dipakai, scan biasa aktif.`;
}

/** Overall readiness for the UI and the AI (`indexer.status`). */
export type IndexerReadiness =
  'not_installed' | 'stopped' | 'degraded' | 'building' | 'ready' | 'error';

/** Honest summary: not installed, stopped/unreachable, building, ready, or error. */
export function readiness(s: IndexerStatus): IndexerReadiness {
  if (!s.installed) return 'not_installed';
  if (!s.reachable) return 'stopped';
  // Queries are off whatever the index state: say so first.
  if (s.degraded) return 'degraded';
  // Per-volume detail wins over the aggregate when the service sent it.
  if (s.volumes.length > 0) {
    if (s.volumes.some((v) => v.state === 'building')) return 'building';
    if (s.volumes.some((v) => v.state === 'ready')) return 'ready';
    return 'error';
  }
  if (s.index === 'ready') return 'ready';
  if (s.index === 'error') return 'error';
  return 'building';
}

export interface IndexerSetModeResult {
  mode: IndexerMode;
  changed: boolean;
  /** The change went through UAC. */
  elevated: boolean;
}

/** A failed mode change; `code` is stable (`declined`, `not_running`, `refused`, …). */
export class IndexerError extends Error {
  constructor(
    public code: string,
    message = code,
  ) {
    super(message);
    this.name = 'IndexerError';
  }
}

export const UNAVAILABLE: IndexerStatus = {
  installed: false,
  scmState: null,
  reachable: false,
  version: null,
  mode: null,
  state: null,
  index: null,
  canLowerPrivacy: false,
  problem: 'unsupported',
  squatterPid: null,
  indexPaused: false,
  degraded: false,
  volumes: [],
};

function str(v: unknown, max = 64): string | null {
  return typeof v === 'string' && v.length > 0 && v.length <= max ? v : null;
}

export function isMode(v: unknown): v is IndexerMode {
  return v === 'high_performance' || v === 'shared';
}

const MAX_VOLUMES = 26;

function asVolume(raw: unknown): IndexerVolume | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const o = raw as Record<string, unknown>;
  const letter = typeof o.letter === 'string' && /^[A-Z]$/.test(o.letter) ? o.letter : null;
  const state =
    o.state === 'building' || o.state === 'ready' || o.state === 'error' ? o.state : null;
  if (!letter || !state) return null;
  const num = (v: unknown, max: number) =>
    typeof v === 'number' && Number.isFinite(v) && v >= 0 ? Math.min(Math.floor(v), max) : 0;
  return {
    letter,
    state,
    error: state === 'error' ? (str(o.error) ?? 'internal') : null,
    entries: num(o.entries, Number.MAX_SAFE_INTEGER),
    progressPermille: num(o.progressPermille, 1000),
    live: o.live === true,
  };
}

/** Defensive parse of the backend's status. */
export function asStatus(raw: unknown): IndexerStatus {
  if (typeof raw !== 'object' || raw === null) return { ...UNAVAILABLE, problem: 'ipc' };
  const o = raw as Record<string, unknown>;
  return {
    installed: o.installed === true,
    scmState: str(o.scmState),
    reachable: o.reachable === true,
    version: str(o.version),
    mode: isMode(o.mode) ? o.mode : null,
    state: str(o.state),
    index: str(o.index),
    canLowerPrivacy: o.canLowerPrivacy === true,
    problem: str(o.problem),
    squatterPid:
      typeof o.squatterPid === 'number' &&
      Number.isInteger(o.squatterPid) &&
      o.squatterPid > 0 &&
      o.squatterPid <= 0xffffffff
        ? o.squatterPid
        : null,
    indexPaused: o.indexPaused === true,
    degraded: o.degraded === true,
    volumes: Array.isArray(o.volumes)
      ? o.volumes
          .slice(0, MAX_VOLUMES)
          .map(asVolume)
          .filter((v): v is IndexerVolume => v !== null)
      : [],
  };
}

export async function indexerStatus(): Promise<IndexerStatus> {
  if (!isInTauri()) return { ...UNAVAILABLE };
  try {
    return asStatus(await invoke('indexer_status'));
  } catch {
    return { ...UNAVAILABLE, problem: 'ipc' };
  }
}

/**
 * Change the mode. FER-512: `confirmed` is accepted for compatibility but the backend IGNORES it
 * as authorization — every change asks in a native dialog owned by the Ferrum window, and only
 * the dialog's Yes reaches the service. Lowering privacy triggers UAC on the backend side.
 */
export async function indexerSetMode(
  mode: IndexerMode,
  confirmed: boolean,
): Promise<IndexerSetModeResult> {
  if (!isInTauri()) throw new IndexerError('unsupported');
  let raw: unknown;
  try {
    raw = await invoke('indexer_set_mode', { mode, confirmed });
  } catch (e) {
    const o = (typeof e === 'object' && e !== null ? e : {}) as Record<string, unknown>;
    throw new IndexerError(str(o.code) ?? 'failed', str(o.message, 300) ?? 'failed');
  }
  const o = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  if (!isMode(o.mode)) throw new IndexerError('failed', 'unexpected reply');
  return { mode: o.mode, changed: o.changed === true, elevated: o.elevated === true };
}

/** FER-477a: folder sizes from the FerrumIndexer (High performance mode only). */
export interface FolderSizesResult {
  /** The service answered for this parent. */
  available: boolean;
  /** Why not: not_local | unavailable | shared_mode | degraded | not_indexed | busy | timeout |
   *  too_many | protocol | ipc. */
  reason: string | null;
  /** One per requested name: total bytes below the folder, or null when unknown. */
  sizes: (number | null)[];
  /** Answered from the previous index while it is rebuilt. */
  stale: boolean;
}

/** Most names one call may carry (mirrors `MAX_FOLDER_SIZE_NAMES` in Rust). */
export const MAX_FOLDER_SIZE_NAMES = 1000;

/**
 * Total size of each folder `parent\name`. Never throws: anything unexpected is "not available"
 * and the file list keeps its "—". A reply whose length does not match the request is refused.
 */
export async function indexerFolderSizes(
  parent: string,
  names: string[],
): Promise<FolderSizesResult> {
  const none = (reason: string): FolderSizesResult => ({
    available: false,
    reason,
    sizes: names.map(() => null),
    stale: false,
  });
  if (!isInTauri()) return none('unavailable');
  if (names.length === 0) return { available: true, reason: null, sizes: [], stale: false };
  if (names.length > MAX_FOLDER_SIZE_NAMES) return none('too_many');
  let raw: unknown;
  try {
    raw = await invoke('indexer_folder_sizes', { parent, names });
  } catch {
    return none('ipc');
  }
  const o = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  if (!Array.isArray(o.sizes) || o.sizes.length !== names.length) return none('protocol');
  const sizes = o.sizes.map((v) =>
    typeof v === 'number' && Number.isFinite(v) && v >= 0 ? v : null,
  );
  return {
    available: o.available === true,
    reason: str(o.reason) ?? null,
    sizes,
    stale: o.stale === true,
  };
}
