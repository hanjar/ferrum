// AI-07 chat store. Semua status yang tampil di UI berasal dari state executor
// (RunUpdate.state), BUKAN dari teks model. Executor/provider disuntikkan:
// store ini tidak membuat plan, tidak menjalankan operasi, tidak menyimpan apa pun ke storage.
import { parseFollowUp } from './followup';
import type { ProviderErrorView } from './providerErrors';
import type { SettleResult } from '../syncService';
import type { CopyAdjustView } from './copyAdjust';
import type { IndexerView } from './indexerControl';
import type { DiskUsageAnswer } from './diskUsageQuery';
import {
  answerItems,
  isOpenable,
  parseActivityAnswer,
  type ActivityAnswer,
  type ActivityLocation,
} from './activityQuery';
import {
  AskError,
  MAX_ATTACHMENTS,
  MAX_FILE_BYTES,
  ackDisclosure,
  boundHistory,
  disclosureAcked,
  type AskTurn,
  type AttachmentAsker,
  type AttachmentInfo,
  type AttachmentKind,
  type SentAttachment,
} from './attachments';
import { describeProviderError, uiCodeOf } from './providerErrors';
import {
  AgentAskError,
  agentCli,
  agentErrorText,
  needsAgent,
  type AgentAsker,
  type AgentCliStore,
  type AgentId,
  type AgentInfo,
  type SkippedItem,
} from './agentCli.svelte';
import type { RemoteProviderErrorCode } from './remote-provider';
import {
  ContractError,
  parseExecutionReceipt,
  parseResolvedPlan,
  type ExecutionReceipt,
  type ResolvedPlan,
} from './contracts';

/** State run menurut spesifikasi bagian 8 (dikendalikan executor). */
export type RunState =
  | 'planning'
  | 'resolving'
  | 'preview'
  | 'awaitingApproval'
  | 'queued'
  | 'running'
  | 'succeeded'
  | 'partial'
  | 'failed'
  | 'cancelled'
  | 'needsInput'
  | 'blockedCapability'
  | 'stale'
  | 'unknownOutcome'
  /** ACT-08: read-only history question answered locally (terminal; no plan, no approval). */
  | 'answered';

export interface ConflictView {
  itemRef: string;
  /** Data tak tepercaya (nama file): hanya dirender sebagai teks. */
  label: string;
  kind: string;
}
export interface JobView {
  jobId: string;
  state: 'queued' | 'running' | 'completed' | 'failed' | 'cancelled';
  done: number;
  total: number;
  message?: string;
}
export interface PlanItemRow {
  ref: string;
  label: string;
  detail?: string;
}
export interface RunUpdate {
  runId: string;
  requestId: string;
  state: RunState;
  plan?: ResolvedPlan;
  receipt?: ExecutionReceipt;
  jobs?: JobView[];
  conflicts?: ConflictView[];
  /** Alasan terstruktur dari executor/registry capability (bukan narasi model). */
  blockedReason?: string;
  errorMessage?: string;
  /** FER-465: galat provider terklasifikasi (pesan + tombol aksi); pesan yang sama ada di errorMessage. */
  providerError?: ProviderErrorView;
  /** Pertanyaan klarifikasi / narasi model: ditampilkan terpisah, tidak pernah menentukan status. */
  modelText?: string;
  /** Ringkasan scope yang disusun lokal (mis. "hanya file"); bukan dari model. */
  scopeNote?: string;
  /** Peringatan lokal untuk preview (mis. nama diganti karena bentrok). */
  warnings?: string[];
  /** AI-09: nomor revision rencana (1 = permintaan awal) dan catatan perubahan yang disusun lokal. */
  revision?: number;
  revisionNotes?: string[];
  /** ACT-08: local answer of an `activity.query` (never from the model; never sent to the provider). */
  activity?: ActivityAnswer;
  /** ACT-08: app-built summary of the query that was run (from the parsed intent). */
  activityQuery?: string;
  /** FER-471: app-built description of a copy tuning that waits for (or got) the user's confirm. */
  copyAdjust?: CopyAdjustView;
  /** FER-476: app-built FerrumIndexer status answer / mode-change confirm card. */
  indexer?: IndexerView;
  /** FER-466: local answer of a `diskUsage.query` (app-built; never sent to the provider). */
  diskUsage?: DiskUsageAnswer;
  /** FER-466: app-built summary of the question that was run (from the parsed intent). */
  diskUsageQuery?: string;
}

/** AI-09: acuan eksplisit ke rencana yang ingin diubah (bukan ke pesan chat lama). */
export interface FollowUpRef {
  planId: string;
  revision: number;
}
export type FollowUpRejectCode =
  'unknown_run' | 'final' | 'running' | 'busy' | 'no_plan' | 'stale_ref' | 'invalid_edit' | 'limit';
export type FollowUpResult =
  | { accepted: true; revision: number; local: boolean }
  | { accepted: false; code: FollowUpRejectCode; message: string };

/** LIMIT-4: hasil aksi "Periksa hasil" pada kartu unknownOutcome. Status baru tetap tiba lewat RunUpdate. */
export type CheckResult =
  { ok: true; state: RunState; message: string } | { ok: false; message: string };

/** Extra facts the card passes with the approve click. */
export interface ApproveOptions {
  /** FER-476: the user ticked the privacy acknowledgement (raising the indexer to high_performance). */
  acknowledged?: boolean;
}

