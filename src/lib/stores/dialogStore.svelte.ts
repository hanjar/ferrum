/**
 * Modal dialog state for the mutation flows (task 10). One dialog at a time;
 * task 13 makes the shortcuts that open these respect focus contexts.
 */
import { activePane, inactivePane, refreshPanesMatching } from './panesRegistry.svelte';
import { toast } from './toastStore.svelte';
import { baseName, jobs as jobStore, type Job, type JobKind } from './jobsStore.svelte';
import { planCopy } from '../ipc/mutations';
import {
  archiveExtract,
  archiveExtractPreflight,
  archiveMultipartCheck,
  archiveTest,
  newArchiveRequestId,
  type ArchiveMultipartInfo,
  type ArchiveProgressEvent,
  type ArchivePreflightConflict,
} from '../ipc/archive';
import { isCancelledError, isMultipartError, isPasswordError } from '../ipc/archiveErrors';
import { isArchiveFile } from './archiveFetcher';
import { defaultPackTarget, type PackFormat } from '../packService';
import { FsError } from '../ipc/types';

const ARCHIVE_EXTS = new Set(['.zip', '.rar', '.7z']);

/** Returns the archive root path if `path` lives inside an archive, else null. */
function archiveRootOf(path: string): string | null {
  const lower = path.toLowerCase();
  for (const ext of ARCHIVE_EXTS) {
    // The extension must END a path segment (`a.zip\inner`), not `a.zip_extracted\x`.
    let idx = lower.lastIndexOf(ext);
    while (idx >= 0) {
      const next = path[idx + ext.length];
      if (next === '\\' || next === '/') return path.slice(0, idx + ext.length);
      idx = idx === 0 ? -1 : lower.lastIndexOf(ext, idx - 1);
    }
  }
  return null;
}

function isInArchive(path: string): boolean {
  return archiveRootOf(path) !== null;
}

/** Status of one archive in the Test Archive report (FER-78). `failed` means
 *  at least one member is corrupt; password and unsupported are separate. */
export type ArchiveTestOutcome =
  | 'passed'
  | 'failed'
  | 'passwordRequired'
  | 'unsupported'
  /** FER-79: the user cancelled (this archive was interrupted, or never started). */
  | 'cancelled';

export interface ArchiveTestItem {
  archivePath: string;
  status: ArchiveTestOutcome;
  passed: number;
  total: number;
  /** Every non-passed member, not truncated. */
  failures: { memberPath: string; status: string; message: string }[];
  durationMs?: number;
  note?: string;
  /** FER-73: a password was given and it was wrong (not merely missing). */
  wrongPassword?: boolean;
}

/** Structured codes that mean "needs a password" (FER-73). Never matched on message text. */
const PASSWORD_CODES = new Set(['passwordRequired', 'badPassword']);

/** Maps the per-member results of one archive to a single outcome. Priority: any corrupt
 *  member wins, then password, then unsupported. A member that failed with a password code
 *  (missing or wrong) is a password problem, not corruption. */
export function summarizeArchiveTest(
  archivePath: string,
  entries: { memberPath: string; status: string; error?: { code?: string; message: string } }[],
): ArchiveTestItem {
  const failures = entries
    .filter((en) => en.status !== 'passed')
    .map((en) => ({
      memberPath: en.memberPath,
      status: en.status,
      message: en.error?.message ?? en.status,
    }));
  const needsPassword = (en: { status: string; error?: { code?: string } }) =>
    en.status === 'passwordRequired' || PASSWORD_CODES.has(en.error?.code ?? '');
  const corrupt = entries.some((en) => en.status === 'failed' && !needsPassword(en));
  const password = entries.some(needsPassword);
  const wrongPassword = entries.some((en) => en.error?.code === 'badPassword');
  let status: ArchiveTestOutcome = 'passed';
  if (corrupt) status = 'failed';
  else if (password) status = 'passwordRequired';
  else if (entries.some((en) => en.status === 'unsupported')) status = 'unsupported';
  else if (entries.some((en) => en.status !== 'passed')) status = 'failed';
  return {
    archivePath,
    status,
    passed: entries.length - failures.length,
    total: entries.length,
    failures,
    ...(status === 'passwordRequired' && wrongPassword ? { wrongPassword: true } : {}),
  };
}

