/**
 * Drive enumeration for the tree sidebar — typed wrapper over the Rust
 * `list_drives` command (docs/ipc.md). Mirrors `fs_service::DriveInfo`.
 */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

export interface DriveInfo {
  /** Volume label, empty when the drive has none. */
  label: string;
  /** Root path with trailing separator, e.g. `C:\`. */
  path: string;
  freeBytes: number;
  totalBytes: number;
  /** `fixed` | `removable` | `network` | `optical` | `ramdisk` | `unknown`. */
  driveType: string;
  /** File system name (NTFS, exFAT…), empty when unknown. */
  fsName: string;
  /** Volume serial `XXXX-XXXX`, empty when unknown. */
  serial: string;
  /** `ok` | `noMedia` (empty slot) | `unavailable` (offline share / timeout). */
  status: 'ok' | 'noMedia' | 'unavailable';
}

function asDriveInfo(raw: unknown): DriveInfo {
  if (typeof raw === 'object' && raw !== null) {
    const r = raw as Record<string, unknown>;
    const label = typeof r.label === 'string' ? r.label : undefined;
    const path = typeof r.path === 'string' ? r.path : undefined;
    const freeBytes =
      typeof r.freeBytes === 'number' && Number.isFinite(r.freeBytes) ? r.freeBytes : undefined;
    const totalBytes =
      typeof r.totalBytes === 'number' && Number.isFinite(r.totalBytes) ? r.totalBytes : undefined;
    if (
      label !== undefined &&
      path !== undefined &&
      freeBytes !== undefined &&
      totalBytes !== undefined
    ) {
      const status =
        r.status === 'noMedia' || r.status === 'unavailable' ? r.status : ('ok' as const);
      return {
        label,
        path,
        freeBytes,
        totalBytes,
        driveType: typeof r.driveType === 'string' ? r.driveType : 'unknown',
        fsName: typeof r.fsName === 'string' ? r.fsName : '',
        serial: typeof r.serial === 'string' ? r.serial : '',
        status,
      };
    }
  }
  throw new FsError({ code: 'io', message: 'Malformed DriveInfo from IPC' });
}

export async function listDrives(): Promise<DriveInfo[]> {
  if (!isInTauri()) {
    throw new FsError({
      code: 'unsupported',
      message: 'Filesystem access requires the desktop app (not browser dev)',
    });
  }
  let raw: unknown;
  try {
    raw = await invoke('list_drives');
  } catch (e) {
    throw FsError.fromIPC(e);
  }
  if (!Array.isArray(raw)) throw FsError.fromIPC(raw);
  return raw.map(asDriveInfo);
}

/**
 * Present drive letters as a bitmask (bit 0 = A:). Instant, no media I/O —
 * cheap enough to poll so the sidebar notices drives added or removed.
 * Resolves 0 outside Tauri or on failure.
 */
export async function driveLettersMask(): Promise<number> {
  if (!isInTauri()) return 0;
  try {
    const raw = await invoke('drive_letters_mask');
    return typeof raw === 'number' ? raw : 0;
  } catch {
    return 0;
  }
}
