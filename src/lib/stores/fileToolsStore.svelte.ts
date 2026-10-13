/**
 * File tools dialog state (F2-24): checksum create/verify and split/
 * combine. Verification is read-only and runs immediately for its report;
 * the file-producing operations enqueue onto the operation queue (progress
 * and cancellation live in the existing OpsPanel).
 */
import { activePane } from './panesRegistry.svelte';
import { splitArchiveLocation } from './archiveFetcher';
import {
  cancelChecksumRequest,
  checksumArchiveMember,
  enqueueChecksumCreate,
  enqueueCombineParts,
  enqueueSplitFile,
  onChecksumProgress,
  verifyChecksums,
  type Algorithm,
  type ArchiveMemberChecksum,
  type ChecksumProgress,
  type OutputPolicy,
  type VerifyReport,
} from '../ipc/fileTools';
import { FsError } from '../ipc/types';

export type FileToolsDialog =
  | { kind: 'checksumCreate'; paths: string[] }
  | { kind: 'checksumVerify'; paths: string[] }
  | { kind: 'archiveChecksum'; archivePath: string; memberPath: string }
  | { kind: 'split'; path: string }
  | { kind: 'combine'; firstPart: string };

const MB = 1024 * 1024;

export const RAR_CHECKSUM_UNSUPPORTED =
  'RAR members cannot be hashed in place yet: the RAR reader loads the whole member into memory. Extract it first, then use Create Checksum File(s).';

function isRarPath(path: string): boolean {
  return path.toLowerCase().endsWith('.rar');
}
let verifySeq = 0;

class FileToolsStore {
  current = $state<FileToolsDialog | null>(null);
  busy = $state(false);
  error = $state('');
  algorithm = $state<Algorithm>('sha256');
  /** Split part size in MiB (integer input). */
  partSizeMb = $state(1);
  /** What to do when an output already exists; `refuse` is the safe default. */
  conflictPolicy = $state<OutputPolicy>('refuse');
  verify = $state<VerifyReport | null>(null);
  verifying = $state(false);
  private verifyRequestId = '';
  /** Live progress of the running verify / archive-member hash (null when idle). */
  progress = $state<ChecksumProgress | null>(null);
  /** Result of the last archive-member hash. */
  memberHash = $state<ArchiveMemberChecksum | null>(null);
  hashing = $state(false);
  /** The last archive-member hash was stopped by the user. */
  memberCancelled = $state(false);
  /** Optional archive password (never persisted; cleared on open/close). */
  password = $state('');
  /** Set after an enqueue: the queue panel owns the rest of the story. */
  enqueued = $state(false);

  open(d: FileToolsDialog) {
    this.current = d;
    this.busy = false;
    this.error = '';
    this.verify = null;
    this.progress = null;
    this.memberHash = null;
    this.memberCancelled = false;
    this.password = '';
    this.enqueued = false;
    this.algorithm = 'sha256';
    this.partSizeMb = 1;
    this.conflictPolicy = 'refuse';
    if (d.kind === 'checksumVerify') void this.runVerify(d.paths);
  }

  close() {
    if (this.busy || this.verifying || this.hashing) return;
    this.password = '';
    this.current = null;
  }

  private fail(e: unknown) {
    this.error = e instanceof FsError ? e.message : String(e);
  }

  /** Run `work` with a progress listener bound to `requestId`; always unlistens. */
  private async withProgress<T>(requestId: string, work: () => Promise<T>): Promise<T> {
    this.progress = null;
    const listener: { stop: (() => void) | null; done: boolean } = { stop: null, done: false };
    void onChecksumProgress((p) => {
      if (!listener.done && p.requestId === requestId) this.progress = p;
    }).then((u) => {
      if (listener.done) u();
      else listener.stop = u;
    });
    try {
      return await work();
    } finally {
      listener.done = true;
      listener.stop?.();
      this.progress = null;
    }
  }

  private async runVerify(paths: string[]) {
    this.verifying = true;
    this.error = '';
    this.verifyRequestId = `verify-${Date.now()}-${++verifySeq}`;
    try {
      const id = this.verifyRequestId;
      this.verify = await this.withProgress(id, () => verifyChecksums(paths, id));
    } catch (e) {
      this.fail(e);
    } finally {
      this.verifying = false;
    }
  }

