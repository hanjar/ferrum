/**
 * FER-578 (FER-577 B2): frontend registry of long-running jobs (pack, extract, test archive,
 * checksum verify). A job is one user-visible flow; an extract batch of several archives is ONE
 * job whose `requestId` changes per archive. The job normally shows its progress in a modal
 * dialog; "Background" (or Esc) docks it: the modal closes, the app stays usable, and the job
 * lives in the operations panel (Alt+B) until it ends.
 *
 * A dock never grabs the focus. A prompt a docked job needs (password, error with a retry) and a
 * result the user may want to read are held as closures here and only open when the user asks
 * (dock row, toast action).
 *
 * One global `archive://progress` listener feeds every archive job; a verify job is fed by its
 * own store through `update()`.
 */
import {
  archiveCancelRequest,
  listenArchiveProgress,
  type ArchiveProgressEvent,
} from '../ipc/archive';
import { toast } from './toastStore.svelte';

export type JobKind = 'pack' | 'extract' | 'test' | 'verify';
export type JobState = 'running' | 'needsInput' | 'cancelling' | 'done' | 'failed' | 'canceled';

/** Something the user opens on demand: a held prompt or a finished job's result. */
export interface JobAction {
  label: string;
  open: () => void;
}

export interface Job {
  id: string;
  kind: JobKind;
  title: string;
  /** The archive / file the job works on (display only). */
  subject: string;
  /** "2/5" when a batch has several archives, else empty. */
  position: string;
  /** Backend request id of the step that runs now (archive jobs); '' before the first step. */
  requestId: string;
  state: JobState;
  /** True once moved to the background: it is listed in the dock and opens no dialog by itself. */
  docked: boolean;
  processedBytes: number;
  totalBytes?: number;
  processedEntries: number;
  totalEntries?: number;
  currentMember?: string;
  writtenEntries?: number;
  skippedEntries?: number;
  renamedEntries?: number;
  /** One-line outcome once the job ended (or what the held prompt is about). */
  summary: string;
  /** Held prompt (password, retry); set while `state === 'needsInput'`. */
  prompt?: JobAction;
  /** "View result" for a finished job (test report, extract summary, error). */
  result?: JobAction;
  startedAt: number;
  finishedAt?: number;
}

export function isJobActive(j: Pick<Job, 'state'>): boolean {
  return j.state === 'running' || j.state === 'needsInput' || j.state === 'cancelling';
}

/** 0..100, or null when the size of the job is not known. */
export function jobPercent(j: Job): number | null {
  if (j.state === 'done') return 100;
  if (j.totalBytes && j.totalBytes > 0)
    return Math.min(100, Math.round((j.processedBytes / j.totalBytes) * 100));
  if (j.totalEntries && j.totalEntries > 0)
    return Math.min(100, Math.round((j.processedEntries / j.totalEntries) * 100));
  return null;
}

/** Last path segment, for short messages. */
export function baseName(path: string): string {
  const i = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
  return i >= 0 ? path.slice(i + 1) : path;
}

let seq = 0;

function safeCancelRequest(requestId: string): void {
  try {
    if (typeof archiveCancelRequest === 'function') void archiveCancelRequest(requestId);
  } catch {
    // Vitest mock proxy without archiveCancelRequest
  }
}

async function safeListen(handler: (ev: ArchiveProgressEvent) => void): Promise<void> {
  try {
    if (typeof listenArchiveProgress === 'function') await listenArchiveProgress(handler);
  } catch {
    // No progress events (or a mock without them): jobs still run and can still be cancelled.
  }
}

class JobsStore {
  list = $state<Job[]>([]);
  private listening: Promise<void> | null = null;
  /** Per-request mirror (the progress dialog copies the same numbers). Not reactive. */
  private sinks = new Map<string, (ev: ArchiveProgressEvent) => void>();
  /** Cancel hook of a job that is not an archive request (checksum verify). */
  private cancelHooks = new Map<string, () => void>();

  /** Jobs that still run or wait for input. */
  get running(): Job[] {
    return this.list.filter(isJobActive);
  }

  get runningCount(): number {
    return this.running.length;
  }

  /** Docked jobs the user has not looked at yet and that need an answer. */
  get needInputCount(): number {
    return this.list.filter((j) => j.state === 'needsInput').length;
  }

  get dockedCount(): number {
    return this.list.filter((j) => j.docked).length;
  }

  /** Docked jobs the user can see in the panel (finished ones too, until dismissed). */
  get docked(): Job[] {
    return this.list.filter((j) => j.docked);
  }

  /** Idempotent: the single `archive://progress` listener. Safe to await before each job. */
  ensureListening(): Promise<void> {
    if (!this.listening) this.listening = safeListen((ev) => this.applyProgress(ev));
    return this.listening;
  }

  get(id: string): Job | undefined {
    return this.list.find((j) => j.id === id);
  }

  byRequest(requestId: string): Job | undefined {
    if (!requestId) return undefined;
    return this.list.find((j) => j.requestId === requestId && isJobActive(j));
  }

  begin(spec: {
    kind: JobKind;
    title: string;
    subject: string;
    position?: string;
    docked?: boolean;
  }): Job {
    this.list.push({
      id: `job-${++seq}`,
      kind: spec.kind,
      title: spec.title,
      subject: spec.subject,
      position: spec.position ?? '',
      requestId: '',
      state: 'running',
      docked: spec.docked ?? false,
      processedBytes: 0,
      processedEntries: 0,
      summary: '',
      startedAt: Date.now(),
    });
    // Read back through the array: the proxied object is the reactive one.
    return this.list[this.list.length - 1]!;
  }

