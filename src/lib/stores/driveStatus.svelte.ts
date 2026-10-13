/**
 * Real drive status for pane status bars (backlog polish): loads the drive
 * list once via list_drives and matches a path to its longest-prefix drive.
 */
import { driveLettersMask, listDrives } from '../ipc/drives';
import type { DriveInfo } from '../ipc/drives';

function normalize(p: string): string {
  return p.replace(/[\\/]+$/, '').toLowerCase();
}

/** Longest-prefix match; `C:\Users\x` matches drive root `C:\`, UNC matches share. */
export function driveForPath(path: string, drives: DriveInfo[]): DriveInfo | null {
  const norm = normalize(path);
  let best: DriveInfo | null = null;
  let bestLen = -1;
  for (const d of drives) {
    const root = normalize(d.path);
    if (root && (norm === root || norm.startsWith(root + '/') || norm.startsWith(root + '\\'))) {
      if (root.length > bestLen) {
        best = d;
        bestLen = root.length;
      }
    }
  }
  return best;
}

class DriveStatusStore {
  drives = $state<DriveInfo[]>([]);
  ready = $state(false);

  private inFlight: Promise<void> | null = null;
  private watchers = 0;
  private stopWatch: (() => void) | null = null;

  /** Reload the drive list; overlapping calls share one in-flight query. */
  refresh(): Promise<void> {
    if (this.inFlight) return this.inFlight;
    this.inFlight = (async () => {
      try {
        this.drives = await listDrives();
      } catch {
        this.drives = []; // browser dev: no drives
      }
      this.ready = true;
    })().finally(() => {
      this.inFlight = null;
    });
    return this.inFlight;
  }

  /**
   * FER-496 refresh policy (reference counted, returns a stop function):
   * on window focus (free space, media inserted) and when the drive-letter
   * bitmask changes. The mask is polled every `pollMs`; it is a single
   * GetLogicalDrives call with no media I/O, so it is cheap even with a
   * dead network share. The full list is only re-queried on a change.
   */
  watch(pollMs = 5000): () => void {
    this.watchers++;
    if (this.watchers === 1) {
      let lastMask: number | null = null;
      const check = async () => {
        const mask = await driveLettersMask();
        if (lastMask !== null && mask !== lastMask) void this.refresh();
        lastMask = mask;
      };
      const onFocus = () => {
        void this.refresh();
        void check();
      };
      void check();
      const timer = setInterval(() => {
        if (typeof document === 'undefined' || document.visibilityState !== 'hidden') void check();
      }, pollMs);
      window.addEventListener('focus', onFocus);
      this.stopWatch = () => {
        clearInterval(timer);
        window.removeEventListener('focus', onFocus);
      };
    }
    let stopped = false;
    return () => {
      if (stopped) return;
      stopped = true;
      this.watchers--;
      if (this.watchers === 0) {
        this.stopWatch?.();
        this.stopWatch = null;
      }
    };
  }

  driveFor(path: string): DriveInfo | null {
    return driveForPath(path, this.drives);
  }
}

export const driveStatus = new DriveStatusStore();
