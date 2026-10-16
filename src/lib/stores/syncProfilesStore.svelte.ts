/**
 * Sync profiles page state (Automation P0.5): the list, the editor, the preview and the manual run.
 *
 * Safety rules this store enforces structurally (design §10.3, SE-1, Q4):
 * - It only ever sends the DRAFT or ids to the backend. No display text for any native dialog, no
 *   "confirmed" flag: permanent-delete and delete-guard confirmations are native dialogs the
 *   backend raises itself, and their answer comes back as a result code (`canceled`, `busy`).
 * - A run needs a fresh preview of the SAME profile revision. `confirmOverwrite: true` is sent only
 *   from `confirmOverwrite()`, which exists only while the overwrite prompt (`runPrompt`) is open;
 *   `requestRun()` opens that prompt whenever the preview lists overwrites.
 * - Readiness, validation errors and the guard verdict are rendered as the backend reported them.
 */
import { isInTauri } from '../ipc/tauri';
import {
  SYNC_PROFILE_RUN_EVENT,
  SyncProfilesError,
  defaultBackend,
  type FieldError,
  type ProfileDto,
  type ProfileSummary,
  type Readiness,
  type RevisionChanges,
  type RootIdentity,
  type SyncPreview,
  type SyncProfilesBackend,
  type SyncRunCounts,
  type SyncRunEvent,
  type SyncRunResult,
} from '../ipc/syncProfiles';
import {
  emptyForm,
  fromPolicy,
  ipcErrorMessage,
  toDraft,
  usedByMessage,
  type IpcOp,
  type ProfileForm,
} from '../components/sync/syncProfileModel';

export interface Notice {
  kind: 'info' | 'success' | 'error';
  text: string;
}

export interface EditorState {
  /** Absent while creating a new profile. */
  id?: string;
  /** The revision the editor loaded (SP-6.2); sent back as `expectedRevision`. */
  revision?: number;
  form: ProfileForm;
  changes: RevisionChanges[];
  identities: { left?: RootIdentity; right?: RootIdentity };
  kinds: { left?: string; right?: string };
  /** Readiness of the SAVED profile (never recomputed here). */
  readiness?: Readiness;
  /** Errors of the last validation of the CURRENT form. */
  errors: FieldError[];
  /** True once a validation of the current form came back (errors above are then meaningful). */
  validated: boolean;
  busy: 'validating' | 'saving' | null;
  dirty: boolean;
  notice: Notice | null;
}

export interface PreviewState {
  profileId: string;
  revision: number;
  data: SyncPreview;
  /**
   * Delete policy CODE of the previewed revision, read from the stored profile. Absent when it
   * could not be read or its revision differs: then the run is treated as non-permanent.
   */
  deletePolicy?: string;
}

export interface RunState {
  profileId: string;
  status: 'starting' | 'running' | 'finished' | 'failed';
  runId?: number;
  phase?: SyncRunEvent['phase'];
  counts?: SyncRunCounts;
  result?: SyncRunResult;
  error?: Notice;
  /** The progress listener could not be set up: the result will not arrive by itself. */
  progressUnavailable?: boolean;
}

export interface StoreDeps {
  backend: SyncProfilesBackend;
  /** Subscribe to `sync-profile://run`; resolves to an unsubscribe function. */
  subscribe: (cb: (e: SyncRunEvent) => void) => Promise<() => void>;
}

async function tauriSubscribe(cb: (e: SyncRunEvent) => void): Promise<() => void> {
  if (!isInTauri()) return () => undefined;
  const { listen } = await import('@tauri-apps/api/event');
  return listen<SyncRunEvent>(SYNC_PROFILE_RUN_EVENT, (e) => cb(e.payload));
}

/** The backend's fixed message for a duplicate profile name (path-free). */
const NAME_TAKEN = 'This name is already in use';

const codeOf = (e: unknown): string => (e instanceof SyncProfilesError ? e.code : 'unknown');