export interface ChatExecutor {
  /** runId dibuat store; executor memakainya agar RunUpdate sampai ke run yang benar. */
  submit(req: { requestId: string; text: string; runId: string }): void;
  approve(runId: string, options?: ApproveOptions): void;
  cancel(runId: string): void;
  edit(runId: string): void;
  /**
   * AI-09: ubah rencana yang masih berada sebelum queue. Terikat ke {planId, revision}; hasilnya revision
   * baru (approval lama tidak berlaku). Ditolak bila rencana final/usang atau job sudah berjalan.
   */
  followUp?(runId: string, ref: FollowUpRef, text: string): FollowUpResult;
  subscribe(listener: (update: RunUpdate) => void): () => void;
  /** Periksa hasil eksekusi yang belum pasti lewat ledger/queue (rekonsiliasi); tidak pernah menjalankan ulang. */
  checkResult?(runId: string): Promise<CheckResult>;
  /** Daftar item plan per halaman (daftar besar tidak pernah dimuat sekaligus). */
  listItems?(
    runId: string,
    offset: number,
    limit: number,
  ): Promise<{ rows: PlanItemRow[]; total: number }>;
  /**
   * ACT-08 R7 v1: local location of one item listed in this run's answer ("Open in panel").
   * Read-only; `null` when the item is not in the answer, not openable or no longer locatable.
   */
  locateActivityItem?(runId: string, entityId: string): Promise<ActivityLocation | null>;
  /**
   * FER-433 / ACT-08 R7 v2: start an `archive.create` run for one item listed in `sourceRunId`'s answer
   * (selector `entity`, resolved locally at prepare time). App-initiated; the model is not involved.
   * Returns false when refused (nothing was started).
   */
  submitEntityArchive?(req: {
    requestId: string;
    runId: string;
    sourceRunId: string;
    entityId: string;
  }): boolean;
}

export interface ItemsPage {
  rows: PlanItemRow[];
  total: number;
  offset: number;
  loading: boolean;
  error?: string;
}

export interface RunView {
  runId: string;
  requestId: string;
  userText: string;
  state: RunState;
  /** Label dari state executor lewat statusLabel(); tidak pernah dari teks model. */
  label: string;
  plan?: ResolvedPlan;
  receipt?: ExecutionReceipt;
  jobs: JobView[];
  conflicts: ConflictView[];
  blockedReason?: string;
  errorMessage?: string;
  providerError?: ProviderErrorView;
  modelText?: string;
  scopeNote?: string;
  warnings?: string[];
  approvalSent: boolean;
  cancelRequested: boolean;
  canApprove: boolean;
  items?: ItemsPage;
  /** AI-09: nomor revision rencana yang tampil (0 sebelum ada plan) dan catatan perubahan lokal. */
  revision: number;
  revisionNotes: string[];
  /** Hasil settlement baseline sync (hanya run directories.sync); tiba setelah job selesai. */
  settlement?: SettleResult;
  /** AI-09: kartu pertanyaan yang sudah dijawab dengan permintaan baru; bukan target follow-up lagi. */
  superseded: boolean;
  /** "Periksa hasil" sedang berjalan / catatan hasil pemeriksaan terakhir (teks aplikasi). */
  checking: boolean;
  checkNote?: string;
  /** ACT-08: local history answer, the query summary and the candidate the user picked (ambiguous). */
  activity?: ActivityAnswer;
  activityQuery?: string;
  /** FER-471: copy tuning card (before/after + result), app-built. */
  copyAdjust?: CopyAdjustView;
  /** FER-476: indexer status answer or mode-change card (app text only). */
  indexer?: IndexerView;
  /** FER-466: local Disk Usage answer and the question summary (app text only). */
  diskUsage?: DiskUsageAnswer;
  diskUsageQuery?: string;
  chosenId?: string;
  /** "Open in panel" in progress / its last note (app text). */
  opening?: boolean;
  openNote?: string;
}

/** FER-486: lampiran di composer (referensi + metadata; isi dibaca backend saat Kirim). */
export interface ChatAttachment {
  path: string;
  name: string;
  isDir: boolean;
  status: 'inspecting' | 'ready' | 'error';
  info?: AttachmentInfo;
  error?: string;
  /** Pengguna mencentang "tetap kirim" untuk item yang terlihat sensitif. */
  confirmed: boolean;
  /** FER-493 O4: id pembatalan pemeriksaan backend selama `status === 'inspecting'`. */
  inspectId?: string;
}

/** FER-486: referensi lampiran yang disimpan di riwayat (bukan isinya). */
export interface AttachmentRefView {
  path: string;
  name: string;
  kind?: AttachmentKind;
  size?: number;
}

export interface ChatMessage {
  id: string;
  /**
   * `notice`: catatan lokal (mis. penolakan follow-up) — teks aplikasi, bukan dari model.
   * `answer` (FER-486): jawaban teks atas pertanyaan berlampiran. Hanya ditampilkan; tidak pernah
   * di-parse menjadi intent/rencana, jadi tidak bisa menjalankan apa pun.
   */
  role: 'user' | 'run' | 'notice' | 'answer';
  text?: string;
  runId?: string;
  /** FER-486: pesan pengguna berlampiran (referensi saja). */
  attachments?: AttachmentRefView[];
  /** FER-486: bagian dari percakapan lampiran (dikirim ulang sebagai history teks). */
  ask?: boolean;
  /** FER-486 `answer`: masih menunggu provider / galat (teks aplikasi) / ringkasan yang dikirim. */
  pending?: boolean;
  error?: string;
  sent?: SentAttachment[];
  /** FER-490: jawaban dari agent CLI lokal (nama tampilan agent). */
  agent?: string;
  /** FER-490: status agent saat bekerja (teks aplikasi, mis. "agent sedang membaca file…"). */
  status?: string;
  /** FER-490: keluaran agent melewati batas; jawaban berisi bagian sebelum batas. */
  truncated?: boolean;
  /** FER-490 E1: items inside attached folders that were not copied for the agent. */
  agentSkipped?: SkippedItem[];
}

/** FER-490: tujuan pertanyaan berlampiran: AI Ferrum (provider) atau agent CLI lokal. */
export type ChatTarget = 'ferrum' | AgentId;

