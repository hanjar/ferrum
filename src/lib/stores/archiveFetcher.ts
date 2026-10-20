/**
 * Archive-aware fetcher: transparently dispatches to listDir (disk) or
 * archiveList (inside archive) based on whether the path ends with a known
 * archive extension. Panes don't need to know about the virtual folder
 * mechanism — they just see FileEntry[] like any other directory.
 */
import { listDir } from '../ipc/tauri';
import { archiveList, type ArchiveListResponse } from '../ipc/archive';
import { FsError } from '../ipc/types';
import { isPasswordError } from '../ipc/archiveErrors';
import type { FileEntry } from '../ipc/types';

const ARCHIVE_EXTS = new Set(['.zip', '.rar', '.7z']);

export function isArchiveFile(path: string): boolean {
  const lower = path.toLowerCase();
  for (const ext of ARCHIVE_EXTS) {
    if (lower.endsWith(ext)) return true;
  }
  return false;
}

/** Returns the archive path if `path` is inside an archive, else null. */
export function findArchiveRoot(path: string, knownArchives: ReadonlySet<string>): string | null {
  for (const root of knownArchives) {
    if (path.startsWith(root + '\\') || path.startsWith(root + '/')) {
      return root;
    }
  }
  return null;
}

/**
 * Splits a path INSIDE an archive virtual location into its on-disk archive
 * file and the member path (empty for the archive root listing), without
 * needing the per-pane knownArchives state — the archive extension followed
 * by a separator marks the virtual boundary. Null when `path` is a plain
 * disk location. FER-369: the watcher uses this to keep an OS watch on the
 * parent archive file while a pane browses inside it.
 */
export function splitArchiveLocation(
  path: string,
): { archivePath: string; memberPath: string } | null {
  const lower = path.toLowerCase();
  let cut = -1;
  let extLen = 0;
  for (const ext of ARCHIVE_EXTS) {
    for (const sep of ['\\', '/']) {
      const i = lower.indexOf(ext + sep);
      if (i >= 0 && (cut === -1 || i < cut)) {
        cut = i;
        extLen = ext.length;
      }
    }
  }
  if (cut >= 0) {
    return {
      archivePath: path.slice(0, cut + extLen),
      memberPath: path.slice(cut + extLen + 1),
    };
  }
  if (isArchiveFile(path)) return { archivePath: path, memberPath: '' };
  return null;
}

function toFileEntries(
  resp: {
    entries: {
      memberPath: string;
      name: string;
      isDir: boolean;
      size: number;
      modifiedMs: number | null;
    }[];
  },
  archiveRoot: string,
): FileEntry[] {
  return resp.entries.map((e) => ({
    name: e.name,
    path: `${archiveRoot}\\${e.memberPath}`,
    isDir: e.isDir,
    isSymlink: false,
    size: e.size,
    modifiedMs: e.modifiedMs ?? 0,
    hidden: false,
  }));
}

/**
 * Creates a fetcher that tracks which archives have been opened and
 * dispatches accordingly. State is shared across panes via `knownArchives`.
 *
 * FER-504: `askPassword` (when given) runs the masked password prompt when a
 * listing answers PasswordRequired/BadPassword — opening a header-encrypted
 * RAR/7z in the pane — and the listing is retried with the typed password.
 * Passwords live ONLY in this fetcher's in-memory map, per pane: a password is
 * remembered only after a listing succeeded with it (a wrong or dismissed one
 * never lingers), reused while the pane stays inside that archive, and the
 * map is dropped as soon as the pane navigates to a disk location or into a
 * different archive. They are never persisted, logged, or sent anywhere
 * except the `archive_list` command.
 */
export function createArchiveAwareFetcher(
  knownArchives: ReadonlySet<string>,
  onNewArchive?: (path: string) => void,
  /** FER-75: opening an archive file failed. The error is still thrown to the caller; this lets
   *  the UI explain it (e.g. "this is part 2 of a multipart archive"). */
  onOpenError?: (path: string, error: unknown) => void,
  /** FER-504: asks for the archive's password; resolves null when dismissed.
   *  `wrong` is true after a BadPassword (the prompt shows the retry note). */
  askPassword?: (archivePath: string, wrong: boolean) => Promise<string | null>,
) {
  // Passwords typed to open a header-encrypted archive, keyed by archive path.
  const passwords = new Map<string, string>();
  // Lowercased archive the pane was last in (null = disk), to detect switches.
  let currentArchive: string | null = null;

  async function listArchive(
    archivePath: string,
    memberPath: string,
  ): Promise<ArchiveListResponse> {
    let password = passwords.get(archivePath);
    for (;;) {
      try {
        const resp = await archiveList(archivePath, memberPath, Date.now(), password);
        // Remember the password only once a listing succeeded with it.
        if (password !== undefined) passwords.set(archivePath, password);
        return resp;
      } catch (e) {
        if (!askPassword || !isPasswordError(e)) throw e;
        // The password just tried (stored or typed) is not valid: forget it.
        passwords.delete(archivePath);
        const wrong = e instanceof FsError && e.archiveCode === 'badPassword';
        const typed = await askPassword(archivePath, wrong);
        if (typed === null) throw e; // dismissed: the pane shows the archive error
        password = typed;
      }
    }
  }

  async function fetch(path: string): Promise<FileEntry[]> {
    // Leaving the archive — for a disk location or a different archive —
    // forgets every remembered password (FER-504).
    const archive = splitArchiveLocation(path)?.archivePath.toLowerCase() ?? null;
    if (archive === null || archive !== currentArchive) passwords.clear();
    currentArchive = archive;

    // Check if we're inside a previously opened archive.
    const root = findArchiveRoot(path, knownArchives);
    if (root) {
      const memberPath = path.slice(root.length + 1).replace(/\\/g, '/');
      const resp = await listArchive(root, memberPath);
      return toFileEntries(resp, root);
    }

    // Check if the path itself is an archive file.
    if (isArchiveFile(path)) {
      let resp;
      try {
        resp = await listArchive(path, '');
      } catch (e) {
        onOpenError?.(path, e);
        throw e;
      }
      onNewArchive?.(path);
      return toFileEntries(resp, path);
    }

    // Regular disk directory.
    return listDir(path);
  }

  return Object.assign(fetch, {
    /** FER-575: the password that unlocked `archivePath` in this pane, if any. */
    passwordFor: (archivePath: string): string | undefined => passwords.get(archivePath),
    /** FER-575: keep a password that just opened a member (cleared on leaving the archive). */
    rememberPassword: (archivePath: string, password: string): void => {
      // Only while the pane is still inside that archive: after it left, the map was cleared and
      // a late success must not bring the password back.
      if (currentArchive === archivePath.toLowerCase()) passwords.set(archivePath, password);
    },
    forgetPassword: (archivePath: string): void => {
      passwords.delete(archivePath);
    },
  });
}
