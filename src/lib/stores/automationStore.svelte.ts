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
 * - Run now / Retry are NOT wired here (blocked by FER-564): this store never calls `runNow`.
 */
import { toast } from './toastStore.svelte';
import {
  AutomationError,
  defaultBackend,
  type AutomationBackend,
  type AutomationDraft,
  type AutomationRow,
  type AutomationRun,
  type AutomationTestResult,
  type HistoryFilter,
  type NextRun,
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
  formFromDraft,
  localErrors,
  mergeErrors,
  newScheduleForm,
  toDraft,
  type ScheduleForm,
} from '../components/automation/automationModel';

export type AutomationView = 'list' | 'editor' | 'review';
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

export interface StoreDeps {
  backend: AutomationBackend;
  profiles: SyncProfilesBackend;
  /** Delay before an editor change is validated; 0 = immediately (tests). */
  validateDelayMs: number;
  now: () => number;
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

  #deps: StoreDeps;
  #loadSeq = 0;
  #validateSeq = 0;
  #validateTimer: ReturnType<typeof setTimeout> | null = null;
  #highlightTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(deps?: Partial<StoreDeps>) {
    this.#deps = {
      backend: deps?.backend ?? defaultBackend(),
      profiles: deps?.profiles ?? defaultProfilesBackend(),
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
    this.#validateSeq++;
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

  async toggleEnabled(row: AutomationRow): Promise<void> {
    this.notice = null;
    try {
      const res = await this.#deps.backend.setEnabled(row.id, !row.enabled);
      if (res.status === 'needsChoice') {
        this.notice = {
          kind: 'info',
          text: 'A run is in progress. Wait for it to finish, then pause the automation.',
        };
      } else if (res.status === 'canceled') {
        this.notice = {
          kind: 'info',
          text: 'Canceled in the confirmation dialog. Nothing was changed.',
        };
      }
    } catch (e) {
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    }
    await this.load();
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
    try {
      const detail = await this.#deps.backend.get(row.id);
      let changes: FieldChange[] = [];
      if (row.review?.reason === 'profileChanged') {
        try {
          const profile = await this.#deps.profiles.get(row.profileId);
          const since = row.approvedRevision ?? row.review.from;
          changes = profile.changes
            .filter((c) => c.revision > since)
            .sort((a, b) => a.revision - b.revision)
            .flatMap((c) => c.changes);
        } catch {
          // The list of changes is informational; the review itself still works.
        }
      }
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
      this.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    }
  }

  /** Review → Editor (new or edited automation) or → list (re-review). */
  backFromReview(): void {
    const r = this.review;
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
    return !!r && !!r.test && r.test.passed && !r.testing && !r.approving && !r.stale;
  }

  /**
   * Approve (AP-5). Sends only `{id?, expectedConfigRev?, draft, testId}`; the backend shows the
   * native confirmation and answers `approved`, `canceled` or `stale`.
   */
  async approve(): Promise<void> {
    const r = this.review;
    if (!r || !this.canApprove || !r.test) return;
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
        this.#markStale(r);
      }
    } catch (e) {
      if (this.review !== r) return;
      if (codeOf(e) === 'stale') this.#markStale(r);
      else r.notice = { kind: 'error', text: automationErrorMessage(codeOf(e)) };
    } finally {
      if (this.review === r) r.approving = false;
    }
  }

  #markStale(r: ReviewState): void {
    r.stale = true;
    r.test = undefined;
    r.notice = { kind: 'error', text: STALE_TEXT };
    void this.load();
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
    try {
      const page = await this.#deps.backend.history({ filter, limit: 50 });
      if (this.history.filter !== filter) return;
      this.history.runs = page.runs;
      this.history.next = page.next;
      this.history.error = null;
    } catch (e) {
      this.history.error = automationErrorMessage(codeOf(e));
    } finally {
      this.history.loading = false;
    }
  }

  showHistory(filter: HistoryFilter): void {
    this.tab = 'history';
    void this.loadHistory(filter);
  }
}

export const automation = new AutomationStore();
