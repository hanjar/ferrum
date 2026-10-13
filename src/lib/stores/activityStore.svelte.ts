/**
 * Reactive cache over the activity IPC for sidebar/settings surfaces.
 * `noteVisit()` debounces refreshes so rapid navigation doesn't hammer the
 * store; browser dev simply stays `available === false`.
 *
 * Privacy semantics (kept distinct on purpose):
 *  - pause: stop new writes everywhere, history untouched.
 *  - private session: THIS window only — no writes from it and it shows no
 *    historical suggestions; other windows are unaffected.
 *  - hide: one item leaves suggestions, statistics are kept.
 *  - forget / exclude / clear: remove history effects (files and pins stay).
 * The privacy epoch only ever moves forward; any response or event carrying
 * an older epoch is dropped so late queries cannot resurrect cleared data.
 */
import {
  activityAckDisclosure,
  activityAddFavorite,
  activityClear,
  activityExclude,
  activityFavorites,
  activityForget,
  activityFrequent,
  activityRecent,
  activityRemoveFavorite,
  activitySetHidden,
  activitySetRecording,
  activitySetRetention,
  activityState,
  onActivityChanged,
  recordActivityVisit,
  type ActivityFavoriteDto,
  type ActivityItemDto,
  type ActivityStateDto,
  type RetentionDays,
} from '../ipc/activity';

const SIDEBAR_LIMIT = 5;
const REFRESH_DEBOUNCE_MS = 400;
/** Lists fetched across an epoch bump are inconsistent; retry this many times. */
const EPOCH_RETRIES = 2;

class ActivityStore {
  recent = $state<ActivityItemDto[]>([]);
  frequent = $state<ActivityItemDto[]>([]);
  favorites = $state<ActivityFavoriteDto[]>([]);
  recording = $state(true);
  privacyEpoch = $state(0);
  retentionDays = $state<number>(90);
  hiddenCount = $state(0);
  /**
   * Fail-closed: NOT acknowledged until the backend state says so (review N1). The first-run banner
   * cannot flash because `needsDisclosure` also requires `available`, which is only set once state arrives;
   * the AI offer (`activity.query`) stays off until then.
   */
  disclosureAcked = $state(false);
  /** Per window (each webview has its own JS context). Never persisted. */
  privateSession = $state(false);
  /** False in browser dev or when the Rust store failed to open. */
  available = $state(false);

  #timer: ReturnType<typeof setTimeout> | null = null;
  #seq = 0;
  #unlisten: (() => void) | null = null;
  #listening = false;

  /** Writes from this window happen only when neither pause nor private is on. */
  get recordingActive(): boolean {
    return this.recording && !this.privateSession;
  }

  /** First recording run: short disclosure until the user acknowledges it. */
  get needsDisclosure(): boolean {
    return this.available && this.recording && !this.disclosureAcked;
  }

  /**
   * Apply a service state. Returns false (and changes nothing) when it is
   * older than what this window already knows — the stale-epoch guard.
   */
  applyState(state: ActivityStateDto): boolean {
    if (state.privacyEpoch < this.privacyEpoch) return false;
    this.recording = state.recordingEnabled;
    this.privacyEpoch = state.privacyEpoch;
    if (state.retentionDays !== undefined) this.retentionDays = state.retentionDays;
    if (state.hiddenCount !== undefined) this.hiddenCount = state.hiddenCount;
    if (state.disclosureAcked !== undefined) this.disclosureAcked = state.disclosureAcked;
    return true;
  }

  async refresh(): Promise<void> {
    const seq = ++this.#seq;
    for (let attempt = 0; attempt <= EPOCH_RETRIES; attempt++) {
      const before = await activityState();
      if (seq !== this.#seq) return; // a newer refresh owns the result
      if (before === null) {
        this.available = false;
        return;
      }
      const priv = this.privateSession;
      const [recent, frequent, favorites] = await Promise.all([
        priv ? Promise.resolve([]) : activityRecent('folder', SIDEBAR_LIMIT),
        priv ? Promise.resolve([]) : activityFrequent('folder', SIDEBAR_LIMIT),
        activityFavorites(),
      ]);
      const after = await activityState();
      if (seq !== this.#seq) return;
      if (after === null) {
        this.available = false;
        return;
      }
      // Epoch moved while the lists were in flight → they may predate a
      // clear/exclude/forget; discard and read again.
      if (before.privacyEpoch !== after.privacyEpoch) continue;
      if (!this.applyState(after)) return; // older than what we already know
      this.available = true;
      this.recent = this.privateSession ? [] : recent;
      this.frequent = this.privateSession ? [] : frequent;
      this.favorites = favorites;
      return;
    }
  }

  isFavorite(path: string): boolean {
    const normalized = path.trim().replace(/[\\/]+$/, '');
    return this.favorites.some((f) => f.path.replace(/[\\/]+$/, '') === normalized);
  }

  /** Pin/unpin the given item; resolves to whether it is now pinned. */
  async toggleFavorite(path: string, kind: 'folder' | 'file'): Promise<boolean> {
    try {
      if (this.isFavorite(path)) {
        await activityRemoveFavorite(path);
      } else {
        await activityAddFavorite(path, kind);
      }
      await this.refresh();
    } catch (e) {
      // Pin failures stay quiet in the UI but must not vanish for debugging.
      console.error('[activity] toggleFavorite failed', path, e);
    }
    return this.isFavorite(path);
  }

  /** Call after every successful visit; coalesces into one refresh. */
  noteVisit(): void {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.refresh().catch(() => undefined);
    }, REFRESH_DEBOUNCE_MS);
  }

