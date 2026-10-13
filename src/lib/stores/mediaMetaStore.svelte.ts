/**
 * Media/EXIF metadata state (PRV-06 / FER-403). Privacy gate: when
 * `ui.preview.readMetadata` is OFF the command is never invoked and `data`
 * is null; turning it OFF mid-request discards the in-flight result.
 * Every load carries a sequence number so stale results (rapid selection,
 * toggle ON→OFF→ON) never overwrite newer state.
 */
import { readMediaMetadata, type MediaMetadata } from '../ipc/mediaMeta';
import { ui } from '../uiState.svelte';

export type MediaMetaState = 'idle' | 'loading' | 'ready' | 'unavailable' | 'error';

class MediaMetaStore {
  /** Path currently targeted (null = nothing selected). */
  target = $state<string | null>(null);
  state = $state<MediaMetaState>('idle');
  data = $state<MediaMetadata | null>(null);

  private seq = 0;

  /** Point the store at a file (or null to clear). Directories: pass null. */
  setTarget(path: string | null): void {
    this.target = path;
    this.refresh();
  }

  /**
   * (Re)evaluate against the current setting. Call again after the
   * readMetadata toggle changes (the component does this via $effect).
   */
  refresh(): void {
    const mySeq = ++this.seq; // invalidates anything in flight
    const path = this.target;
    if (path === null || !ui.preview.readMetadata) {
      this.state = 'idle';
      this.data = null;
      return;
    }
    this.state = 'loading';
    this.data = null;
    void this.load(path, mySeq);
  }

  private async load(path: string, mySeq: number): Promise<void> {
    try {
      const data = await readMediaMetadata(path);
      if (mySeq !== this.seq || !ui.preview.readMetadata) return;
      this.data = data;
      this.state = 'ready';
    } catch (e) {
      if (mySeq !== this.seq || !ui.preview.readMetadata) return;
      this.data = null;
      // Not media / offline placeholder / dev browser: quiet "no data".
      const code = (e as { code?: string } | null)?.code;
      this.state = code === 'unsupported' ? 'unavailable' : 'error';
    }
  }

  reset(): void {
    this.seq++;
    this.target = null;
    this.state = 'idle';
    this.data = null;
  }
}

export const mediaMetaStore = new MediaMetaStore();

/** "1920 × 1080" or null. */
export function formatDimensions(m: MediaMetadata): string | null {
  return m.width && m.height ? `${m.width} × ${m.height}` : null;
}

/** "m:ss" / "h:mm:ss" from milliseconds. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const mm = Math.floor((total % 3600) / 60);
  const ss = String(total % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(mm).padStart(2, '0')}:${ss}` : `${mm}:${ss}`;
}

/** Plain coordinate text, e.g. "6.50000° S, 106.75000° E". No map/link. */
export function formatGps(lat: number, lon: number): string {
  const f = (v: number, pos: string, neg: string) =>
    `${Math.abs(v).toFixed(5)}° ${v >= 0 ? pos : neg}`;
  return `${f(lat, 'N', 'S')}, ${f(lon, 'E', 'W')}`;
}