export class SyncProfilesStore {
  profiles = $state<ProfileSummary[]>([]);
  loaded = $state(false);
  loading = $state(false);
  listError = $state<string | null>(null);
  notice = $state<Notice | null>(null);
  editor = $state<EditorState | null>(null);
  /** Profile whose delete awaits the user's confirmation. */
  confirmDeleteId = $state<string | null>(null);
  preview = $state<PreviewState | null>(null);
  previewBusyId = $state<string | null>(null);
  /** Open while the user is asked to confirm the overwrites listed in the preview. */
  runPrompt = $state<{
    profileId: string;
    previewId: string;
    overwrites: number;
    deletes: number;
  } | null>(null);
  /** Set while the user is asked whether to throw away unsaved editor changes. */
  discardTarget = $state<'list' | 'page' | null>(null);
  run = $state<RunState | null>(null);

  #deps: StoreDeps;
  #loadSeq = 0;
  #validateSeq = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #unlisten: (() => void) | null = null;
  #buffer: SyncRunEvent[] | null = null;
  #runId: number | null = null;

  constructor(deps?: Partial<StoreDeps>) {
    this.#deps = {
      backend: deps?.backend ?? defaultBackend(),
      subscribe: deps?.subscribe ?? tauriSubscribe,
    };
  }

  // ── List ──

  async load(): Promise<void> {
    const seq = ++this.#loadSeq;
    this.loading = true;
    try {
      const list = await this.#deps.backend.list();
      if (seq !== this.#loadSeq) return;
      this.profiles = list;
      this.listError = null;
      this.loaded = true;
      // A preview belongs to one revision of one profile; drop it when that changed or is gone.
      const p = this.preview;
      if (p && !list.some((s) => s.id === p.profileId && s.revision === p.revision)) {
        this.preview = null;
        this.runPrompt = null;
      }
      if (this.confirmDeleteId && !list.some((s) => s.id === this.confirmDeleteId)) {
        this.confirmDeleteId = null;
      }
    } catch (e) {
      if (seq !== this.#loadSeq) return;
      this.listError = ipcErrorMessage(codeOf(e), 'load');
      this.loaded = true;
    } finally {
      if (seq === this.#loadSeq) this.loading = false;
    }
  }

  /** True while any readiness is still being computed by the backend (SP-8 budget). */
  get anyChecking(): boolean {
    return this.profiles.some((p) => p.readiness.status === 'checking');
  }

  summary(id: string): ProfileSummary | undefined {
    return this.profiles.find((p) => p.id === id);
  }

  // ── Editor ──

  openNew(prefill: Partial<ProfileForm> = {}): void {
    this.#clearTimer();
    this.#validateSeq++;
    this.notice = null;
    this.editor = {
      form: { ...emptyForm(), ...prefill },
      changes: [],
      identities: {},
      kinds: {},
      errors: [],
      validated: false,
      busy: null,
      dirty: false,
      notice: null,
    };
    if (prefill.left || prefill.right) void this.validate();
  }

  /** SP-1.2: the active pane becomes the left root, the inactive pane the right root. */
  useCurrentPanes(active: string | undefined, inactive: string | undefined): void {
    this.openNew({ left: active ?? '', right: inactive ?? '' });
  }

  async openEdit(id: string): Promise<void> {
    this.notice = null;
    try {
      const profile = await this.#deps.backend.get(id);
      this.#adopt(profile, null);
      await this.validate();
    } catch (e) {
      this.notice = { kind: 'error', text: ipcErrorMessage(codeOf(e), 'load') };
      if (codeOf(e) === 'not_found') void this.load();
    }
  }

  /** Leave the editor; unsaved changes first need the user's say (`discardTarget`). True when it closed. */
  leaveEditor(target: 'list' | 'page'): boolean {
    if (this.editor?.dirty) {
      this.discardTarget = target;
      return false;
    }
    this.closeEditor();
    return true;
  }

  closeEditor(): void {
    this.discardTarget = null;
    this.#clearTimer();
    this.#validateSeq++;
    this.editor = null;
  }

  #adopt(profile: ProfileDto, notice: Notice | null): void {
    this.#clearTimer();
    this.#validateSeq++;
    this.editor = {
      id: profile.id,
      revision: profile.revision,
      form: fromPolicy(profile.name, profile.policy),
      changes: profile.changes,
      identities: profile.identities,
      kinds: {},
      readiness: profile.readiness,
      errors: [],
      validated: false,
      busy: null,
      dirty: false,
      notice,
    };
  }

  /** The form was edited: mark it dirty, forget stale results and validate shortly after. */
  touch(): void {
    const ed = this.editor;
    if (!ed) return;
    ed.dirty = true;
    ed.validated = false;
    ed.notice = null;
    this.#clearTimer();
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.validate();
    }, 250);
  }

  /** Ask the backend to validate the current form. Resolves true when it has no errors. */
  async validate(): Promise<boolean> {
    const ed = this.editor;
    if (!ed) return false;
    this.#clearTimer();
    const seq = ++this.#validateSeq;
    const wasSaving = ed.busy === 'saving';
    if (!wasSaving) ed.busy = 'validating';
    try {
      const res = await this.#deps.backend.validate(toDraft(ed.form));
      if (seq !== this.#validateSeq || this.editor !== ed) return false;
      ed.errors = res.errors;
      ed.identities = res.identities;
      ed.kinds = res.kinds;
      ed.validated = true;
      return res.errors.length === 0;
    } catch (e) {
      if (seq !== this.#validateSeq || this.editor !== ed) return false;
      ed.validated = false;
      ed.notice = { kind: 'error', text: ipcErrorMessage(codeOf(e), 'save') };
      return false;
    } finally {
      if (seq === this.#validateSeq && !wasSaving) ed.busy = null;
    }
  }

  async save(): Promise<void> {
    const ed = this.editor;
    if (!ed || ed.busy === 'saving') return;
    ed.busy = 'saving';
    ed.notice = null;
    try {
      if (!(await this.validate())) return;
      let result;
      try {
        result = await this.#deps.backend.save({
          id: ed.id,
          expectedRevision: ed.revision,
          draft: toDraft(ed.form),
        });
      } catch (e) {
        const code = codeOf(e);
        if (code === 'stale') {
          await this.#reloadAfterStale(ed);
        } else if (
          code === 'invalid_input' &&
          e instanceof SyncProfilesError &&
          e.detail === NAME_TAKEN
        ) {
          // SP-1.3: show it on the name field; the next edit re-validates.
          ed.errors = [{ field: 'name', code: 'name_taken' }];
          ed.validated = true;
        } else if (code === 'invalid_input') {
          ed.notice = { kind: 'error', text: ipcErrorMessage(code, 'save') };
          await this.validate();
        } else {
          ed.notice = { kind: 'error', text: ipcErrorMessage(code, 'save') };
          if (code === 'not_found') void this.load();
        }
        return;
      }
      if (result.status === 'saved') {
        const p = result.profile;
        const changed = ed.revision !== undefined && p.revision === ed.revision;
        this.preview = this.preview?.profileId === p.id ? null : this.preview;
        this.runPrompt = null;
        this.editor = null;
        this.notice = {
          kind: 'success',
          text: changed
            ? `Saved "${p.name}". No policy field changed, so it stays at revision ${p.revision}.`
            : `Saved "${p.name}" as revision ${p.revision}.`,
        };
        await this.load();
      } else if (result.status === 'canceled') {
        ed.notice = { kind: 'info', text: 'Save canceled. Nothing was saved.' };
      } else {
        await this.#reloadAfterStale(ed);
      }
    } finally {
      if (this.editor === ed) ed.busy = null;
    }
  }

  async #reloadAfterStale(ed: EditorState): Promise<void> {
    const note: Notice = { kind: 'error', text: ipcErrorMessage('stale', 'save') };
    if (!ed.id) {
      ed.notice = note;
      return;
    }
    try {
      const fresh = await this.#deps.backend.get(ed.id);
      if (this.editor !== ed) return;
      this.#adopt(fresh, note);
      void this.load();
      await this.validate();
    } catch (e) {
      if (this.editor !== ed) return;
      ed.notice = { kind: 'error', text: ipcErrorMessage(codeOf(e), 'load') };
      if (codeOf(e) === 'not_found') void this.load();
    }
  }

  // ── Delete (SP-9.5) ──

  askDelete(id: string): void {
    this.notice = null;
    this.confirmDeleteId = id;
  }

  cancelDelete(): void {
    this.confirmDeleteId = null;
  }

  async confirmDelete(): Promise<void> {
    const id = this.confirmDeleteId;
    if (!id) return;
    const s = this.summary(id);
    this.confirmDeleteId = null;
    if (!s) return;
    try {
      const res = await this.#deps.backend.delete(id, s.revision);
      if (res.status !== 'deleted') {
        this.notice = { kind: 'error', text: ipcErrorMessage('unknown', 'delete') };
        return;
      }
      if (this.preview?.profileId === id) this.preview = null;
      this.notice = { kind: 'success', text: `Deleted "${s.name}".` };
    } catch (e) {
      const code = codeOf(e);
      this.notice = {
        kind: 'error',
        text:
          code === 'in_use' ? usedByMessage(s.usedBy) : ipcErrorMessage(code, 'delete', s.usedBy),
      };
    }
    await this.load();
  }

  // ── Preview and run (SP-9.4) ──

  async previewProfile(id: string): Promise<void> {
    const s = this.summary(id);
    if (!s || this.previewBusyId) return;
    this.notice = null;
    this.preview = null;
    this.runPrompt = null;
    if (
      this.run?.profileId === id &&
      this.run.status !== 'starting' &&
      this.run.status !== 'running'
    )
      this.run = null;
    this.previewBusyId = id;
    try {
      const data = await this.#deps.backend.preview(id);
      let deletePolicy: string | undefined;
      try {
        const profile = await this.#deps.backend.get(id);
        if (profile.revision === s.revision) deletePolicy = profile.policy.deletePolicy;
      } catch {
        deletePolicy = undefined; // unknown policy: the webview prompt is shown (fail safe)
      }
      this.preview = { profileId: id, revision: s.revision, data, deletePolicy };
    } catch (e) {
      this.#fail('preview', e);
    } finally {
      this.previewBusyId = null;
    }
  }

  /** Whether the run button may be pressed for this profile right now. */
  canRun(id: string): boolean {
    const p = this.preview;
    const s = this.summary(id);
    if (!p || !s || p.profileId !== id || p.revision !== s.revision) return false;
    if (s.readiness.status !== 'ready') return false;
    if (p.data.blockers?.length || !p.data.previewId) return false;
    if (p.data.guard.verdict === 'emptySource') return false;
    if (this.run && (this.run.status === 'starting' || this.run.status === 'running')) return false;
    return true;
  }

  /**
   * "Run now": opens the confirmation prompt when the preview lists overwrites, or deletions that
   * follow a Recycle Bin / versioning policy (SP-9.4: those are confirmed in the webview; the
   * `permanent` policy is confirmed natively by the backend instead). Otherwise starts at once.
   */
  async requestRun(id: string): Promise<void> {
    if (!this.canRun(id)) return;
    const data = this.preview!.data;
    const overwrites = data.summary.overwrites;
    const deletes = data.summary.deletes;
    const permanent = this.preview!.deletePolicy === 'permanent';
    if (overwrites > 0 || (deletes > 0 && !permanent)) {
      this.runPrompt = {
        profileId: id,
        previewId: data.previewId,
        overwrites,
        deletes: permanent ? 0 : deletes,
      };
      return;
    }
    await this.#startRun(id, false);
  }

  /**
   * The user accepted the run prompt. The ONLY caller that can send `confirmOverwrite: true`, and
   * only for the very preview the prompt was opened for.
   */
  async confirmRunPrompt(): Promise<void> {
    const prompt = this.runPrompt;
    this.runPrompt = null;
    if (!prompt || !this.canRun(prompt.profileId)) return;
    if (this.preview?.data.previewId !== prompt.previewId) return;
    await this.#startRun(prompt.profileId, prompt.overwrites > 0);
  }

  cancelRunPrompt(): void {
    this.runPrompt = null;
  }

  async #startRun(id: string, confirmOverwrite: boolean): Promise<void> {
    const preview = this.preview;
    if (!preview || preview.profileId !== id) return;
    this.notice = null;
    this.run = { profileId: id, status: 'starting' };
    this.#stopListening();
    // `sync-profile://run` is broadcast app-wide: hold every event until `run` names OUR run id.
    this.#buffer = [];
    let listening = true;
    try {
      this.#unlisten = await this.#deps.subscribe((e) => this.#onEvent(id, e));
    } catch {
      this.#unlisten = null;
      listening = false;
    }
    try {
      const started = await this.#deps.backend.run({
        id,
        previewId: preview.data.previewId,
        confirmOverwrite,
      });
      // The backend consumed the preview when the run started.
      this.preview = null;
      const held = this.#buffer ?? [];
      this.#buffer = null;
      this.#runId = started.runId;
      this.run = {
        profileId: id,
        status: 'running',
        runId: started.runId,
        ...(listening ? {} : { progressUnavailable: true }),
      };
      for (const e of held) this.#onEvent(id, e);
    } catch (e) {
      this.#buffer = null;
      this.#runId = null;
      this.#stopListening();
      const code = codeOf(e);
      this.run = {
        profileId: id,
        status: 'failed',
        error: { kind: code === 'canceled' ? 'info' : 'error', text: ipcErrorMessage(code, 'run') },
      };
      // `stale` means the preview no longer matches; every other refusal leaves it usable.
      if (code === 'stale') this.preview = null;
      if (code === 'not_found') void this.load();
    }
  }

  #onEvent(id: string, e: SyncRunEvent): void {
    const cur = this.run;
    if (!cur || cur.profileId !== id || cur.status === 'finished' || cur.status === 'failed')
      return;
    if (this.#buffer) {
      this.#buffer.push(e);
      return;
    }
    if (this.#runId === null || e.runId !== this.#runId) return;
    const next: RunState = { ...cur };
    if (e.phase) next.phase = e.phase;
    if (e.counts) next.counts = e.counts;
    if (e.result) {
      next.status = 'finished';
      next.result = e.result;
      next.counts = e.result.counts ?? next.counts;
      this.run = next;
      this.#stopListening();
      void this.load();
      return;
    }
    this.run = next;
  }

  /**
   * Hide the run status. Also the way out of a run whose result never arrived (events missed or
   * the listener failed): it releases the buttons; the list is reloaded to show the current state.
   */
  dismissRun(): void {
    if (!this.run) return;
    this.#stopListening();
    this.#buffer = null;
    this.#runId = null;
    this.run = null;
    void this.load();
  }

  #fail(op: IpcOp, e: unknown): void {
    const code = codeOf(e);
    this.notice = { kind: 'error', text: ipcErrorMessage(code, op) };
    if (code === 'not_found') void this.load();
  }

  #stopListening(): void {
    const off = this.#unlisten;
    this.#unlisten = null;
    off?.();
  }

  #clearTimer(): void {
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
  }

  /** Test hook: back to the initial state. */
  reset(): void {
    this.#clearTimer();
    this.#stopListening();
    this.#loadSeq++;
    this.#validateSeq++;
    this.profiles = [];
    this.loaded = false;
    this.loading = false;
    this.listError = null;
    this.notice = null;
    this.editor = null;
    this.confirmDeleteId = null;
    this.preview = null;
    this.previewBusyId = null;
    this.runPrompt = null;
    this.discardTarget = null;
    this.#buffer = null;
    this.#runId = null;
    this.run = null;
  }
}

export const syncProfiles = new SyncProfilesStore();
