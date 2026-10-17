/**
 * Automation page state (Automation P1.5): the list, the schedule editor and the review screen.
 *
 * Safety rules this store enforces structurally (design §8, SE-1):
 * - There is NO "approved" or "confirmed" state on the client. `approve()` sends exactly
 *   `{id?, expectedConfigRev?, draft, testId}` and only when the backend's own test answered
 *   `passed`; the confirmation is a native dialog the backend raises, and its answer comes back
 *   as `status` (`approved`, `canceled`, `stale`).
 * - Every outcome is rendered as the backend reported it: `canceled` keeps the user on the review
 *   screen, `stale` shows one fixed text and drops the test.
 * - Run now / Retry / Stop / Recheck (P1.6) are REQUESTS: the buttons only call the backend, which
 *   decides at its gate; the answer is rendered as the backend reported it. A pending set stops
 *   double submits and an answer for a screen that has since changed is dropped (stale response).
 * - Pause with a run in progress shows the three-way dialog; its choice is sent as
 *   `automation_pause{mode}` and nothing else.
 */
import { toast } from './toastStore.svelte';
import { ui } from '../uiState.svelte';
import {
  AutomationError,
  defaultBackend,
  queueToast,
  type AutomationBackend,
  type AutomationDetail,
  type AutomationDraft,
  type AutomationEvent,
  type AutomationProgress,
  type AutomationRow,
  type AutomationRun,
  type AutomationTestResult,
  type HistoryFilter,
  type NextRun,
  type PauseMode,
  type RunOpsPage,
  type Step,
} from '../ipc/automation';
import {
  SyncProfilesError,
  defaultBackend as defaultProfilesBackend,
  type FieldChange,
  type FieldError,
  type ProfileSummary,
  type SyncProfilesBackend,
} from '../ipc/syncProfiles';
import {
  STALE_TEXT,
  automationErrorMessage,
  errorMessage,
  formFromDraft,
  localErrors,
  mergeErrors,
  newScheduleForm,
  OPS_PAGE,
  toDraft,
  type ScheduleForm,
} from '../components/automation/automationModel';

export type AutomationView = 'list' | 'editor' | 'review' | 'detail';
export type AutomationTab = 'automations' | 'history';
/** How the review ends: a new automation, a saved edit, or a re-review after a change. */
export type ReviewMode = 'new' | 'edit' | 'resume';

export interface Notice {
  kind: 'info' | 'success' | 'error';
  text: string;
}

export interface EditorState {
  /** Absent while creating a new automation. */
  id?: string;
  /** The `configRev` the editor loaded; sent back as `expectedConfigRev`. */
  expectedConfigRev?: number;
  form: ScheduleForm;
  /** Merged local and backend field errors of the CURRENT form. */
  errors: FieldError[];
  checks: Step[];
  next: NextRun[];
  /** True once a backend validation of the current form came back. */
  validated: boolean;
  busy: boolean;
  /** The backend could not be reached (for example outside the desktop app). */
  unavailable?: string;
  dirty: boolean;
}

export interface ReviewState {
  id?: string;
  expectedConfigRev?: number;
  mode: ReviewMode;
  draft: AutomationDraft;
  /** Row of the automation being edited / re-reviewed (trigger text, review reason). */
  row?: AutomationRow;
  /** Revision the stored approval covers, when re-reviewing. */
  approvedRevision?: number;
  /** What changed in the profile since the approved revision (field, from → to). */
  changes: FieldChange[];
  test?: AutomationTestResult;
  testing: boolean;
  approving: boolean;
  notice: Notice | null;
  /** The backend said the profile or automation changed: the test is void, run it again. */
  stale: boolean;
}

export interface HistoryState {
  filter: HistoryFilter;
  runs: AutomationRun[];
  next?: number;
  loading: boolean;
  error: string | null;
}

/** The detail page of one automation (MH-5). */
export interface DetailState {
  id: string;
  data: AutomationDetail | null;
  loading: boolean;
  error: string | null;
  /** Events paged in after the first screen ("Show earlier events"), newest first. */
  moreEvents: AutomationEvent[];
  eventsNext?: number;
  eventsLoading: boolean;
}

