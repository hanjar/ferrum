/**
 * Synchronize Directories planning (F2-19). A sync plan is a classified
 * dry-run derived from a Compare Directories report: every entry gets an
 * action, the preview shows all of them, and execution is a separate,
 * explicit step that first re-validates the snapshot against a fresh
 * compare. Pure and unit-tested.
 */
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from './ipc/compare';
import {
  isBaselineTrusted,
  sideChange,
  type BaselineEntry,
  type SyncBaseline,
} from './syncBaseline';

export type SyncDirection = 'leftToRight' | 'rightToLeft' | 'both';

export type SyncAction =
  | 'copyToRight'
  | 'copyToLeft'
  | 'updateRight'
  | 'updateLeft'
  | 'deleteRight'
  | 'deleteLeft'
  | 'conflict'
  | 'skip';

/** Mengapa sebuah item menjadi `conflict` (spec AI-10 bagian 6). */
export type SyncConflictReason =
  | 'noBaseline' // path sama, isi berbeda, tidak ada bukti siapa yang berubah
  | 'bothChanged' // baseline valid, kedua sisi berubah
  | 'modifyDelete' // satu sisi dihapus, sisi lain dimodifikasi
  | 'deletedLeft' // baseline valid, kiri hilang; tidak ada propagasi delete
  | 'deletedRight'
  | 'caseCollision' // nama beda huruf besar/kecil saja
  | 'typeMismatch'; // satu sisi folder, sisi lain file (atau folder dua sisi berbeda)

/** Keputusan pengguna untuk satu konflik (FER-111). */
export type SyncDecision = 'keepLeft' | 'keepRight' | 'skip';

export interface SyncPlanItem {
  name: string;
  state: DirDiffState;
  action: SyncAction;
  reason?: SyncConflictReason;
  /** Entri adalah folder (satu sisi = satu aksi salin/hapus folder). */
  isDir?: boolean;
  /** Keputusan pengguna yang sudah diterapkan pada konflik ini. */
  decision?: SyncDecision;
  leftPath?: string;
  rightPath?: string;
}

export interface SyncOptions {
  direction: SyncDirection;
  /** One-directional mode only: extras on the target side are deleted
   *  (still routed through the delete confirmation before execution). */
  deleteExtras?: boolean;
  /** Baseline pasangan root ini. Tidak ada/tidak tepercaya = first-run. */
  baseline?: SyncBaseline | null;
}

export interface Classification {
  action: SyncAction;
  reason?: SyncConflictReason;
}

const conflict = (reason: SyncConflictReason): Classification => ({ action: 'conflict', reason });

function entryBase(entry: DirDiffEntry, trusted: SyncBaseline | null): BaselineEntry | undefined {
  return trusted?.entries[entry.name];
}

/**
 * Klasifikasi satu entri. Mode searah mengikuti arah eksplisit pengguna
 * (mirror). Mode `both` TIDAK pernah memilih pemenang dari timestamp: tanpa
 * baseline, perbedaan = conflict; dengan baseline, hanya sisi yang terbukti
 * berubah yang boleh menimpa, dan satu sisi hilang tidak pernah menghapus.
 */
export function classifyDetailed(
  entry: DirDiffEntry,
  opts: SyncOptions,
  trusted: SyncBaseline | null = null,
): Classification {
  const both = opts.direction === 'both';
  const base = both ? entryBase(entry, trusted) : undefined;
  const left = { size: entry.leftSize, modifiedMs: entry.leftModifiedMs };
  const right = { size: entry.rightSize, modifiedMs: entry.rightModifiedMs };

  // Folder hanya sah sebagai entri satu-sisi (satu aksi) atau 'same'.
  if (
    entry.isDir &&
    entry.state !== 'same' &&
    entry.state !== 'leftOnly' &&
    entry.state !== 'rightOnly'
  ) {
    return conflict('typeMismatch');
  }

  switch (entry.state) {
    case 'same':
      return { action: 'skip' };
    case 'leftOnly':
      if (opts.direction === 'rightToLeft') {
        return { action: opts.deleteExtras ? 'deleteLeft' : 'skip' };
      }
      if (base) {
        // Kanan pernah ada (baseline) lalu hilang: bukan "file baru".
        return sideChange(base.left, left) === 'changed'
          ? conflict('modifyDelete')
          : conflict('deletedRight');
      }
      return { action: 'copyToRight' };
    case 'rightOnly':
      if (opts.direction === 'leftToRight') {
        return { action: opts.deleteExtras ? 'deleteRight' : 'skip' };
      }
      if (base) {
        return sideChange(base.right, right) === 'changed'
          ? conflict('modifyDelete')
          : conflict('deletedLeft');
      }
      return { action: 'copyToLeft' };
    case 'newerLeft':
      if (opts.direction === 'rightToLeft') return { action: 'skip' };
      if (opts.direction === 'leftToRight') return { action: 'updateRight' };
      return resolveBoth(base, left, right);
    case 'newerRight':
      if (opts.direction === 'leftToRight') return { action: 'skip' };
      if (opts.direction === 'rightToLeft') return { action: 'updateLeft' };
      return resolveBoth(base, left, right);
    case 'sizeDifferent':
    case 'contentDifferent':
      // One-directional overwrite is the user's explicit direction; 'both'
      // cannot decide without proof of which side changed.
      if (opts.direction === 'leftToRight') return { action: 'updateRight' };
      if (opts.direction === 'rightToLeft') return { action: 'updateLeft' };
      return resolveBoth(base, left, right);
  }
}

