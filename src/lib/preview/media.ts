/**
 * Media preview helpers (PRV-05). Playback is limited to what the WebView
 * decodes natively (WebView2: H.264/AAC/VP9/AV1 in MP4/WebM, MP3/WAV/OGG/
 * FLAC/AAC); containers such as MKV/AVI usually fail and the player says so.
 */
import { mediaGrantOne } from '../ipc/media';

const VIDEO_EXT = new Set(['mp4', 'webm', 'mov', 'm4v', 'ogv']);
const AUDIO_EXT = new Set(['mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac']);

function extOf(name: string): string {
  const base = name.slice(Math.max(name.lastIndexOf('/'), name.lastIndexOf('\\')) + 1);
  const dot = base.lastIndexOf('.');
  return dot <= 0 ? '' : base.slice(dot + 1).toLowerCase();
}

export function isPlayableVideoExt(name: string): boolean {
  return VIDEO_EXT.has(extOf(name));
}

export function isPlayableAudioExt(name: string): boolean {
  return AUDIO_EXT.has(extOf(name));
}

/**
 * `ferrum-media` src (FER-452): the backend grants a token for `path` under
 * `scope`; the webview streams the file with Range requests (no bytes via
 * IPC). null when the file may not be served.
 */
export function mediaSrc(path: string, scope: string): Promise<string | null> {
  return mediaGrantOne(path, scope);
}