export type Dialog =
  | { kind: 'mkdir'; parent: string }
  | { kind: 'rename'; path: string; original: string }
  /** FER-364: `permanent` is the dialog's initial checkbox state (D05) —
   *  carried through from beginDelete so Shift+F8 opens pre-checked while
   *  F8 (trash) opens unchecked. The checkbox stays user-editable. */
  | {
      kind: 'delete';
      paths: string[];
      permanent: boolean;
      /** Called once when the dialog is no longer current (confirmed and done, cancelled, replaced
       *  or reset) - lets a flow that must hold a lock across the delete wait for the end. */
      onSettled?: () => void;
    }
  | { kind: 'error'; title: string; message: string }
  | { kind: 'testReport'; title: string; reports: ArchiveTestItem[] }
  | {
      kind: 'password';
      archivePath: string;
      retry: (password: string) => Promise<void>;
      /** Shown above the field, e.g. "Password salah" after a failed attempt. */
      message?: string;
      /** Called when the user dismisses the dialog without submitting (Esc / Cancel). */
      cancel?: () => void;
    }
  /** FER-79: a running pack / extract / Test Archive job. Batal cancels it by `requestId`;
   *  FER-578: Esc and Background move it to the dock instead. */
  | {
      kind: 'archiveProgress';
      /** FER-578: the job this dialog shows (jobsStore); Background docks it. */
      jobId?: string;
      title: string;
      archivePath: string;
      /** "2/5" when a batch has several archives, else empty. */
      position: string;
      requestId: string;
      processedBytes: number;
      totalBytes?: number;
      processedEntries: number;
      totalEntries?: number;
      currentMember?: string;
      cancelling: boolean;
      writtenEntries?: number;
      skippedEntries?: number;
      renamedEntries?: number;
    }
  /** FER-75: this file is a non-first volume, or the first volume with volumes missing. */
  | { kind: 'multipart'; archivePath: string; info: ArchiveMultipartInfo }
  | {
      kind: 'pack';
      paths: string[];
      target: string;
      format: PackFormat;
      initialError?: string;
    }
  | {
      kind: 'copy' | 'move';
      paths: string[];
      target: string;
      totalFiles: number;
      totalBytes: number;
      conflicts: { source: string; dest: string }[];
      /** FER-572: a move pasted from the Windows clipboard (cut); the clipboard is emptied once it completes. */
      clipboardSequence?: number;
    }
  /** F2-14: Select/Unselect Group with a live match-count preview. */
  | { kind: 'selectGroup'; unselect: boolean; parent: string }
  /** F2-14: persist the current selection to a versioned file / load one. */
  | { kind: 'saveSelection'; parent: string; names: string[] }
  | { kind: 'loadSelection'; parent: string }
  /** F2-15: custom mask filter for the active pane's Show preset. */
  | { kind: 'showFilter'; parent: string }
  /** F2-27: edit one file's comment (per-directory sidecar). */
  | { kind: 'comment'; path: string; name: string; initial: string }
  /** F2-27: quit confirmation while operations are still in flight. */
  | { kind: 'quitConfirm'; operations: number }
  /** FER-368 (D21): the ONE extraction conflict ask per batch. `run` applies
   *  the chosen policy to the whole batch — apply-to-all is inherent to a
   *  single choice. Escape/Cancel just closes: nothing has been written. */
  /** FER-460: "Scan lebih cepat?" before a Disk usage scan of a local NTFS
   *  drive. `choose(null, …)` = dismissed. "Jangan tanya lagi" only turns
   *  the OFFER off; elevation is asked by UAC every time. */
  | {
      kind: 'fastScanOffer';
      root: string;
      volume: string;
      /** FER-472: the same offer before a Sync scan (`root` is then the left folder). */
      purpose?: 'sync';
      /** Right folder of the pair when `purpose` is 'sync'. */
      rightRoot?: string;
      /** FER-476: the elevated MFT scan is available (default true). */
      fastAvailable?: boolean;
      /** FER-476: "Instan (indeks)" (FerrumIndexer, no UAC) is available. */
      indexAvailable?: boolean;
      choose: (mode: 'fast' | 'normal' | 'index' | null, dontAskAgain: boolean) => void;
    }
  | {
      kind: 'extractConflicts';
      target: string;
      conflicts: ArchivePreflightConflict[];
      run: (policy: 'skip' | 'overwrite' | 'rename') => void;
    }
  /** FER-55: summary after extraction completes */
  | {
      kind: 'extractSummary';
      title?: string;
      target?: string;
      written: number;
      skipped: number;
      renamed?: number;
    };

/** One archive in a pending extraction batch (FER-368). */
interface ExtractJob {
  archivePath: string;
  memberPaths: string[];
}

const WRONG_PASSWORD = 'Password salah. Coba lagi.';
const EXTRACT_CANCELLED =
  'Dibatalkan. Berkas yang sudah selesai diekstrak tetap ada; berkas yang sedang ditulis dibuang.';

/** 7z cannot tell a wrong password from damaged encrypted data (the decoder only sees garbage),
 *  so its message says both. ZIP and RAR report a wrong password exactly. */
const WRONG_PASSWORD_7Z = 'Password salah atau data terenkripsi rusak. Coba lagi.';

function wrongPasswordText(archivePath: string): string {
  return /\.7z(ip)?$/i.test(archivePath) ? WRONG_PASSWORD_7Z : WRONG_PASSWORD;
}

function wrongPasswordMessage(e: unknown, archivePath: string): string | undefined {
  return e instanceof FsError && e.archiveCode === 'badPassword'
    ? wrongPasswordText(archivePath)
    : undefined;
}

/** Drop every password held by a batch (FER-73). */
function wipePasswords(passwords: Record<string, string>): void {
  for (const k of Object.keys(passwords)) delete passwords[k];
}

function cancelledItem(archivePath: string, note: string): ArchiveTestItem {
  return { archivePath, status: 'cancelled', passed: 0, total: 0, failures: [], note };
}

