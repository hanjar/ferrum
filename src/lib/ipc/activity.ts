/**
 * Activity IPC (phase-4): typed wrappers over the Rust activity service.
 * Every call degrades to an empty result in browser dev — activity data is
 * desktop-only and must never break browsing.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { isInTauri } from './tauri';
import type { MatchKind, ReasonCode } from '../activity/contract';

export interface ActivityEntityDto {
  id: string;
  kind: 'file' | 'folder';
  provider: string;
  volumeId: string;
  locator: string;
  parentPath: string;
  displayName: string;
  availability: string;
}

export interface ActivityItemDto {
  entity: ActivityEntityDto;
  lastUtcMs: number;
  episodeCount: number;
  activeDays: number;
  score: number | null;
  /** Why the item is listed; absent only on old backends. */
  reasonCodes?: ReasonCode[];
}

export interface ActivityMatchDto {
  entity: ActivityEntityDto;
  matchClass: string;
  /** Contract match kind (subsequence matches report `fuzzy`). */
  matchKind?: MatchKind;
  reasonCodes?: ReasonCode[];
}

export interface ActivityStateDto {
  recordingEnabled: boolean;
  privacyEpoch: number;
  /** 7 | 30 | 90 — persisted by the service; absent only on old backends. */
  retentionDays?: number;
  hiddenCount?: number;
  disclosureAcked?: boolean;
}

export type RetentionDays = 7 | 30 | 90;
export type ActivityWindowDays = RetentionDays;
export type ActivitySort = 'recent' | 'frequent';

export interface ActivityOverviewItemDto extends ActivityItemDto {
  /** Hidden from suggestions; statistics still count it. */
  hidden: boolean;
}

export interface ActivityDailyBucketDto {
  dayUtcMs: number;
  /** Distinct items whose latest access is on this UTC day (not clicks). */
  items: number;
}

export interface ActivityOverviewDto {
  days: number;
  sort: ActivitySort;
  items: ActivityOverviewItemDto[];
  daily: ActivityDailyBucketDto[];
  totalItems: number;
  totalEpisodes: number;
  /** Oldest retained access among scanned items: the real coverage start. */
  oldestUtcMs: number | null;
  truncated: boolean;
  retentionDays: number;
  privacyEpoch: number;
}

export type ClearScope = 'all' | 'forget' | 'exclude' | 'retention';

export interface ScopePreviewDto {
  scope: ClearScope;
  items: number;
  episodes: number;
  sample: string[];
  truncated: boolean;
  favoritesKept: number;
  hiddenKept: number;
}

export interface ActivityFavoriteDto {
  path: string;
  label: string;
  /** Folders navigate; files open with the OS default app. */
  kind: 'folder' | 'file';
}

export type ActivityQueryKind = 'folder' | 'file';
export type ActivitySearchKind = ActivityQueryKind | 'all';

/**
 * `workspaceRoot` (optional absolute path) restricts results to that folder and
 * everything below it. Ferrum has no persisted workspace entity, so the caller
 * names the root; omit it (or pass null) for no filter.
 */
const scoped = (workspaceRoot?: string | null): { workspaceRoot?: string } =>
  workspaceRoot ? { workspaceRoot } : {};

export async function activityRecent(
  kind: ActivityQueryKind,
  limit = 8,
  workspaceRoot?: string | null,
): Promise<ActivityItemDto[]> {
  if (!isInTauri()) return [];
  return invoke<ActivityItemDto[]>('activity_recent', { kind, limit, ...scoped(workspaceRoot) });
}

export async function activityFrequent(
  kind: ActivityQueryKind,
  limit = 8,
  workspaceRoot?: string | null,
): Promise<ActivityItemDto[]> {
  if (!isInTauri()) return [];
  return invoke<ActivityItemDto[]>('activity_frequent', { kind, limit, ...scoped(workspaceRoot) });
}

/**
 * `null` means the privacy epoch went stale; drop the response.
 *
 * No backend cancellation exists: an `activity_search` invoke always runs to
 * completion. Callers cancel client-side only, by dropping responses that are
 * no longer current (see CommandPalette `searchSeq`, "stale-drop").
 */
export async function activitySearch(
  query: string,
  kind: ActivitySearchKind,
  limit = 8,
  workspaceRoot?: string | null,
): Promise<ActivityMatchDto[] | null> {
  const trimmed = query.trim();
  if (!isInTauri() || trimmed.length < 2) return [];
  return invoke<ActivityMatchDto[] | null>('activity_search', {
    query: trimmed,
    kind,
    limit,
    ...scoped(workspaceRoot),
  });
}