export const PAGE_SIZE = 50;

const LABELS: Record<RunState, string> = {
  planning: 'Planning…',
  resolving: 'Resolving scope…',
  preview: 'Preview ready',
  awaitingApproval: 'Waiting for your approval',
  queued: 'Queued',
  running: 'Running',
  succeeded: 'Completed',
  partial: 'Partially completed',
  failed: 'Failed',
  cancelled: 'Cancelled',
  needsInput: 'Needs your input',
  blockedCapability: 'Not available',
  stale: 'Out of date — plan again',
  unknownOutcome: 'Outcome unknown — check the result',
  answered: 'Answered from local history',
};
const TERMINAL = new Set<RunState>([
  'succeeded',
  'partial',
  'failed',
  'cancelled',
  'blockedCapability',
  'stale',
  'unknownOutcome',
  'answered',
]);
const PRE_QUEUE = new Set<RunState>([
  'planning',
  'resolving',
  'preview',
  'awaitingApproval',
  'needsInput',
]);
const isLocalCancel = (text: string): boolean => {
  const p = parseFollowUp(text);
  return p.kind === 'local' && p.edit.kind === 'cancel';
};

/** Teks aplikasi (bukan dari model) untuk status baseline sync di kartu hasil. */
export function describeSettlement(s: SettleResult): {
  text: string;
  tone: 'good' | 'warn' | 'bad';
} {
  switch (s.status) {
    case 'saved':
      return { text: `Baseline saved (${s.entries} entries).`, tone: 'good' };
    case 'unchanged':
      return { text: `Baseline already up to date (${s.entries} entries).`, tone: 'good' };
    case 'saveFailed':
      return {
        text: 'Baseline could not be saved; the next sync will compare from scratch.',
        tone: 'bad',
      };
    case 'skipped':
      return {
        text:
          s.reason === 'jobsNotFinished'
            ? 'Baseline not updated: the copy jobs did not finish in time.'
            : s.reason === 'notVerified'
              ? 'Baseline not updated: the result could not be fully verified.'
              : 'Baseline not updated: verification failed.',
        tone: 'warn',
      };
  }
}

export function statusLabel(state: RunState): string {
  return LABELS[state];
}
export function isTerminal(state: RunState): boolean {
  return TERMINAL.has(state);
}
/** Persen gabungan 0..100 dari job executor; null bila belum ada job. */
export function jobsProgress(jobs: JobView[]): number | null {
  if (!jobs.length) return null;
  let done = 0;
  let total = 0;
  for (const j of jobs) {
    if (j.state === 'completed') {
      done += j.total || 1;
      total += j.total || 1;
    } else {
      done += Math.min(j.done, j.total);
      total += j.total;
    }
  }
  return total ? Math.round((done / total) * 100) : 0;
}

export class ChatStore {
  messages = $state<ChatMessage[]>([]);
  runs = $state<RunView[]>([]);
  draft = $state('');
  /** FER-486: lampiran aktif di composer. Tetap ada setelah Kirim untuk pertanyaan lanjutan. */
  attachments = $state<ChatAttachment[]>([]);
  /** FER-486: pertanyaan berlampiran sedang menunggu provider. */
  asking = $state(false);
  /** Naik saat disclosure diakui, agar getter yang membaca storage dihitung ulang. */
  private disclosureVersion = $state(0);
  private askAbort: AbortController | null = null;
  private seq = 0;
  /**
   * FER-486 A4: first message index whose question/answer turns may be sent as history. Moved to
   * the end when every attachment is removed or the provider endpoint changes, so earlier answers
   * (which can quote attachment content) never travel to a new conversation or another host.
   */
  private historyFrom = 0;
  private lastAskEndpoint: string | undefined;
  private unsubscribe: (() => void) | null;

  /** FER-490: tujuan pertanyaan berlampiran (pilihan di composer). */
  target = $state<ChatTarget>('ferrum');

  constructor(
    private readonly executor: ChatExecutor,
    private readonly asker?: AttachmentAsker,
    private readonly agentAsker?: AgentAsker,
    private readonly agents: AgentCliStore = agentCli,
  ) {
    this.unsubscribe = executor.subscribe((u) => this.apply(u));
  }

  /** Ada run yang belum selesai/menunggu input — composer menolak pesan baru. */
  get busy(): boolean {
    return this.asking || this.runs.some((r) => !isTerminal(r.state) && r.state !== 'needsInput');
  }

  /** FER-486: lampiran didukung oleh runtime ini. */
  get canAttach(): boolean {
    return this.asker !== undefined;
  }

  /** FER-490: agent CLI tersedia di runtime ini (desktop app). */
  get canUseAgents(): boolean {
    return this.agentAsker !== undefined;
  }

  /** FER-490: agent tujuan yang dipilih (undefined = AI Ferrum atau agent tidak lagi didukung). */
  get targetAgent(): AgentInfo | undefined {
    if (this.target === 'ferrum' || !this.agentAsker) return undefined;
    const a = this.agents.agent(this.target);
    return a?.status === 'supported' && a.enabled ? a : undefined;
  }

  setTarget(target: ChatTarget): void {
    this.target = target;
  }

  /**
   * FER-490: tawaran "Tanya lewat <agent default>?" saat ada lampiran yang tidak bisa dibaca utuh
   * oleh AI Ferrum (PDF/Office, metadata saja, atau melewati batas 100 KB).
   */
  get agentOffer(): AgentInfo | undefined {
    if (this.target !== 'ferrum' || !this.agentAsker) return undefined;
    const def = this.agents.defaultAgent;
    if (!def) return undefined;
    const want = this.attachments.some(
      (a) => a.status === 'ready' && a.info && needsAgent(a.info, MAX_FILE_BYTES),
    );
    return want ? def : undefined;
  }

