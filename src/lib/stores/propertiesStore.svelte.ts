/**
 * Properties / occupied space / volume / system info dialog state (F2-21).
 * One dialog at a time. Heavy calculations run on the Rust blocking pool so
 * the UI stays responsive; Occupied Space is cancellable and reports running
 * totals, and closing its dialog cancels the walk instead of orphaning it.
 */
import { activePane } from './panesRegistry.svelte';
import {
  entryProperties,
  occupiedSpace,
  cancelOccupiedSpace,
  onOccupiedProgress,
  volumeInfo,
  setAttributes,
  hostInfo,
  type EntryProperties,
  type OccupiedReport,
  type OccupiedProgress,
  type VolumeInfo,
  type HostInfo,
} from '../ipc/properties';
import { FsError } from '../ipc/types';

export type PropertiesDialog =
  | { kind: 'properties'; path: string }
  | { kind: 'attributes'; paths: string[] }
  | { kind: 'occupied'; paths: string[] }
  | { kind: 'volume'; path: string }
  | { kind: 'system' };

/** One selected item in the Change Attributes dialog. */
export interface AttributeItem {
  path: string;
  props: EntryProperties | null;
  /** Why the item could not be read (it is then skipped on apply). */
  error?: string;
}

/** `null` = leave the attribute as it is on every item. */
export interface AttributeChoice {
  readOnly: boolean | null;
  hidden: boolean | null;
}

/** What Apply will do to one item (shown in the preview step). */
export interface AttributePlanRow {
  path: string;
  /** Human-readable per-attribute changes, empty when nothing changes. */
  changes: string[];
  /** Value to write per attribute, `null` = not touched for this item. */
  readOnly: boolean | null;
  hidden: boolean | null;
  error?: string;
}

const yesNo = (v: boolean) => (v ? 'yes' : 'no');

/** Pure preview of the attribute changes, per item. */
export function planAttributeChanges(
  items: AttributeItem[],
  choice: AttributeChoice,
): AttributePlanRow[] {
  return items.map((item) => {
    if (!item.props) {
      return {
        path: item.path,
        changes: [],
        readOnly: null,
        hidden: null,
        error: item.error ?? 'Could not be read',
      };
    }
    const changes: string[] = [];
    let readOnly: boolean | null = null;
    let hidden: boolean | null = null;
    if (choice.readOnly !== null && choice.readOnly !== item.props.readOnly) {
      readOnly = choice.readOnly;
      changes.push(`Read-only: ${yesNo(item.props.readOnly)} → ${yesNo(choice.readOnly)}`);
    }
    if (choice.hidden !== null && choice.hidden !== item.props.hidden) {
      hidden = choice.hidden;
      changes.push(`Hidden: ${yesNo(item.props.hidden)} → ${yesNo(choice.hidden)}`);
    }
    return { path: item.path, changes, readOnly, hidden };
  });
}

/** The shared value of an attribute, or `null` (mixed → leave as is). */
function shared(values: boolean[]): boolean | null {
  return values.length > 0 && values.every((v) => v === values[0]) ? values[0] : null;
}

let scanSeq = 0;

class PropertiesStore {
  current = $state<PropertiesDialog | null>(null);
  loading = $state(false);
  busy = $state(false);
  error = $state('');
  /** Code of the load error, so the UI can explain `unsupported` clearly. */
  errorCode = $state('');
  properties = $state<EntryProperties | null>(null);
  occupied = $state<OccupiedReport | null>(null);
  volume = $state<VolumeInfo | null>(null);
  host = $state<HostInfo | null>(null);

  /** Running totals of the active occupied-space walk. */
  progress = $state<OccupiedProgress | null>(null);
  /** True between Cancel and the (partial) report arriving. */
  cancelling = $state(false);
  private scanId: string | null = null;
  private loadSeq = 0;
  private unlisten: (() => void) | null = null;

  /** Change Attributes dialog: items, choices, stage and apply failures. */
  attributeItems = $state<AttributeItem[]>([]);
  attributeChoice = $state<AttributeChoice>({ readOnly: null, hidden: null });
  attributeStage = $state<'edit' | 'preview'>('edit');
  /** Failed attribute applies, one `path: message` per item. */
  attributeErrors = $state<string[]>([]);
  attributesApplied = $state(0);

