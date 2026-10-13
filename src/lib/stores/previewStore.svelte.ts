/**
 * Quick View preview state (F2-17). The ACTIVE pane's cursor drives the
 * target; loads are debounced and superseded by sequence number so rapid
 * selection changes never race. Browser dev (no Tauri) reports the honest
 * unsupported error instead of a fake preview.
 */
import { untrack } from 'svelte';
import {
  readPreview,
  readArchiveMemberPreview,
  cancelArchiveRequest,
  type PreviewData,
  type PreviewKind,
} from '../ipc/preview';
import { splitArchiveLocation } from './archiveFetcher';
import type { PaneEntryInfo } from './panesRegistry.svelte';
import { ui } from '../uiState.svelte';

export type PreviewState = 'idle' | 'loading' | 'ready' | 'error';

/** Debounce window for cursor changes (ms) — keeps rapid arrow-key walks cheap. */
export const PREVIEW_DEBOUNCE_MS = 120;

/** Extensions the backend never reads (media-protocol kinds, PDF): the text limit does not apply. */
const NON_TEXT_EXT =
  /\.(png|jpe?g|gif|webp|bmp|ico|svg|mp4|webm|mov|m4v|ogv|mp3|wav|ogg|flac|m4a|aac|pdf)$/i;

/** Kind guess (extension only) for a metadata-only result; nothing was read. */
function guessKind(name: string): PreviewKind {
  if (/\.json$/i.test(name)) return 'json';
  if (/\.(md|markdown)$/i.test(name)) return 'markdown';
  return 'text';
}

/**
 * FER-104: the archive file + member when `path` points at a MEMBER inside an
 * archive the pane is browsing; null for disk files (and for the archive
 * root / archive file itself). Member separators are normalized to '/'.
 */
export function archiveMemberOf(path: string): { archivePath: string; memberPath: string } | null {
  const loc = splitArchiveLocation(path);
  if (!loc || !loc.memberPath) return null;
  return { archivePath: loc.archivePath, memberPath: loc.memberPath.replaceAll('\\', '/') };
}

/** Decimal limit label: 100000 -> "100 KB", 2000000 -> "2 MB". */
export function formatPreviewLimit(bytes: number): string {
  if (bytes >= 1_000_000) return `${+(bytes / 1_000_000).toFixed(2)} MB`;
  return `${+(bytes / 1000).toFixed(1)} KB`;
}

export class PreviewStore {
  target = $state<PaneEntryInfo | null>(null);
  state = $state<PreviewState>('idle');
  data = $state<PreviewData | null>(null);
  errorText = $state('');

  private seq = 0;
  /** Request id of the archive-member read currently running in the backend (FER-438). */
  private inflightArchiveId: string | null = null;
  private requestCounter = 0;
  /**
   * Random per-instance prefix: the backend registry is process-wide, so ids
   * from two stores (or two windows) must never collide (FER-438).
   */
  private readonly idPrefix = `pv-${
    typeof crypto !== 'undefined' && 'randomUUID' in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2)
  }-`;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private appliedLimit = ui.preview.maxTextBytes;

  constructor() {
    // A changed text limit reloads the current target and drops any
    // in-flight result that was read under the old limit.
    $effect.root(() => {
      $effect(() => {
        const limit = ui.preview.maxTextBytes;
        untrack(() => {
          if (limit === this.appliedLimit) return;
          this.appliedLimit = limit;
          const t = this.target;
          if (!t || t.isDir) return;
          this.invalidate();
          this.scheduleLoad();
        });
      });
    });
  }

  /**
   * Set (or clear) the preview target; the actual load is debounced.
   * FilePane calls this from an $effect: the body reads and then writes
   * `target`/`state`, so it runs untracked or the caller's effect would
   * depend on what it writes and re-run itself forever (FER-454).
   */
  setTarget(t: PaneEntryInfo | null): void {
    untrack(() => this.applyTarget(t));
  }

