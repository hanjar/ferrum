// AI-06 / FER-195..198: executor chat AI. Mengimplementasikan `ChatExecutor` (chatStore) di atas
// facade (prepare -> approve -> execute), policy/approval lokal (AI-05), ledger idempotensi backend,
// dan operation queue. Aturan yang ditegakkan di sini:
//   - Model hanya mengusulkan intent; approval/risiko/availability dihitung lokal.
//   - Mutasi sebelum persetujuan = nol file/job (prepare read-only; execute hanya dari token approval).
//   - "Berhasil" hanya dari hasil service/queue (`summarizeRun`), bukan kalimat model.
//   - Idempotensi: reserve di ledger SEBELUM execute; hasil tak diketahui => unknownOutcome, tanpa replay.
//   - Progress/cancel hanya untuk execution dan job ID milik run (dari ledger).
//   - Respons terlambat (provider/prepare/execute) setelah cancel/edit dibuang (epoch + status).
//   - Sebab engine yang pasti (`execution_failed` + `cause`, mis. izin ditolak / disk penuh) => `failed` beserta
//     sebabnya; tanpa sebab tetap unknownOutcome. "Periksa hasil" (`checkResult`) merekonsiliasi unknownOutcome.
//   - AI-09: follow-up (`followUp`) terikat ke {planId, revision}; hanya sebelum queue; hasilnya revision baru
//     (handle/approval/slot backend lama dicabut); setelah queued/running edit ditolak, job aktif tidak berubah.
//   - ACT-08 R5: intent baca `activity.query` TIDAK menuju prepare/approval/ResolvedPlan/ledger/queue. Query
//     dijalankan lokal lewat `ActivityQueryPort`, hasilnya diparse ketat dan run berakhir `answered`. Hasil
//     riwayat tidak pernah dikirim ke provider (tidak ada giliran kedua, tidak masuk context model).
import type { OperationProgress } from '../ipc/types';
import { entityArchiveEnvelope } from './entitySelector';
import {
  answerForError,
  answerItems,
  describeQuery,
  isOpenable,
  parseActivityAnswer,
  parseActivityLocation,
  type ActivityAnswer,
  type ActivityLocation,
  type ActivityQueryPort,
} from './activityQuery';
import { offeredReadCapabilityIds } from './readCapabilities';
import {
  alreadyInMode,
  describeIndexerStatus,
  describeModeChange,
  modeChangeFailure,
  modeChangeSuccess,
  NOT_INSTALLED_FOR_CHANGE,
  NOT_RUNNING_FOR_CHANGE,
  offeredIndexerCapabilityIds,
  type IndexerControlPort,
  type IndexerModeView,
  type IndexerView,
} from './indexerControl';
import { readiness } from '../ipc/indexer';
import { pathTypedByUser } from './paneNavigate';
import {
  describeDiskUsageQuery,
  type DiskUsageAnswer,
  type DiskUsageQueryPort,
  runDiskUsageQuery,
} from './diskUsageQuery';
import type { CopyControlState } from '../ipc/copyControl';
import {
  describeCopyAdjust,
  describeJob,
  offeredControlCapabilityIds,
  previewCopyAdjust,
  sameSettings,
  settingsOf,
  toConfirmedChange,
  type CopyAdjustView,
  type CopyControlPort,
} from './copyAdjust';
import {
  decideConflicts,
  ApprovalManager,
  type ApprovalTicket,
  type ConflictPort,
} from './approval';
import type {
  ChatExecutor,
  CheckResult,
  ConflictView,
  FollowUpRef,
  FollowUpResult,
  JobView,
  PlanItemRow,
  RunState,
  RunUpdate,
} from './chatStore.svelte';
import {
  err,
  type ApprovedPreparedCommand,
  type Availability,
  type CommandError,
  type CommandFacade,
  type PreparedCommand,
  type PreviewInfo,
} from './commands';
import {
  describeNavigate,
  drivePathCount,
  userAskedToDelete,
  type NavigatePort,
  type NavigateTarget,
} from './paneNavigate';
import {
  ACTIVITY_QUERY_CAPABILITY,
  DISK_USAGE_QUERY_CAPABILITY,
  PANE_NAVIGATE_CAPABILITY,
  UI_HINT_CAPABILITY,
  isSafeNavigatePath,
  type ChainEnvelope,
  type PaneRef,
  parseDiskUsageQueryKind,
  type DiskUsageQueryKind,
  COPY_ADJUST_CAPABILITY,
  INDEXER_SET_MODE_CAPABILITY,
  INDEXER_STATUS_CAPABILITY,
  parseActivityQuery,
  parseCopyAdjustRequest,
  parseIndexerSetModeRequest,
  parseExecutionReceipt,
  type ActivityQuery,
  type CopyAdjustRequest,
  type ExecutionReceipt,
  type IndexerSetModeRequest,
  type IntentEnvelope,
  type PaneSnapshot,
  type PlanStep,
  type ResolvedPlan,
} from './contracts';
import {
  DependencyScheduler,
  canControlTransition,
  canTransition,
  isFinal,
  summarizeRun,
  type RunOutcome,
  type RunSummary,
} from './executorState';
import {
  ownedJobIds,
  type ExecutionLedgerPort,
  type LedgerEntry,
  type Resolution,
} from './executorLedger';
import type { QueuePort } from './executorQueue';
import { DEFAULT_PREFS, evaluatePolicy, type PolicyPrefs } from './policy';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';
import {
  classifyProviderFailure,
  describeProviderError,
  endpointHostPort,
  type ProviderErrorView,
  type ProviderUiCode,
} from './providerErrors';
import { RemoteProviderError } from './remote-provider';
import {
  IntentHistory,
  MAX_REVISIONS,
  applyEdit,
  composeProviderText,
  describeEdit,
  mergeProviderIntent,
  parseFollowUp,
  providerNote,
} from './followup';

/** Pandangan registry capability yang dibutuhkan executor (dipasok oleh CapabilityRegistry). */
export interface CapabilityView {
  availability(id: string): Availability;
  describe(): Array<{ id: string; available: boolean }>;
  /** Tersedia pada registry terbaru; bila tidak ada, diturunkan dari describe(). */
  offeredCapabilityIds?(): readonly string[];
}

/** Cek terakhir di backend tepat sebelum menulis (ApprovalLedger Rust). Gagal = stale, nol penulisan. */
export interface ExecutionGuard {
  /**
   * Called when the preview is built, BEFORE it is shown: the backend fingerprints the sources and keeps
   * the approval slot. Failing here fails closed (no preview, nothing can be approved).
   */
  register?(
    ticket: ApprovalTicket,
    plan: ResolvedPlan,
  ): Promise<{ ok: true } | { ok: false; message: string }>;
  authorize(
    ticket: ApprovalTicket,
    plan: ResolvedPlan,
  ): Promise<{ ok: true } | { ok: false; message: string }>;
  /** The plan was edited, cancelled or finished: the slot no longer applies. */
  revoke?(ticket: ApprovalTicket): void;
  /**
   * FER-576 (review R1, S1): the opaque digest the BACKEND computed from its own fingerprints (size, time) of the
   * sources when `register` ran. Two registrations over the same unchanged sources give the same digest.
   */
  scopeDigestOf?(ticket: ApprovalTicket): string | undefined;
}

export interface ExecutorDeps {
  provider: IntentProvider;
  capture(requestId: string): PaneSnapshot;
  modelContext(snapshot: PaneSnapshot): ProviderPlanRequest['context'];
  facade: Pick<CommandFacade, 'prepare' | 'approve' | 'execute' | 'discard'>;
  capabilities: CapabilityView;
  ledger: ExecutionLedgerPort;
  queue: QueuePort;
  prefs?: () => PolicyPrefs;
  conflicts?: ConflictPort;
  conflictViews?: (plan: ResolvedPlan) => ConflictView[];
  guard?: ExecutionGuard;
  listItems?: (
    plan: ResolvedPlan,
    offset: number,
    limit: number,
  ) => Promise<{ rows: PlanItemRow[]; total: number }>;
  now?: () => number;
  newId?: (prefix: string) => string;
  /**
   * ACT-08: local activity history (read-only). Absent = `activity.query` is never offered (old callers
   * unchanged); present = offered only while `available()` and the read gate is ready.
   */
  activity?: ActivityQueryPort;
  /**
   * FER-471: running copy jobs + the one performance write of `copy.adjust`. Absent = never offered.
   * The write runs only after the user confirms on the card.
   */
  copyControl?: CopyControlPort;
  /**
   * FER-476: FerrumIndexer status (read) and mode change (write, only after the card's confirm).
   * Absent = never offered.
   */
  indexer?: IndexerControlPort;
  /**
   * FER-466: the Disk Usage workspace (read-only). Absent = `diskUsage.query` is never offered. The
   * answer is built here from a local snapshot and never sent to the provider.
   */
  diskUsage?: DiskUsageQueryPort;
  /**
   * FER-576: open a path the user typed in a pane. Absent = `pane.navigate` is never offered. Read-only:
   * nothing is executed or modified.
   */
  navigate?: NavigatePort;
  /**
   * FER-576: Test Archive for the "archive, then delete" gate. A chain deletes the original ONLY after this
   * says the archive passed; absent or failing = the delete step is never created (fail closed).
   */
  verifyArchive?: (
    outputPath: string,
    expected?: { members?: number },
  ) => Promise<{ ok: boolean; message: string }>;
  /** FER-465: endpoint provider (hanya host:port yang dipakai di pesan galat). */
  providerEndpoint?: () => string | undefined;
  /** FER-573: model yang dipakai (nama model bukan rahasia); hanya untuk ringkasan galat. */
  providerModel?: () => string | undefined;
  /** FER-465: dipanggil dengan kode aman saat provider gagal (mis. untuk indikator status header). */
  onProviderFailure?: (code: ProviderUiCode, kind: string) => void;
}