  /** FER-490: agent tujuan yang pemberitahuannya belum disetujui (sekali per agent). */
  get pendingAgentDisclosure(): AgentInfo | undefined {
    const a = this.targetAgent;
    return a && !a.acknowledged ? a : undefined;
  }

  async ackAgentDisclosure(): Promise<void> {
    const a = this.pendingAgentDisclosure;
    if (a && this.agentAsker) await this.agentAsker.acknowledge(a.id);
  }

  /** FER-486: tujuan isi lampiran (host:port provider) untuk teks composer. */
  get attachmentDestination() {
    return this.asker?.destination();
  }

  /** FER-486: host di luar PC yang disclosure-nya belum diakui (undefined bila tidak perlu). */
  get pendingDisclosure(): string | undefined {
    void this.disclosureVersion;
    // FER-490: ke agent, isi file tidak lewat provider Ferrum (agent punya disclosure sendiri).
    if (this.target !== 'ferrum') return undefined;
    const dest = this.asker?.destination();
    if (!dest || dest.local || !this.attachments.length) return undefined;
    return disclosureAcked(dest.hostPort) ? undefined : dest.hostPort;
  }

  ackAttachmentDisclosure(): void {
    const host = this.pendingDisclosure;
    if (!host) return;
    ackDisclosure(host);
    this.disclosureVersion++;
  }

  /** Lampiran siap dikirim: semua terbaca, item sensitif sudah dicentang, disclosure diakui. */
  get attachmentsReady(): boolean {
    return (
      this.attachments.every((a) => a.status === 'ready' && (!a.info?.sensitive || a.confirmed)) &&
      this.pendingDisclosure === undefined &&
      this.pendingAgentDisclosure === undefined
    );
  }

  /**
   * FER-486: tambahkan item terpilih sebagai lampiran (maks. 10 per pesan, duplikat diabaikan).
   * Hanya metadata dan flag sensitif yang dibaca sekarang; isi dibaca backend saat Kirim.
   */
  async addAttachments(items: { path: string; name: string; isDir: boolean }[]): Promise<number> {
    const asker = this.asker;
    if (!asker) {
      this.notice('Lampiran tidak tersedia di mode ini.');
      return 0;
    }
    const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
    const fresh = items.filter(
      (item, i) =>
        !this.attachments.some((a) => same(a.path, item.path)) &&
        !items.slice(0, i).some((b) => same(b.path, item.path)),
    );
    const room = Math.max(0, MAX_ATTACHMENTS - this.attachments.length);
    const take = fresh.slice(0, room);
    if (fresh.length > take.length) {
      this.notice(
        `Maksimal ${MAX_ATTACHMENTS} lampiran per pesan; ${fresh.length - take.length} item tidak ditambahkan.`,
      );
    }
    if (!take.length) return 0;
    const inspectRun = `inspect-${Date.now().toString(36)}-${++this.seq}`;
    take.forEach((item, i) => {
      this.attachments.push({
        path: item.path,
        name: item.name,
        isDir: item.isDir,
        status: 'inspecting',
        confirmed: false,
        inspectId: `${inspectRun}:${i}`,
      });
    });
    const chip = (path: string) => this.attachments.find((a) => a.path === path);
    try {
      const results = await asker.inspect(
        take.map((t) => t.path),
        inspectRun,
      );
      for (const item of take) {
        const c = chip(item.path);
        const res = results.find((r) => r.path === item.path);
        if (!c) continue;
        if (res?.info) {
          c.info = res.info;
          c.name = res.info.name;
          c.status = 'ready';
        } else {
          c.status = 'error';
          c.error = res?.error ?? 'File tidak dapat dibaca.';
        }
      }
    } catch {
      for (const item of take) {
        const c = chip(item.path);
        if (c && c.status === 'inspecting') {
          c.status = 'error';
          c.error = 'Info file tidak dapat dibaca.';
        }
      }
    }
    return take.length;
  }

  removeAttachment(path: string): void {
    this.cancelInspections(this.attachments.filter((a) => a.path === path));
    this.attachments = this.attachments.filter((a) => a.path !== path);
    if (!this.attachments.length) this.resetAskHistory();
  }

  clearAttachments(): void {
    this.cancelInspections(this.attachments);
    this.attachments = [];
    this.resetAskHistory();
  }

  /** FER-493 O4: chip yang dihapus saat masih diperiksa menghentikan ekstraksinya di backend. */
  private cancelInspections(chips: ChatAttachment[]): void {
    for (const c of chips) {
      if (c.status === 'inspecting' && c.inspectId) this.asker?.cancelInspect?.(c.inspectId);
    }
  }

  /**
   * FER-493: backend menemukan rahasia yang belum terlihat saat chip diperiksa (mis. dokumen
   * Office yang baru diekstrak saat kirim). Tandai chip supaya kotak centang muncul.
   */
  private markSensitive(path: string | undefined): ChatAttachment | undefined {
    const c = path ? this.attachments.find((a) => a.path === path) : undefined;
    if (c) {
      c.confirmed = false;
      if (c.info && !c.info.sensitive) c.info = { ...c.info, sensitive: 'content' };
    }
    return c;
  }

  /** FER-486 A4: later questions start without the earlier question/answer turns. */
  resetAskHistory(): void {
    this.historyFrom = this.messages.length;
  }

  /** Centang manual "tetap kirim" untuk item sensitif. */
  confirmAttachment(path: string, confirmed: boolean): void {
    const c = this.attachments.find((a) => a.path === path);
    if (c) c.confirmed = confirmed;
  }