  /**
   * Hash the opened archive member in place. RAR is refused up front (the
   * backend refuses it too): its reader buffers the whole member in memory.
   */
  async runArchiveHash() {
    const d = this.current;
    if (!d || d.kind !== 'archiveChecksum' || this.hashing) return;
    if (isRarPath(d.archivePath)) {
      this.error = RAR_CHECKSUM_UNSUPPORTED;
      return;
    }
    this.hashing = true;
    this.error = '';
    this.memberHash = null;
    this.memberCancelled = false;
    const id = `member-${Date.now()}-${++verifySeq}`;
    this.verifyRequestId = id;
    try {
      this.memberHash = await this.withProgress(id, () =>
        checksumArchiveMember(d.archivePath, d.memberPath, this.algorithm, {
          password: this.password || undefined,
          requestId: id,
        }),
      );
    } catch (e) {
      if (e instanceof FsError && e.code === 'cancelled') this.memberCancelled = true;
      else this.fail(e);
    } finally {
      this.hashing = false;
    }
  }

  /** Stop a running verification / member hash; verify keeps what finished (`cancelled`). */
  async cancelVerify() {
    if (!this.verifying && !this.hashing) return;
    try {
      await cancelChecksumRequest(this.verifyRequestId);
    } catch (e) {
      this.fail(e);
    }
  }

  /** Split output goes next to the source (TC default). */
  private splitOutDir(path: string): string {
    const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
    return i === -1 ? '.' : path.slice(0, i + 1);
  }

  /** Combine output: the reconstructed file next to the parts. */
  private combineOutFile(firstPart: string): string {
    const stem = firstPart.replace(/\.[0-9]{3}$/, '').replace(/\.parts\.json$/, '');
    return stem;
  }

  async submit() {
    const d = this.current;
    if (!d || this.busy) return;
    this.busy = true;
    this.error = '';
    this.enqueued = false;
    try {
      switch (d.kind) {
        case 'checksumCreate':
          await enqueueChecksumCreate(d.paths, this.algorithm, this.conflictPolicy);
          break;
        case 'split': {
          const partBytes = Math.max(1, Math.round(this.partSizeMb)) * MB;
          await enqueueSplitFile(d.path, partBytes, this.splitOutDir(d.path), this.conflictPolicy);
          break;
        }
        case 'combine':
          await enqueueCombineParts(
            d.firstPart,
            this.combineOutFile(d.firstPart),
            this.conflictPolicy,
          );
          break;
        case 'checksumVerify':
          await this.runVerify(d.paths);
          this.busy = false;
          return;
        case 'archiveChecksum':
          this.busy = false;
          await this.runArchiveHash();
          return;
      }
      this.enqueued = true;
      this.busy = false;
    } catch (e) {
      this.fail(e);
      this.busy = false;
    }
  }
}

export const fileTools = new FileToolsStore();

// ── openers bound to the active pane ─────────────────────────────────────

export function openChecksumCreate(): void {
  const pane = activePane();
  const paths = pane?.selectedPaths() ?? [];
  if (paths.length === 0) return;
  fileTools.open({ kind: 'checksumCreate', paths });
}

export function openChecksumVerify(): void {
  const pane = activePane();
  const paths = pane?.selectedPaths() ?? [];
  if (paths.length === 0) return;
  fileTools.open({ kind: 'checksumVerify', paths });
}

/**
 * Member of an archive the pane is browsing: the selected path is the virtual
 * `<archive>\<member>`. Null unless exactly one non-directory member is selected.
 */
export function archiveMemberSelection(): { archivePath: string; memberPath: string } | null {
  const pane = activePane();
  const paths = pane?.selectedPaths() ?? [];
  if (paths.length !== 1) return null;
  const loc = splitArchiveLocation(paths[0]);
  if (!loc || loc.memberPath === '') return null;
  const entry = pane?.visibleEntries?.().find((e) => e.path === paths[0]);
  if (entry?.isDir) return null;
  return { archivePath: loc.archivePath, memberPath: loc.memberPath.replace(/\\/g, '/') };
}

export function openArchiveChecksum(): void {
  const m = archiveMemberSelection();
  if (m) fileTools.open({ kind: 'archiveChecksum', ...m });
}

/** Split works on a single selected file. */
export function openSplitFile(): void {
  const pane = activePane();
  const paths = pane?.selectedPaths() ?? [];
  if (paths.length !== 1) return;
  fileTools.open({ kind: 'split', path: paths[0] });
}

/** Combine works from the first `.001` part (or the manifest); the action
 *  availability guards this shape, so open assumes a valid selection. */
export function openCombineParts(): void {
  const pane = activePane();
  const paths = pane?.selectedPaths() ?? [];
  if (paths.length !== 1) return;
  fileTools.open({ kind: 'combine', firstPart: paths[0] });
}

/** True when the single selection can drive a combine. */
export function isCombineCandidate(first: string): boolean {
  return /\.\d{3}$/.test(first) || first.endsWith('.parts.json');
}
