/** Media/EXIF metadata IPC (PRV-06) — typed wrapper over the bounded Rust reader. */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';

export type MediaKind = 'image' | 'audio' | 'video';

export interface MediaCamera {
  make?: string | null;
  model?: string | null;
}

export interface MediaGps {
  lat: number;
  lon: number;
}

export interface MediaMetadata {
  kind: MediaKind | null;
  width?: number;
  height?: number;
  durationMs?: number;
  camera?: MediaCamera;
  /** Local-time text "YYYY-MM-DD HH:MM:SS" as recorded by the camera. */
  takenAt?: string;
  /** EXIF orientation 1..8. */
  orientation?: number;
  gps?: MediaGps;
  other: [string, string][];
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

const KINDS = ['image', 'audio', 'video'];

/** Read EXIF/media metadata from the head (≤256 KiB) of a file. */
export async function readMediaMetadata(path: string): Promise<MediaMetadata> {
  if (!isInTauri()) throw unsupported();
  try {
    const raw = await invoke<unknown>('read_media_metadata', { path });
    if (typeof raw !== 'object' || raw === null) throw FsError.fromIPC(raw);
    const r = raw as Record<string, unknown>;
    if (!Array.isArray(r.other) || (r.kind != null && !KINDS.includes(r.kind as string))) {
      throw new FsError({ code: 'io', message: 'Malformed MediaMetadata from IPC' });
    }
    const out: MediaMetadata = {
      kind: (r.kind as MediaKind | null | undefined) ?? null,
      other: (r.other as unknown[]).flatMap((p): [string, string][] =>
        Array.isArray(p) && typeof p[0] === 'string' && typeof p[1] === 'string'
          ? [[p[0], p[1]]]
          : [],
      ),
    };
    if (typeof r.width === 'number') out.width = r.width;
    if (typeof r.height === 'number') out.height = r.height;
    if (typeof r.durationMs === 'number') out.durationMs = r.durationMs;
    if (typeof r.takenAt === 'string') out.takenAt = r.takenAt;
    if (typeof r.orientation === 'number') out.orientation = r.orientation;
    if (typeof r.camera === 'object' && r.camera !== null) {
      const c = r.camera as Record<string, unknown>;
      out.camera = {
        make: typeof c.make === 'string' ? c.make : null,
        model: typeof c.model === 'string' ? c.model : null,
      };
    }
    if (typeof r.gps === 'object' && r.gps !== null) {
      const g = r.gps as Record<string, unknown>;
      if (typeof g.lat === 'number' && typeof g.lon === 'number') {
        out.gps = { lat: g.lat, lon: g.lon };
      }
    }
    return out;
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