function resolveBoth(
  base: BaselineEntry | undefined,
  left: { size: number; modifiedMs: number },
  right: { size: number; modifiedMs: number },
): Classification {
  if (!base) return conflict('noBaseline');
  const l = sideChange(base.left, left);
  const r = sideChange(base.right, right);
  if (l === 'changed' && r === 'unchanged') return { action: 'updateRight' };
  if (r === 'changed' && l === 'unchanged') return { action: 'updateLeft' };
  if (l === 'changed' && r === 'changed') return conflict('bothChanged');
  // FER-414: baseline mencatat kedua sisi terverifikasi identik (isi) dan keduanya tidak
  // berubah sejak itu. Beda mtime/state yang tersisa hanya artefak copy (mtime tidak
  // dipertahankan) => tidak ada yang perlu disalin; bukan konflik.
  if (l === 'unchanged' && r === 'unchanged') return { action: 'skip' };
  return conflict('noBaseline'); // satu sisi hilang/tidak terbaca: tidak bisa dijelaskan
}

/** Backward-compatible: aksi saja (tanpa baseline, first-run). */
export function classifyEntry(
  entry: DirDiffEntry,
  _leftRoot: string,
  _rightRoot: string,
  opts: SyncOptions,
): SyncAction {
  const trusted =
    opts.baseline && isBaselineTrusted(opts.baseline, _leftRoot, _rightRoot) ? opts.baseline : null;
  return classifyDetailed(entry, opts, trusted).action;
}

/** Alasan yang membuat sebuah plan tidak boleh dijalankan apa adanya. */
export function syncPlanBlockers(report: CompareDirsReport): string[] {
  const reasons: string[] = [];
  if (report.truncated) reasons.push('truncated');
  if (report.cancelled) reasons.push('cancelled');
  if (report.errors && report.errors.length > 0) reasons.push('errors');
  return reasons;
}

/** Pesan manusiawi untuk blocker; kosong bila plan boleh dipakai. */
export function describeBlockers(report: CompareDirsReport): string {
  const parts: string[] = [];
  if (report.truncated) {
    parts.push('the comparison was truncated (too many entries), so the plan would be incomplete');
  }
  if (report.cancelled) parts.push('the comparison was cancelled, so the result is partial');
  const errs = report.errors ?? [];
  if (errs.length > 0) {
    const sample = errs
      .slice(0, 3)
      .map((e) => `${e.name}: ${e.message}`)
      .join('; ');
    parts.push(
      `${errs.length} entr${errs.length === 1 ? 'y' : 'ies'} could not be read (${sample}${errs.length > 3 ? '; …' : ''})`,
    );
  }
  return parts.length ? `Synchronization blocked: ${parts.join('; ')}.` : '';
}

const UNRESOLVABLE: readonly SyncConflictReason[] = ['typeMismatch', 'caseCollision'];

/** True bila pengguna boleh memilih keepLeft/keepRight untuk konflik ini. */
export function isResolvable(item: SyncPlanItem): boolean {
  return item.action === 'conflict' && !(item.reason && UNRESOLVABLE.includes(item.reason));
}

/**
 * Terapkan keputusan pengguna (per nama) ke plan. Konflik tanpa keputusan
 * tetap `conflict` (tidak dieksekusi, tetap terlihat). keepLeft = isi kiri
 * menang (salin ke kanan; bila kiri tidak ada = hapus kanan), keepRight
 * sebaliknya. Hapus tetap lewat dialog konfirmasi hapus.
 */
export function applyDecisions(
  plan: readonly SyncPlanItem[],
  decisions: Readonly<Record<string, SyncDecision>>,
): SyncPlanItem[] {
  return plan.map((item) => {
    const decision = decisions[item.name];
    if (item.action !== 'conflict' || !decision) return item;
    if (decision === 'skip') return { ...item, action: 'skip', decision };
    if (!isResolvable(item)) return item;
    let action: SyncAction;
    if (decision === 'keepLeft') {
      action =
        item.state === 'rightOnly'
          ? 'deleteRight'
          : item.state === 'leftOnly'
            ? 'copyToRight'
            : 'updateRight';
    } else {
      action =
        item.state === 'leftOnly'
          ? 'deleteLeft'
          : item.state === 'rightOnly'
            ? 'copyToLeft'
            : 'updateLeft';
    }
    return { ...item, action, decision };
  });
}