  /** Every loaded item can really be changed on this platform. */
  get attributesSupported(): boolean {
    const loaded = this.attributeItems.filter((i) => i.props);
    return loaded.length > 0 && loaded.every((i) => i.props?.attributesEditable);
  }

  get attributePlan(): AttributePlanRow[] {
    return planAttributeChanges(this.attributeItems, this.attributeChoice);
  }

  get hasAttributeChanges(): boolean {
    return this.attributePlan.some((r) => r.changes.length > 0);
  }

  get occupiedRunning(): boolean {
    return this.scanId !== null;
  }

  open(d: PropertiesDialog) {
    this.abortOccupied();
    this.loadSeq += 1;
    this.current = d;
    this.loading = false;
    this.busy = false;
    this.error = '';
    this.errorCode = '';
    this.properties = null;
    this.occupied = null;
    this.volume = null;
    this.host = null;
    this.progress = null;
    this.cancelling = false;
    this.attributeItems = [];
    this.attributeChoice = { readOnly: null, hidden: null };
    this.attributeStage = 'edit';
    this.attributeErrors = [];
    this.attributesApplied = 0;
    if (d.kind === 'properties') void this.loadProperties(d.path);
    if (d.kind === 'attributes') void this.loadAttributeItems(d.paths);
    if (d.kind === 'occupied') void this.loadOccupied(d.paths);
    if (d.kind === 'volume') void this.loadVolume(d.path);
    if (d.kind === 'system') void this.loadHost();
  }

  close() {
    if (this.busy) return;
    // Closing while the walk runs cancels it; the late report is ignored.
    this.abortOccupied();
    this.current = null;
  }

  /** Cancel button: the walk resolves with a partial, `cancelled` report. */
  cancelOccupied() {
    if (this.scanId === null || this.cancelling) return;
    this.cancelling = true;
    void cancelOccupiedSpace(this.scanId);
  }

  private abortOccupied() {
    if (this.scanId !== null) void cancelOccupiedSpace(this.scanId);
    this.scanId = null;
    this.stopListening();
    this.loading = false;
  }

  private stopListening() {
    this.unlisten?.();
    this.unlisten = null;
  }

  private fail(e: unknown) {
    this.loading = false;
    this.error = e instanceof FsError ? e.message : String(e);
    this.errorCode = e instanceof FsError ? e.code : '';
  }

  private async loadProperties(path: string) {
    this.loading = true;
    this.error = '';
    try {
      this.properties = await entryProperties(path);
      this.loading = false;
    } catch (e) {
      this.fail(e);
    }
  }

  private async loadAttributeItems(paths: string[]) {
    const seq = ++this.loadSeq;
    this.loading = true;
    this.error = '';
    const settled = await Promise.allSettled(paths.map((p) => entryProperties(p)));
    // A dialog closed/reopened meanwhile must not receive stale items.
    if (seq !== this.loadSeq || this.current?.kind !== 'attributes') return;
    this.attributeItems = settled.map((r, i) =>
      r.status === 'fulfilled'
        ? { path: paths[i], props: r.value }
        : {
            path: paths[i],
            props: null,
            error: r.reason instanceof FsError ? r.reason.message : String(r.reason),
          },
    );
    const loaded = this.attributeItems.flatMap((i) => (i.props ? [i.props] : []));
    if (loaded.length === 0 && settled.length > 0) {
      const first = settled[0];
      if (first.status === 'rejected') this.fail(first.reason);
    }
    this.attributeChoice = {
      readOnly: shared(loaded.map((p) => p.readOnly)),
      hidden: shared(loaded.map((p) => p.hidden)),
    };
    this.loading = false;
  }