/** One page of a run's operation list (RQ-5; pages of 200). */
export interface OpsState {
  runId: number;
  offset: number;
  page: RunOpsPage | null;
  loading: boolean;
  error: string | null;
}

/** The Pause / Stop dialogs (RQ-8). Their buttons are the only way a pause mode or a stop is sent. */
export type DialogState =
  { kind: 'pause'; id: string; name: string } | { kind: 'stop'; runId: number; name: string };

/** Folders for "Review in Sync" - always read from the backend (profile roots), never typed. */
export interface SyncRootsArg {
  left: string;
  right: string;
}

export interface StoreDeps {
  backend: AutomationBackend;
  profiles: SyncProfilesBackend;
  /** Opens Synchronize Dirs pinned to these folders (and leaves the Automation page). */
  openSync: (roots: SyncRootsArg) => Promise<void>;
  /** Delay before an editor change is validated; 0 = immediately (tests). */
  validateDelayMs: number;
  now: () => number;
}

/** Default: leave the Automation page and open Synchronize Dirs for the profile's two folders. */
async function openSyncDefault(roots: SyncRootsArg): Promise<void> {
  const { compareStore } = await import('./compareStore.svelte');
  ui.automationOpen = false;
  await compareStore.openSync(roots);
}

const codeOf = (e: unknown): string =>
  e instanceof AutomationError || e instanceof SyncProfilesError ? e.code : 'unknown';

export class AutomationStore {
  rows = $state<AutomationRow[]>([]);
  profiles = $state<ProfileSummary[]>([]);
  loaded = $state(false);
  loading = $state(false);
  listError = $state<string | null>(null);
  notice = $state<Notice | null>(null);
  view = $state<AutomationView>('list');
  tab = $state<AutomationTab>('automations');
  editor = $state<EditorState | null>(null);
  review = $state<ReviewState | null>(null);
  history = $state<HistoryState>({
    filter: 'all',
    runs: [],
    loading: false,
    error: null,
  });
  /** The row highlighted for 5 s after an approval (AP-5). */
  highlightId = $state<string | null>(null);
  /** Set while the user is asked whether to throw away unsaved editor changes. */
  discardPending = $state(false);
  detail = $state<DetailState | null>(null);
  /** Pause / Stop dialog, when one is open. */
  dialog = $state<DialogState | null>(null);
  /** Requests in flight (`run:<id>`, `stop:<runId>`, `recheck:<runId>`, `retry:<runId>`, `pause:<id>`, `sync:<runId>`). */
  pending = $state<Record<string, boolean>>({});
  /** Run ids whose rows are expanded (history and recent runs). */
  expanded = $state<Record<number, boolean>>({});
  /** Operation pages by run id. */
  opsByRun = $state<Record<number, OpsState>>({});

