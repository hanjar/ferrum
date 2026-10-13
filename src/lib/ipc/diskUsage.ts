/** Disk usage IPC (DSK-02/03) — typed wrappers over disk_usage_start/
 *  cancel/snapshot/query_children/top_files and the `disk_usage://progress`
 *  event stream. Shapes mirror `src/lib/ipc/types.ts` (contract binding:
 *  docs/disk-usage-contract.md); the Rust source of truth is
 *  `src-tauri/src/disk_usage.rs` + `src-tauri/src/ipc.rs`.
 *
 *  Consumers (DSK-04 store) own ALL generation guards: late events/queries of
 *  an older scan must be discarded by scanId, never applied (contract §3.4).
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError, type DiskUsageChildrenPage, type DiskUsageFilter } from './types';
import type {
  DiskUsageCacheEntry,
  DiskUsageCacheLoad,
  DiskUsageFastEligibility,
  DiskUsageProgressEvent,
  DiskUsageScanOptions,
  DiskUsageSnapshot,
  DiskUsageSort,
  DiskUsageTopFiles,
} from './types';
import { isInTauri } from './tauri';

/** Kick off a progressive scan of `root`; results stream as events for the
 *  returned scanId. The scan keeps running until its single `done` event.
 *  FER-460 `fast`: try the elevated MFT helper first (UAC prompt); any
 *  refusal/failure falls back to the regular walk under the same scanId,
 *  with the reason in the events' `notice`. */
export async function diskUsageStart(
  root: string,
  options: DiskUsageScanOptions,
  fast = false,
  /** FER-476: 'index' = read the FerrumIndexer's index (no UAC); any
   *  failure falls back to the regular walk with a notice. */
  source?: 'index',
): Promise<string> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  try {
    return await invoke<string>('disk_usage_start', {
      root,
      options,
      fast,
      ...(source ? { source } : {}),
    });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** FER-460: cheap check (no elevation) whether `root` is on a local NTFS
 *  drive and the helper is installed. Browser dev / IPC failure = not
 *  eligible, so the offer simply never appears. */
export async function diskUsageFastEligibility(root: string): Promise<DiskUsageFastEligibility> {
  if (!isInTauri()) return { eligible: false, reason: 'desktop app only' };
  try {
    return await invoke<DiskUsageFastEligibility>('disk_usage_fast_eligibility', { root });
  } catch {
    return { eligible: false };
  }
}

/** FER-476: may Disk usage offer "Instan (indeks)" for `root`? True only
 *  when FerrumIndexer runs and the root's drive index is ready. Browser dev /
 *  IPC failure = not eligible. */
export async function diskUsageIndexEligibility(root: string): Promise<DiskUsageFastEligibility> {
  if (!isInTauri()) return { eligible: false, reason: 'desktop app only' };
  try {
    return await invoke<DiskUsageFastEligibility>('disk_usage_index_eligibility', { root });
  } catch {
    return { eligible: false };
  }
}

/** FER-464: open the on-disk cached scan of `root` (only for roots outside
 *  the index). null = none, damaged, or not available (browser dev). */
export async function diskUsageCacheLoad(root: string): Promise<DiskUsageCacheLoad | null> {
  if (!isInTauri()) return null;
  try {
    return (await invoke<DiskUsageCacheLoad | null>('disk_usage_cache_load', { root })) ?? null;
  } catch {
    return null;
  }
}

/** FER-464: cached roots, for Settings. */
export async function diskUsageCacheList(): Promise<DiskUsageCacheEntry[]> {
  if (!isInTauri()) return [];
  try {
    return await invoke<DiskUsageCacheEntry[]>('disk_usage_cache_list');
  } catch {
    return [];
  }
}

/** FER-464 "Hapus cache": one root, or every cached root when omitted.
 *  Returns how many cached roots were removed. */
export async function diskUsageCacheDelete(root?: string): Promise<number> {
  if (!isInTauri()) return 0;
  try {
    return await invoke<number>('disk_usage_cache_delete', { root: root ?? null });
  } catch {
    return 0;
  }
}

/** FER-464: a delete/rename/move succeeded in Ferrum; cached roots around
 *  `paths` become certainly stale. Best effort. */
export async function diskUsageCacheNoteMutation(paths: string[]): Promise<void> {
  if (!isInTauri() || paths.length === 0) return;
  try {
    await invoke<number>('disk_usage_cache_note_mutation', { paths });
  } catch {
    /* the in-memory stale marker still applies */
  }
}

/** Flag a running scan cancelled; the final event reports done + cancelled.
 *  Returns false when nothing runs under the id. */
export async function diskUsageCancel(scanId: string): Promise<boolean> {
  if (!isInTauri()) return false;
  try {
    return await invoke<boolean>('disk_usage_cancel', { scanId });
  } catch {
    return false;
  }
}

/** Stored result of a finished scan; null when unknown or evicted (bounded
 *  store — the UI answers with a rescan, gap G8). */
export async function diskUsageSnapshot(scanId: string): Promise<DiskUsageSnapshot | null> {
  if (!isInTauri()) return null;
  try {
    const snap = await invoke<DiskUsageSnapshot | null>('disk_usage_snapshot', { scanId });
    return snap ?? null;
  } catch {
    return null;
  }
}

/** Direct children of one node from the STORED snapshot (largest-first by
 *  default, paginated). Unknown/evicted scanId or an out-of-tree nodeId
 *  rejects with FsError { code: 'notFound' }. */
export async function diskUsageQueryChildren(
  scanId: string,
  nodeId: string,
  offset: number,
  limit: number,
  sort: DiskUsageSort = 'largestFirst',
  filter: DiskUsageFilter = {},
): Promise<DiskUsageChildrenPage> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  try {
    return await invoke<DiskUsageChildrenPage>('disk_usage_query_children', {
      scanId,
      nodeId,
      offset,
      limit,
      sort,
      filter,
    });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Largest FILES of one node's whole subtree, recursive, capped — the label
 *  on the result is verbatim "Top N", never a claim of all matches. */
export async function diskUsageTopFiles(
  scanId: string,
  nodeId: string,
  limit: number,
  filter: DiskUsageFilter = {},
): Promise<DiskUsageTopFiles> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  try {
    return await invoke<DiskUsageTopFiles>('disk_usage_top_files', {
      scanId,
      nodeId,
      limit,
      filter,
    });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function onDiskUsageEvent(
  handler: (e: DiskUsageProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<DiskUsageProgressEvent>('disk_usage://progress', (e) => handler(e.payload));
}