  /** FER-486: batalkan pertanyaan berlampiran yang sedang berjalan. */
  cancelAsk(): void {
    this.askAbort?.abort();
  }
  get activeRun(): RunView | undefined {
    for (let i = this.runs.length - 1; i >= 0; i--)
      if (!isTerminal(this.runs[i].state) && !this.runs[i].superseded) return this.runs[i];
    return undefined;
  }

  /** Boleh mengirim: bila executor menerima follow-up, pesan saat sibuk menghasilkan catatan penolakan yang jelas. */
  canSend(raw: string): boolean {
    const text = raw.trim();
    if (!text) return false;
    if (this.asking) return isLocalCancel(text);
    // FER-490: ke agent hanya pertanyaan berlampiran (agent membaca lampiran itu saja).
    if (this.target !== 'ferrum')
      return !!this.targetAgent && !!this.attachments.length && !this.busy && this.attachmentsReady;
    if (this.attachments.length && this.asker) return !this.busy && this.attachmentsReady;
    return !this.busy || isLocalCancel(text) || typeof this.executor.followUp === 'function';
  }

  /** Catatan lokal di chat (teks aplikasi). */
  notice(text: string): void {
    this.messages.push({ id: `m-${++this.seq}-n`, role: 'notice', text });
  }

  run(runId: string): RunView | undefined {
    return this.runs.find((r) => r.runId === runId);
  }

  send(raw: string): void {
    const text = raw.trim();
    if (!text) return;
    if (this.asking) {
      if (isLocalCancel(text)) {
        this.draft = '';
        this.cancelAsk();
      }
      return;
    }
    // FER-490: tujuan agent CLI lokal: pertanyaan berlampiran saja, jawaban teks saja.
    if (this.target !== 'ferrum') {
      if (!this.attachments.length) {
        this.notice('Lampirkan file dulu (Add to chat) untuk bertanya ke agent.');
        return;
      }
      if (this.canSend(text)) void this.sendAgent(text);
      return;
    }
    // FER-486: dengan lampiran, pesan menjadi pertanyaan bebas (jawaban teks saja, tanpa rencana).
    if (this.attachments.length && this.asker) {
      if (this.canSend(text)) void this.sendAsk(text);
      return;
    }
    const active = this.activeRun;
    // Aturan 10: "batal" diproses lokal, tanpa menunggu provider.
    if (isLocalCancel(text)) {
      this.draft = '';
      if (active) this.cancel(active.runId);
      else this.notice('Tidak ada rencana atau job aktif untuk dibatalkan.');
      return;
    }
    // AI-09: pesan saat ada rencana aktif = perubahan atas rencana itu, terikat ke plan id + revision yang tampil.
    if (active && this.executor.followUp) {
      const res = this.followUp(active.runId, text);
      if (!res || res.accepted || res.code !== 'no_plan') return;
      // Kartu pertanyaan (belum ada plan): jawaban pengguna adalah permintaan baru; kartu lama digantikan.
      active.superseded = true;
    } else if (this.busy) {
      return;
    } else if (this.executor.followUp) {
      // Rencana sebelumnya sudah final: frasa perubahan TIDAK menjadi permintaan baru dan tidak memutasi hasil lama.
      const last = this.runs.at(-1);
      if (last && isTerminal(last.state) && parseFollowUp(text).kind !== 'provider') {
        this.draft = '';
        this.notice(
          `Rencana sebelumnya sudah berakhir (${last.label}); perubahan tidak diterapkan pada hasil itu. ` +
            'Buat permintaan baru, mis. "arsipkan folder kiri jadi zip namanya x.zip".',
        );
        return;
      }
    }
    const n = ++this.seq;
    const requestId = `req-${Date.now().toString(36)}-${n}`;
    const runId = `run-${n}`;
    this.messages.push({ id: `m-${n}-u`, role: 'user', text });
    this.runs.push({
      runId,
      requestId,
      userText: text,
      state: 'planning',
      label: statusLabel('planning'),
      jobs: [],
      conflicts: [],
      approvalSent: false,
      cancelRequested: false,
      canApprove: false,
      revision: 0,
      revisionNotes: [],
      superseded: false,
      checking: false,
    });
    this.messages.push({ id: `m-${n}-r`, role: 'run', runId });
    this.draft = '';
    this.executor.submit({ requestId, text, runId });
  }

  /** Tanya-jawab berlampiran sebelumnya (teks saja) untuk pertanyaan lanjutan. */
  private askTurns(): AskTurn[] {
    const turns: AskTurn[] = [];
    for (let i = this.historyFrom; i < this.messages.length; i++) {
      const m = this.messages[i];
      const next = this.messages[i + 1];
      if (m.role === 'user' && m.ask && next?.role === 'answer' && next.text && m.text) {
        turns.push({ role: 'user', text: m.text }, { role: 'assistant', text: next.text });
      }
    }
    return boundHistory(turns);
  }

