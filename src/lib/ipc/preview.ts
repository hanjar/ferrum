/** Preview IPC (F2-17) — typed wrapper over the bounded Rust reader. */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

export type PreviewKind =
  'text' | 'json' | 'markdown' | 'image' | 'video' | 'audio' | 'pdf' | 'binary';

export interface PreviewData {
  kind: PreviewKind;
  /**
   * Base64 of the bytes actually read (head of the file). Empty for images:
   * they render through the `ferrum-media` protocol instead of a data URL.
   * Also empty for video/audio and for files over the text limit.
   */
  data: string;
  truncated: boolean;
  /** True when the file exceeds the text limit: nothing was read (PRV-03). */
  tooLarge: boolean;
  size: number;
  modifiedMs: number;
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

function parsePreview(raw: unknown): PreviewData {
  if (typeof raw !== 'object' || raw === null) throw FsError.fromIPC(raw);
  const r = raw as Record<string, unknown>;
  if (
    typeof r.kind !== 'string' ||
    typeof r.data !== 'string' ||
    typeof r.truncated !== 'boolean' ||
    typeof r.size !== 'number' ||
    typeof r.modifiedMs !== 'number'
  ) {
    throw new FsError({ code: 'io', message: 'Malformed PreviewData from IPC' });
  }
  return {
    kind: r.kind as PreviewKind,
    data: r.data,
    truncated: r.truncated,
    tooLarge: r.tooLarge === true,
    size: r.size,
    modifiedMs: r.modifiedMs,
  };
}

/** Ask the backend for the head of a file (≤ maxBytes, 4 MiB ceiling); files over the limit are not read. */
export async function readPreview(path: string, maxBytes: number): Promise<PreviewData> {
  if (!isInTauri()) throw unsupported();
  try {
    return parsePreview(await invoke<unknown>('read_preview', { path, maxBytes }));
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/**
 * FER-104: bounded in-memory preview of ONE archive member (zip/7z/rar).
 * Nothing is extracted. Unlike disk images, image members come back as base64
 * in `data` (there is no file for the asset protocol to serve). Encrypted or
 * suspicious members reject with a safe, path-free message.
 */
export async function readArchiveMemberPreview(
  archivePath: string,
  memberPath: string,
  maxBytes: number,
  requestId?: string,
): Promise<PreviewData> {
  if (!isInTauri()) throw unsupported();
  try {
    return parsePreview(
      await invoke<unknown>('read_archive_member_preview', {
        archivePath,
        memberPath,
        maxBytes,
        requestId,
      }),
    );
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/**
 * FER-438: ask the backend to stop an archive read/preview tagged with
 * `requestId` (cooperative: it stops at its next 64 KiB chunk). Fire and
 * forget: an unknown or already finished id is harmless, and a failure here
 * must never surface to the user.
 */
export function cancelArchiveRequest(requestId: string): void {
  if (!isInTauri()) return;
  void invoke('archive_cancel_request', { requestId }).catch(() => {});
}
