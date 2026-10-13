/** FER-103: downsampled image preview IPC (mirror of `preview_image::ImagePreview`). */
import { invoke } from '@tauri-apps/api/core';
import { mediaGrantOne } from './media';
import { FsError } from './types';
import { isInTauri } from './tauri';

export interface ImagePreview {
  /** True: `data` is a shrunk base64 copy. False: load the original via `ferrum-media`. */
  downsampled: boolean;
  /** True: `data` holds the original bytes (SVG); shown from a `data:` URL. */
  inline?: boolean;
  mime: string;
  data: string;
  width: number;
  height: number;
  originalWidth: number;
  originalHeight: number;
}

function parse(raw: unknown): ImagePreview {
  const r = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  if (
    typeof r.downsampled !== 'boolean' ||
    typeof r.mime !== 'string' ||
    typeof r.data !== 'string' ||
    typeof r.width !== 'number' ||
    typeof r.height !== 'number' ||
    typeof r.originalWidth !== 'number' ||
    typeof r.originalHeight !== 'number'
  ) {
    throw new FsError({ code: 'io', message: 'Malformed ImagePreview from IPC' });
  }
  return r as unknown as ImagePreview;
}

/**
 * Ask the backend whether `path` needs downsampling. Outside Tauri (browser
 * dev) there is no backend: report a passthrough so the placeholder
 * path of the caller applies.
 */
export async function readImagePreview(path: string): Promise<ImagePreview> {
  if (!isInTauri()) {
    return {
      downsampled: false,
      mime: '',
      data: '',
      width: 0,
      height: 0,
      originalWidth: 0,
      originalHeight: 0,
    };
  }
  try {
    return parse(await invoke<unknown>('read_image_preview', { path }));
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/**
 * `src` for an <img>: a data URL (shrunk copy, or inline SVG bytes), else a
 * `ferrum-media` URL of the original granted under `scope` ('' when not servable).
 */
export async function imagePreviewSrc(
  path: string,
  p: ImagePreview,
  scope: string,
): Promise<string> {
  if (p.downsampled || p.inline) return `data:${p.mime};base64,${p.data}`;
  return (await mediaGrantOne(path, scope)) ?? '';
}