/** Build the full plan. Entries are kept in report order; both side paths
 *  are always resolved (the missing side is the copy DESTINATION). */
export function buildSyncPlan(report: CompareDirsReport, opts: SyncOptions): SyncPlanItem[] {
  const sep = report.leftRoot.includes('\\') || report.rightRoot.includes('\\') ? '\\' : '/';
  const trusted =
    opts.baseline && isBaselineTrusted(opts.baseline, report.leftRoot, report.rightRoot)
      ? opts.baseline
      : null;

  // Nama yang hanya beda huruf besar/kecil bisa menimpa file yang sama pada
  // filesystem case-insensitive: dijaga konservatif di semua platform.
  const byFold = new Map<string, number>();
  for (const entry of report.entries) {
    const key = entry.name.toLowerCase();
    byFold.set(key, (byFold.get(key) ?? 0) + 1);
  }

  return report.entries.map((entry) => {
    const rel = sep === '\\' ? entry.name.replace(/\//g, '\\') : entry.name;
    let { action, reason } = classifyDetailed(entry, opts, trusted);
    if (action !== 'skip' && (byFold.get(entry.name.toLowerCase()) ?? 0) > 1) {
      action = 'conflict';
      reason = 'caseCollision';
    }
    return {
      name: entry.name,
      state: entry.state,
      action,
      ...(reason ? { reason } : {}),
      ...(entry.isDir ? { isDir: true } : {}),
      leftPath: `${report.leftRoot}${sep}${rel}`,
      rightPath: `${report.rightRoot}${sep}${rel}`,
    };
  });
}

export interface PlanSummary {
  total: number;
  toRight: number;
  toLeft: number;
  /** Subset of toRight/toLeft that replaces an existing target file. */
  overwrites: number;
  deletes: number;
  conflicts: number;
  skipped: number;
  /** Subset of skipped that the user explicitly chose to skip (conflict). */
  userSkipped: number;
}

export function planSummary(items: readonly SyncPlanItem[]): PlanSummary {
  const s: PlanSummary = {
    total: items.length,
    toRight: 0,
    toLeft: 0,
    overwrites: 0,
    deletes: 0,
    conflicts: 0,
    skipped: 0,
    userSkipped: 0,
  };
  for (const item of items) {
    if (item.action === 'updateRight' || item.action === 'updateLeft') s.overwrites++;
    if (item.action === 'copyToRight' || item.action === 'updateRight') s.toRight++;
    else if (item.action === 'copyToLeft' || item.action === 'updateLeft') s.toLeft++;
    else if (item.action === 'deleteRight' || item.action === 'deleteLeft') s.deletes++;
    else if (item.action === 'conflict') s.conflicts++;
    else {
      s.skipped++;
      if (item.decision === 'skip') s.userSkipped++;
    }
  }
  return s;
}

/** Raw per-entry facts independent of state derivation, so a stale check
 *  can compare snapshots even when the fresh compare used a different
 *  by-content mode. */
export function planFacts(report: CompareDirsReport, withState = false): string[] {
  // FER-472: a folder's own mtime is not a plan input (a folder is only 'same' or one-sided),
  // moves whenever a child is added, and a directory listing reports it lazily. It must not
  // make an unchanged plan look stale.
  return report.entries.map(
    (e) =>
      `${e.name}|${e.isDir ? 'd' : 'f'}|${e.leftSize}|${e.isDir ? 0 : e.leftModifiedMs}|${e.rightSize}|${
        e.isDir ? 0 : e.rightModifiedMs
      }` + (withState ? `|${e.state}` : ''),
  );
}

/** True when the fresh compare no longer matches the snapshot the plan was
 *  built from (files changed/appeared/disappeared since the preview). */
export function isPlanStale(
  previous: CompareDirsReport,
  fresh: CompareDirsReport,
  opts: { byContent?: boolean } = {},
): boolean {
  if (previous.leftRoot !== fresh.leftRoot || previous.rightRoot !== fresh.rightRoot) return true;
  if (Boolean(previous.recursive) !== Boolean(fresh.recursive)) return true;
  // Mode content: mtime+size sama TIDAK cukup (isi bisa berubah), jadi state
  // hasil bandingkan isi ikut dibandingkan.
  const a = planFacts(previous, opts.byContent);
  const b = planFacts(fresh, opts.byContent);
  return a.length !== b.length || a.some((fact, i) => fact !== b[i]);
}