interface Run {
  runId: string;
  requestId: string;
  userText: string;
  state: RunState;
  /** Naik pada edit/cancel: hasil async dari epoch lama dibuang. */
  epoch: number;
  abort: AbortController;
  snapshot?: PaneSnapshot;
  prepared?: PreparedCommand;
  /** True setelah guard.revoke + facade.discard untuk `prepared` saat ini (idempoten). */
  preparedDropped: boolean;
  /** AI-09: riwayat intent immutable (revision 1 = permintaan awal) dan acuan plan terakhir yang berhasil disusun. */
  history?: IntentHistory;
  planId?: string;
  revision: number;
  approving: boolean;
  cancelRequested: boolean;
  executionId?: string;
  ledgerKey?: string;
  jobIds: string[];
  /** Job ID yang pernah terlihat di queue; sebelum itu dianggap queued (enqueue baru saja dikonfirmasi). */
  seen: Set<string>;
  unsub?: () => void;
  completedInProcess?: number;
  /** Sync (FER-168): items that never reached the queue / conflicts the plan skipped; shown honestly in the result. */
  failedToQueue: number;
  conflictsSkipped: number;
  /** "Periksa hasil" sedang berjalan: klik ganda tidak memicu rekonsiliasi kedua. */
  checking: boolean;
  /** ACT-08: jawaban riwayat lokal (hanya untuk "Buka di panel"; tidak pernah ke provider). */
  answer?: ActivityAnswer;
  /** FER-471: penyesuaian copy yang menunggu tombol konfirmasi (dari intent `copy.adjust`). */
  copyAdjust?: { operationId: string; request: CopyAdjustRequest; view: CopyAdjustView };
  /** FER-476: indexer mode change waiting for (or applying) the user's confirm. */
  indexerMode?: { request: IndexerSetModeRequest; view: IndexerModeView };
  /** FER-433: this run was started by the APP from a history card (selector `entity`), not by the model. */
  entity?: boolean;
  /** FER-471/476: this run is a control request (copy.adjust / indexer.*; allows its extra state edges only). */
  control?: boolean;
  /** FER-576: this run is one step of a multi-step plan; the rest waits here until this step succeeds. */
  chain?: ChainState;
  /** FER-576: app-built text for a partial result (e.g. which deletes failed). */
  partialNote?: string;
}

type ChainStep = ChainEnvelope['steps'][number];

/** FER-576: where a multi-step plan stands. Lives on the run that is currently executing a step. */
interface ChainState {
  /** Capability of the step this run executes. */
  capability: ChainStep['capability'];
  /** Ids the follow-up runs derive from. */
  baseRunId: string;
  /** Zero-based index of the step this run executes. */
  index: number;
  total: number;
  /** Steps after this one. Cleared by any failure, cancel or refusal: the rest never runs. */
  rest: ChainStep[];
  /** The request text of step 1 (a navigation path must be in it). */
  userText: string;
  /** Request id the step ids derive from. */
  baseRequestId: string;
  /** The previous step was `archive.create`: a delete step needs the archive verified and the same selection. */
  afterArchive: boolean;
  /** The chain contains a delete step (review R1: stricter archive, navigate and verification rules apply). */
  hasDelete: boolean;
  /** The chain starts with a navigation and deletes: the target must be a FILE and the selection exactly it. */
  requireFile: boolean;
  /** Where the navigation put the typed file; restored before the steps that follow the archive. */
  navTarget?: { side: 'left' | 'right'; target: Extract<NavigateTarget, { kind: 'file' }> };
  /** Backend digest of the archive step's sources (size, time) when its card was built. */
  archiveScopeDigest?: string;
  /** How many members the archive must hold (files + empty folders) for the original to be deleted. */
  archiveExpectedMembers?: number;
  /** Digest of the frozen selection the archive step used. */
  archiveSelectionDigest?: string;
  /** Test Archive passed for the archive this chain created. */
  verified?: boolean;
  /** App-built line shown on the card of this step. */
  note?: string;
}

/** FER-573: fixed text (never model text) shown when a size question arrives while Disk Usage is closed. */
export const DISK_USAGE_HINT_TEXT =
  'Ukuran file dan folder dihitung lewat Disk Usage. Buka Disk Usage (Alt+Shift+D) di panel itu, lalu tanya lagi.';
export const DISK_USAGE_ALREADY_OPEN_TEXT =
  'Disk Usage sudah terbuka. Tanyakan lagi tentang ukurannya.';
const FOLLOWUP_PROVIDER_ERROR =
  'Provider tidak dapat memproses perubahan; rencana sebelumnya tidak berlaku lagi. Ulangi perubahan atau ketik "batal".';
const RUNNING_MESSAGE =
  'Pekerjaan sudah antre atau berjalan. Perubahan tidak mengubah job yang aktif. ' +
  'Ketik "batal" untuk menghentikan job ini, lalu buat permintaan baru.';
const PRE_QUEUE: ReadonlySet<RunState> = new Set([
  'planning',
  'resolving',
  'preview',
  'awaitingApproval',
  'needsInput',
]);
/** Error facade yang berarti "tidak ada yang ditulis". */
const NOT_STARTED_CODES: ReadonlySet<CommandError['code']> = new Set([
  'stale',
  'target_changed',
  'expired',
  'invalid_handle',
  'not_approved',
  'blocked_capability',
  'resolve_failed',
  'invalid_plan',
  'unsupported_plan',
  'invalid_intent',
]);
/**
 * Sebab `execution_failed` yang berarti engine MENJAWAB gagal (bukan hasil tak pasti). Adapter hanya boleh
 * memberi sebab ini bila panggilan service berakhir dengan error pasti dan tidak membuat job antrean
 * (kontrak dengan adapters/archive.ts, FER-165). Label = teks aplikasi; pesan/path engine tidak ditampilkan.
 */
const ENGINE_FAILURE_CAUSES: Readonly<Record<string, string>> = {
  permissionDenied: 'akses ditolak',
  sourceUnreadable: 'file sumber tidak dapat dibaca (terkunci atau akses ditolak)',
  diskFull: 'ruang disk tujuan tidak cukup',
  notFound: 'file atau folder tidak ditemukan',
  invalidPath: 'path tidak valid',
  notADirectory: 'tujuan bukan folder',
  unsupported: 'operasi tidak didukung oleh engine',
  io: 'kesalahan baca/tulis disk',
  recycleRefused: 'backend menolak memindahkan item ke Recycle Bin (tidak ada yang dihapus)',
  recycleBypassed: 'Windows menghapus item secara permanen, bukan ke Recycle Bin',
};
export function engineFailureMessage(cause: string): string | undefined {
  const label = Object.hasOwn(ENGINE_FAILURE_CAUSES, cause)
    ? ENGINE_FAILURE_CAUSES[cause]
    : undefined;
  return label
    ? `Operasi gagal: ${label}. Tidak dijalankan ulang otomatis; periksa folder tujuan sebelum mencoba lagi.`
    : undefined;
}

export class AiExecutor implements ChatExecutor {
  private readonly runs = new Map<string, Run>();
  private readonly listeners = new Set<(u: RunUpdate) => void>();
  private readonly approvals: ApprovalManager;
  private readonly prefs: () => PolicyPrefs;
  private readonly now: () => number;
  private seq = 0;
  private readonly offQueue: () => void;

  constructor(private readonly deps: ExecutorDeps) {
    this.approvals = new ApprovalManager(deps.facade, { now: deps.now });
    this.prefs = deps.prefs ?? (() => DEFAULT_PREFS);
    this.now = deps.now ?? Date.now;
    this.offQueue = deps.queue.subscribe((p) => this.onProgress(p));
  }

  // ───────────────────────── ChatExecutor ─────────────────────────

