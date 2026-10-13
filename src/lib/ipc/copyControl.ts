/**
 * FER-471: live copy-thread controls. The backend owns the numbers; the UI shows
 * what `operations://copy-control` (and the snapshot/adjust commands) report and
 * never guesses. Only performance knobs: no file is touched by any of this.
 */
import { invoke } from '@tauri-apps/api/core';
import type { OperationId } from './types';
import { isInTauri } from './tauri';

export const COPY_THREADS_MIN = 1;
export const COPY_THREADS_MAX = 16;

export type CopyPriority = 'normal' | 'background';
export type CopyMedia = 'ssd' | 'hdd' | 'network' | 'unknown';

export interface CopyControlState {
  operationId: OperationId;
  /** Automatic (adaptive) worker count. */
  auto: boolean;
  /** Target worker count: the number between [-] and [+]. */
  threads: number;
  /** Workers alive now; a [-] takes effect at the next file boundary. */
  activeWorkers: number;
  /** Ceiling of the automatic mode for this media. */
  autoMax: number;
  media: CopyMedia;
  priority: CopyPriority;
  running: boolean;
}

export type CopyAdjust =
  | { kind: 'up'; by?: number }
  | { kind: 'down'; by?: number }
  | { kind: 'set'; threads: number }
  | { kind: 'auto'; enabled: boolean }
  | { kind: 'priority'; priority: CopyPriority };

export const COPY_CONTROL_EVENT = 'operations://copy-control';

export function isCopyControlState(value: unknown): value is CopyControlState {
  if (!value || typeof value !== 'object') return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.operationId === 'string' &&
    typeof v.auto === 'boolean' &&
    Number.isInteger(v.threads) &&
    Number.isInteger(v.activeWorkers) &&
    Number.isInteger(v.autoMax) &&
    (v.priority === 'normal' || v.priority === 'background') &&
    typeof v.running === 'boolean'
  );
}

export async function copyControlSnapshot(): Promise<CopyControlState[]> {
  if (!isInTauri()) return [];
  const raw = await invoke<unknown>('copy_control_snapshot');
  return Array.isArray(raw) ? raw.filter(isCopyControlState) : [];
}

/** Resolves with the state after the change; rejects when the job is no longer running. */
export async function copyControlAdjust(
  operationId: OperationId,
  change: CopyAdjust,
): Promise<CopyControlState> {
  const raw = await invoke<unknown>('copy_control_adjust', { operationId, change });
  if (!isCopyControlState(raw)) throw new Error('Invalid copy control state');
  return raw;
}

/** Label for the media class in the UI. */
export function mediaLabel(media: CopyMedia): string {
  return media === 'ssd'
    ? 'SSD'
    : media === 'hdd'
      ? 'HDD'
      : media === 'network'
        ? 'Network'
        : 'Unknown disk';
}