  private async loadOccupied(paths: string[]) {
    this.loading = true;
    this.error = '';
    const scanId = `occ-${Date.now().toString(36)}-${++scanSeq}`;
    this.scanId = scanId;
    try {
      const unlisten = await onOccupiedProgress((p) => {
        if (p.scanId === scanId && this.scanId === scanId) this.progress = p;
      });
      if (this.scanId === scanId) this.unlisten = unlisten;
      else unlisten();
      const report = await occupiedSpace(scanId, paths);
      if (this.scanId !== scanId) return; // closed/replaced: drop the result
      this.occupied = report;
      this.progress = null;
      this.scanId = null;
      this.cancelling = false;
      this.stopListening();
      this.loading = false;
    } catch (e) {
      if (this.scanId !== scanId) return;
      this.scanId = null;
      this.cancelling = false;
      this.stopListening();
      this.fail(e);
    }
  }

  private async loadVolume(path: string) {
    this.loading = true;
    this.error = '';
    try {
      this.volume = await volumeInfo(path);
      this.loading = false;
    } catch (e) {
      this.fail(e);
    }
  }

  /** Host facts are a bonus on top of navigator data; failure is silent. */
  private async loadHost() {
    try {
      this.host = await hostInfo();
    } catch {
      this.host = null;
    }
  }

  /** Edit → preview. Only when something would actually change. */
  reviewAttributes() {
    if (!this.attributesSupported || !this.hasAttributeChanges) return;
    this.attributeStage = 'preview';
  }

  backToEdit() {
    if (this.busy) return;
    this.attributeStage = 'edit';
  }

  /** Apply the previewed changes item by item. A failing item is reported
   *  with its path and never stops the remaining items. */
  async applyAttributes(): Promise<void> {
    const d = this.current;
    if (!d || d.kind !== 'attributes' || this.busy || !this.attributesSupported) return;
    const plan = this.attributePlan.filter((r) => r.changes.length > 0);
    if (plan.length === 0) return;
    this.busy = true;
    this.attributeErrors = [];
    this.error = '';
    const failures: string[] = [];
    let applied = 0;
    for (const row of plan) {
      try {
        await setAttributes(row.path, row.readOnly, row.hidden);
        applied += 1;
      } catch (e) {
        failures.push(`${row.path}: ${e instanceof FsError ? e.message : String(e)}`);
      }
    }
    this.busy = false;
    this.attributeErrors = failures;
    this.attributesApplied = applied;
    // Re-read so the dialog shows what is really on disk, even after a
    // partial failure; the choices keep what the user asked for.
    const choice = { ...this.attributeChoice };
    await this.loadAttributeItems(d.paths);
    this.attributeChoice = choice;
    this.attributeStage = failures.length > 0 ? 'preview' : 'edit';
  }
}

/** System facts for the read-only System Information dialog. */
export interface SystemInfo {
  userAgent: string;
  platform: string;
  language: string;
  cpuThreads: number;
  memoryGB: number | null;
  screen: string;
}

export function systemInfo(navigatorObj: Navigator = globalThis.navigator): SystemInfo {
  const nav = navigatorObj as Navigator & { deviceMemory?: number };
  return {
    userAgent: nav.userAgent,
    platform: nav.platform ?? '',
    language: nav.language,
    cpuThreads: nav.hardwareConcurrency ?? 0,
    memoryGB: typeof nav.deviceMemory === 'number' ? nav.deviceMemory : null,
    screen: typeof window !== 'undefined' ? `${window.screen.width}x${window.screen.height}` : '',
  };
}

export const propertiesStore = new PropertiesStore();

// ── openers bound to the active pane ─────────────────────────────────────

export function openProperties(): void {
  const pane = activePane();
  const selected = pane?.selectedPaths() ?? [];
  if (selected.length !== 1) return;
  propertiesStore.open({ kind: 'properties', path: selected[0] });
}

export function openAttributes(): void {
  const pane = activePane();
  const selected = pane?.selectedPaths() ?? [];
  if (selected.length === 0) return;
  propertiesStore.open({ kind: 'attributes', paths: selected });
}

export function openOccupiedSpace(): void {
  const pane = activePane();
  const paths = pane?.selectedPaths() ?? [];
  if (paths.length === 0) return;
  propertiesStore.open({ kind: 'occupied', paths });
}

export function openVolumeInfo(): void {
  const pane = activePane();
  const path = pane?.path;
  if (!path) return;
  propertiesStore.open({ kind: 'volume', path });
}

export function openSystemInfo(): void {
  propertiesStore.open({ kind: 'system' });
}
