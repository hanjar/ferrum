// FER-576: the real `NavigatePort`: the disk check (`path_resolve`) and the two panes. Everything that touches
// a store or the IPC is injected so the logic is testable without the app.
import type { PaneProbe } from './context';
import { parentFolder, type NavigatePort, type NavigateTarget } from './paneNavigate';

export interface PaneOpener {
  /** Open a folder in the pane. */
  openPath?: (path: string) => void;
  /** Open a file's folder and select the file. */
  revealPath?: (path: string) => void;
}

export interface NavigatePortDeps {
  available(): boolean;
  /** `path_resolve`-like classification of an absolute path (the same gate as typing it in the address bar). */
  resolvePath(
    path: string,
  ): Promise<{ path: string; kind: 'dir' | 'archive' | 'file' | 'missing' }>;
  pane(side: 'left' | 'right'): PaneOpener | undefined;
  probe(side: 'left' | 'right'): PaneProbe | undefined;
  /** How long to wait for a pane to show the folder (default 5 s). */
  timeoutMs?: number;
  pollMs?: number;
  wait?: (ms: number) => Promise<void>;
}

const samePath = (a: string, b: string): boolean => {
  const norm = (p: string) => p.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
  return norm(a) === norm(b);
};

export function createNavigatePort(deps: NavigatePortDeps): NavigatePort {
  const wait =
    deps.wait ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const timeout = deps.timeoutMs ?? 5000;
  const poll = deps.pollMs ?? 40;

  return {
    available: () => deps.available(),

    async resolve(path: string): Promise<NavigateTarget> {
      const resolved = await deps.resolvePath(path);
      // The address-bar resolver also expands %VARIABLES%: a path that comes back different from what was
      // typed is not the path the user wrote, so it is treated as not found.
      if (resolved.kind === 'missing' || !samePath(resolved.path, path)) return { kind: 'missing' };
      if (resolved.kind === 'dir') return { kind: 'dir', path: resolved.path };
      // A file, or an archive file: never entered, only revealed in its folder.
      const folder = parentFolder(resolved.path);
      return folder ? { kind: 'file', path: resolved.path, folder } : { kind: 'missing' };
    },

    async open(side, target) {
      const pane = deps.pane(side);
      if (!pane) return false;
      if (target.kind === 'dir') {
        if (!pane.openPath) return false;
        pane.openPath(target.path);
      } else {
        if (!pane.revealPath) return false;
        pane.revealPath(target.path);
      }
      const folder = target.kind === 'dir' ? target.path : target.folder;
      for (let waited = 0; waited <= timeout; waited += poll) {
        const probe = deps.probe(side);
        if (
          probe &&
          probe.viewKind === 'directory' &&
          samePath(probe.path, folder) &&
          (target.kind === 'dir' || probe.selectedPaths().some((p) => samePath(p, target.path)))
        ) {
          return true;
        }
        await wait(poll);
      }
      return false;
    },

    async settled() {
      // The listing object changes identity on every re-list; stable for a few polls = settled.
      const sides = ['left', 'right'] as const;
      const seen = () => sides.map((side) => deps.probe(side)?.listing);
      let last = seen();
      let stable = 0;
      for (let waited = 0; waited <= timeout && stable < 3; waited += poll) {
        await wait(poll);
        const now = seen();
        stable = now.every((item, i) => item === last[i]) ? stable + 1 : 0;
        last = now;
      }
    },
  };
}