  /**
   * FER-486: kirim pertanyaan berlampiran. Hanya referensi (path + centang sensitif) yang dikirim
   * ke backend; backend membaca ulang isi file sekarang (file bisa berubah sejak dilampirkan).
   * Jawabannya teks: tidak ada rencana, kartu, atau eksekusi yang bisa muncul dari jalur ini.
   */
  private async sendAsk(text: string): Promise<void> {
    const asker = this.asker;
    if (!asker) return;
    const n = ++this.seq;
    const requestId = `ask-${Date.now().toString(36)}-${n}`;
    const dest = asker.destination();
    const endpoint = dest?.endpoint ?? dest?.hostPort;
    if (this.lastAskEndpoint !== undefined && endpoint !== this.lastAskEndpoint)
      this.resetAskHistory();
    this.lastAskEndpoint = endpoint;
    const history = this.askTurns();
    const refs = this.attachments.map((a) => ({
      path: a.path,
      confirmedSensitive: a.info?.sensitive ? a.confirmed : false,
    }));
    this.messages.push({
      id: `m-${n}-u`,
      role: 'user',
      text,
      ask: true,
      attachments: this.attachments.map((a) => ({
        path: a.path,
        name: a.name,
        kind: a.info?.kind,
        size: a.info?.size,
      })),
    });
    const answerId = `m-${n}-a`;
    this.messages.push({ id: answerId, role: 'answer', pending: true });
    this.draft = '';
    this.asking = true;
    const abort = new AbortController();
    this.askAbort = abort;
    const answer = () => this.messages.find((m) => m.id === answerId);
    try {
      const out = await asker.ask({
        requestId,
        question: text,
        history,
        attachments: refs,
        signal: abort.signal,
      });
      const m = answer();
      if (m && !abort.signal.aborted) {
        m.text = out.answer;
        m.sent = out.attachments;
      } else if (m) {
        m.error = this.askErrorText(new AskError('cancelled'));
      }
    } catch (error) {
      const m = answer();
      if (m) m.error = this.askErrorText(error);
    } finally {
      const m = answer();
      if (m) m.pending = false;
      this.asking = false;
      if (this.askAbort === abort) this.askAbort = null;
    }
  }

  /**
   * FER-490: pertanyaan berlampiran ke agent CLI lokal. Hanya referensi lampiran yang dikirim ke
   * backend; agent membaca file sendiri dalam mode baca-saja. Jawaban di-stream sebagai teks dan
   * hanya ditampilkan (MarkdownText); tidak ada rencana, kartu, atau eksekusi dari jalur ini.
   */
  private async sendAgent(text: string): Promise<void> {
    const asker = this.agentAsker;
    const agent = this.targetAgent;
    if (!asker || !agent) return;
    const n = ++this.seq;
    const requestId = `agent-${Date.now().toString(36)}-${n}`;
    // History tidak menyeberang tujuan (A4): AI Ferrum dan tiap agent adalah layanan berbeda.
    const endpoint = `agent:${agent.id}`;
    if (this.lastAskEndpoint !== undefined && endpoint !== this.lastAskEndpoint)
      this.resetAskHistory();
    this.lastAskEndpoint = endpoint;
    const history = this.askTurns();
    const refs = this.attachments.map((a) => ({
      path: a.path,
      confirmedSensitive: a.info?.sensitive ? a.confirmed : false,
    }));
    this.messages.push({
      id: `m-${n}-u`,
      role: 'user',
      text,
      ask: true,
      attachments: this.attachments.map((a) => ({
        path: a.path,
        name: a.name,
        kind: a.info?.kind,
        size: a.info?.size,
      })),
    });
    const answerId = `m-${n}-a`;
    this.messages.push({
      id: answerId,
      role: 'answer',
      pending: true,
      agent: agent.name,
      status: 'agent sedang membaca…',
    });
    this.draft = '';
    this.asking = true;
    const abort = new AbortController();
    this.askAbort = abort;
    const answer = () => this.messages.find((m) => m.id === answerId);
    try {
      const out = await asker.ask({
        requestId,
        agent: agent.id,
        question: text,
        history,
        attachments: refs,
        signal: abort.signal,
        onEvent: (kind, chunk) => {
          const m = answer();
          if (!m || !m.pending) return;
          if (kind === 'status') m.status = chunk;
          else m.text = m.text ? `${m.text}\n\n${chunk}` : chunk;
        },
      });
      const m = answer();
      if (m && !abort.signal.aborted) {
        m.text = out.answer;
        m.truncated = out.truncated;
        m.agentSkipped = out.skipped;
      } else if (m) {
        m.text = undefined;
        m.error = 'Dibatalkan.';
      }
    } catch (error) {
      const m = answer();
      if (m) {
        const e = error instanceof AgentAskError ? error : new AgentAskError('backend_failure');
        if (e.kind === 'sensitive_unconfirmed') this.markSensitive(e.path);
        m.error = agentErrorText(e, agent.name);
        // Teks yang sempat di-stream bukan jawaban (tidak tampil, tidak masuk history).
        m.text = undefined;
      }
    } finally {
      const m = answer();
      if (m) {
        m.pending = false;
        m.status = undefined;
      }
      this.asking = false;
      if (this.askAbort === abort) this.askAbort = null;
    }
  }

  private askErrorText(error: unknown): string {
    const kind = error instanceof AskError ? error.kind : 'backend_failure';
    if (kind === 'cancelled') return 'Dibatalkan.';
    if (kind === 'sensitive_unconfirmed') {
      const c = this.markSensitive(error instanceof AskError ? error.path : undefined);
      return `Lampiran ${c?.name ?? ''} terlihat sensitif dan belum dicentang. Tidak ada yang dikirim.`;
    }
    return describeProviderError(uiCodeOf(kind as RemoteProviderErrorCode), {
      hostPort: this.asker?.destination()?.hostPort,
    }).message;
  }

  /**
   * AI-09: ubah rencana pada run ini. `ref` eksplisit (plan id + revision yang dimaksud pengguna); bila tidak
   * diberikan dipakai plan yang sedang tampil. Penolakan executor menjadi catatan di chat.
   */
  followUp(runId: string, raw: string, ref?: FollowUpRef): FollowUpResult | undefined {
    const text = raw.trim();
    const r = this.run(runId);
    const fn = this.executor.followUp?.bind(this.executor);
    if (!r || !fn || !text) return undefined;
    const target = ref ?? (r.plan ? { planId: r.plan.planId, revision: r.revision } : undefined);
    if (!target)
      return { accepted: false, code: 'no_plan', message: 'Belum ada rencana yang bisa diubah.' };
    const res = fn(runId, target, text);
    if (res.accepted) {
      this.messages.push({ id: `m-${++this.seq}-u`, role: 'user', text, runId });
      this.draft = '';
      r.approvalSent = false;
      r.canApprove = false;
      r.errorMessage = undefined;
      r.providerError = undefined;
    } else if (res.code !== 'no_plan') {
      this.notice(res.message);
      if (res.code !== 'invalid_edit' && res.code !== 'busy') this.draft = '';
    }
    return res;
  }

