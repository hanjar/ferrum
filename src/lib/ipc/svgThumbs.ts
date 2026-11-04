/**
 * FER-593: SVG thumbnails. SVG never travels through `ferrum-media` (it can carry script); its
 * bytes come inline from `read_image_preview` and are shown only as `<img src="data:...">`.
 * One queue per grid keeps a folder of thousands of SVGs from firing thousands of IPC reads.
 */
import { imagePreviewSrc, readImagePreview } from './previewImage';

export const SVG_EXT = /\.svg$/i;
/** Reads in flight at once per grid. */
export const SVG_THUMB_CONCURRENCY = 4;

export interface SvgThumbHandle {
  /** Resolves to a data URL, or null (too big, unreadable, not inline SVG, or cancelled). */
  promise: Promise<string | null>;
  /** Drop the request; a read that has not started never starts. */
  cancel(): void;
}

export interface SvgThumbQueue {
  request(path: string): SvgThumbHandle;
}

export function createSvgThumbQueue(
  scope: string,
  limit: number = SVG_THUMB_CONCURRENCY,
): SvgThumbQueue {
  interface Job {
    path: string;
    cancelled: boolean;
    resolve: (url: string | null) => void;
  }
  const pending: Job[] = [];
  let running = 0;

  async function read(path: string): Promise<string | null> {
    try {
      const p = await readImagePreview(path);
      // Only inline SVG bytes become a data URL; anything else falls back to the icon.
      if (!p.inline || p.mime !== 'image/svg+xml') return null;
      return (await imagePreviewSrc(path, p, scope)) || null;
    } catch {
      return null; // no per-file error toast: the cell keeps its icon
    }
  }

  function pump() {
    while (running < limit) {
      const job = pending.shift();
      if (!job) return;
      if (job.cancelled) continue;
      running += 1;
      void read(job.path).then((url) => {
        running -= 1;
        job.resolve(job.cancelled ? null : url);
        pump();
      });
    }
  }

  return {
    request(path) {
      let job!: Job;
      const promise = new Promise<string | null>((resolve) => {
        job = { path, cancelled: false, resolve };
      });
      pending.push(job);
      pump();
      return {
        promise,
        cancel() {
          if (job.cancelled) return;
          job.cancelled = true;
          const i = pending.indexOf(job);
          if (i >= 0) {
            pending.splice(i, 1);
            job.resolve(null);
          }
        },
      };
    },
  };
}