  private applyTarget(t: PaneEntryInfo | null): void {
    const samePath = this.target?.path === t?.path;
    this.target = t;
    if (t === null) {
      this.cancelPending();
      this.invalidate();
      this.state = 'idle';
      this.data = null;
      this.errorText = '';
      return;
    }
    if (samePath && this.state !== 'error') return;
    if (!samePath) {
      // Drop the previous file's result at once: during the debounce window
      // the panel must not pair old data (player, metadata, too-large info)
      // with the new target. Also invalidates any in-flight load.
      this.invalidate();
      this.data = null;
      this.state = 'loading';
      this.errorText = '';
    }
    this.scheduleLoad();
  }

  /**
   * Supersede any in-flight load: bump the sequence AND tell the backend to
   * stop an archive-member read nobody will look at any more (FER-438).
   */
  private invalidate(): void {
    this.seq++;
    this.abandonArchiveRead();
  }

  private abandonArchiveRead(): void {
    const id = this.inflightArchiveId;
    if (id === null) return;
    this.inflightArchiveId = null;
    cancelArchiveRequest(id);
  }

  private cancelPending(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  private scheduleLoad(): void {
    this.cancelPending();
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.load();
    }, PREVIEW_DEBOUNCE_MS);
  }

  private async load(): Promise<void> {
    const target = this.target;
    if (!target) return;
    this.abandonArchiveRead();
    const mySeq = ++this.seq;
    if (target.isDir) {
      // Only real directories reach here; preview shows metadata only.
      this.state = 'ready';
      this.data = null;
      this.errorText = '';
      return;
    }
    const limit = ui.preview.maxTextBytes;
    this.errorText = '';
    if (target.size > limit && !NON_TEXT_EXT.test(target.name)) {
      // Over the text limit: never read the payload, show metadata only.
      this.data = {
        kind: guessKind(target.name),
        data: '',
        truncated: false,
        tooLarge: true,
        size: target.size,
        modifiedMs: target.modifiedMs,
      };
      this.state = 'ready';
      return;
    }
    this.state = 'loading';
    try {
      const member = archiveMemberOf(target.path);
      let requestId: string | null = null;
      if (member) {
        requestId = `${this.idPrefix}${++this.requestCounter}`;
        this.inflightArchiveId = requestId;
      }
      let data: PreviewData;
      try {
        data = member
          ? await readArchiveMemberPreview(
              member.archivePath,
              member.memberPath,
              limit,
              requestId ?? undefined,
            )
          : await readPreview(target.path, limit);
      } finally {
        if (requestId !== null && this.inflightArchiveId === requestId) {
          this.inflightArchiveId = null; // finished: nothing left to cancel
        }
      }
      if (mySeq !== this.seq) return; // superseded by a newer selection/limit
      this.data = data;
      this.state = 'ready';
    } catch (e) {
      if (mySeq !== this.seq) return;
      this.state = 'error';
      this.data = null;
      this.errorText = e instanceof Error ? e.message : String(e);
    }
  }

  /** Test hook: run the debounced load immediately. */
  async flush(): Promise<void> {
    this.cancelPending();
    await this.load();
  }

  reset(): void {
    this.cancelPending();
    this.invalidate();
    this.target = null;
    this.state = 'idle';
    this.data = null;
    this.errorText = '';
  }
}

export const previewStore = new PreviewStore();

const IMAGE_MIME: Record<string, string> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
  svg: 'image/svg+xml',
};

/**
 * Image src for ARCHIVE members: a `data:` URL (their bytes were read into
 * memory, never extracted). An <img> never runs scripts, so SVG members stay
 * inert. Disk images are not handled here: PreviewImage gets a `ferrum-media`
 * grant (FER-452), so this returns '' for them.
 */
export function previewImageSrc(path: string, data: PreviewData): string {
  if (data.kind !== 'image') return '';
  if (!archiveMemberOf(path)) return '';
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  const mime = IMAGE_MIME[ext];
  return mime && data.data ? `data:${mime};base64,${data.data}` : '';
}

/** Decode preview base64 into text (lossy UTF-8). */
export function previewText(base64: string): string {
  if (!base64) return '';
  const raw = atob(base64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return new TextDecoder('utf-8', { fatal: false }).decode(bytes);
}