  /** FER-465: [Coba lagi] pada kartu galat provider: kirim ulang teks permintaan yang sama sebagai permintaan baru. */
  retry(runId: string): void {
    const r = this.run(runId);
    if (!r || r.state !== 'failed' || !r.providerError || this.busy) return;
    this.send(r.userText);
  }

  approve(runId: string, options?: ApproveOptions): void {
    const r = this.run(runId);
    if (!r || r.state !== 'awaitingApproval' || r.approvalSent) return;
    // FER-476: the privacy tick is part of the approval; without it nothing is sent or consumed.
    if (r.indexer?.kind === 'setMode' && r.indexer.needsAck && options?.acknowledged !== true)
      return;
    r.approvalSent = true;
    r.canApprove = false;
    if (options) this.executor.approve(runId, options);
    else this.executor.approve(runId);
  }

  cancel(runId: string): void {
    const r = this.run(runId);
    if (!r || isTerminal(r.state)) return;
    // A confirmed control change (copy tuning, indexer mode) is already being applied: it cannot be cancelled.
    if (r.state === 'awaitingApproval' && r.approvalSent && (r.copyAdjust || r.indexer)) return;
    this.executor.cancel(runId);
    if (PRE_QUEUE.has(r.state)) {
      // Belum ada job: tutup lokal agar respons provider yang terlambat tidak berlaku.
      this.setState(r, 'cancelled');
    } else {
      // Job berjalan: tunggu acknowledgement terminal dari executor.
      r.cancelRequested = true;
    }
  }

  edit(runId: string): void {
    const r = this.run(runId);
    if (!r || isTerminal(r.state) || r.state === 'queued' || r.state === 'running') return;
    this.executor.edit(runId);
    this.draft = r.userText;
    r.approvalSent = false;
    r.canApprove = false;
  }

  /** Executor mendukung "Periksa hasil" untuk kartu unknownOutcome. */
  get canCheckResult(): boolean {
    return typeof this.executor.checkResult === 'function';
  }

  /** LIMIT-4: periksa hasil yang belum pasti. Status hanya berubah lewat RunUpdate dari executor. */
  async checkResult(runId: string): Promise<void> {
    const r = this.run(runId);
    const check = this.executor.checkResult?.bind(this.executor);
    if (!r || !check || r.state !== 'unknownOutcome' || r.checking) return;
    r.checking = true;
    r.checkNote = undefined;
    try {
      const res = await check(runId);
      r.checkNote = res.message;
    } catch {
      r.checkNote = 'Hasil tidak dapat diperiksa; status tetap belum pasti.';
    } finally {
      r.checking = false;
    }
  }

  /** ACT-08: pick one candidate of an ambiguous answer (local UI state only; nothing is queried or sent). */
  chooseCandidate(runId: string, entityId: string): void {
    const r = this.run(runId);
    if (!r || r.state !== 'answered' || r.activity?.status !== 'ambiguous') return;
    if (!r.activity.candidates.some((c) => c.id === entityId)) return;
    r.chosenId = entityId;
    r.openNote = undefined;
  }

  /** Executor can locate listed history items for "Open in panel". */
  get canOpenActivityItems(): boolean {
    return typeof this.executor.locateActivityItem === 'function';
  }

  /**
   * ACT-08 R7 v1: resolve one listed item locally and hand it to `place` (pane API only). Nothing is
   * mutated; a follow-up operation still needs the normal prepared plan + approval in that pane.
   */
  async openActivityItem(
    runId: string,
    entityId: string,
    place: (location: ActivityLocation) => boolean,
  ): Promise<boolean> {
    const r = this.run(runId);
    const locate = this.executor.locateActivityItem?.bind(this.executor);
    if (!r || !locate || r.state !== 'answered' || !r.activity || r.opening) return false;
    if (!answerItems(r.activity).some((i) => i.id === entityId)) return false;
    r.opening = true;
    r.openNote = undefined;
    try {
      const location = await locate(runId, entityId);
      if (!location) {
        r.openNote =
          'Item ini tidak dapat dibuka lagi (dipindah, disembunyikan, atau riwayat dijeda).';
        return false;
      }
      if (!place(location)) {
        r.openNote = 'Panel aktif tidak dapat membuka lokasi ini.';
        return false;
      }
      r.openNote =
        'Dibuka di panel aktif. Untuk ZIP/sync, pilih item di panel lalu minta rencana baru.';
      return true;
    } catch {
      r.openNote = 'Lokasi item tidak dapat dibaca; tidak ada yang dibuka.';
      return false;
    } finally {
      r.opening = false;
    }
  }

  /** FER-433: the executor can start a ZIP of a listed history item. */
  get canZipActivityItems(): boolean {
    return typeof this.executor.submitEntityArchive === 'function';
  }

