/**
 * FER-484: state of the Alt+Down folder history dropdown. Opening reads the pane's tab history and,
 * only while activity recording is on and acknowledged, the activity store. Nothing is recorded here.
 */
import { activityFrequent, activityRecent } from '../ipc/activity';
import { activity } from './activityStore.svelte';
import { getPane } from './panesRegistry.svelte';
import type { PaneId } from '../uiState.svelte';
import {
  buildHistoryList,
  FREQUENT_MAX,
  HISTORY_MAX,
  type HistoryList,
  type HistoryRow,
} from '../historyDropdown';

const EMPTY: HistoryList = { top: [], bottom: [], rows: [] };

class HistoryMenuStore {
  isOpen = $state(false);
  pane = $state<PaneId>(0);
  list = $state<HistoryList>(EMPTY);
  /** Index into `list.rows`. */
  cursor = $state(-1);
  /** The frequent section is withheld (recording off / paused / private / not acknowledged). */
  topWithheld = $state(false);
  #seq = 0;

  /** Whether the activity store may be read for this window right now. */
  get activityAllowed(): boolean {
    return activity.available && activity.recordingActive && activity.disclosureAcked;
  }

  async openFor(pane: PaneId): Promise<void> {
    const api = getPane(pane);
    if (!api) return;
    const seq = ++this.#seq;
    const allowed = this.activityAllowed;
    const back = api.historyBack?.() ?? [];
    const [frequent, recent] = allowed
      ? await Promise.all([
          activityFrequent('folder', FREQUENT_MAX).catch(() => []),
          back.length === 0 ? activityRecent('folder', HISTORY_MAX).catch(() => []) : [],
        ])
      : [[], []];
    if (seq !== this.#seq) return; // closed or reopened while the lists were in flight
    this.pane = pane;
    this.topWithheld = !allowed;
    this.list = buildHistoryList({
      frequent: frequent.map((it) => ({
        path: it.entity.locator,
        count: it.episodeCount,
        lastUtcMs: it.lastUtcMs,
      })),
      tabBack: back,
      current: api.path,
      fallbackRecent: recent.map((it) => it.entity.locator),
    });
    this.cursor = this.list.rows.length - 1; // the current folder, as in Total Commander
    this.isOpen = true;
  }

  toggle(pane: PaneId): void {
    if (this.isOpen) this.close();
    else void this.openFor(pane);
  }

  close(): void {
    this.#seq++;
    this.isOpen = false;
  }

  move(delta: number): void {
    const n = this.list.rows.length;
    if (n === 0) return;
    this.cursor = Math.max(0, Math.min(n - 1, this.cursor + delta));
  }

  select(row: HistoryRow | undefined): void {
    if (!row) return;
    const api = getPane(this.pane);
    this.close();
    api?.openPath?.(row.path);
  }
}

export const historyMenu = new HistoryMenuStore();