export async function activityState(): Promise<ActivityStateDto | null> {
  if (!isInTauri()) return null;
  return invoke<ActivityStateDto | null>('activity_state', {});
}

export async function activitySetRecording(enabled: boolean): Promise<ActivityStateDto | null> {
  if (!isInTauri()) return null;
  return invoke<ActivityStateDto | null>('activity_set_recording', { enabled });
}

export async function activityForget(entityId: string): Promise<void> {
  if (!isInTauri()) return;
  await invoke('activity_forget', { entityId });
}

export async function activityExclude(path: string): Promise<void> {
  if (!isInTauri()) return;
  await invoke('activity_exclude', { path });
}

export async function activityClear(): Promise<void> {
  if (!isInTauri()) return;
  await invoke('activity_clear', {});
}

export async function activityFavorites(): Promise<ActivityFavoriteDto[]> {
  if (!isInTauri()) return [];
  return invoke<ActivityFavoriteDto[]>('activity_favorites', {});
}

/** `true` when the path is pinned after this call; duplicate pins are no-ops. */
export async function activityAddFavorite(path: string, kind: 'folder' | 'file'): Promise<boolean> {
  if (!isInTauri()) return false;
  return invoke<boolean>('activity_add_favorite', { path, kind });
}

export async function activityRemoveFavorite(path: string): Promise<boolean> {
  if (!isInTauri()) return false;
  return invoke<boolean>('activity_remove_favorite', { path });
}

export async function activitySetRetention(days: RetentionDays): Promise<ActivityStateDto | null> {
  if (!isInTauri()) return null;
  return invoke<ActivityStateDto | null>('activity_set_retention', { days });
}

/** Hide = drop from suggestions only; statistics are kept. */
export async function activitySetHidden(
  entityId: string,
  hidden: boolean,
): Promise<ActivityStateDto | null> {
  if (!isInTauri()) return null;
  return invoke<ActivityStateDto | null>('activity_set_hidden', { entityId, hidden });
}

export async function activityAckDisclosure(): Promise<ActivityStateDto | null> {
  if (!isInTauri()) return null;
  return invoke<ActivityStateDto | null>('activity_ack_disclosure', {});
}

export async function activityOverview(
  days: ActivityWindowDays,
  kind: ActivityQueryKind,
  sort: ActivitySort,
  limit = 100,
): Promise<ActivityOverviewDto | null> {
  if (!isInTauri()) return null;
  return invoke<ActivityOverviewDto>('activity_overview', { days, kind, sort, limit });
}

/** Read-only: what the scope would remove. Never mutates. */
export async function activityPreview(
  scope: ClearScope,
  target?: string,
): Promise<ScopePreviewDto | null> {
  if (!isInTauri()) return null;
  return invoke<ScopePreviewDto>('activity_preview', { scope, target: target ?? null });
}

/** Another window (or this one) changed privacy state; payload is the new state. */
export function onActivityChanged(cb: (state: ActivityStateDto) => void): Promise<UnlistenFn> {
  if (!isInTauri()) return Promise.resolve(() => undefined);
  return listen<ActivityStateDto>('activity://changed', (e) => cb(e.payload));
}

/**
 * OS open with the default association; returns false when declined.
 * `record` is mandatory (FER-432, fail-closed): callers pass
 * `activity.recordingActive`; `false` (private session / pause) opens without
 * a receipt. The backend treats a missing flag as `false` too.
 */
export async function shellOpen(path: string, record: boolean): Promise<boolean> {
  if (!isInTauri()) return false;
  return invoke<boolean>('shell_open', { path, record });
}

/**
 * Fire-and-forget receipt: navigation/open success is recorded best-effort
 * and must never surface an error into the UI path that caused it.
 */
export function recordActivityVisit(path: string, kind: ActivityQueryKind): void {
  if (!isInTauri()) return;
  void invoke('activity_record_visit', { path, kind }).catch(() => undefined);
}

/**
 * Lazy availability check for one suggestion path (stat is bounded and only
 * runs when the stored identity is stale). Resolves to the lowercase
 * availability label, or null when unknown / outside Tauri.
 */
export async function activityValidate(path: string): Promise<string | null> {
  if (!isInTauri()) return null;
  return invoke<string | null>('activity_validate', { path });
}
