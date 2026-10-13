/**
 * Activity page state: windowed history (7/30/90 days), item detail and the
 * preview-then-confirm flow for every destructive scope. All server reads are
 * guarded by a request sequence AND the privacy epoch so a slow query can
 * never repaint data the user has since cleared/forgotten/excluded.
 */
import {
  activityOverview,
  activityPreview,
  type ActivityOverviewDto,
  type ActivityOverviewItemDto,
  type ActivityQueryKind,
  type ActivitySort,
  type ActivityWindowDays,
  type ClearScope,
  type RetentionDays,
  type ScopePreviewDto,
} from '../ipc/activity';
import { activity } from './activityStore.svelte';

export interface PendingScope {
  scope: ClearScope;
  target?: string;
  /** Human label, e.g. the item name or "semua riwayat". */
  label: string;
  preview: ScopePreviewDto | null;
  /** Epoch the preview was computed under; confirm re-previews if it moved. */
  epoch: number;
  /** True when the epoch moved and the user must look at the new scope. */
  refreshed: boolean;
  busy: boolean;
}

class ActivityPageStore {
  days = $state<ActivityWindowDays>(30);
  kind = $state<ActivityQueryKind>('folder');
  sort = $state<ActivitySort>('recent');
  overview = $state<ActivityOverviewDto | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);
  selectedId = $state<string | null>(null);
  pending = $state<PendingScope | null>(null);

  #seq = 0;

  get selected(): ActivityOverviewItemDto | null {
    return this.overview?.items.find((i) => i.entity.id === this.selectedId) ?? null;
  }

  async load(retries = 2): Promise<void> {
    const seq = ++this.#seq;
    this.loading = true;
    this.error = null;
    try {
      const result = await activityOverview(this.days, this.kind, this.sort);
      if (seq !== this.#seq) return;
      if (result === null) {
        this.overview = null;
        return;
      }
      // Older than what this window knows (cleared/excluded meanwhile): drop
      // and re-read instead of painting stale rows.
      if (result.privacyEpoch < activity.privacyEpoch) {
        if (retries > 0) return this.load(retries - 1);
        return; // never paint a snapshot older than the known epoch
      }
      activity.privacyEpoch = Math.max(activity.privacyEpoch, result.privacyEpoch);
      this.overview = result;
      if (this.selectedId && !result.items.some((i) => i.entity.id === this.selectedId)) {
        this.selectedId = null;
      }
    } catch (e) {
      if (seq === this.#seq) this.error = e instanceof Error ? e.message : String(e);
    } finally {
      if (seq === this.#seq) this.loading = false;
    }
  }

  setDays(days: ActivityWindowDays): Promise<void> {
    this.days = days;
    return this.load();
  }

  setKind(kind: ActivityQueryKind): Promise<void> {
    this.kind = kind;
    this.selectedId = null;
    return this.load();
  }

  setSort(sort: ActivitySort): Promise<void> {
    this.sort = sort;
    return this.load();
  }

  select(id: string | null): void {
    this.selectedId = id;
  }

  /** Step 1 of any destructive scope: compute and show what would go. */
  async requestScope(scope: ClearScope, label: string, target?: string): Promise<void> {
    this.pending = {
      scope,
      target,
      label,
      preview: null,
      epoch: activity.privacyEpoch,
      refreshed: false,
      busy: true,
    };
    const epoch = activity.privacyEpoch;
    const preview = await activityPreview(scope, target).catch(() => null);
    // Cancelled or replaced while computing.
    if (!this.pending || this.pending.scope !== scope || this.pending.target !== target) return;
    this.pending = { ...this.pending, preview, epoch, busy: false };
  }

  cancelScope(): void {
    this.pending = null;
  }

  /**
   * Step 2: execute only if the previewed scope is still current. When the
   * privacy epoch moved (another window cleared/excluded), re-preview and make
   * the user confirm again instead of acting on a stale scope.
   */
  async confirmScope(): Promise<'done' | 'refreshed' | 'none'> {
    const pending = this.pending;
    if (!pending || pending.busy || pending.preview === null) return 'none';
    if (pending.epoch !== activity.privacyEpoch) {
      await this.requestScope(pending.scope, pending.label, pending.target);
      if (this.pending) this.pending = { ...this.pending, refreshed: true };
      return 'refreshed';
    }
    this.pending = { ...pending, busy: true };
    try {
      switch (pending.scope) {
        case 'all':
          await activity.clearAll();
          break;
        case 'forget':
          if (pending.target) await activity.forget(pending.target);
          break;
        case 'exclude':
          if (pending.target) await activity.excludeRoot(pending.target);
          break;
        case 'retention':
          if (pending.target) await activity.setRetention(Number(pending.target) as RetentionDays);
          break;
      }
    } finally {
      this.pending = null;
    }
    if (pending.scope === 'forget' && this.selectedId === pending.target) this.selectedId = null;
    await this.load();
    return 'done';
  }

  /** Hide/unhide never needs a preview: nothing is deleted. */
  async setHidden(id: string, hidden: boolean): Promise<void> {
    await activity.hideSuggestion(id, hidden);
    await this.load();
  }

  reset(): void {
    this.#seq++;
    this.overview = null;
    this.pending = null;
    this.selectedId = null;
    this.error = null;
    this.loading = false;
  }
}

export const activityPage = new ActivityPageStore();