  #deps: StoreDeps;
  #loadSeq = 0;
  #reviewSeq = 0;
  #historySeq = 0;
  #detailSeq = 0;
  #opsSeq: Record<number, number> = {};
  #opsTimer: ReturnType<typeof setTimeout> | null = null;
  #validateSeq = 0;
  #validateTimer: ReturnType<typeof setTimeout> | null = null;
  #highlightTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(deps?: Partial<StoreDeps>) {
    this.#deps = {
      backend: deps?.backend ?? defaultBackend(),
      profiles: deps?.profiles ?? defaultProfilesBackend(),
      openSync: deps?.openSync ?? openSyncDefault,
      validateDelayMs: deps?.validateDelayMs ?? 200,
      now: deps?.now ?? Date.now,
    };
  }

  get attentionCount(): number {
    return this.rows.filter((r) => r.showReview || r.attention !== undefined).length;
  }

  /** The running, queued and pending-rerun rows of the queue strip (MH-4). */
  get queue(): AutomationRow[] {
    return this.rows.filter((r) => ['running', 'queued', 'pending'].includes(r.status.kind));
  }

  profile(id: string): ProfileSummary | undefined {
    return this.profiles.find((p) => p.id === id);
  }

  dispose(): void {
    if (this.#validateTimer) clearTimeout(this.#validateTimer);
    if (this.#highlightTimer) clearTimeout(this.#highlightTimer);
    this.#validateTimer = null;
    this.#highlightTimer = null;
    if (this.#opsTimer) clearTimeout(this.#opsTimer);
    this.#opsTimer = null;
    this.#validateSeq++;
    this.#detailSeq++;
  }

  // ── List ──

  async load(): Promise<void> {
    const seq = ++this.#loadSeq;
    this.loading = true;
    const [rows, profiles] = await Promise.allSettled([
      this.#deps.backend.list(),
      this.#deps.profiles.list(),
    ]);
    if (seq !== this.#loadSeq) return;
    this.loading = false;
    this.loaded = true;
    if (rows.status === 'fulfilled') {
      this.rows = rows.value;
      this.listError = null;
    } else {
      this.listError = automationErrorMessage(codeOf(rows.reason));
    }
    // A profile list failure is not fatal for the list; the editor shows the empty state.
    if (profiles.status === 'fulfilled') this.profiles = profiles.value;
  }

  async toggleEnabled(row: { id: string; name: string; enabled: boolean }): Promise<void> {
    const key = `pause:${row.id}`;
    if (this.pending[key]) return;
    this.pending[key] = true;
    this.notice = null;
    try {
      const res = await this.#deps.backend.setEnabled(row.id, !row.enabled);
      if (res.status === 'needsChoice') {
        // A run is in progress: ask how to pause (RQ-8.2). The choice is sent by `pauseWith`.
        this.dialog = { kind: 'pause', id: row.id, name: row.name };
      } else if (res.status === 'canceled') {
        this.notice = {
          kind: 'info',
          text: 'Canceled in the confirmation dialog. Nothing was changed.',
        };
      }
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
    }
    await this.#refreshShown();
  }

  /** Reload the list and whatever else is on screen. */
  async #refreshShown(): Promise<void> {
    await this.load();
    if (this.view === 'detail') await this.refreshDetail();
    if (this.tab === 'history' && this.view === 'list') await this.loadHistory();
  }

  // ── Pause / Stop dialogs (RQ-8) ──

  /** Stop the current run: ask first (RQ-8.1); the dialog's button sends the request. */
  askStop(run: AutomationRun): void {
    this.dialog = { kind: 'stop', runId: run.id, name: run.automationName ?? 'this automation' };
  }

  closeDialog(): void {
    const d = this.dialog;
    // While the request runs the dialog stays, so a second click cannot send it twice.
    if (d && this.pending[d.kind === 'pause' ? `pause:${d.id}` : `stop:${d.runId}`]) return;
    this.dialog = null;
  }

  /** "Pause & let it finish" / "Pause & stop current run": sends `automation_pause{mode}` (RQ-8.2). */
  async pauseWith(mode: PauseMode): Promise<void> {
    const d = this.dialog;
    if (!d || d.kind !== 'pause') return;
    const key = `pause:${d.id}`;
    if (this.pending[key]) return;
    this.pending[key] = true;
    try {
      await this.#deps.backend.pause(d.id, mode);
      toast.show(
        mode === 'stop'
          ? `${d.name} paused — stopping the current run`
          : `${d.name} paused — the current run will finish`,
        4000,
      );
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
      if (this.dialog === d) this.dialog = null;
    }
    await this.#refreshShown();
  }

  /** Confirmed Stop: a request; the host stops after the current file (RQ-8.1). */
  async confirmStop(): Promise<void> {
    const d = this.dialog;
    if (!d || d.kind !== 'stop') return;
    const key = `stop:${d.runId}`;
    if (this.pending[key]) return;
    this.pending[key] = true;
    try {
      await this.#deps.backend.stopRun(d.runId);
      toast.show('Stop requested — stopping after the current file', 4000);
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
      if (this.dialog === d) this.dialog = null;
    }
    await this.#refreshShown();
  }

  // ── Run now, Recheck, Retry, Review in Sync (RQ-4, RQ-10, RQ-6) ──

  /** Run now: the backend queues it (or refuses); the answer is the toast of RQ-4. */
  async runNow(row: { id: string; canRunNow?: boolean }): Promise<void> {
    const key = `run:${row.id}`;
    // A dimmed button stays dimmed: the reason is shown instead of asking (the backend checks again).
    if (row.canRunNow === false || this.pending[key]) return;
    this.pending[key] = true;
    this.notice = null;
    try {
      const res = await this.#deps.backend.runNow(row.id);
      toast.show(queueToast(res), 4000);
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
    }
    await this.#refreshShown();
  }

  /** Recheck: a fresh read-only comparison; the result arrives on the run (change event). */
  async recheck(run: AutomationRun): Promise<void> {
    const key = `recheck:${run.id}`;
    if (this.pending[key]) return;
    this.pending[key] = true;
    this.notice = null;
    try {
      await this.#deps.backend.recheck(run.id);
      toast.show('Recheck requested — comparing the folders (read-only)', 4000);
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
    }
    await this.#refreshShown();
  }

  /** Retry what the recheck found remaining: a fresh plan, nothing is repeated (RQ-10.2). */
  async retry(run: AutomationRun): Promise<void> {
    const key = `retry:${run.id}`;
    if (this.pending[key]) return;
    this.pending[key] = true;
    this.notice = null;
    try {
      const res = await this.#deps.backend.retry(run.id);
      toast.show(queueToast(res), 4000);
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
    }
    await this.#refreshShown();
  }

  /**
   * "Review in Sync": opens Synchronize Dirs for the profile's two folders. The folders come from
   * the backend (the automation detail or the profile list), never from free input.
   */
  async reviewInSync(run: AutomationRun): Promise<void> {
    const key = `sync:${run.id}`;
    if (this.pending[key]) return;
    this.pending[key] = true;
    try {
      let roots: SyncRootsArg | null = null;
      const shown = this.detail?.data?.profile;
      if (shown && shown.id === run.profileId) roots = { left: shown.left, right: shown.right };
      if (!roots) {
        const p = this.profile(run.profileId);
        if (p) roots = { left: p.left, right: p.right };
      }
      if (!roots && run.automationId) {
        const d = await this.#deps.backend.get(run.automationId);
        if (d.profile.id === run.profileId) {
          roots = { left: d.profile.left, right: d.profile.right };
        }
      }
      if (!roots) {
        this.notice = { kind: 'error', text: automationErrorMessage('not_found') };
        return;
      }
      await this.#deps.openSync(roots);
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      delete this.pending[key];
    }
  }

  // ── Detail page (MH-5) ──

  async openDetail(id: string): Promise<void> {
    this.notice = null;
    this.expanded = {};
    this.opsByRun = {};
    this.detail = {
      id,
      data: null,
      loading: true,
      error: null,
      moreEvents: [],
      eventsLoading: false,
    };
    this.view = 'detail';
    await this.refreshDetail();
  }

  closeDetail(): void {
    this.#detailSeq++;
    this.detail = null;
    this.expanded = {};
    this.opsByRun = {};
    this.dialog = null;
    this.view = 'list';
    void this.load();
  }

  /** Re-read the detail. A slower answer for an older request, or another automation, is dropped. */
  async refreshDetail(): Promise<void> {
    const d = this.detail;
    if (!d) return;
    const seq = ++this.#detailSeq;
    try {
      const data = await this.#deps.backend.get(d.id);
      if (seq !== this.#detailSeq || this.detail !== d) return;
      d.data = data;
      d.error = null;
      if (d.eventsNext === undefined && d.moreEvents.length === 0) d.eventsNext = data.events.next;
    } catch (e) {
      if (seq !== this.#detailSeq || this.detail !== d) return;
      d.error = automationErrorMessage(codeOf(e));
    } finally {
      if (seq === this.#detailSeq && this.detail === d) d.loading = false;
    }
    this.#reloadExpandedOps();
  }

  /** Keep the expanded operation lists current. */
  #reloadExpandedOps(): void {
    for (const key of Object.keys(this.expanded)) {
      const runId = Number(key);
      if (this.expanded[runId]) void this.loadOps(runId, this.opsByRun[runId]?.offset ?? 0);
    }
  }

  /** "Show earlier events": pages the log by the `next` cursor (50 per page). */
  async moreEvents(): Promise<void> {
    const d = this.detail;
    if (!d || d.eventsLoading || d.eventsNext === undefined) return;
    d.eventsLoading = true;
    try {
      const page = await this.#deps.backend.events(d.id, d.eventsNext, 50);
      if (this.detail !== d) return;
      const seen = [...(d.data?.events.events ?? []), ...d.moreEvents].map((e) => e.id);
      d.moreEvents = [...d.moreEvents, ...page.events.filter((e) => !seen.includes(e.id))];
      d.eventsNext = page.next;
    } catch (e) {
      if (this.detail === d) {
        this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
      }
    } finally {
      if (this.detail === d) d.eventsLoading = false;
    }
  }

  /** `automation://progress`: update the shown run in place; ids, phase and counts only. */
  applyProgress(p: AutomationProgress): void {
    const data = this.detail?.data;
    if (!data || data.automation.id !== p.automationId) return;
    const cur = data.currentRun;
    if (!cur || cur.id !== p.runId) {
      // A run we do not know yet (it just started): re-read once.
      void this.refreshDetail();
      return;
    }
    cur.phase = p.phase;
    cur.opsTotal = p.opsTotal;
    cur.counts = p.counts;
    const recent = data.recentRuns.find((r) => r.id === p.runId);
    if (recent) {
      recent.phase = p.phase;
      recent.opsTotal = p.opsTotal;
      recent.counts = p.counts;
    }
    if (this.expanded[p.runId] && !this.#opsTimer) {
      this.#opsTimer = setTimeout(() => {
        this.#opsTimer = null;
        void this.loadOps(p.runId, this.opsByRun[p.runId]?.offset ?? 0);
      }, 1000);
    }
  }

  /** Another process (or the host) changed the store: re-read what is shown (MH-10). */
  async onChanged(): Promise<void> {
    await this.load();
    if (this.tab === 'history' && this.view === 'list') {
      await this.loadHistory();
      this.#reloadExpandedOps();
    }
    if (this.view === 'detail') await this.refreshDetail();
  }

  // ── Expanded runs and operation pages ──

  toggleRun(run: AutomationRun): void {
    const open = !this.expanded[run.id];
    this.expanded[run.id] = open;
    if (open && run.opsTotal > 0) void this.loadOps(run.id, 0);
  }

  /** One page (200) of a run's stored operations. */
  async loadOps(runId: number, offset: number): Promise<void> {
    const seq = (this.#opsSeq[runId] = (this.#opsSeq[runId] ?? 0) + 1);
    const prev = this.opsByRun[runId];
    this.opsByRun[runId] = { runId, offset, page: prev?.page ?? null, loading: true, error: null };
    try {
      const page = await this.#deps.backend.runOps(runId, offset, OPS_PAGE);
      if (seq !== this.#opsSeq[runId]) return;
      this.opsByRun[runId] = { runId, offset, page, loading: false, error: null };
    } catch (e) {
      if (seq !== this.#opsSeq[runId]) return;
      this.opsByRun[runId] = {
        runId,
        offset,
        page: prev?.page ?? null,
        loading: false,
        error: automationErrorMessage(codeOf(e)),
      };
    }
  }

  // ── Editor ──

  openNew(): void {
    this.notice = null;
    const first = this.profiles.find((p) => p.readiness.status === 'ready');
    this.#openEditor({ form: newScheduleForm(first?.id ?? '', this.#deps.now()) });
  }

  async openEdit(id: string): Promise<void> {
    this.notice = null;
    try {
      const detail = await this.#deps.backend.get(id);
      const draft: AutomationDraft = {
        name: detail.automation.name,
        profileId: detail.automation.profileId,
        policy: detail.policy,
      };
      const form = formFromDraft(draft, this.#deps.now());
      if (!form) {
        this.notice = {
          kind: 'info',
          text: 'Editing watch-folder automations is not available yet.',
        };
        return;
      }
      this.#openEditor({ id, expectedConfigRev: detail.automation.configRev, form });
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    }
  }

  #openEditor(init: { id?: string; expectedConfigRev?: number; form: ScheduleForm }): void {
    this.review = null;
    this.discardPending = false;
    this.editor = {
      id: init.id,
      expectedConfigRev: init.expectedConfigRev,
      form: init.form,
      errors: localErrors(init.form, this.#deps.now()),
      checks: [],
      next: [],
      validated: false,
      busy: false,
      dirty: false,
    };
    this.view = 'editor';
    this.#scheduleValidate(0);
  }

  /** The editor changed a field: re-check locally now and ask the backend shortly. */
  changeForm(patch: Partial<ScheduleForm>): void {
    const ed = this.editor;
    if (!ed) return;
    ed.form = { ...ed.form, ...patch };
    ed.dirty = true;
    ed.validated = false;
    ed.errors = localErrors(ed.form, this.#deps.now());
    this.#scheduleValidate(this.#deps.validateDelayMs);
  }

  #scheduleValidate(delay: number): void {
    if (this.#validateTimer) clearTimeout(this.#validateTimer);
    this.#validateTimer = null;
    const seq = ++this.#validateSeq;
    if (delay <= 0) {
      void this.#validateNow(seq);
      return;
    }
    this.#validateTimer = setTimeout(() => {
      this.#validateTimer = null;
      void this.#validateNow(seq);
    }, delay);
  }

  async #validateNow(seq: number): Promise<void> {
    const ed = this.editor;
    if (!ed) return;
    const form = ed.form;
    const local = localErrors(form, this.#deps.now());
    // The backend validates a draft that already has a name/profile only; local errors are shown anyway.
    ed.busy = true;
    try {
      const res = await this.#deps.backend.validate(toDraft(form));
      if (seq !== this.#validateSeq || this.editor !== ed) return;
      ed.errors = mergeErrors(local, res.errors);
      ed.checks = res.checks;
      ed.next = res.next;
      ed.unavailable = undefined;
      ed.validated = true;
    } catch (e) {
      if (seq !== this.#validateSeq || this.editor !== ed) return;
      ed.errors = local;
      ed.checks = [];
      ed.next = [];
      ed.validated = false;
      ed.unavailable = automationErrorMessage(codeOf(e));
    } finally {
      if (seq === this.#validateSeq && this.editor === ed) ed.busy = false;
    }
  }

  get canContinue(): boolean {
    const ed = this.editor;
    return !!ed && ed.validated && ed.errors.length === 0 && !ed.busy;
  }

  /** Ask before throwing away unsaved editor changes. Returns true when the editor closed. */
  leaveEditor(): boolean {
    if (this.editor?.dirty && !this.discardPending) {
      this.discardPending = true;
      return false;
    }
    this.closeEditor();
    return true;
  }

  closeEditor(): void {
    if (this.#validateTimer) clearTimeout(this.#validateTimer);
    this.#validateTimer = null;
    this.#validateSeq++;
    this.editor = null;
    this.review = null;
    this.discardPending = false;
    this.view = 'list';
  }

  // ── Review ──

  /** Editor → Review ("Continue to review"). */
  continueToReview(): void {
    const ed = this.editor;
    if (!ed || !this.canContinue) return;
    // The clock moved on since the last edit: a Once time may have passed meanwhile (SC-1.1).
    const fresh = localErrors(ed.form, this.#deps.now());
    if (fresh.length > 0) {
      ed.errors = mergeErrors(ed.errors, fresh);
      return;
    }
    this.review = {
      id: ed.id,
      expectedConfigRev: ed.expectedConfigRev,
      mode: ed.id ? 'edit' : 'new',
      draft: toDraft(ed.form),
      row: ed.id ? this.rows.find((r) => r.id === ed.id) : undefined,
      changes: [],
      testing: false,
      approving: false,
      notice: null,
      stale: false,
    };
    this.view = 'review';
  }

  /** List → Review of an automation that needs a review (AP-6). */
  async openReview(row: AutomationRow): Promise<void> {
    this.notice = null;
    const seq = ++this.#reviewSeq;
    try {
      const detail = await this.#deps.backend.get(row.id);
      const changes = await this.#profileChanges(row);
      if (seq !== this.#reviewSeq) return;
      this.editor = null;
      this.review = {
        id: row.id,
        expectedConfigRev: detail.automation.configRev,
        mode: 'resume',
        draft: {
          name: detail.automation.name,
          profileId: detail.automation.profileId,
          policy: detail.policy,
        },
        row,
        approvedRevision: row.approvedRevision,
        changes,
        testing: false,
        approving: false,
        notice: null,
        stale: false,
      };
      this.view = 'review';
    } catch (e) {
      if (seq !== this.#reviewSeq) return;
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    }
  }

  /** What changed in the profile since the approved revision (field, from → to). */
  async #profileChanges(row: AutomationRow): Promise<FieldChange[]> {
    if (row.review?.reason !== 'profileChanged') return [];
    try {
      const profile = await this.#deps.profiles.get(row.profileId);
      const since = row.approvedRevision ?? row.review.from;
      return profile.changes
        .filter((c) => c.revision > since)
        .sort((a, b) => a.revision - b.revision)
        .flatMap((c) => c.changes);
    } catch {
      // The list of changes is informational; the review itself still works.
      return [];
    }
  }

  /** Review → Editor (new or edited automation) or → list (re-review). */
  backFromReview(): void {
    const r = this.review;
    // A native confirmation may be open: leaving now would orphan the answer.
    if (r?.approving) return;
    this.#reviewSeq++;
    this.review = null;
    this.view = r && this.editor ? 'editor' : 'list';
  }

  async runTest(): Promise<void> {
    const r = this.review;
    if (!r || r.testing || r.approving) return;
    r.testing = true;
    r.notice = null;
    r.test = undefined;
    r.stale = false;
    try {
      const res = await this.#deps.backend.test({ id: r.id, draft: r.draft });
      if (this.review !== r) return;
      r.test = res;
    } catch (e) {
      if (this.review !== r) return;
      r.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      if (this.review === r) r.testing = false;
    }
  }

  /** The approve button is enabled only after a passed test of exactly this draft (AP-4.4). */
  get canApprove(): boolean {
    const r = this.review;
    return (
      !!r &&
      !!r.test &&
      r.test.passed &&
      !r.testing &&
      !r.approving &&
      !r.stale &&
      !this.resumeBlock
    );
  }

  /**
   * A re-review of a Once schedule that already fired can never be approved as it is (its date is
   * past). Say so and point to Edit instead of looping test and approve.
   */
  get resumeBlock(): string | null {
    const r = this.review;
    if (!r || r.mode !== 'resume') return null;
    const form = formFromDraft(r.draft, this.#deps.now());
    if (!form || form.freq !== 'once') return null;
    const bad = localErrors(form, this.#deps.now()).some(
      (e) => e.code === 'time_passed' || e.code === 'date_out_of_range',
    );
    return bad ? 'This one-time schedule has passed — edit its date, then review again.' : null;
  }

  /** Stale in edit mode: open the editor again from the latest saved version. */
  async reloadEdit(): Promise<void> {
    const r = this.review;
    if (!r?.id || r.approving) return;
    const id = r.id;
    this.closeEditor();
    await this.openEdit(id);
  }

  /**
   * Approve (AP-5). Sends only `{id?, expectedConfigRev?, draft, testId}`; the backend shows the
   * native confirmation and answers `approved`, `canceled` or `stale`.
   */
  async approve(): Promise<void> {
    const r = this.review;
    if (!r || !this.canApprove || !r.test) return;
    // Re-check the clock-dependent rules right before asking (a Once time may have passed).
    // A re-review keeps its stored schedule; `resumeBlock` handles an expired Once there.
    const form = r.mode === 'resume' ? null : formFromDraft(r.draft, this.#deps.now());
    const fresh = form ? localErrors(form, this.#deps.now()) : [];
    if (fresh.length > 0) {
      r.test = undefined;
      r.notice = { kind: 'error', text: errorMessage(fresh[0]) };
      return;
    }
    r.approving = true;
    r.notice = null;
    try {
      const res = await this.#deps.backend.approve({
        id: r.id,
        expectedConfigRev: r.expectedConfigRev,
        draft: r.draft,
        testId: r.test.testId,
      });
      if (this.review !== r) return;
      if (res.status === 'approved') {
        const a = res.automation;
        this.closeEditor();
        this.tab = 'automations';
        this.#highlight(a.id);
        toast.show(
          `${a.name} enabled — background runs won't ask again for rev ${a.profileRevision}`,
          4000,
        );
        await this.load();
      } else if (res.status === 'canceled') {
        // The user stays on the review; the test is still valid, they can approve again.
        r.notice = { kind: 'info', text: 'Approval was canceled. Nothing was changed.' };
      } else {
        await this.#markStale(r);
      }
    } catch (e) {
      if (this.review !== r) return;
      if (codeOf(e) === 'stale') await this.#markStale(r);
      else r.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      if (this.review === r) r.approving = false;
    }
  }

  /**
   * The backend says the profile or automation changed. The test is void. Re-read the stored
   * automation so the next approval carries its CURRENT `configRev` (otherwise every retry would
   * be stale again); a new test is still required.
   */
  async #markStale(r: ReviewState): Promise<void> {
    r.stale = true;
    r.test = undefined;
    r.notice = { kind: 'error', text: STALE_TEXT };
    await this.load();
    // New: nothing stored. Edit: do NOT adopt the newer revision under the user's old draft (that
    // would overwrite another window's change); the review offers Reload instead.
    if (!r.id || r.mode !== 'resume' || this.review !== r) return;
    try {
      const detail = await this.#deps.backend.get(r.id);
      if (this.review !== r) return;
      r.expectedConfigRev = detail.automation.configRev;
      r.row = this.rows.find((x) => x.id === r.id) ?? r.row;
      r.draft = {
        name: detail.automation.name,
        profileId: detail.automation.profileId,
        policy: detail.policy,
      };
      if (r.row) {
        r.changes = await this.#profileChanges(r.row);
        r.approvedRevision = r.row.approvedRevision;
      }
    } catch {
      // Keep the old revision: the next approval then reports stale again, with the same text.
    }
  }

  #highlight(id: string): void {
    if (this.#highlightTimer) clearTimeout(this.#highlightTimer);
    this.highlightId = id;
    this.#highlightTimer = setTimeout(() => {
      this.highlightId = null;
      this.#highlightTimer = null;
    }, 5000);
  }

  // ── History (basic list; the detail view and its actions are P1.6) ──

  async loadHistory(filter: HistoryFilter = this.history.filter): Promise<void> {
    this.history.filter = filter;
    this.history.loading = true;
    const seq = ++this.#historySeq;
    try {
      const page = await this.#deps.backend.history({ filter, limit: 50 });
      if (seq !== this.#historySeq) return;
      this.history.runs = page.runs;
      this.history.next = page.next;
      this.history.error = null;
    } catch (e) {
      if (seq !== this.#historySeq) return;
      this.history.error = automationErrorMessage(codeOf(e));
    } finally {
      if (seq === this.#historySeq) this.history.loading = false;
    }
  }

  showHistory(filter: HistoryFilter): void {
    this.tab = 'history';
    void this.loadHistory(filter);
  }
}

export const automation = new AutomationStore();