  subscribe(listener: (update: RunUpdate) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  submit(req: { requestId: string; text: string; runId?: string }): void {
    const runId = req.runId ?? `run-${req.requestId}`;
    if (this.runs.has(runId)) return; // submit ganda: abaikan
    const run: Run = {
      runId,
      requestId: req.requestId,
      userText: req.text,
      state: 'planning',
      epoch: 0,
      abort: new AbortController(),
      preparedDropped: false,
      revision: 0,
      approving: false,
      cancelRequested: false,
      jobIds: [],
      seen: new Set(),
      failedToQueue: 0,
      conflictsSkipped: 0,
      checking: false,
    };
    this.runs.set(runId, run);
    void this.plan(run, run.epoch).catch(() =>
      this.fail(run, run.epoch, 'Terjadi kesalahan internal; tidak ada yang dijalankan.'),
    );
  }

  /**
   * FER-433 / ACT-08 R7 v2: "Buat ZIP" on a history answer card. The APP (not the model) builds an
   * `archive.create` envelope with selector `entity`; it is resolved locally at prepare time and then
   * follows the normal prepared plan + approval + backend guard path. Only an item that the source run
   * actually listed (and that is not missing/offline) can be chosen. Returns false when refused.
   */
  submitEntityArchive(req: {
    requestId: string;
    runId: string;
    sourceRunId: string;
    entityId: string;
  }): boolean {
    if (this.runs.has(req.runId)) return false;
    const source = this.runs.get(req.sourceRunId);
    const port = this.deps.activity;
    if (!source?.answer || source.state !== 'answered' || !port?.locate || !port.available())
      return false;
    const item = answerItems(source.answer).find((i) => i.id === req.entityId);
    if (!item || !isOpenable(item)) return false;
    const run: Run = {
      runId: req.runId,
      requestId: req.requestId,
      userText: `Buat ZIP dari riwayat: ${item.displayName}`,
      state: 'planning',
      epoch: 0,
      abort: new AbortController(),
      preparedDropped: false,
      revision: 0,
      approving: false,
      cancelRequested: false,
      jobIds: [],
      seen: new Set(),
      failedToQueue: 0,
      conflictsSkipped: 0,
      checking: false,
      entity: true,
    };
    this.runs.set(run.runId, run);
    void this.planEntityArchive(run, run.epoch, req.entityId).catch(() =>
      this.fail(run, run.epoch, 'Terjadi kesalahan internal; tidak ada yang dijalankan.'),
    );
    return true;
  }

  private async planEntityArchive(run: Run, epoch: number, entityId: string): Promise<void> {
    let snapshot: PaneSnapshot;
    try {
      snapshot = this.deps.capture(run.requestId);
    } catch {
      this.fail(run, epoch, 'Panel tidak tersedia; tidak ada yang dijalankan.');
      return;
    }
    run.snapshot = snapshot;
    const envelope = entityArchiveEnvelope(run.requestId, entityId);
    run.history = new IntentHistory(envelope);
    if (!this.emit(run, 'resolving', {})) return;
    await this.buildPlan(run, epoch, envelope);
  }

  approve(runId: string, shown?: { planDigest?: string; acknowledged?: boolean }): void {
    const run = this.runs.get(runId);
    if (!run || run.state !== 'awaitingApproval' || run.approving) return;
    if (run.indexerMode) {
      // FER-476: the click on the card IS the approval, but raising to high_performance also needs the
      // privacy tick. Without it nothing is consumed: the card can still be confirmed properly.
      if (run.indexerMode.view.needsAck && shown?.acknowledged !== true) return;
      run.approving = true;
      const epoch = run.epoch;
      void this.applyIndexerMode(run, epoch).catch(() =>
        this.fail(run, epoch, 'Terjadi kesalahan internal; mode tidak diubah.'),
      );
      return;
    }
    if (run.copyAdjust) {
      // FER-471: no plan, no ticket, no ledger: the click on the card IS the approval.
      run.approving = true;
      const epoch = run.epoch;
      void this.applyCopyAdjust(run, epoch).catch(() =>
        this.fail(run, epoch, 'Terjadi kesalahan internal; tidak ada yang diubah.'),
      );
      return;
    }
    const ticket = this.approvals.ticketOf(runId);
    if (!ticket) return;
    // UI boleh menyebut digest yang ia tampilkan; selisih = pratinjau usang.
    if (shown?.planDigest !== undefined && shown.planDigest !== ticket.planDigest) {
      this.setFinal(run, 'stale', {
        errorMessage: 'Pratinjau yang Anda setujui sudah usang; susun ulang.',
      });
      return;
    }
    run.approving = true;
    const epoch = run.epoch;
    void this.approveFlow(run, epoch).catch(() =>
      this.fail(run, epoch, 'Terjadi kesalahan internal saat eksekusi; periksa hasilnya.'),
    );
  }

  cancel(runId: string): void {
    const run = this.runs.get(runId);
    if (!run || isFinal(run.state)) return;
    if ((run.copyAdjust || run.indexerMode) && run.approving) return; // the confirmed change is already being applied
    if (PRE_QUEUE.has(run.state)) {
      this.dropPrepared(run);
      this.invalidate(run);
      this.emit(run, 'cancelled', {});
      return;
    }
    // Sudah menuju queue: hentikan jadwal langkah turunan, kirim cancel ke job milik run.
    run.cancelRequested = true;
    void this.cancelJobs(run);
  }

  edit(runId: string): void {
    const run = this.runs.get(runId);
    if (!run || isFinal(run.state) || !PRE_QUEUE.has(run.state)) return;
    this.dropPrepared(run);
    this.invalidate(run);
    run.approving = false;
    // Plan lama dan approval-nya tidak berlaku lagi; pengguna mengirim instruksi baru.
    this.emit(run, 'needsInput', {});
  }

  /**
   * AI-09 (FER-207..210): ubah rencana yang belum masuk queue. Keputusan penerimaan sinkron (UI langsung
   * menampilkan penolakan); penyusunan ulang berjalan async dengan epoch baru sehingga hasil terlambat dibuang.
   */
  followUp(runId: string, ref: FollowUpRef, text: string): FollowUpResult {
    const no = (
      code: Extract<FollowUpResult, { accepted: false }>['code'],
      message: string,
    ): FollowUpResult => ({
      accepted: false,
      code,
      message,
    });
    const run = this.runs.get(runId);
    if (!run) return no('unknown_run', 'Rencana tidak dikenal; mulai permintaan baru.');
    if (isFinal(run.state)) {
      return no(
        'final',
        `Rencana ini sudah berakhir (${run.state}); perubahan tidak diterapkan. Buat permintaan baru.`,
      );
    }
    const parsed = parseFollowUp(text);
    // "batal" selalu menargetkan run ini saja (job milik run lewat ledger), termasuk saat queued/running.
    if (parsed.kind === 'local' && parsed.edit.kind === 'cancel') {
      this.cancel(runId);
      return { accepted: true, revision: run.revision, local: true };
    }
    if (run.state === 'queued' || run.state === 'running') return no('running', RUNNING_MESSAGE);
    if (run.approving)
      return no('busy', 'Rencana sedang disetujui; perubahan ditunda. Tunggu hasilnya.');
    if (run.state === 'planning' || run.state === 'resolving') {
      return no('busy', 'Rencana sedang disusun; tunggu sampai siap, atau ketik "batal".');
    }
    const history = run.history;
    if (!history || run.planId === undefined)
      return no('no_plan', 'Belum ada rencana yang bisa diubah.');
    if (ref.planId !== run.planId || ref.revision !== run.revision) {
      return no(
        'stale_ref',
        `Rencana yang Anda maksud (revisi ${ref.revision}) sudah berganti; yang berlaku sekarang revisi ${run.revision}. Tinjau pratinjau terbaru.`,
      );
    }
    if (parsed.kind === 'rejected') return no('invalid_edit', parsed.message);
    if (history.length >= MAX_REVISIONS) {
      return no('limit', `Batas ${MAX_REVISIONS} revisi tercapai; buat permintaan baru.`);
    }
    const prev = history.current.envelope;
    if (prev.intent.capability === 'items.delete') {
      return no(
        'invalid_edit',
        'Penghapusan tidak bisa diubah lewat perubahan. Batalkan, pilih ulang item, lalu buat permintaan baru.',
      );
    }
    const next = history.revision + 1;
    if (parsed.kind === 'local') {
      const applied = applyEdit(prev, parsed.edit);
      if (!applied.ok) return no('invalid_edit', applied.message);
      this.beginRevision(run);
      this.emit(run, 'resolving', {});
      const epoch = run.epoch;
      void this.buildPlan(run, epoch, applied.envelope, {
        cause: 'local',
        note: describeEdit(parsed.edit),
      }).catch(() => this.failRevision(run, epoch));
      return { accepted: true, revision: next, local: true };
    }
    this.beginRevision(run);
    this.emit(run, 'planning', {});
    const epoch = run.epoch;
    void this.reviseViaProvider(run, epoch, prev, text).catch(() => this.failRevision(run, epoch));
    return { accepted: true, revision: next, local: false };
  }

  async listItems(
    runId: string,
    offset: number,
    limit: number,
  ): Promise<{ rows: PlanItemRow[]; total: number }> {
    const plan = this.runs.get(runId)?.prepared?.plan;
    if (!plan) return { rows: [], total: 0 };
    if (!this.deps.listItems) return { rows: [], total: plan.preview.itemCount };
    return this.deps.listItems(plan, offset, limit);
  }

  // ───────────────────────── pemulihan / rekonsiliasi ─────────────────────────

  /** Eksekusi dengan hasil enqueue tak pasti (mis. setelah restart). Tidak pernah di-replay. */
  recover(): Promise<LedgerEntry[]> {
    return this.deps.ledger.unresolved();
  }

  /** Rekonsiliasi eksplisit setelah pengguna/aplikasi memeriksa queue. */
  reconcile(key: string, resolution: Resolution): Promise<LedgerEntry> {
    return this.deps.ledger.resolveUnknown(key, resolution);
  }

  /**
   * LIMIT-4: aksi "Periksa hasil" pada kartu unknownOutcome. Membaca ledger backend untuk execution run ini,
   * merekonsiliasi entri yang belum pasti (`reconcile`) dan, bila ada job milik run, mengambil status dari queue.
   * Tidak pernah menjalankan ulang apa pun; status baru hanya dari ledger/queue.
   */
  async checkResult(runId: string): Promise<CheckResult> {
    const run = this.runs.get(runId);
    if (!run) {
      return {
        ok: false,
        message:
          'Hasil tidak dapat diperiksa dari sesi ini (asisten dimuat ulang). Periksa folder tujuan dan panel Operations.',
      };
    }
    if (run.state !== 'unknownOutcome')
      return { ok: false, message: 'Hanya hasil yang belum pasti yang perlu diperiksa.' };
    if (run.checking) return { ok: false, message: 'Pemeriksaan sedang berjalan.' };
    run.checking = true;
    try {
      return await this.checkOnce(run);
    } catch {
      return {
        ok: false,
        message: 'Ledger eksekusi tidak dapat dibaca; hasil tetap belum pasti. Coba lagi nanti.',
      };
    } finally {
      run.checking = false;
    }
  }

  private async checkOnce(run: Run): Promise<CheckResult> {
    let entry = run.executionId ? await this.deps.ledger.byExecution(run.executionId) : undefined;
    if (!entry) {
      return {
        ok: true,
        state: run.state,
        message:
          'Tidak ada catatan eksekusi untuk rencana ini; tidak ada yang dijalankan ulang. Periksa folder tujuan.',
      };
    }
    if (entry.state === 'reserved') {
      // markUnknown gagal tersimpan saat eksekusi: rapikan dulu agar bisa direkonsiliasi.
      await this.deps.ledger.markUnknown(entry.key);
      entry = { ...entry, state: 'unknownOutcome' };
    }
    if (entry.state === 'unknownOutcome') {
      // Job ID hanya diketahui dari receipt. Tanpa receipt, executor tidak pernah menebak job lain di antrean.
      entry = await this.reconcile(
        entry.key,
        run.jobIds.length > 0 ? { kind: 'found', jobIds: [...run.jobIds] } : { kind: 'notFound' },
      );
    }
    run.ledgerKey = entry.key;
    if (entry.state === 'enqueued' && entry.jobIds.length > 0) {
      run.jobIds = [...entry.jobIds];
      const summary = summarizeRun(run.jobIds, (id) => this.lookup(run, id));
      if (summary.outcome !== 'unknownOutcome') {
        // unknownOutcome tidak pernah kembali ke queued (replay); job yang masih antre tampil sebagai running.
        this.finish(
          run,
          summary.outcome === 'queued' ? { ...summary, outcome: 'running' } : summary,
          {},
        );
        return { ok: true, state: run.state, message: 'Status diambil dari antrean operasi.' };
      }
      return {
        ok: true,
        state: run.state,
        message:
          'Job milik rencana ini sudah tidak ada di antrean; status akhirnya tidak dapat dibaca. Periksa folder tujuan.',
      };
    }
    return {
      ok: true,
      state: run.state,
      message:
        'Tidak ada job antrean yang tercatat untuk eksekusi ini, dan Ferrum tidak akan menjalankannya ulang. ' +
        'Periksa folder tujuan; bila hasilnya tidak ada, buat permintaan baru.',
    };
  }

  /** Status dari queue untuk execution yang sudah dipetakan di ledger. */
  async summaryOf(executionId: string): Promise<RunSummary> {
    const ids = await ownedJobIds(this.deps.ledger, executionId);
    return summarizeRun(ids, (id) => this.deps.queue.get(id));
  }

  dispose(): void {
    this.offQueue();
    for (const run of this.runs.values()) run.unsub?.();
    this.listeners.clear();
  }

  // ───────────────────────── perencanaan ─────────────────────────

  private alive(run: Run, epoch: number): boolean {
    return run.epoch === epoch && !isFinal(run.state);
  }

  private offered(): readonly string[] {
    const c = this.deps.capabilities;
    const base =
      c.offeredCapabilityIds?.() ??
      c
        .describe()
        .filter((e) => e.available)
        .map((e) => e.id);
    const activity = this.deps.activity;
    const read = activity
      ? offeredReadCapabilityIds({ activityAvailable: safeAvailable(activity) })
      : [];
    const copy = this.deps.copyControl;
    const control = copy ? offeredControlCapabilityIds({ copyRunning: safeCopyRunning(copy) }) : [];
    const duPort = this.deps.diskUsage;
    const diskUsage = duPort
      ? offeredReadCapabilityIds({
          activityAvailable: false,
          diskUsageAvailable: safeDiskUsageAvailable(duPort),
        })
      : [];
    const indexerPort = this.deps.indexer;
    const indexer = indexerPort
      ? offeredIndexerCapabilityIds({ desktopAvailable: safeIndexerAvailable(indexerPort) })
      : [];
    const nav =
      this.deps.navigate && safeNavigateAvailable(this.deps.navigate)
        ? [PANE_NAVIGATE_CAPABILITY]
        : [];
    // FER-573: while Disk Usage is NOT open the model may only point the user to it (never read sizes).
    const hint = duPort && !safeDiskUsageAvailable(duPort) ? [UI_HINT_CAPABILITY] : [];
    return read.length ||
      control.length ||
      indexer.length ||
      diskUsage.length ||
      nav.length ||
      hint.length
      ? [...base, ...read, ...control, ...indexer, ...diskUsage, ...nav, ...hint]
      : base;
  }

  /**
   * FER-471: cabang kontrol (bukan plan). Tidak memanggil facade/policy/guard/ledger/queue. Pekerjaan
   * copy yang dituju dipilih aplikasi (bukan model); kartu konfirmasi menampilkan perubahannya, dan
   * penulisan hanya terjadi setelah pengguna menekan konfirmasi.
   */
  private async proposeCopyAdjust(run: Run, epoch: number, raw: CopyAdjustRequest): Promise<void> {
    let request: CopyAdjustRequest;
    try {
      request = parseCopyAdjustRequest(raw);
    } catch {
      this.fail(run, epoch, 'Permintaan penyesuaian copy tidak valid; tidak ada yang diubah.');
      return;
    }
    run.control = true;
    const port = this.deps.copyControl;
    const unavailable = 'Tidak ada copy yang sedang berjalan; tidak ada yang diubah.';
    if (!port || !this.offered().includes(COPY_ADJUST_CAPABILITY)) {
      this.emit(run, 'blockedCapability', { blockedReason: unavailable });
      return;
    }
    let jobs: CopyControlState[];
    try {
      jobs = (await port.running()).filter((j) => j.running);
    } catch {
      jobs = [];
    }
    if (!this.alive(run, epoch)) return; // dibatalkan saat membaca job
    const target = jobs.at(-1); // satu job berat sekali waktu; bila ada beberapa, yang terbaru
    if (!target) {
      this.emit(run, 'blockedCapability', { blockedReason: unavailable });
      return;
    }
    const before = settingsOf(target);
    const after = previewCopyAdjust(before, request);
    const view: CopyAdjustView = {
      summary: describeCopyAdjust(request, before, after),
      job: describeJob(target),
      before,
      after,
    };
    if (sameSettings(before, after)) {
      // Nothing would change: no approval needed, nothing is written.
      this.setFinal(run, 'succeeded', {
        copyAdjust: {
          ...view,
          result: { ok: true, message: 'Tidak ada perubahan: pengaturan sudah seperti itu.' },
        },
      });
      return;
    }
    run.copyAdjust = { operationId: target.operationId, request, view };
    this.emit(run, 'awaitingApproval', { copyAdjust: view });
  }

  private async applyCopyAdjust(run: Run, epoch: number): Promise<void> {
    const pending = run.copyAdjust;
    const port = this.deps.copyControl;
    if (!pending || !port) return;
    let state = null;
    try {
      state = await port.adjust(
        pending.operationId,
        toConfirmedChange(pending.request, pending.view.after),
      );
    } catch {
      state = null;
    }
    if (!this.alive(run, epoch)) return;
    if (!state) {
      this.setFinal(run, 'failed', {
        errorMessage: 'Copy itu sudah selesai atau tidak lagi berjalan; tidak ada yang diubah.',
        copyAdjust: {
          ...pending.view,
          result: { ok: false, message: 'Copy sudah selesai; tidak ada yang diubah.' },
        },
      });
      return;
    }
    const applied = settingsOf(state);
    this.setFinal(run, 'succeeded', {
      copyAdjust: {
        ...pending.view,
        result: { ok: true, message: 'Diterapkan ke copy yang sedang berjalan.', applied },
      },
    });
  }

  /**
   * FER-476: `indexer.status` — read-only. Reads the live status, builds the answer from it (app text
   * only; no path) and ends `succeeded`. No facade/policy/guard/ledger/queue, no second provider turn.
   */
  private async answerIndexerStatus(run: Run, epoch: number): Promise<void> {
    run.control = true;
    const port = this.deps.indexer;
    if (!port || !this.offered().includes(INDEXER_STATUS_CAPABILITY)) {
      this.emit(run, 'blockedCapability', {
        blockedReason: 'Status FerrumIndexer hanya tersedia di aplikasi desktop Ferrum.',
      });
      return;
    }
    let view: IndexerView;
    try {
      view = describeIndexerStatus(await port.status());
    } catch {
      this.fail(run, epoch, 'Status FerrumIndexer tidak dapat dibaca; tidak ada yang diubah.');
      return;
    }
    if (!this.alive(run, epoch)) return; // dibatalkan saat membaca status
    this.setFinal(run, 'succeeded', { indexer: view });
  }

  /**
   * FER-476: `indexer.setMode` — ALWAYS behind the confirmation card. The model only picks the target
   * mode; the live status decides whether anything is left to do, the card text is app-built, and the
   * write (`setMode(mode, true)`, UAC on the backend when raising) happens only after the click.
   */
  private async proposeIndexerMode(
    run: Run,
    epoch: number,
    raw: IndexerSetModeRequest,
  ): Promise<void> {
    let request: IndexerSetModeRequest;
    try {
      request = parseIndexerSetModeRequest(raw);
    } catch {
      this.fail(run, epoch, 'Permintaan mode FerrumIndexer tidak valid; mode tidak diubah.');
      return;
    }
    run.control = true;
    const port = this.deps.indexer;
    if (!port || !this.offered().includes(INDEXER_SET_MODE_CAPABILITY)) {
      this.emit(run, 'blockedCapability', {
        blockedReason: 'Mode FerrumIndexer hanya bisa diubah dari aplikasi desktop Ferrum.',
      });
      return;
    }
    let status;
    try {
      status = await port.status();
    } catch {
      this.fail(run, epoch, 'Status FerrumIndexer tidak dapat dibaca; mode tidak diubah.');
      return;
    }
    if (!this.alive(run, epoch)) return; // dibatalkan saat membaca status
    const state = readiness(status);
    const view = describeModeChange(request.mode, status.mode, status.canLowerPrivacy);
    if (state === 'not_installed' || state === 'stopped') {
      // Nothing to switch: be honest, no confirm card, nothing written.
      const message = state === 'not_installed' ? NOT_INSTALLED_FOR_CHANGE : NOT_RUNNING_FOR_CHANGE;
      this.setFinal(run, 'failed', {
        errorMessage: message,
        indexer: {
          ...view,
          needsAck: false,
          result: {
            ok: false,
            code: state === 'stopped' ? 'not_running' : 'not_installed',
            message,
          },
        },
      });
      return;
    }
    if (status.mode === request.mode) {
      // Already there: nothing is executed and no approval is needed.
      this.setFinal(run, 'succeeded', { indexer: alreadyInMode(view) });
      return;
    }
    run.indexerMode = { request, view };
    this.emit(run, 'awaitingApproval', { indexer: view });
  }

  private async applyIndexerMode(run: Run, epoch: number): Promise<void> {
    const pending = run.indexerMode;
    const port = this.deps.indexer;
    if (!pending || !port) return;
    let outcome: { ok: true; message: string } | { ok: false; message: string; code: string };
    try {
      outcome = modeChangeSuccess(await port.setMode(pending.request.mode, true));
    } catch (error) {
      outcome = { ok: false, ...modeChangeFailure(error) };
    }
    if (!this.alive(run, epoch)) return;
    const view: IndexerModeView = { ...pending.view, needsAck: false, result: outcome };
    if (outcome.ok) this.setFinal(run, 'succeeded', { indexer: view });
    else this.setFinal(run, 'failed', { errorMessage: outcome.message, indexer: view });
  }

  /**
   * ACT-08 R5: cabang non-mutasi. Tidak memanggil facade, policy, guard, ledger atau queue. Hasil query
   * hanya di-emit ke UI (`answered`); tidak ada giliran provider kedua.
   */
  private async answer(run: Run, epoch: number, raw: ActivityQuery): Promise<void> {
    const port = this.deps.activity;
    let request: ActivityQuery;
    try {
      const parsed = parseActivityQuery(raw);
      request =
        parsed.limit === undefined
          ? { query: parsed.query }
          : { query: parsed.query, limit: parsed.limit };
    } catch {
      this.finishAnswer(run, epoch, { status: 'rejected', reason: 'invalid query' }, undefined);
      return;
    }
    const summary = describeQuery(request);
    // Ditawarkan saat submit, tetapi layanan bisa hilang sesudahnya: jawab jujur, jangan menebak.
    if (!port || !this.offered().includes(ACTIVITY_QUERY_CAPABILITY)) {
      this.finishAnswer(run, epoch, { status: 'unavailable' }, summary);
      return;
    }
    let answer: ActivityAnswer;
    try {
      const value = await port.query(request);
      if (!this.alive(run, epoch)) return; // dibatalkan saat query berjalan: hasil dibuang
      try {
        answer = parseActivityAnswer(value);
      } catch {
        this.fail(run, epoch, 'Data riwayat aktivitas tidak valid; tidak ada yang ditampilkan.');
        return;
      }
    } catch (error) {
      answer = answerForError(error);
    }
    this.finishAnswer(run, epoch, answer, summary);
  }

  /**
   * FER-466: `diskUsage.query` — read-only. The question runs locally against the Disk Usage snapshot
   * on screen; the answer is app-built and ends the run `answered`. Nothing from the scan goes into
   * a provider request: no facade/policy/guard/ledger/queue, no second provider turn.
   */
  private async answerDiskUsage(run: Run, epoch: number, raw: DiskUsageQueryKind): Promise<void> {
    let query: DiskUsageQueryKind;
    try {
      query = parseDiskUsageQueryKind(raw);
    } catch {
      this.fail(run, epoch, 'Pertanyaan Disk Usage tidak valid; tidak ada yang dibaca.');
      return;
    }
    const summary = describeDiskUsageQuery(query);
    const port = this.deps.diskUsage;
    let answer: DiskUsageAnswer;
    if (!port || !this.offered().includes(DISK_USAGE_QUERY_CAPABILITY)) {
      // Offered at submit, but the workspace can be gone by now: answer honestly, do not guess.
      answer = { status: 'noScan', reason: 'none' };
    } else {
      answer = await runDiskUsageQuery(port, query);
    }
    if (!this.alive(run, epoch)) return; // dibatalkan saat query berjalan: hasil dibuang
    this.setFinal(run, 'answered', { diskUsage: answer, diskUsageQuery: summary });
  }

  private finishAnswer(
    run: Run,
    epoch: number,
    answer: ActivityAnswer,
    summary: string | undefined,
  ): void {
    if (!this.alive(run, epoch)) return;
    run.answer = answer;
    this.setFinal(run, 'answered', { activity: answer, activityQuery: summary });
  }

  /**
   * ACT-08 R7 v1: lokasi lokal satu item yang DITAMPILKAN di jawaban run ini (untuk "Buka di panel").
   * Item lain, item missing/offline, atau run tanpa jawaban -> null. Read-only; path tidak keluar renderer.
   */
  async locateActivityItem(runId: string, entityId: string): Promise<ActivityLocation | null> {
    const run = this.runs.get(runId);
    const locate = this.deps.activity?.locate?.bind(this.deps.activity);
    if (!run?.answer || run.state !== 'answered' || !locate) return null;
    const item = answerItems(run.answer).find((i) => i.id === entityId);
    if (!item || !isOpenable(item)) return null;
    let location: ActivityLocation | null;
    try {
      location = parseActivityLocation(await locate(entityId));
    } catch {
      return null;
    }
    return location && location.kind === item.kind ? location : null;
  }

  private async plan(run: Run, epoch: number): Promise<void> {
    let snapshot: PaneSnapshot;
    try {
      snapshot = this.deps.capture(run.requestId);
    } catch {
      this.fail(run, epoch, 'Panel tidak tersedia; tidak ada yang dijalankan.');
      return;
    }
    run.snapshot = snapshot;
    const request: ProviderPlanRequest & { capabilities: readonly string[] } = {
      requestId: run.requestId,
      userText: run.userText,
      context: this.deps.modelContext(snapshot),
      capabilities: this.offered(),
      signal: run.abort.signal,
    };
    let event: ProviderEvent | undefined;
    try {
      for await (const ev of this.deps.provider.plan(request)) {
        event = ev;
        break;
      }
    } catch (error) {
      if (this.alive(run, epoch)) this.failProvider(run, epoch, error);
      return;
    }
    if (!this.alive(run, epoch)) return; // respons terlambat setelah cancel/edit
    if (!event) {
      this.failProvider(run, epoch, new RemoteProviderError('malformed', 'Empty provider output'));
      return;
    }
    if (event.kind === 'question') {
      this.emit(run, 'needsInput', { modelText: event.text });
      return;
    }
    if (event.kind === 'activityQuery') {
      await this.answer(run, epoch, event.request);
      return;
    }
    if (event.kind === 'diskUsageQuery') {
      await this.answerDiskUsage(run, epoch, event.query);
      return;
    }
    if (event.kind === 'hint') {
      this.showHint(run, epoch);
      return;
    }
    if (event.kind === 'copyAdjust') {
      await this.proposeCopyAdjust(run, epoch, event.request);
      return;
    }
    if (event.kind === 'indexerStatus') {
      await this.answerIndexerStatus(run, epoch);
      return;
    }
    if (event.kind === 'indexerSetMode') {
      await this.proposeIndexerMode(run, epoch, event.request);
      return;
    }
    if (event.kind === 'paneNavigate') {
      await this.runNavigate(run, epoch, event.pane, event.path);
      return;
    }
    if (event.kind === 'chain') {
      await this.startChain(run, epoch, event.steps);
      return;
    }
    run.history = new IntentHistory(event.intent);
    if (!this.emit(run, 'resolving', {})) return;
    await this.buildPlan(run, epoch, event.intent);
  }

  /** Prepare + policy + preview untuk satu intent (permintaan awal atau revision baru). */
  private async buildPlan(
    run: Run,
    epoch: number,
    envelope: IntentEnvelope,
    pending?: { cause: 'local' | 'provider'; note: string },
  ): Promise<void> {
    const snapshot = run.snapshot!;
    if (
      envelope.intent.capability === 'items.delete' &&
      !userAskedToDelete(run.chain?.userText ?? run.userText)
    ) {
      // Review R1 S5: a delete card exists only for a request that asks to delete.
      this.emit(run, 'needsInput', {
        errorMessage: 'Permintaan Anda tidak meminta menghapus, jadi tidak ada kartu hapus.',
      });
      return;
    }
    const avail = this.deps.capabilities.availability(envelope.intent.capability);
    if (!avail.available) {
      this.emit(run, 'blockedCapability', { blockedReason: avail.reason });
      return;
    }
    const prepared = await this.deps.facade.prepare(
      envelope,
      { snapshot, ...(run.chain?.hasDelete ? { needCensus: true } : {}) },
      run.entity ? { allowEntitySelector: true } : {},
    );
    if (!this.alive(run, epoch)) {
      if (prepared.ok) this.deps.facade.discard(prepared.value.handle.handleId);
      return;
    }
    if (!prepared.ok) {
      this.mapPrepareError(run, prepared.error);
      return;
    }
    run.prepared = prepared.value;
    run.preparedDropped = false;
    const plan = prepared.value.plan;
    const chainRefusal = this.chainGate(run, plan.capability, prepared.value.preview);
    if (chainRefusal) {
      this.dropPrepared(run);
      this.setFinal(run, 'stale', { errorMessage: chainRefusal });
      return;
    }
    // Revision baru hanya tercatat bila prepare berhasil: perubahan yang gagal tidak "menempel".
    if (pending) run.history!.append(envelope, pending.cause, pending.note);
    run.planId = plan.planId;
    run.revision = run.history!.revision;
    const decision = evaluatePolicy({ plan, availability: avail, prefs: this.prefs() });
    if (decision.kind === 'blocked') {
      this.emit(run, 'blockedCapability', { blockedReason: decision.reason });
      return;
    }
    const ticket = this.approvals.present(run.runId, prepared.value);
    const registered = await this.registerGuard(ticket, plan);
    if (!this.alive(run, epoch)) {
      this.deps.guard?.revoke?.(ticket);
      this.deps.facade.discard(prepared.value.handle.handleId);
      return;
    }
    if (!registered.ok) {
      this.fail(run, epoch, registered.message);
      return;
    }
    const scopeRefusal = this.chainScopeGate(run, plan.capability, ticket);
    if (scopeRefusal) {
      this.deps.guard?.revoke?.(ticket);
      this.dropPrepared(run);
      this.setFinal(run, 'stale', { errorMessage: scopeRefusal });
      return;
    }
    const conflicts = this.deps.conflictViews?.(plan) ?? [];
    const scope = {
      scopeNote: prepared.value.preview.summary,
      warnings: prepared.value.preview.warnings,
      revision: run.revision,
      revisionNotes: run
        .history!.entries()
        .slice(1)
        .map((e) => `Revisi ${e.revision}: ${e.note}`),
    };
    this.emit(run, 'preview', { plan, conflicts, modelText: undefined, ...scope });
    if (decision.kind === 'autoApproved') {
      const approved = this.approvals.autoApprove(run.runId, decision);
      if (!approved.ok) {
        this.fail(run, epoch, approved.error.message);
        return;
      }
      run.approving = true;
      await this.execute(run, epoch, approved.value);
      return;
    }
    this.emit(run, 'awaitingApproval', { plan, conflicts, ...scope });
  }

  /** Cabut plan/approval lama dan siapkan epoch baru untuk revision (AI-09). */
  private beginRevision(run: Run): void {
    this.dropPrepared(run);
    run.prepared = undefined;
    this.invalidate(run);
    run.abort = new AbortController(); // invalidate() membatalkan sinyal lama; revision butuh sinyal baru
    run.approving = false;
  }

  private async reviseViaProvider(
    run: Run,
    epoch: number,
    prev: IntentEnvelope,
    text: string,
  ): Promise<void> {
    const request: ProviderPlanRequest & { capabilities: readonly string[] } = {
      requestId: run.requestId,
      userText: composeProviderText(prev, text),
      context: this.deps.modelContext(run.snapshot!),
      capabilities: this.offered(),
      signal: run.abort.signal,
    };
    let event: ProviderEvent | undefined;
    try {
      for await (const ev of this.deps.provider.plan(request)) {
        event = ev;
        break;
      }
    } catch (error) {
      if (this.alive(run, epoch)) {
        const view = this.providerView(error);
        this.emit(run, 'needsInput', {
          errorMessage: `${view.message} Rencana sebelumnya tidak berlaku lagi; ulangi perubahan atau ketik "batal".`,
          providerError: view,
        });
      }
      return;
    }
    if (!this.alive(run, epoch)) return; // respons terlambat setelah cancel/edit/revision lain
    if (!event) {
      this.emit(run, 'needsInput', { errorMessage: FOLLOWUP_PROVIDER_ERROR });
      return;
    }
    if (event.kind === 'question') {
      this.emit(run, 'needsInput', { modelText: event.text });
      return;
    }
    if (
      event.kind === 'activityQuery' ||
      event.kind === 'copyAdjust' ||
      event.kind === 'indexerStatus' ||
      event.kind === 'indexerSetMode' ||
      event.kind === 'diskUsageQuery' ||
      event.kind === 'hint' ||
      event.kind === 'paneNavigate' ||
      event.kind === 'chain'
    ) {
      // Pertanyaan riwayat / penyesuaian copy bukan perubahan rencana: rencana tidak diubah.
      this.emit(run, 'needsInput', {
        errorMessage: 'Jawaban model untuk perubahan tidak valid; rencana tidak diubah.',
      });
      return;
    }
    const merged = mergeProviderIntent(prev, event.intent);
    if (!merged.ok) {
      this.emit(run, 'needsInput', {
        errorMessage: 'Jawaban model untuk perubahan tidak valid; rencana tidak diubah.',
      });
      return;
    }
    if (!this.emit(run, 'resolving', {})) return;
    await this.buildPlan(run, epoch, merged.envelope, {
      cause: 'provider',
      note: providerNote(text),
    });
  }

  private failRevision(run: Run, epoch: number): void {
    if (!this.alive(run, epoch)) return;
    this.dropPrepared(run);
    this.emit(run, 'needsInput', {
      errorMessage: 'Terjadi kesalahan internal saat mengubah rencana; tidak ada yang dijalankan.',
    });
  }

  private mapPrepareError(run: Run, error: CommandError): void {
    switch (error.code) {
      case 'blocked_capability':
        this.emit(run, 'blockedCapability', { blockedReason: error.message });
        return;
      case 'resolve_failed':
        if (error.cause === 'stale_context')
          this.emit(run, 'stale', { errorMessage: error.message });
        else this.emit(run, 'needsInput', { errorMessage: error.message });
        return;
      case 'stale':
        this.emit(run, 'stale', { errorMessage: error.message });
        return;
      default:
        this.emit(run, 'failed', { errorMessage: error.message });
    }
  }

  // ───────────────────────── persetujuan + eksekusi ─────────────────────────

  private async approveFlow(run: Run, epoch: number): Promise<void> {
    const plan = run.prepared!.plan;
    let ticket = this.approvals.ticketOf(run.runId)!;
    const views = this.deps.conflictViews?.(plan) ?? [];
    if (views.length > 0 && this.deps.conflicts) {
      // Satu keputusan konflik untuk seluruh batch (dialog konflik existing).
      const decision = await decideConflicts(this.deps.conflicts, views);
      if (!this.alive(run, epoch)) return;
      if (decision === 'cancel') {
        this.dropPrepared(run);
        this.emit(run, 'cancelled', {
          errorMessage: 'Keputusan konflik dibatalkan; tidak ada yang dijalankan.',
        });
        return;
      }
      ticket = this.approvals.present(run.runId, run.prepared!, { conflictDecision: decision });
      const registered = await this.registerGuard(ticket, plan);
      if (!this.alive(run, epoch)) {
        this.deps.guard?.revoke?.(ticket);
        return;
      }
      if (!registered.ok) {
        this.dropPrepared(run);
        this.setFinal(run, 'failed', { errorMessage: registered.message });
        return;
      }
    }
    const approved = this.approvals.approve(run.runId, { bindingDigest: ticket.bindingDigest });
    if (!approved.ok) {
      this.dropPrepared(run);
      this.setFinal(
        run,
        approved.error.code === 'expired' || approved.error.code === 'stale' ? 'stale' : 'failed',
        {
          errorMessage: approved.error.message,
        },
      );
      return;
    }
    await this.execute(run, epoch, approved.value);
  }

  private async execute(run: Run, epoch: number, approved: ApprovedPreparedCommand): Promise<void> {
    const plan = run.prepared!.plan;
    if (!this.emit(run, 'queued', { plan })) return;
    const executionId = this.newId('exe');
    run.executionId = executionId;

    // 1. Catat niat di ledger backend SEBELUM menulis apa pun.
    let reservation;
    try {
      reservation = await this.deps.ledger.reserve(executionId, plan.planId, plan.previewDigest);
    } catch {
      this.setFinal(run, 'failed', {
        errorMessage: 'Ledger eksekusi tidak tersedia; tidak ada yang dijalankan.',
      });
      return;
    }
    if (reservation.kind === 'existing') {
      await this.adoptExisting(run, reservation.entry);
      return;
    }
    const key = reservation.key;
    run.ledgerKey = key;
    const notStarted = async (to: RunState, extra: Partial<RunUpdate>) => {
      await this.deps.ledger.recordNotStarted(key).catch(() => undefined);
      this.dropPrepared(run);
      this.setFinal(run, to, extra);
    };
    if (!this.alive(run, epoch) || run.cancelRequested) {
      await notStarted('cancelled', {});
      return;
    }

    // 2. Revalidasi backend (approval ledger Rust): stale = nol penulisan.
    const ticket = this.approvals.ticketOf(run.runId);
    if (this.deps.guard && ticket) {
      let verdict;
      try {
        verdict = await this.deps.guard.authorize(ticket, plan);
      } catch {
        verdict = {
          ok: false as const,
          message: 'Validasi backend gagal; tidak ada yang ditulis.',
        };
      }
      if (!verdict.ok) {
        await notStarted('stale', { errorMessage: verdict.message });
        return;
      }
      if (run.cancelRequested) {
        await notStarted('cancelled', {});
        return;
      }
    }

    // 3. Langkah plan menurut dependensi; operasi tulis hanya lewat facade.
    const scheduler = new DependencyScheduler(plan.steps);
    let finalResult: Awaited<ReturnType<CommandFacade['execute']>> | undefined;
    for (let ready = scheduler.ready(); ready.length > 0; ready = scheduler.ready()) {
      const step = ready[0];
      scheduler.start(step.id);
      const out = await this.runStep(step, plan, approved);
      if (!out.ok) {
        scheduler.fail(step.id);
        finalResult = out;
        break;
      }
      scheduler.complete(step.id);
      finalResult ??= out;
      if (run.cancelRequested) scheduler.cancel();
    }
    if (!finalResult) {
      await notStarted('failed', {
        errorMessage: 'Rencana tidak memiliki langkah yang bisa dijalankan.',
      });
      return;
    }
    if (!finalResult.ok) {
      const e = finalResult.error;
      if (NOT_STARTED_CODES.has(e.code)) {
        await notStarted(
          e.code === 'blocked_capability'
            ? 'blockedCapability'
            : e.code === 'resolve_failed'
              ? 'failed'
              : 'stale',
          e.code === 'blocked_capability'
            ? { blockedReason: e.message }
            : { errorMessage: e.message },
        );
        return;
      }
      const engineMessage =
        e.code === 'execution_failed' && e.cause ? engineFailureMessage(e.cause) : undefined;
      if (engineMessage) {
        // LIMIT-5: engine menjawab gagal dengan sebab pasti. Ledger ditutup (tanpa job) agar plan ini tidak
        // pernah di-replay; status = failed beserta sebabnya, bukan unknownOutcome.
        await this.deps.ledger.recordEnqueued(key, []).catch(() => undefined);
        this.dropPrepared(run);
        this.setFinal(run, 'failed', { errorMessage: engineMessage });
        return;
      }
      // execution_failed tanpa sebab: service bisa saja sudah menulis/enqueue sebagian. Jangan replay.
      await this.deps.ledger.markUnknown(key).catch(() => undefined);
      this.dropPrepared(run);
      this.setFinal(run, 'unknownOutcome', {
        errorMessage:
          'Hasil operasi tidak dapat dipastikan. Periksa hasilnya sebelum menjalankan ulang.',
      });
      return;
    }

    // 4. Receipt -> ledger (peta key -> job). Gagal menyimpan = tetap dilacak di memori.
    const receipt = finalResult.value;
    run.jobIds = [...receipt.jobIds];
    run.failedToQueue = receipt.failedToQueue ?? 0;
    run.conflictsSkipped = receipt.conflictsSkipped;
    let mapped = true;
    try {
      await this.deps.ledger.recordEnqueued(key, run.jobIds);
    } catch {
      mapped = false;
    }
    this.dropPrepared(run);
    run.partialNote = receipt.failureNote;
    if (receipt.state === 'completed' && run.chain?.rest[0]?.capability === 'items.delete') {
      // FER-576: "archive, then delete": the delete step exists ONLY if the archive passes Test Archive.
      const verdict = await this.verifyChainArchive(
        receipt.outputPath,
        run.chain.archiveExpectedMembers,
      );
      if (!verdict.ok) {
        run.chain.rest = [];
        this.setFinal(run, 'failed', { errorMessage: verdict.message });
        return;
      }
      run.chain.verified = true;
      run.chain.note = verdict.message;
      if (run.cancelRequested) run.chain.rest = [];
    }
    if (receipt.state === 'completed') {
      // Service selesai in-process (mis. Pack): hasil engine = hasil run.
      run.completedInProcess = receipt.completed;
      this.finish(
        run,
        { outcome: 'completed', completedFiles: receipt.completed, failed: 0 },
        {
          errorMessage: mapped ? undefined : 'Pemetaan receipt tidak tersimpan.',
        },
      );
      return;
    }
    run.unsub = undefined;
    this.evaluate(
      run,
      mapped ? {} : { errorMessage: 'Pemetaan receipt tidak tersimpan; jangan jalankan ulang.' },
    );
    if (run.cancelRequested) void this.cancelJobs(run);
  }

  /** Default: facade menjalankan seluruh plan pada langkah pertama; langkah lain butuh runner khusus. */
  private runStep(
    step: PlanStep,
    plan: ResolvedPlan,
    approved: ApprovedPreparedCommand,
  ): ReturnType<CommandFacade['execute']> {
    if (step.id !== plan.steps[0].id) {
      return Promise.resolve(
        err('unsupported_plan', 'Langkah turunan belum didukung oleh executor; tidak dijalankan.'),
      );
    }
    return this.deps.facade.execute(approved);
  }

  private async adoptExisting(run: Run, entry: LedgerEntry): Promise<void> {
    // Execution/plan yang sama sudah pernah dicatat: JANGAN menulis lagi.
    this.dropPrepared(run);
    run.executionId = entry.executionId;
    run.ledgerKey = entry.key;
    if (entry.state === 'enqueued') {
      run.jobIds = [...entry.jobIds];
      this.evaluate(run, {});
      return;
    }
    this.setFinal(run, 'unknownOutcome', {
      errorMessage:
        'Eksekusi untuk rencana ini sudah tercatat tetapi hasilnya belum pasti. Periksa hasilnya.',
    });
  }

  // ───────────────────────── progres / cancel / hasil ─────────────────────────

  private onProgress(p: OperationProgress): void {
    for (const run of this.runs.values()) {
      if (isFinal(run.state) || !run.jobIds.includes(p.operationId)) continue; // bukan milik run ini
      this.evaluate(run, {});
    }
  }

  private jobViews(run: Run): JobView[] {
    const views: JobView[] = [];
    for (const id of run.jobIds) {
      const j = this.lookup(run, id);
      if (!j) continue;
      views.push({
        jobId: id,
        state: j.state === 'paused' ? 'running' : j.state,
        done: j.processedFiles ?? 0,
        total: j.totalFiles ?? 0,
        ...(j.error ? { message: j.error } : {}),
      });
    }
    return views;
  }

  private buildReceipt(run: Run, summary: RunSummary): ExecutionReceipt {
    const errors: ExecutionReceipt['errors'] = [];
    for (const id of run.jobIds) {
      const j = this.lookup(run, id);
      for (const f of j?.failures ?? []) {
        if (errors.length >= 256) break;
        errors.push({ code: 'invalid_reference', message: f.message.slice(0, 200) });
      }
    }
    const state: ExecutionReceipt['state'] =
      summary.outcome === 'completed'
        ? 'completed'
        : summary.outcome === 'queued'
          ? 'queued'
          : summary.outcome;
    const queued = state === 'queued';
    return parseExecutionReceipt({
      schemaVersion: 1,
      executionId: run.executionId ?? 'exe_unknown',
      planId: run.prepared?.plan.planId ?? 'plan_unknown',
      jobIds: run.jobIds,
      state,
      completed: queued ? 0 : summary.completedFiles,
      failed: queued || state === 'completed' ? 0 : summary.failed,
      errors: queued || state === 'completed' ? [] : errors,
    });
  }

  /** Status job untuk run: job yang baru dikonfirmasi enqueue tetapi belum muncul di queue dianggap queued; yang hilang setelah terlihat = unknown. */
  private lookup(run: Run, id: string): OperationProgress | undefined {
    const j = this.deps.queue.get(id);
    if (j) {
      run.seen.add(id);
      return j;
    }
    return run.seen.has(id)
      ? undefined
      : { operationId: id, kind: 'copy', state: 'queued', processedBytes: 0, processedFiles: 0 };
  }

  /** Hitung ulang status run dari queue (hanya job milik run) dan emit. */
  private evaluate(run: Run, extra: Partial<RunUpdate>): void {
    if (isFinal(run.state)) return;
    const summary = summarizeRun(run.jobIds, (id) => this.lookup(run, id));
    this.finish(run, summary, extra);
  }

  private finish(run: Run, summary: RunSummary, extra: Partial<RunUpdate>): void {
    if (summary.outcome === 'completed' && run.failedToQueue > 0) {
      // Some items never reached the queue: a finished job list must not read as "all done".
      summary = { ...summary, outcome: 'partial', failed: summary.failed + run.failedToQueue };
      extra = {
        ...extra,
        errorMessage:
          extra.errorMessage ??
          run.partialNote ??
          `${run.failedToQueue} item gagal masuk antrean dan tidak disalin.`,
      };
    }
    const jobs = this.jobViews(run);
    const target: RunState | undefined = FINAL_MAP[summary.outcome];
    let receipt: ExecutionReceipt | undefined;
    try {
      receipt = this.buildReceipt(run, summary);
    } catch {
      receipt = undefined;
    }
    const payload: Partial<RunUpdate> = { jobs, ...(receipt ? { receipt } : {}), ...extra };
    if (target && run.conflictsSkipped > 0) {
      payload.warnings ??= [`${run.conflictsSkipped} konflik dilewati dan tidak diubah.`];
    }
    if (target) {
      if (summary.outcome === 'partial' && run.cancelRequested) {
        payload.errorMessage ??=
          'Dibatalkan setelah sebagian item selesai. Item yang sudah diproses tidak dibatalkan (tanpa rollback).';
      }
      this.setFinal(run, target, payload);
      return;
    }
    this.emit(run, summary.outcome === 'running' ? 'running' : 'queued', payload);
  }

  private async cancelJobs(run: Run): Promise<void> {
    if (!run.executionId) return; // belum ada eksekusi; ditangani di checkpoint
    const owned = await ownedJobIds(this.deps.ledger, run.executionId);
    const targets = owned.length ? owned : run.ledgerKey ? [] : run.jobIds;
    if (targets.length === 0) {
      if (run.state === 'running' || run.state === 'queued') {
        this.emit(run, run.state, {
          errorMessage: 'Operasi ini tidak berjalan lewat antrean dan tidak dapat dibatalkan.',
        });
      }
      return;
    }
    for (const jobId of targets) {
      // Hanya job milik execution ini; hasil akhir tetap dari progres queue.
      if (!run.jobIds.includes(jobId)) continue;
      try {
        await this.deps.queue.cancel(jobId);
      } catch {
        this.emit(run, run.state, { errorMessage: 'Permintaan batal gagal dikirim ke antrean.' });
      }
    }
  }

  // ───────────────────────── FER-576: navigasi + rantai langkah ─────────────────────────

  /**
   * `pane.navigate`: open a folder the USER typed. The path must be in the request text (also checked by the
   * contract parsers and by the Rust provider), absolute and clean, and it must exist. A file opens its folder
   * and selects the file. No facade/policy/guard/ledger/queue: nothing is written or executed, and the
   * result (names, listing) never goes to the provider.
   */
  private async runNavigate(run: Run, epoch: number, pane: PaneRef, path: string): Promise<void> {
    run.control = true;
    const port = this.deps.navigate;
    if (!port || !this.offered().includes(PANE_NAVIGATE_CAPABILITY)) {
      this.emit(run, 'blockedCapability', {
        blockedReason:
          'Membuka folder dari path yang diketik hanya tersedia di aplikasi desktop Ferrum.',
      });
      return;
    }
    // Defence in depth: the model's path is only ever one the user typed.
    if (!isSafeNavigatePath(path) || !pathTypedByUser(path, run.chain?.userText ?? run.userText)) {
      this.fail(run, epoch, 'Path itu tidak tertulis di permintaan Anda; tidak ada yang dibuka.');
      return;
    }
    const side = pane === 'activeAtSubmit' ? run.snapshot!.activePaneAtSubmit : pane;
    let target: NavigateTarget;
    try {
      target = await port.resolve(path);
    } catch {
      target = { kind: 'missing' };
    }
    if (!this.alive(run, epoch)) return;
    if (run.chain?.requireFile && target.kind === 'dir') {
      // A chain that deletes works on exactly the file the user typed, never on "whatever is in this folder".
      this.setFinal(run, 'failed', {
        errorMessage:
          'Rantai penghapusan hanya berlaku untuk path FILE yang Anda ketik, bukan folder. Tidak ada yang dijalankan.',
        navigate: { summary: `Membuka ${path}`, result: { ok: false, message: 'Bukan file.' } },
      });
      return;
    }
    if (target.kind === 'missing') {
      this.setFinal(run, 'failed', {
        errorMessage: `Path tidak ditemukan atau tidak bisa dibuka: ${path}. Tidak ada yang diubah.`,
        navigate: {
          summary: `Membuka ${path}`,
          result: { ok: false, message: 'Path tidak ditemukan.' },
        },
      });
      return;
    }
    let opened = false;
    try {
      opened = await port.open(side, target);
    } catch {
      opened = false;
    }
    if (!this.alive(run, epoch)) return;
    const summary = describeNavigate(target);
    if (!opened) {
      this.setFinal(run, 'failed', {
        errorMessage: 'Panel tidak menampilkan folder itu; langkah berikutnya tidak dijalankan.',
        navigate: {
          summary,
          result: { ok: false, message: 'Panel tidak menampilkan folder itu.' },
        },
      });
      return;
    }
    if (run.chain && target.kind === 'file') run.chain.navTarget = { side, target };
    this.setFinal(run, 'succeeded', {
      navigate: { summary, result: { ok: true, message: 'Folder dibuka di panel.' } },
    });
  }

  /**
   * A multi-step plan ("chain"). Step 1 runs in THIS run; every later step becomes its own run with its own
   * card, prepared against the panes as they are by then. The remaining steps live on the run that is executing
   * a step and are dropped by any failure, cancel or refusal. Panel references are bound to a concrete side now,
   * so clicking the other panel in between cannot redirect a step.
   */
  private async startChain(run: Run, epoch: number, raw: ChainEnvelope['steps']): Promise<void> {
    const pane = run.snapshot!.activePaneAtSubmit;
    const steps = raw.map((step) => bindPane(step, pane));
    const offered = this.offered();
    if (steps.length < 2 || steps.some((step) => !offered.includes(step.capability))) {
      this.emit(run, 'blockedCapability', {
        blockedReason:
          'Salah satu langkah rencana tidak tersedia sekarang; tidak ada yang dijalankan.',
      });
      return;
    }
    const hasDelete = steps.some((step) => step.capability === 'items.delete');
    if (hasDelete && steps[0].capability !== PANE_NAVIGATE_CAPABILITY) {
      this.emit(run, 'blockedCapability', {
        blockedReason:
          'Penghapusan setelah arsip hanya berlaku untuk satu file yang path-nya Anda ketik; tidak ada yang dijalankan.',
      });
      return;
    }
    if (hasDelete && (!userAskedToDelete(run.userText) || drivePathCount(run.userText) > 1)) {
      this.emit(run, 'blockedCapability', {
        blockedReason:
          'Rencana penghapusan butuh permintaan hapus yang jelas dan satu path; tidak ada yang dijalankan.',
      });
      return;
    }
    run.chain = {
      index: 0,
      total: steps.length,
      rest: steps.slice(1),
      capability: steps[0].capability,
      userText: run.userText,
      baseRequestId: run.requestId,
      baseRunId: run.runId,
      afterArchive: false,
      hasDelete,
      requireFile: hasDelete && steps[0].capability === PANE_NAVIGATE_CAPABILITY,
    };
    await this.runChainStep(run, epoch, steps[0]);
  }

  private async runChainStep(run: Run, epoch: number, step: ChainStep): Promise<void> {
    if (step.capability === PANE_NAVIGATE_CAPABILITY) {
      await this.runNavigate(run, epoch, step.pane, step.path);
      return;
    }
    const envelope: IntentEnvelope = { schemaVersion: 1, requestId: run.requestId, intent: step };
    run.history = new IntentHistory(envelope);
    if (!this.emit(run, 'resolving', {})) return;
    await this.buildPlan(run, epoch, envelope);
  }

  /** The run for the step after `from`, registered and ready to start. */
  private makeChainRun(from: Run): { run: Run; step: ChainStep } {
    const chain = from.chain!;
    const [step, ...rest] = chain.rest;
    const index = chain.index + 1;
    const run: Run = {
      runId: `${chain.baseRunId}-c${index}`,
      requestId: `${chain.baseRequestId}-c${index}`,
      userText: from.userText,
      state: 'planning',
      epoch: 0,
      abort: new AbortController(),
      preparedDropped: false,
      revision: 0,
      approving: false,
      cancelRequested: false,
      jobIds: [],
      seen: new Set(),
      failedToQueue: 0,
      conflictsSkipped: 0,
      checking: false,
      chain: {
        ...chain,
        index,
        rest,
        capability: step.capability,
        afterArchive: chain.capability === 'archive.create',
        ...(chain.capability === 'archive.create' && chain.note
          ? { note: chain.note }
          : { note: undefined }),
      },
    };
    this.runs.set(run.runId, run);
    return { run, step };
  }

  private async startChainRun(run: Run, step: ChainStep): Promise<void> {
    // Let the panes finish re-listing (the previous step may have refreshed them), then look at them fresh.
    try {
      await this.deps.navigate?.settled?.();
    } catch {
      // best effort: a stale snapshot is refused later, never acted on
    }
    if (!this.alive(run, run.epoch)) return;
    const nav = run.chain?.navTarget;
    if (nav && run.chain?.afterArchive) {
      // The archive step refreshed the panels (which clears marks): mark the typed file again, so the delete
      // card is built for exactly that file.
      let restored = false;
      try {
        restored = (await this.deps.navigate?.open(nav.side, nav.target)) === true;
        await this.deps.navigate?.settled?.();
      } catch {
        restored = false;
      }
      if (!this.alive(run, run.epoch)) return;
      if (!restored) {
        this.setFinal(run, 'failed', {
          errorMessage:
            'File yang Anda ketik tidak dapat ditampilkan lagi di panel; tidak ada yang dihapus.',
        });
        return;
      }
    }
    try {
      run.snapshot = this.deps.capture(run.requestId);
    } catch {
      this.fail(run, run.epoch, 'Panel tidak tersedia; langkah berikutnya tidak dijalankan.');
      return;
    }
    await this.runChainStep(run, run.epoch, step);
  }

  /**
   * "Archive, then delete" gates, checked when the step's plan is prepared. The archive step records the digest
   * of the selection it used; the delete step must see the very same selection, and only after Test Archive
   * passed. Anything else refuses the delete (nothing is deleted).
   */
  private chainGate(
    run: Run,
    capability: ResolvedPlan['capability'],
    preview: PreviewInfo,
  ): string | undefined {
    const chain = run.chain;
    if (!chain) return undefined;
    // Review R2 S1: the card of a step is that step's operation, whatever a follow-up asked for.
    if (capability !== chain.capability) {
      return 'Jenis operasi langkah ini tidak boleh diganti; tidak ada yang dijalankan.';
    }
    // Review R1 S6: a chain that navigated to a file and deletes works on exactly that file.
    if (chain.requireFile && chain.navTarget) {
      if (!preview.selectionIsOnly?.(chain.navTarget.target.path)) {
        return 'Seleksi bukan persis file yang Anda ketik; tidak ada yang diarsipkan atau dihapus.';
      }
    }
    if (capability === 'archive.create') {
      if (chain.hasDelete) {
        // Review R1 S3: the archive must not be written inside what is about to be deleted.
        if (preview.outputInsideSource !== false) {
          return 'Arsip akan disimpan di dalam item yang akan dihapus; ia ikut terhapus. Pilih tujuan lain; tidak ada yang dijalankan.';
        }
        // Review R1 S2: the Pack engine silently skips symlinks and junctions, so the archive would not hold them.
        if ((preview.linkCount ?? 1) > 0 || preview.expectedMembers === undefined) {
          return 'Isi arsip tidak bisa dipastikan lengkap (ada symlink/junction, atau jumlah isi tidak diketahui); tidak ada yang dijalankan.';
        }
        chain.archiveExpectedMembers = preview.expectedMembers;
      }
      chain.archiveSelectionDigest = preview.selectionDigest;
      return undefined;
    }
    if (capability === 'items.delete') {
      // Review R1 S5: also checked here, whatever the provider layers did.
      if (!userAskedToDelete(chain.userText)) {
        return 'Permintaan Anda tidak meminta menghapus; tidak ada yang dihapus.';
      }
      if (chain.afterArchive) {
        if (!chain.verified) return 'Arsip belum lolos uji; tidak ada yang dihapus.';
        if (!preview.selectionDigest || preview.selectionDigest !== chain.archiveSelectionDigest) {
          return 'Item terpilih berubah setelah arsip dibuat. Yang dihapus harus persis yang diarsipkan; tidak ada yang dihapus.';
        }
      }
    }
    return undefined;
  }

  /**
   * Review R1 S1: the backend fingerprints the sources (size, time, identity) when each card is built. The delete
   * card of "archive, then delete" must fingerprint to the SAME digest as the archive card did, so a file edited
   * after it was archived is never deleted as if the archive held it. Fail closed when a digest is missing.
   */
  private chainScopeGate(
    run: Run,
    capability: ResolvedPlan['capability'],
    ticket: ApprovalTicket,
  ): string | undefined {
    const chain = run.chain;
    const guard = this.deps.guard;
    if (!chain) return undefined;
    if (!guard?.scopeDigestOf) {
      // Without the backend's own fingerprint the delete cannot be tied to what was archived: fail closed.
      return capability === 'items.delete' && chain.afterArchive
        ? 'Sidik jari backend tidak tersedia, jadi isi yang dihapus tidak bisa dipastikan sama dengan yang diarsipkan; tidak ada yang dihapus.'
        : undefined;
    }
    const digest = guard.scopeDigestOf(ticket);
    if (capability === 'archive.create') {
      chain.archiveScopeDigest = digest;
      return undefined;
    }
    if (capability === 'items.delete' && chain.afterArchive) {
      if (!digest || digest !== chain.archiveScopeDigest) {
        return 'Isi item berubah sejak arsip dibuat (ukuran atau waktu ubah). Yang dihapus harus persis yang diarsipkan; tidak ada yang dihapus.';
      }
    }
    return undefined;
  }

  private async verifyChainArchive(
    outputPath: string | undefined,
    expectedMembers: number | undefined,
  ): Promise<{ ok: boolean; message: string }> {
    const name = outputPath ? (outputPath.split(/[\\/]/).pop() ?? outputPath) : 'arsip';
    const refuse = (why: string) => ({
      ok: false,
      message: `Arsip ${name} dibuat, tetapi ${why} File asli TIDAK dihapus; periksa arsipnya.`,
    });
    const verify = this.deps.verifyArchive;
    if (!verify || !outputPath) return refuse('tidak dapat diuji, jadi penghapusan dibatalkan.');
    try {
      if (expectedMembers === undefined) return refuse('isinya tidak bisa dipastikan lengkap.');
      const verdict = await verify(outputPath, { members: expectedMembers });
      if (!verdict.ok) return refuse(`uji arsip GAGAL (${verdict.message}).`);
      return {
        ok: true,
        message: `Arsip ${name} sudah dibuat dan lolos uji arsip (${verdict.message}).`,
      };
    } catch {
      return refuse('uji arsip tidak dapat dijalankan, jadi penghapusan dibatalkan.');
    }
  }

  // ───────────────────────── util ─────────────────────────

  private newId(prefix: string): string {
    return (
      this.deps.newId?.(prefix) ??
      `${prefix}_${this.now().toString(36)}_${(++this.seq).toString(36)}`
    );
  }

  private invalidate(run: Run): void {
    run.epoch++;
    run.abort.abort();
    this.approvals.invalidate(run.runId);
  }

  /** Lepas manifest/handle yang belum dieksekusi (no-op bila facade sudah menjalankannya). Plan tetap untuk UI/receipt. */
  private dropPrepared(run: Run): void {
    if (run.preparedDropped) return; // idempoten: revoke/discard hanya sekali per prepared
    run.preparedDropped = true;
    const ticket = this.approvals.ticketOf(run.runId);
    if (ticket) this.deps.guard?.revoke?.(ticket);
    if (run.prepared) this.deps.facade.discard(run.prepared.handle.handleId);
  }

  /** Fail closed: a guard that exists but cannot register means nothing may be approved. */
  private async registerGuard(
    ticket: ApprovalTicket,
    plan: ResolvedPlan,
  ): Promise<{ ok: true } | { ok: false; message: string }> {
    const guard = this.deps.guard;
    if (!guard?.register) return { ok: true };
    try {
      return await guard.register(ticket, plan);
    } catch {
      return {
        ok: false,
        message: 'Validasi backend gagal; tidak ada yang disetujui atau ditulis.',
      };
    }
  }

  /**
   * FER-573: the model said it cannot read sizes. Fail closed: the fixed text + button appear only
   * when `ui.hint` is offered now AND Disk Usage is still not open; otherwise the reply is invalid.
   * The text is written here, never taken from the model.
   */
  private showHint(run: Run, epoch: number): void {
    const duPort = this.deps.diskUsage;
    // Race: Disk Usage was opened after the question was sent. Say so calmly; no button, no error.
    if (duPort && safeDiskUsageAvailable(duPort)) {
      this.emit(run, 'needsInput', { modelText: DISK_USAGE_ALREADY_OPEN_TEXT });
      return;
    }
    if (!duPort || !this.offered().includes(UI_HINT_CAPABILITY)) {
      this.failProvider(run, epoch, new RemoteProviderError('malformed', 'Unexpected hint'));
      return;
    }
    this.emit(run, 'needsInput', { modelText: DISK_USAGE_HINT_TEXT, uiHint: 'openDiskUsage' });
  }

  /** FER-465: galat provider -> kode aman + pesan Indonesia + tombol aksi (tanpa isi respons mentah). */
  private providerView(error: unknown): ProviderErrorView {
    const code = classifyProviderFailure(error);
    const view = describeProviderError(code, {
      hostPort: endpointHostPort(this.deps.providerEndpoint?.()),
      capabilities: this.offered(),
      detail: error instanceof RemoteProviderError ? error.detail : undefined,
      model: this.deps.providerModel?.(),
    });
    const raw = error instanceof RemoteProviderError ? error.code : 'backend_failure';
    this.deps.onProviderFailure?.(code, raw);
    return view;
  }

  private failProvider(run: Run, epoch: number, error: unknown): void {
    if (!this.alive(run, epoch)) return;
    const view = this.providerView(error);
    this.dropPrepared(run);
    this.emit(run, 'failed', { errorMessage: view.message, providerError: view });
  }

  private fail(run: Run, epoch: number, message: string): void {
    if (!this.alive(run, epoch)) return;
    this.dropPrepared(run);
    this.emit(run, 'failed', { errorMessage: message });
  }

  private setFinal(run: Run, state: RunState, extra: Partial<RunUpdate>): void {
    // FER-576: only a SUCCEEDED step (not cancelled by the user meanwhile) hands over to the next one.
    const next =
      state === 'succeeded' && run.chain && run.chain.rest.length > 0 && !run.cancelRequested
        ? this.makeChainRun(run)
        : undefined;
    const update = next
      ? {
          ...extra,
          chainNext: {
            runId: next.run.runId,
            requestId: next.run.requestId,
            userText: next.run.userText,
          },
        }
      : extra;
    if (this.emit(run, state, update) && isFinal(state)) {
      run.unsub?.();
      this.approvals.forget(run.runId);
      if (next) {
        void this.startChainRun(next.run, next.step).catch(() =>
          this.fail(
            next.run,
            next.run.epoch,
            'Terjadi kesalahan internal; tidak ada yang dijalankan.',
          ),
        );
      }
    } else if (next) {
      this.runs.delete(next.run.runId);
    }
  }

  /** Emit hanya bila transisi sah (atau pembaruan pada state yang sama yang belum final). */
  private emit(run: Run, state: RunState, extra: Partial<RunUpdate>): boolean {
    if (state !== run.state) {
      const allowed =
        canTransition(run.state, state) ||
        (run.control === true && canControlTransition(run.state, state));
      if (!allowed) return false;
    } else if (isFinal(state)) {
      return false;
    }
    run.state = state;
    const update: RunUpdate = {
      runId: run.runId,
      requestId: run.requestId,
      state,
      ...(run.chain
        ? {
            chain: {
              index: run.chain.index,
              total: run.chain.total,
              ...(run.chain.note ? { note: run.chain.note } : {}),
            },
          }
        : {}),
      ...stripUndefined(extra),
    };
    for (const l of [...this.listeners]) l(update);
    return true;
  }
}

const FINAL_MAP: Partial<Record<RunOutcome, RunState>> = {
  completed: 'succeeded',
  partial: 'partial',
  failed: 'failed',
  cancelled: 'cancelled',
  unknownOutcome: 'unknownOutcome',
};

function safeCopyRunning(port: CopyControlPort): boolean {
  try {
    return port.available() === true;
  } catch {
    return false; // fail closed: not offered
  }
}

function safeIndexerAvailable(port: IndexerControlPort): boolean {
  try {
    return port.available() === true;
  } catch {
    return false; // fail closed: not offered
  }
}

function safeDiskUsageAvailable(port: DiskUsageQueryPort): boolean {
  try {
    return port.available() === true;
  } catch {
    return false; // fail closed: not offered
  }
}

function safeNavigateAvailable(port: NavigatePort): boolean {
  try {
    return port.available() === true;
  } catch {
    return false; // fail closed: not offered
  }
}

/** FER-576: bind `activeAtSubmit` to the concrete side that was active when the request was made. */
function bindPane(step: ChainStep, pane: 'left' | 'right'): ChainStep {
  if (step.capability === PANE_NAVIGATE_CAPABILITY) {
    return step.pane === 'activeAtSubmit' ? { ...step, pane } : step;
  }
  const sourcePane = step.sourcePane === 'activeAtSubmit' ? pane : step.sourcePane;
  if (step.capability === 'archive.create' && step.target.kind === 'pane') {
    return {
      ...step,
      sourcePane,
      target: {
        kind: 'pane',
        pane: step.target.pane === 'activeAtSubmit' ? pane : step.target.pane,
      },
    };
  }
  return { ...step, sourcePane };
}

function safeAvailable(port: ActivityQueryPort): boolean {
  try {
    return port.available() === true;
  } catch {
    return false;
  }
}

function stripUndefined<T extends object>(o: T): T {
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;
}