  /**
   * FER-433 / ACT-08 R7 v2: "Buat ZIP" on a listed history item. Starts a normal archive run (plan card,
   * approval, backend guard); nothing is written until the user approves that plan. Refused while another
   * plan is active so two approvals can never be confused.
   */
  zipActivityItem(runId: string, entityId: string): boolean {
    const r = this.run(runId);
    const start = this.executor.submitEntityArchive?.bind(this.executor);
    if (!r || !start || r.state !== 'answered' || !r.activity || this.busy) return false;
    const item = answerItems(r.activity).find((i) => i.id === entityId);
    if (!item || !isOpenable(item)) return false;
    const n = ++this.seq;
    const requestId = `req-${Date.now().toString(36)}-${n}`;
    const newRunId = `run-${n}`;
    const text = `Buat ZIP dari riwayat: ${item.displayName}`;
    this.runs.push({
      runId: newRunId,
      requestId,
      userText: text,
      state: 'planning',
      label: statusLabel('planning'),
      jobs: [],
      conflicts: [],
      approvalSent: false,
      cancelRequested: false,
      canApprove: false,
      revision: 0,
      revisionNotes: [],
      superseded: false,
      checking: false,
    });
    if (!start({ requestId, runId: newRunId, sourceRunId: runId, entityId })) {
      this.runs.pop();
      return false;
    }
    this.messages.push({ id: `m-${n}-u`, role: 'user', text });
    this.messages.push({ id: `m-${n}-r`, role: 'run', runId: newRunId });
    return true;
  }

  async loadItems(runId: string, offset: number): Promise<void> {
    const r = this.run(runId);
    const list = this.executor.listItems?.bind(this.executor);
    if (!r || !list) return;
    const prev = r.items;
    r.items = { rows: prev?.rows ?? [], total: prev?.total ?? 0, offset, loading: true };
    try {
      const page = await list(runId, offset, PAGE_SIZE);
      r.items = { rows: page.rows, total: page.total, offset, loading: false };
    } catch {
      r.items = {
        rows: prev?.rows ?? [],
        total: prev?.total ?? 0,
        offset,
        loading: false,
        error: 'Could not load items.',
      };
    }
  }

  /**
   * Hasil `settleSyncRun` untuk run sync yang rencananya memakai `previewKey` (= leftManifest). Tiba terlambat,
   * sering setelah run terminal, jadi tidak lewat `apply`. Tidak memuat nama/path.
   */
  noteSettlement(previewKey: string | undefined, result: SettleResult): void {
    if (!previewKey) return;
    const r = this.runs.find(
      (x) =>
        x.plan?.operation.kind === 'directories.sync' &&
        x.plan.operation.leftManifest === previewKey,
    );
    if (r) r.settlement = result;
  }

  /**
   * Ambil riwayat dari store runtime lama (setup provider berubah). Run yang belum selesai tidak bisa
   * dilanjutkan oleh runtime baru: sebelum antrean/job -> Cancelled, sesudahnya -> Outcome unknown.
   * Store lama dilepas dari executor-nya.
   */
  adoptHistory(old: ChatStore): void {
    if (old === this) return;
    old.dispose();
    this.seq = Math.max(this.seq, old.seq);
    const runs = old.runs.map((r) => {
      const copy = $state.snapshot(r) as RunView;
      if (!isTerminal(copy.state)) {
        this.setState(copy, PRE_QUEUE.has(copy.state) ? 'cancelled' : 'unknownOutcome');
        copy.canApprove = false;
      }
      return copy;
    });
    this.runs = [...runs, ...this.runs];
    this.messages = [...($state.snapshot(old.messages) as ChatMessage[]), ...this.messages];
    this.draft = this.draft || old.draft;
    // FER-486: lampiran ikut pindah (referensi saja); jawaban yang masih menunggu dibatalkan.
    old.cancelAsk();
    // FER-486 A4: a new runtime means a new provider setup; earlier answers are not resent.
    this.resetAskHistory();
    if (!this.attachments.length)
      this.attachments = $state.snapshot(old.attachments) as ChatAttachment[];
    if (this.target === 'ferrum') this.target = old.target;
    for (const m of this.messages) {
      if (m.role === 'answer' && m.pending) {
        m.pending = false;
        m.error = 'Dibatalkan karena pengaturan provider berubah.';
      }
    }
  }

  dispose(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
  }

  private setState(r: RunView, state: RunState): void {
    r.state = state;
    r.label = statusLabel(state);
    r.canApprove = state === 'awaitingApproval' && !r.approvalSent;
  }

  private apply(u: RunUpdate): void {
    const r = this.run(u.runId);
    // unknownOutcome hanya boleh ditinggalkan lewat rekonsiliasi executor ("Periksa hasil"); final lain tetap final.
    if (!r || r.requestId !== u.requestId || (isTerminal(r.state) && r.state !== 'unknownOutcome'))
      return;
    try {
      if (u.plan) r.plan = parseResolvedPlan(u.plan);
      if (u.receipt) r.receipt = parseExecutionReceipt(u.receipt);
      if (u.activity) r.activity = parseActivityAnswer(u.activity);
    } catch (e) {
      if (!(e instanceof ContractError)) throw e;
      r.errorMessage = 'Invalid data received from the executor; nothing was run.';
      this.setState(r, 'failed');
      return;
    }
    if (u.jobs) r.jobs = u.jobs;
    if (u.conflicts) r.conflicts = u.conflicts;
    if (u.blockedReason !== undefined) r.blockedReason = u.blockedReason;
    if (u.errorMessage !== undefined) r.errorMessage = u.errorMessage;
    if (u.providerError !== undefined) r.providerError = u.providerError;
    if (u.modelText !== undefined) r.modelText = u.modelText;
    if (u.scopeNote !== undefined) r.scopeNote = u.scopeNote;
    if (u.warnings) r.warnings = u.warnings;
    if (u.revision !== undefined) r.revision = u.revision;
    if (u.revisionNotes) r.revisionNotes = u.revisionNotes;
    if (u.activityQuery !== undefined) r.activityQuery = u.activityQuery;
    if (u.copyAdjust !== undefined) r.copyAdjust = u.copyAdjust;
    if (u.indexer !== undefined) r.indexer = u.indexer;
    if (u.diskUsage !== undefined) r.diskUsage = u.diskUsage;
    if (u.diskUsageQuery !== undefined) r.diskUsageQuery = u.diskUsageQuery;
    this.setState(r, u.state);
  }
}