  /** A new step of the job (the next archive of a batch, the retry after a password). */
  attempt(
    job: Job,
    requestId: string,
    step?: { subject?: string; position?: string },
    sink?: (ev: ArchiveProgressEvent) => void,
  ): void {
    if (job.requestId) this.sinks.delete(job.requestId);
    job.requestId = requestId;
    if (step?.subject !== undefined) job.subject = step.subject;
    if (step?.position !== undefined) job.position = step.position;
    job.state = 'running';
    job.prompt = undefined;
    job.processedBytes = 0;
    job.totalBytes = undefined;
    job.processedEntries = 0;
    job.totalEntries = undefined;
    job.currentMember = undefined;
    job.writtenEntries = undefined;
    job.skippedEntries = undefined;
    job.renamedEntries = undefined;
    if (sink) this.sinks.set(requestId, sink);
  }

  /** The step ended (settled or failed); its progress mirror is no longer needed. */
  endAttempt(job: Job): void {
    if (job.requestId) this.sinks.delete(job.requestId);
  }

  private applyProgress(ev: ArchiveProgressEvent): void {
    const job = this.byRequest(ev.requestId);
    if (!job) return;
    job.processedBytes = ev.processedBytes;
    job.totalBytes = ev.totalBytes;
    job.processedEntries = ev.processedEntries;
    job.totalEntries = ev.totalEntries;
    job.currentMember = ev.currentMember;
    if (ev.writtenEntries !== undefined) job.writtenEntries = ev.writtenEntries;
    if (ev.skippedEntries !== undefined) job.skippedEntries = ev.skippedEntries;
    if (ev.renamedEntries !== undefined) job.renamedEntries = ev.renamedEntries;
    this.sinks.get(ev.requestId)?.(ev);
  }

  /** Progress of a job that has no archive request (checksum verify). */
  update(
    job: Job,
    patch: Partial<
      Pick<
        Job,
        | 'processedBytes'
        | 'totalBytes'
        | 'processedEntries'
        | 'totalEntries'
        | 'currentMember'
        | 'subject'
      >
    >,
  ): void {
    Object.assign(job, patch);
  }

  /** Register how a non-archive job is cancelled. */
  setCancelHook(job: Job, hook: () => void): void {
    this.cancelHooks.set(job.id, hook);
  }

  /** Move the job to the background. The caller closes its own dialog. */
  dock(job: Job): void {
    if (job.docked || !isJobActive(job)) return;
    job.docked = true;
  }

  /** Cancel a backend request that has no job (a dialog opened without one). */
  cancelRequest(requestId: string): void {
    safeCancelRequest(requestId);
  }

  /** Stop the job. Idempotent. The flow settles it (`finish`) once the backend really stopped. */
  cancel(job: Job): void {
    if (job.state === 'cancelling' || !isJobActive(job)) return;
    job.state = 'cancelling';
    const hook = this.cancelHooks.get(job.id);
    if (hook) hook();
    else if (job.requestId) safeCancelRequest(job.requestId);
  }

  /**
   * The flow is over. A job nobody docked just disappears (its dialogs tell the story). A docked
   * one stays in the dock with its outcome and gets a short toast; it never opens a dialog.
   */
  finish(
    job: Job,
    outcome: 'done' | 'failed' | 'canceled',
    info?: { summary?: string; result?: JobAction; silent?: boolean },
  ): void {
    this.endAttempt(job);
    this.cancelHooks.delete(job.id);
    job.prompt = undefined;
    job.state = outcome;
    job.summary = info?.summary ?? '';
    job.result = info?.result;
    job.finishedAt = Date.now();
    if (!job.docked) {
      this.remove(job);
      return;
    }
    if (!info?.silent) {
      const text = job.summary || `${job.title}: ${outcomeLabel(outcome)}`;
      toast.show(text, 4000);
    }
  }

  /**
   * The flow needs an answer. A foreground job opens its dialog right away (the caller does
   * that). A docked one is held: it turns `needsInput`, a toast points at it, and `prompt.open`
   * runs only when the user chooses it.
   */
  holdPrompt(job: Job, prompt: JobAction, summary: string): void {
    job.prompt = prompt;
    job.state = 'needsInput';
    job.summary = summary;
    toast.show(summary, 6000, {
      label: 'Buka',
      run: () => this.openPrompt(job),
    });
  }

  /** The user chose the held prompt (dock row or toast): show it now. */
  openPrompt(job: Job): void {
    const prompt = job.prompt;
    if (!prompt) return;
    job.prompt = undefined;
    // The step continues from the answer; until then the job is "running" again.
    job.state = 'running';
    job.summary = '';
    prompt.open();
  }

  openResult(job: Job): void {
    job.result?.open();
  }

  remove(job: Job): void {
    this.endAttempt(job);
    this.cancelHooks.delete(job.id);
    const at = this.list.findIndex((j) => j.id === job.id);
    if (at >= 0) this.list.splice(at, 1);
  }

  /** Drop finished docked jobs (Clear history). */
  clearFinished(): void {
    for (const j of [...this.list]) if (!isJobActive(j)) this.remove(j);
  }

  get hasFinished(): boolean {
    return this.list.some((j) => !isJobActive(j));
  }

  /** Test helper: forget everything. */
  reset(): void {
    this.list = [];
    this.listening = null;
    this.sinks.clear();
    this.cancelHooks.clear();
  }
}

function outcomeLabel(o: 'done' | 'failed' | 'canceled'): string {
  return o === 'done' ? 'selesai' : o === 'failed' ? 'gagal' : 'dibatalkan';
}

export const jobs = new JobsStore();
