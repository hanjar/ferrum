/**
 * FER-598: IPC for the recent-file sources that live outside the Activity store: Windows Recent
 * and newly added items of watched folders. Outside Tauri every call resolves to an empty result.
 */
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';

export interface RecentFileDto {
  path: string;
  name: string;
  /** null = unknown (never from a blocking stat). */
  isDir: boolean | null;
  lastUsedMs: number;
  /** 'windows' (Windows Recent) or 'new' (newly added item of a watched folder). */
  source: 'windows' | 'new';
  /** Display label: Windows, Downloads or the watched folder's name. */
  label: string;
}

export async function recentWindows(limit = 200): Promise<RecentFileDto[]> {
  if (!isInTauri()) return [];
  return invoke<RecentFileDto[]>('recent_windows', { limit });
}

export async function recentNewItems(
  folders: string[],
  maxAgeDays: number,
  limit = 200,
): Promise<RecentFileDto[]> {
  if (!isInTauri()) return [];
  return invoke<RecentFileDto[]>('recent_new_items', { folders, maxAgeDays, limit });
}

/** The default watched folder list (Downloads). */
export async function recentDefaultFolders(): Promise<string[]> {
  if (!isInTauri()) return [];
  return invoke<string[]>('recent_default_folders', {});
}

/** The subset of `folders` the backend accepts as watched folders (absolute, not Ferrum's own data). */
export async function recentValidateFolders(folders: string[]): Promise<string[]> {
  if (!isInTauri()) return folders;
  return invoke<string[]>('recent_validate_folders', { folders });
}
