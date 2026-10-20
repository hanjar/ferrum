/**
 * FER-575: Enter / double-click on a FILE inside an archive shown as a virtual folder
 * (`C:\x\a.rar\docs\report.pdf`). Windows cannot open that path, so the backend extracts just that
 * member into Ferrum's temp folder and launches it through the confirmed shell path
 * (`archive_open_member`). This module owns the UI side: split the virtual path, run the password
 * retry loop with the existing archive password prompt, and show brief progress and clear errors.
 */
import { archiveOpenMember, newArchiveRequestId } from '../ipc/archive';
import { isCancelledError, isPasswordError } from '../ipc/archiveErrors';
import { FsError } from '../ipc/types';
import { findArchiveRoot } from './archiveFetcher';

export interface OpenArchiveMemberDeps {
  /** Archives this pane actually opened as virtual folders. A path is routed to the archive
   *  command only when it lies inside one of these: a REAL disk folder that merely happens to be
   *  named `backup.zip` keeps opening through the normal shell path. */
  knownArchives: ReadonlySet<string>;
  /** The password that already unlocked this archive in the pane, if any. */
  passwordFor?: (archivePath: string) => string | undefined;
  /** Masked prompt (FER-504 dialog); resolves null when dismissed. */
  askPassword: (archivePath: string, wrong: boolean) => Promise<string | null>;
  /** Keep a password that just worked, for the next open while the pane stays in the archive. */
  rememberPassword?: (archivePath: string, password: string) => void;
  /** Drop a remembered password that turned out to be wrong. */
  forgetPassword?: (archivePath: string) => void;
  /** Called after `slowMs` while the extraction is still running; the returned function clears it. */
  showWorking?: (memberName: string) => () => void;
  /** Failure other than a dismissed password prompt or a cancel. */
  onError: (message: string) => void;
  /** Delay before the "working" notice appears (small members never flash it). */
  slowMs?: number;
}

/** What `openArchiveMemberAt` did: `notMember` leaves the path for the normal disk open. */
export type OpenArchiveMemberResult = 'notMember' | 'opened' | 'declined' | 'failed';

/** `archive path + member path` when `path` is a file INSIDE a known (opened) archive, else null. */
export function archiveMemberOf(
  path: string,
  knownArchives: ReadonlySet<string>,
): { archivePath: string; memberPath: string } | null {
  const root = findArchiveRoot(path, knownArchives);
  if (!root) return null;
  const memberPath = path
    .slice(root.length + 1)
    .replace(/\\/g, '/')
    .replace(/\/+$/, '');
  if (memberPath === '') return null;
  return { archivePath: root, memberPath };
}

function messageOf(e: unknown): string {
  if (e instanceof FsError) {
    return e.message || 'Tidak bisa membuka file dari arsip.';
  }
  return e instanceof Error ? e.message : String(e);
}

export async function openArchiveMemberAt(
  path: string,
  deps: OpenArchiveMemberDeps,
): Promise<OpenArchiveMemberResult> {
  const target = archiveMemberOf(path, deps.knownArchives);
  if (!target) return 'notMember';
  const { archivePath, memberPath } = target;
  const name = memberPath.slice(memberPath.lastIndexOf('/') + 1);

  let password = deps.passwordFor?.(archivePath);
  let clear: (() => void) | undefined;
  const timer = setTimeout(() => {
    clear = deps.showWorking?.(name);
  }, deps.slowMs ?? 400);
  const stopWorking = () => {
    clearTimeout(timer);
    clear?.();
    clear = undefined;
  };
  try {
    for (;;) {
      try {
        const accepted = await archiveOpenMember(
          archivePath,
          memberPath,
          password,
          newArchiveRequestId(),
        );
        if (password !== undefined) deps.rememberPassword?.(archivePath, password);
        return accepted ? 'opened' : 'declined';
      } catch (e) {
        if (!isPasswordError(e)) throw e;
        stopWorking(); // the prompt must not sit under a "working" notice
        const wrong = e instanceof FsError && e.archiveCode === 'badPassword';
        if (wrong) deps.forgetPassword?.(archivePath);
        const typed = await deps.askPassword(archivePath, wrong);
        if (typed === null) return 'declined'; // dismissed: no error, nothing opened
        password = typed;
      }
    }
  } catch (e) {
    // A native "run this?" answer of No and a user cancel are not failures.
    if (isCancelledError(e)) return 'declined';
    deps.onError(messageOf(e));
    return 'failed';
  } finally {
    stopWorking();
  }
}