function freshRequestId(): string {
  try {
    if (typeof newArchiveRequestId === 'function') {
      return newArchiveRequestId();
    }
  } catch {
    // Vitest mock proxy without newArchiveRequestId
  }
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return `arc-${c.randomUUID()}`;
  return `arc-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * P3-2: Display sanitizer for member names in error messages and test reports.
 * Replaces control characters (\n, \r, \t, \0, C0/C1), bidi override/isolate/mark
 * characters (U+202A–U+202E, U+2066–U+2069, U+200E, U+200F, U+061C), and Unicode line/paragraph
 * separators (U+2028, U+2029) with visible escapes (e.g. \u{202E}, \n, \u{2028}) so raw
 * archive member paths cannot spoof UI text or break layouts.
 */
export function sanitizeMemberPath(member: string): string {
  return member.replace(
    /[\p{Cc}\u061c\u200e\u200f\u2028\u2029\u202a-\u202e\u2066-\u2069]/gu,
    (ch) => {
      switch (ch) {
        case '\n':
          return '\\n';
        case '\r':
          return '\\r';
        case '\t':
          return '\\t';
        case '\0':
          return '\\0';
        default: {
          const hex = (ch.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
          return `\\u{${hex}}`;
        }
      }
    },
  );
}

/** Run a delete dialog's `onSettled` hook (at most once; a throwing hook never breaks the store). */
function settleDelete(d: Dialog | null): void {
  if (d?.kind !== 'delete' || !d.onSettled) return;
  const hook = d.onSettled;
  d.onSettled = undefined;
  try {
    hook();
  } catch {
    // A hook is a notification; it must not break dialog handling.
  }
}

class DialogStore {
  private _current = $state<Dialog | null>(null);
  queue = $state<Dialog[]>([]);
  /** The delete dialog whose confirmed delete is running right now (see `beginDeleteRun`). */
  private runningDelete: Dialog | null = null;

  /** Mark `d` (the current delete dialog) as running: `open()` queues other dialogs instead of
   *  replacing it, so its `onSettled` (a lease release) cannot fire before the delete ends. */
  beginDeleteRun(d: Dialog): void {
    this.runningDelete = d;
  }

  endDeleteRun(): void {
    this.runningDelete = null;
  }

  get current(): Dialog | null {
    return this._current;
  }

  set current(val: Dialog | null) {
    const prev = this._current;
    this._current = val;
    if (prev !== val) settleDelete(prev);
    if (val === null) {
      if (prev?.kind === 'password') prev.cancel?.();
      if (prev?.kind === 'fastScanOffer') prev.choose(null, false);
      for (const d of this.queue) {
        if (d.kind === 'password') d.cancel?.();
        if (d.kind === 'fastScanOffer') d.choose(null, false);
      }
      this.queue = [];
    }
  }

  closeAll(): void {
    this.current = null;
  }

  reset(): void {
    this.current = null;
  }

  open(d: Dialog) {
    if (this._current === null) {
      this._current = d;
      return;
    }
    const isPrompt = (item: Dialog | null): boolean =>
      item?.kind === 'password' || item?.kind === 'fastScanOffer';

    if (isPrompt(d)) {
      this.queue.push(d);
      return;
    }

    // FER-549: a delete that holds a lease (onSettled) is running; replacing its dialog would
    // settle it - and release the lease - mid-delete. Queue the newcomer; it appears when the
    // delete dialog closes.
    if (this._current === this.runningDelete && this.runningDelete?.kind === 'delete') {
      this.queue.push(d);
      return;
    }

    if (isPrompt(this._current)) {
      this.queue.unshift(this._current);
    } else {
      settleDelete(this._current);
    }

    this._current = d;
  }

  close() {
    if (this._current === null) return;
    const d = this._current;
    // FER-578: closing a running job's dialog (Esc) moves the job to the background; it keeps
    // running (Batal is the explicit way to stop it).
    if (d.kind === 'archiveProgress') {
      this.sendToBackground();
      return;
    }
    // FER-460: a dismissed scan offer must still settle its caller (no scan).
    if (d.kind === 'fastScanOffer') d.choose(null, false);
    // FER-73: a dismissed password prompt settles its caller and lets it drop the password.
    if (d.kind === 'password') d.cancel?.();

    settleDelete(d);
    this._current = this.queue.shift() ?? null;
  }

  /** Close without any cancel/settle hooks: a flow moving on to its next step. */
  private dismiss() {
    settleDelete(this._current);
    this._current = this.queue.shift() ?? null;
  }

  // ── FER-79: cancellable job with progress ─────────────────────────────

  /** Runs `run(requestId)` as a step of a job. A foreground job shows an `archiveProgress` dialog
   *  with the step's progress events; a docked one (FER-578) runs silently in the dock. The id is
   *  registered in the backend cancel registry, so Batal stops the job. The dialog is removed when
   *  the step settles, whatever the outcome.
   *
   *  `flow` is the job a multi-step flow (extract batch, test run, pack) owns: it survives the
   *  step, keeps its docked flag for the next one and is finished by the flow. Without `flow` the
   *  call is a one-step job that is finished here. */
  async withProgress<T>(
    label: { title: string; archivePath: string; position?: string; kind?: JobKind },
    run: (requestId: string) => Promise<T>,
    flow?: Job,
  ): Promise<T> {
    const requestId = freshRequestId();
    const job =
      flow ??
      jobStore.begin({
        kind: label.kind ?? 'extract',
        title: label.title,
        subject: label.archivePath,
        position: label.position,
      });
    jobStore.attempt(
      job,
      requestId,
      { subject: label.archivePath, position: label.position ?? '' },
      (ev) => this.applyProgress(ev),
    );
    if (!job.docked) {
      this.open({
        kind: 'archiveProgress',
        jobId: job.id,
        title: label.title,
        archivePath: label.archivePath,
        position: label.position ?? '',
        requestId,
        processedBytes: 0,
        processedEntries: 0,
        cancelling: false,
      });
    }
    // The one global listener must be up before the backend starts emitting.
    await jobStore.ensureListening();
    try {
      const result = await run(requestId);
      if (!flow) jobStore.finish(job, 'done', { silent: true });
      return result;
    } catch (e) {
      if (!flow)
        jobStore.finish(job, isCancelledError(e) ? 'canceled' : 'failed', { silent: true });
      throw e;
    } finally {
      jobStore.endAttempt(job);
      const d = this._current;
      if (d?.kind === 'archiveProgress' && d.requestId === requestId) {
        this._current = this.queue.shift() ?? null;
      }
    }
  }

  /** Mirrors a progress event into the open progress dialog (the job already has the numbers). */
  private applyProgress(ev: ArchiveProgressEvent) {
    const d = this.current;
    if (d?.kind !== 'archiveProgress' || d.requestId !== ev.requestId) return;
    d.processedBytes = ev.processedBytes;
    d.totalBytes = ev.totalBytes;
    d.processedEntries = ev.processedEntries;
    d.totalEntries = ev.totalEntries;
    d.currentMember = ev.currentMember;
    if (ev.writtenEntries !== undefined) d.writtenEntries = ev.writtenEntries;
    if (ev.skippedEntries !== undefined) d.skippedEntries = ev.skippedEntries;
    if (ev.renamedEntries !== undefined) d.renamedEntries = ev.renamedEntries;
  }

  /** FER-578: "Background" button / Esc on the progress dialog. The job keeps running in the dock
   *  (Alt+B); the dialog closes so the rest of the app is usable and the focus goes back. */
  sendToBackground(): void {
    const d = this._current;
    if (d?.kind !== 'archiveProgress') return;
    const job = d.jobId ? jobStore.get(d.jobId) : undefined;
    if (job) jobStore.dock(job);
    this._current = this.queue.shift() ?? null;
    toast.show(`${d.title} berjalan di latar belakang (Alt+B)`);
  }

  /** FER-55: explicitly show extract summary dialog */
  showExtractSummary(summary: {
    written: number;
    skipped?: number;
    renamed?: number;
    target?: string;
    title?: string;
  }) {
    this.open({
      kind: 'extractSummary',
      title: summary.title ?? 'Ekstrak selesai',
      target: summary.target,
      written: summary.written,
      skipped: summary.skipped ?? 0,
      renamed: summary.renamed,
    });
  }

  /** Batal on the progress dialog. Idempotent. */
  cancelArchiveJob() {
    const d = this.current;
    if (d?.kind !== 'archiveProgress' || d.cancelling) return;
    d.cancelling = true;
    const job = d.jobId ? jobStore.get(d.jobId) : undefined;
    if (job) jobStore.cancel(job);
    else jobStore.cancelRequest(d.requestId);
  }

  // ── FER-73: password prompt as a promise ──────────────────────────────

  /** Opens the masked password dialog; resolves with the typed password, or null when the user
   *  dismisses it. The password is handed to the caller once and not kept here. */
  private askPassword(archivePath: string, message?: string, job?: Job): Promise<string | null> {
    return new Promise((resolve) => {
      this.openOrHold(
        job,
        {
          kind: 'password',
          archivePath,
          message,
          retry: async (pw) => {
            this.dismiss();
            resolve(pw);
          },
          cancel: () => resolve(null),
        },
        'Password',
      );
    });
  }

  /** FER-578: opens `d` now, unless it belongs to a docked job: that one is held (job turns
   *  "needs input", a toast says so) and opens only when the user picks it in the dock. */
  private openOrHold(job: Job | undefined, d: Dialog, label: string): void {
    if (job?.docked) {
      const subject = 'archivePath' in d ? baseName(d.archivePath) : job.subject;
      jobStore.holdPrompt(
        job,
        { label, open: () => this.open(d) },
        `${label} diperlukan: ${subject}`,
      );
      return;
    }
    this.open(d);
  }

  /** FER-578: ends a flow. A docked job keeps `show` as its "Lihat hasil" and opens nothing; a
   *  foreground job is removed and `show` runs now (the dialogs tell the story, as before). */
  private settleJob(
    job: Job,
    outcome: 'done' | 'failed' | 'canceled',
    summary: string,
    show: () => void,
  ): void {
    if (job.docked) {
      jobStore.finish(job, outcome, { summary, result: { label: 'Lihat hasil', open: show } });
      return;
    }
    jobStore.finish(job, outcome, { summary, silent: true });
    show();
  }

  /** FER-504: the same masked prompt for LISTING — opening a header-encrypted
   *  archive in a pane. `wrong` adds the retry note after a BadPassword. The
   *  password resolves once; this store never keeps it. */
  askArchivePassword(archivePath: string, wrong: boolean): Promise<string | null> {
    return this.askPassword(archivePath, wrong ? wrongPasswordText(archivePath) : undefined);
  }

  // ── FER-75: multipart messages ────────────────────────────────────────

  /** Explains why a volume cannot be opened, extracted or tested alone. Falls back to the
   *  backend's own message when the folder check is unavailable. */
  async showMultipart(archivePath: string, fallbackMessage: string, title = 'Arsip multipart') {
    let info: ArchiveMultipartInfo | null = null;
    try {
      info = await archiveMultipartCheck(archivePath);
    } catch {
      info = null;
    }
    if (info && (info.kind === 'notFirst' || info.kind === 'missing')) {
      this.open({ kind: 'multipart', archivePath, info });
    } else {
      this.open({ kind: 'error', title, message: fallbackMessage });
    }
  }

  /** "Buka bagian pertama": leaves the dialog and opens the first volume in the active pane. */
  openFirstPart() {
    const d = this.current;
    if (d?.kind !== 'multipart' || d.info.kind !== 'notFirst' || !d.info.firstExists) return;
    const first = d.info.firstPath;
    this.dismiss();
    activePane()?.openPath?.(first);
  }

  // ── openers bound to the active pane ──────────────────────────────────

  beginMkdir() {
    const pane = activePane();
    if (pane) this.open({ kind: 'mkdir', parent: pane.path });
  }

  beginRename() {
    const pane = activePane();
    if (!pane) return;
    const target = pane.cursorEntry() ?? null;
    if (!target) {
      this.open({ kind: 'error', title: 'Rename', message: 'Select exactly one entry first.' });
      return;
    }
    this.open({ kind: 'rename', path: target.path, original: target.name });
  }

  /** F6 flow: same as copy but the engine removes verified sources. */
  async beginMoveFlow() {
    await this.beginTransferFlow('move');
  }

  /** F5 flow: plan the copy of the ACTIVE pane's selection to the OTHER
   *  pane, then confirm (D21: one ask with apply-to-all policy). */
  async beginCopyFlow() {
    await this.beginTransferFlow('copy');
  }

  /** Drop flow (cross-pane drag): explicit paths and explicit target dir —
   *  no active/inactive pane lookups. planCopy still runs to surface
   *  conflicts (D21) before the confirm dialog opens. */
  async beginTransferTo(
    kind: 'copy' | 'move',
    paths: string[],
    target: string,
    opts?: { clipboardSequence?: number; fromDisk?: boolean },
  ) {
    await this.beginTransferFlow(kind, target, paths, opts?.clipboardSequence, opts?.fromDisk);
  }

  private async beginTransferFlow(
    kind: 'copy' | 'move',
    explicitTarget?: string,
    explicitPaths?: string[],
    clipboardSequence?: number,
    sourcesOnDisk = false,
  ) {
    const src = activePane();
    const dst = inactivePane();
    const paths = explicitPaths ?? src?.selectedPaths() ?? [];
    const target = explicitTarget ?? dst?.path;
    if ((!explicitTarget && (!src || !dst)) || !target || paths.length === 0) {
      this.open({ kind: 'error', title: 'Copy', message: 'Nothing selected.' });
      return;
    }

    // Source inside archive → extract members to the target directory.
    // FER-572: sources pasted from the clipboard are proven to exist on disk, so a folder
    // that merely has ".zip" in its name is not an archive.
    const srcArchiveRoot = paths.length > 0 && !sourcesOnDisk ? archiveRootOf(paths[0]) : null;
    if (srcArchiveRoot) {
      if (kind === 'move') {
        this.open({
          kind: 'error',
          title: 'Move',
          message: 'Moving out of an archive is not supported. Use Extract instead.',
        });
        return;
      }
      // Member paths are contract-level slash-relative (MemberPath): the
      // virtual folder joins entry paths with the OS separator, so convert
      // back to '/' here — the Rust contract rejects backslash members and
      // the engine normalizes to '/' internally (F2-03).
      const memberPaths = paths.map((p) => {
        const rest = p.slice(srcArchiveRoot.length);
        return (rest.startsWith('\\') || rest.startsWith('/') ? rest.slice(1) : rest).replace(
          /\\/g,
          '/',
        );
      });
      // FER-368: same one-ask conflict flow as Alt+F9 unpack.
      this.runExtractQueue([{ archivePath: srcArchiveRoot, memberPaths }], target, 'Extract', () =>
        refreshPanesMatching(target),
      );
      return;
    }

    // Target inside archive → read-only.
    if (isInArchive(target)) {
      this.open({
        kind: 'error',
        title: kind === 'copy' ? 'Copy' : 'Move',
        message: 'Cannot copy into an archive. Open the archive and extract instead.',
      });
      return;
    }

    try {
      const plan = await planCopy(paths, target);
      this.open({
        kind,
        paths,
        target,
        totalFiles: plan.totalFiles,
        totalBytes: plan.totalBytes,
        conflicts: plan.conflicts,
        ...(clipboardSequence !== undefined ? { clipboardSequence } : {}),
      });
    } catch (e) {
      this.open({
        kind: 'error',
        title: kind === 'copy' ? 'Copy' : 'Move',
        message: e instanceof FsError ? e.message : String(e),
      });
    }
  }

  /** FER-364: the `permanent` flag is carried into the delete dialog as its
   *  initial state instead of being dropped (`file.delete` trash default,
   *  `file.delete.permanent` pre-checked — D05: explicit confirm either way).
   *  DSK-06: Disk usage snapshot rows pass `explicitPaths` and ride the SAME
   *  confirm dialog — there is still exactly ONE delete path in the app. */
  beginDelete(permanent = false, explicitPaths?: string[]) {
    const pane = activePane();
    if (explicitPaths !== undefined) {
      // DSK-06: snapshot targets skip the pane selection entirely.
      if (explicitPaths.length === 0) return;
      this.open({ kind: 'delete', paths: [...explicitPaths], permanent });
      return;
    }
    if (!pane) return;
    const paths = pane.selectedPaths();
    if (paths.length === 0) {
      this.open({ kind: 'error', title: 'Delete', message: 'Nothing selected.' });
      return;
    }
    this.open({ kind: 'delete', paths, permanent });
  }

  /** Pack selected items into an archive (ZIP/7z/RAR) in the target pane. */
  beginPack(format: PackFormat = 'zip') {
    const pane = activePane();
    if (!pane) return;
    const paths = pane.selectedPaths();
    if (paths.length === 0) {
      this.open({ kind: 'error', title: 'Pack', message: 'Nothing selected.' });
      return;
    }
    this.open({ kind: 'pack', paths, target: defaultPackTarget(pane.path, format), format });
  }

  /** Unpack selected archive files (Alt+F9): extract full contents of each
   *  archive into the OTHER pane. TC-parity with F5's member extract; the
   *  FER-368 one-ask conflict dialog covers the WHOLE batch, and the
   *  password dialog re-drives the SAME archive until it succeeds or the
   *  user cancels. Later archives only start after the earlier one
   *  finished (success or terminal error). */
  beginUnpack() {
    const src = activePane();
    const dst = inactivePane();
    if (!src || !dst) return;
    const archives = src.selectedPaths().filter((p) => isArchiveFile(p));
    if (archives.length === 0) {
      this.open({
        kind: 'error',
        title: 'Unpack',
        message: 'Select an archive file (.zip, .7z or .rar) first.',
      });
      return;
    }
    this.runExtractQueue(
      archives.map((archivePath) => ({ archivePath, memberPaths: [] })),
      dst.path,
      'Unpack',
      () => dst.refresh(),
    );
  }

  // ── FER-368: extraction with the D21 one-ask conflict dialog ──────────
  // 1. Preflight every archive in the batch (read-only). Conflicts at the
  //    target are collected across the whole batch; password-required
  //    archives open the password dialog first, so the conflict ask always
  //    happens AFTER a successful unlock.
  // 2. No conflicts → extract immediately with the safe 'skip' policy
  //    (indistinguishable from the pre-FER-368 behavior).
  // 3. Conflicts → ONE extractConflicts dialog; a single Skip / Overwrite /
  //    Rename-auto choice applies to the whole batch; Escape/Cancel aborts
  //    cleanly because nothing has been written yet.
  private runExtractQueue(
    jobs: ExtractJob[],
    target: string,
    errorTitle: string,
    onDone?: () => void,
  ): void {
    const passwords: Record<string, string> = {};
    const unlocked: ExtractJob[] = [];
    // FER-504: a password typed during preflight now unlists header-encrypted
    // archives too, so `locked` below only catches a preflight that STILL
    // fails with a password — the direct extract (with that password) and its
    // own retry loop keep the batch moving.
    const locked: ExtractJob[] = [];
    const conflicts: ArchivePreflightConflict[] = [];

    const preflightOne = (job: ExtractJob): Promise<void> =>
      archiveExtractPreflight(
        job.archivePath,
        job.memberPaths,
        target,
        passwords[job.archivePath],
      ).then((report) => {
        conflicts.push(...report.conflicts);
        unlocked.push(job);
      });

    const preflightFrom = (index: number): void => {
      if (index >= jobs.length) {
        if (conflicts.length === 0) {
          this.extractQueue(
            [...unlocked, ...locked],
            target,
            'skip',
            passwords,
            errorTitle,
            onDone,
          );
          return;
        }
        const batch = [...unlocked, ...locked];
        this.open({
          kind: 'extractConflicts',
          target,
          conflicts,
          run: (policy) => {
            this.close();
            this.extractQueue(batch, target, policy, passwords, errorTitle, onDone);
          },
        });
        return;
      }
      const job = jobs[index]!;
      preflightOne(job)
        .then(() => preflightFrom(index + 1))
        .catch((e: unknown) => {
          const msg = e instanceof FsError ? e.message : String(e);
          // FER-73: the structured code decides, never the message text.
          if (!isPasswordError(e)) {
            wipePasswords(passwords);
            if (isMultipartError(e)) {
              void this.showMultipart(job.archivePath, msg, errorTitle);
              return;
            }
            const member = e instanceof FsError ? e.memberPath : undefined;
            const displayMsg = member ? `Gagal di: ${sanitizeMemberPath(member)}\n${msg}` : msg;
            this.open({ kind: 'error', title: errorTitle, message: displayMsg });
            return;
          }
          this.open({
            kind: 'password',
            archivePath: job.archivePath,
            message: wrongPasswordMessage(e, job.archivePath),
            cancel: () => wipePasswords(passwords),
            retry: async (pw) => {
              this.dismiss();
              passwords[job.archivePath] = pw;
              await preflightOne(job)
                .then(() => preflightFrom(index + 1))
                .catch(() => {
                  // Still unlistable with a password (FER-504: rare — e.g. a
                  // wrong password the next extract attempt will re-ask).
                  // Direct extract (with the typed password) keeps the flow
                  // working; its own password-retry still applies.
                  locked.push(job);
                  preflightFrom(index + 1);
                });
            },
          });
        });
    };
    preflightFrom(0);
  }

  /** Serially extract a batch under ONE policy, preserving the legacy
   *  per-archive password-retry loop. Each archive runs as a cancellable job with a progress
   *  dialog (FER-79); passwords live only in `passwords` and are wiped when the batch settles
   *  (done, error, cancel or a dismissed prompt) (FER-73). */
  private extractQueue(
    jobs: ExtractJob[],
    target: string,
    policy: 'skip' | 'overwrite' | 'rename' | 'overwriteOlder',
    passwords: Record<string, string>,
    errorTitle: string,
    onDone?: () => void,
  ): void {
    let batchWritten = 0;
    let batchSkipped = 0;
    let batchRenamed = 0;
    // FER-578: the whole batch is ONE job (docked once, stays docked for the next archive).
    const flow = jobStore.begin({
      kind: 'extract',
      title: errorTitle,
      subject: jobs[0]?.archivePath ?? '',
    });
    /** Ends the batch at its first failure: the error dialog now, or held in a docked job. */
    const failBatch = (outcome: 'failed' | 'canceled', summary: string, show: () => void) => {
      wipePasswords(passwords);
      refreshPanesMatching(target);
      onDone?.();
      this.settleJob(flow, outcome, summary, show);
    };
    const runFrom = (index: number): void => {
      if (index >= jobs.length) {
        wipePasswords(passwords);
        refreshPanesMatching(target);
        onDone?.();
        const parts = [`${batchWritten} ditulis`];
        if (batchSkipped > 0) parts.push(`${batchSkipped} dilewati`);
        if (batchRenamed > 0) parts.push(`${batchRenamed} dinamai ulang`);
        const text = `Ekstrak selesai: ${parts.join(', ')}`;
        const any = batchWritten > 0 || batchSkipped > 0 || batchRenamed > 0;
        if (flow.docked) {
          jobStore.finish(flow, 'done', {
            summary: any ? text : `${errorTitle} selesai`,
            result: any
              ? {
                  label: 'Lihat hasil',
                  open: () =>
                    this.showExtractSummary({
                      written: batchWritten,
                      skipped: batchSkipped,
                      renamed: batchRenamed > 0 ? batchRenamed : undefined,
                      target,
                    }),
                }
              : undefined,
          });
        } else {
          jobStore.finish(flow, 'done', { silent: true });
          if (any) toast.show(text);
        }
        return;
      }
      const job = jobs[index]!;
      const attempt = async (password?: string): Promise<void> => {
        try {
          await this.withProgress(
            {
              title: errorTitle,
              archivePath: job.archivePath,
              position: jobs.length > 1 ? `${index + 1}/${jobs.length}` : '',
            },
            async (requestId) => {
              const res = await archiveExtract(
                job.archivePath,
                job.memberPaths,
                target,
                policy,
                password,
                requestId,
              );
              if (flow.requestId === requestId) {
                if (flow.writtenEntries !== undefined) batchWritten += flow.writtenEntries;
                if (flow.skippedEntries !== undefined) batchSkipped += flow.skippedEntries;
                if (flow.renamedEntries !== undefined) batchRenamed += flow.renamedEntries;
              }
              return res;
            },
            flow,
          );
        } catch (e: unknown) {
          if (isCancelledError(e)) {
            // Members that finished stay (they are whole); the one in flight was discarded.
            failBatch('canceled', 'Dibatalkan', () =>
              this.open({ kind: 'error', title: errorTitle, message: EXTRACT_CANCELLED }),
            );
            return;
          }
          if (isPasswordError(e)) {
            this.openOrHold(
              flow,
              {
                kind: 'password',
                archivePath: job.archivePath,
                message: wrongPasswordMessage(e, job.archivePath),
                cancel: () => {
                  wipePasswords(passwords);
                  jobStore.finish(flow, 'canceled', {
                    summary: 'Dibatalkan: password tidak diberikan',
                    silent: true,
                  });
                },
                retry: async (pw) => {
                  this.dismiss();
                  void attempt(pw);
                },
              },
              'Password',
            );
            return;
          }
          const member = e instanceof FsError ? e.memberPath : undefined;
          const msg = e instanceof FsError ? e.message : String(e);
          if (isMultipartError(e)) {
            failBatch(
              'failed',
              `${errorTitle} gagal: ${baseName(job.archivePath)}`,
              () => void this.showMultipart(job.archivePath, msg, errorTitle),
            );
            return;
          }
          const displayMsg = member
            ? `Gagal di: ${sanitizeMemberPath(member)}
${msg}`
            : msg;
          failBatch('failed', `${errorTitle} gagal: ${baseName(job.archivePath)}`, () =>
            this.open({ kind: 'error', title: errorTitle, message: displayMsg }),
          );
          return;
        }
        runFrom(index + 1);
      };
      void attempt(passwords[job.archivePath]);
    };
    runFrom(0);
  }

  /** D21 decision for the extractConflicts dialog: ONE choice for the whole
   *  batch. Wired to the dialog's Extract button and Enter; Escape/Cancel
   *  close without ever calling this, so no extract runs. */
  confirmExtractConflicts(policy: 'skip' | 'overwrite' | 'rename'): void {
    const d = this.current;
    if (d?.kind !== 'extractConflicts') return;
    d.run(policy);
  }

  /** Test selected archives (Alt+Shift+F9): one report per archive, run one
   *  after another (no queue), shown in one dialog with a scrollable list. */
  beginTestArchive() {
    const pane = activePane();
    if (!pane) return;
    const archives = pane.selectedPaths().filter((p) => isArchiveFile(p));
    if (archives.length === 0) {
      this.open({
        kind: 'error',
        title: 'Test archive',
        message: 'Select an archive file (.zip, .7z or .rar) first.',
      });
      return;
    }
    void this.runTestArchive(archives);
  }

  /** One archive after another, each a cancellable job with progress (FER-79). Cancelling
   *  stops the whole run: the interrupted archive and the ones not yet started are marked
   *  "dibatalkan" in the report. */
  private async runTestArchive(archives: string[]): Promise<void> {
    const reports: ArchiveTestItem[] = [];
    // FER-578: the whole run is ONE job (docked once, stays docked for the next archive).
    const flow = jobStore.begin({
      kind: 'test',
      title: 'Test archive',
      subject: archives[0] ?? '',
    });
    for (let i = 0; i < archives.length; i++) {
      const item = await this.testOneArchive(
        archives[i]!,
        archives.length > 1 ? `${i + 1}/${archives.length}` : '',
        flow,
      );
      reports.push(item);
      if (item.status === 'cancelled') {
        for (const rest of archives.slice(i + 1)) {
          reports.push(cancelledItem(rest, 'Belum diuji karena dibatalkan.'));
        }
        break;
      }
    }
    const cancelled = reports.some((r) => r.status === 'cancelled');
    const problems = reports.filter((r) => r.status !== 'passed').length;
    const title = cancelled
      ? 'Test archive dibatalkan'
      : problems === 0
        ? 'Archive test passed'
        : `Archive test: ${problems} problem(s)`;
    this.settleJob(flow, cancelled ? 'canceled' : problems === 0 ? 'done' : 'failed', title, () =>
      this.open({ kind: 'testReport', title, reports }),
    );
  }

  /** Tests one archive. An encrypted one asks for the password in the masked dialog and retries
   *  until it passes, the user dismisses the prompt, or the test cancels (FER-73). The password
   *  exists only inside this call: it is not stored in state, history or the report. */
  private async testOneArchive(
    archivePath: string,
    position: string,
    flow: Job,
  ): Promise<ArchiveTestItem> {
    const started = performance.now();
    let password: string | undefined;
    let item: ArchiveTestItem;
    try {
      for (;;) {
        let wrong = false;
        try {
          const report = await this.withProgress(
            { title: 'Test archive', archivePath, position },
            (requestId) => archiveTest(archivePath, password, requestId),
            flow,
          );
          item = summarizeArchiveTest(archivePath, report.entries);
          if (item.status !== 'passwordRequired') break;
          wrong = item.wrongPassword === true;
        } catch (e: unknown) {
          const message = e instanceof FsError ? e.message : String(e);
          if (isCancelledError(e)) {
            item = cancelledItem(archivePath, 'Dibatalkan saat diuji.');
            break;
          }
          if (!isPasswordError(e)) {
            const unsupported =
              isMultipartError(e) || (e instanceof FsError && e.code === 'unsupported');
            item = {
              archivePath,
              status: unsupported ? 'unsupported' : 'failed',
              passed: 0,
              total: 0,
              failures: [],
              note: message,
            };
            break;
          }
          wrong = e instanceof FsError && e.archiveCode === 'badPassword';
          item = {
            archivePath,
            status: 'passwordRequired',
            passed: 0,
            total: 0,
            failures: [],
            note: message,
            ...(wrong ? { wrongPassword: true } : {}),
          };
        }
        const typed = await this.askPassword(
          archivePath,
          wrong ? wrongPasswordText(archivePath) : undefined,
          flow,
        );
        if (typed === null) {
          item.note = 'Password tidak diberikan.';
          break;
        }
        password = typed;
      }
    } finally {
      password = undefined;
    }
    item.durationMs = Math.round(performance.now() - started);
    return item;
  }

  // ── F2-14 Mark openers ────────────────────────────────────────────────

  beginSelectGroup(unselect = false) {
    const pane = activePane();
    if (!pane?.visibleEntries) return;
    this.open({ kind: 'selectGroup', unselect, parent: pane.path });
  }

  beginSaveSelection() {
    const pane = activePane();
    if (!pane) return;
    const names = pane.selectedPaths();
    if (names.length === 0) {
      this.open({ kind: 'error', title: 'Save Selection', message: 'Nothing selected.' });
      return;
    }
    this.open({ kind: 'saveSelection', parent: pane.path, names });
  }

  beginLoadSelection() {
    const pane = activePane();
    if (!pane) return;
    this.open({ kind: 'loadSelection', parent: pane.path });
  }

  beginShowFilter() {
    const pane = activePane();
    if (!pane) return;
    this.open({ kind: 'showFilter', parent: pane.path });
  }

  /** F2-27: Edit Comment on exactly one selected file. */
  beginComment(initial: string) {
    const pane = activePane();
    const target = pane?.cursorEntry();
    if (!pane || !target || target.isDir) return;
    if (pane.selectedPaths().length !== 1) return;
    this.open({ kind: 'comment', path: target.path, name: target.name, initial });
  }
}

export const dialogs = new DialogStore();