  /**
   * Call after a rename/move/delete succeeded. The backend applies the change
   * to the activity store on its worker queue (and emits no event), so the
   * cached Recent/Frequent labels would stay stale until the next visit.
   * Debounced; independent of the recording switch (structural updates only
   * touch rows that already exist).
   */
  noteFsChange(): void {
    this.noteVisit();
  }

  /**
   * Deliberate-visit receipt gated by this window's privacy mode. Paused or
   * private windows write nothing; the visit is also not refreshed into
   * suggestions.
   */
  recordVisit(path: string): void {
    if (!this.recordingActive) return;
    recordActivityVisit(path, 'folder');
    this.noteVisit();
  }

  /**
   * A file the user deliberately opened through the OS association (Enter
   * path); call only AFTER the OS accepted the launch. Same gating as folders.
   */
  recordFileOpen(path: string): void {
    if (!this.recordingActive) return;
    recordActivityVisit(path, 'file');
    this.noteVisit();
  }

  /** Per-window private session; hides suggestions immediately. */
  setPrivateSession(on: boolean): void {
    this.privateSession = on;
    if (on) {
      this.recent = [];
      this.frequent = [];
    }
    void this.refresh().catch(() => undefined);
  }

  async setRecording(enabled: boolean): Promise<void> {
    const state = await activitySetRecording(enabled);
    if (state) this.applyState(state);
  }

  async setRetention(days: RetentionDays): Promise<void> {
    const state = await activitySetRetention(days);
    if (state) this.applyState(state);
    await this.refresh();
  }

  async ackDisclosure(): Promise<void> {
    this.disclosureAcked = true; // immediate; service confirms below
    const state = await activityAckDisclosure();
    if (state) this.applyState(state);
  }

  /** Hide one suggestion; statistics stay. Local lists update immediately. */
  async hideSuggestion(entityId: string, hidden = true): Promise<void> {
    if (hidden) {
      this.recent = this.recent.filter((i) => i.entity.id !== entityId);
      this.frequent = this.frequent.filter((i) => i.entity.id !== entityId);
    }
    const state = await activitySetHidden(entityId, hidden);
    if (state) this.applyState(state);
    await this.refresh();
  }

  /** Remove one item's history effects; files and pins are untouched. */
  async forget(entityId: string): Promise<void> {
    this.recent = this.recent.filter((i) => i.entity.id !== entityId);
    this.frequent = this.frequent.filter((i) => i.entity.id !== entityId);
    await activityForget(entityId);
    await this.refresh();
  }

  async clearAll(): Promise<void> {
    this.recent = [];
    this.frequent = [];
    await activityClear();
    await this.refresh();
  }

  async excludeRoot(path: string): Promise<void> {
    await activityExclude(path);
    await this.refresh();
  }

  /**
   * Subscribe to privacy changes made in any window. A newer epoch applies
   * immediately and triggers a refresh; an older one is ignored. Idempotent.
   */
  async startSync(): Promise<void> {
    if (this.#listening) return;
    this.#listening = true;
    try {
      this.#unlisten = await onActivityChanged((state) => {
        if (!this.applyState(state)) return;
        // Cached suggestions may predate the change: drop, then re-read.
        this.recent = [];
        this.frequent = [];
        void this.refresh().catch(() => undefined);
      });
    } catch {
      this.#listening = false;
    }
  }

  stopSync(): void {
    this.#unlisten?.();
    this.#unlisten = null;
    this.#listening = false;
  }
}

export const activity = new ActivityStore();
