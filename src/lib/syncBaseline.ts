/**
 * Baseline sync dua arah (AI-10 / FER-212). Baseline = keadaan terakhir yang
 * TERBUKTI sama di kedua sisi untuk satu pasangan root. Hanya modul sync ini
 * yang boleh menulis baseline; tidak ada logika sync di modul AI.
 *
 * File ini murni (tanpa I/O): persistensi lokal berversi ada di Rust
 * (`src-tauri/src/sync_baseline.rs`, FER-414) lewat `ipc/syncBaseline.ts`. Baseline yang tidak
 * tepercaya (versi/bentuk salah, root berbeda/ditukar) diperlakukan sebagai
 * first-run, yaitu mode paling konservatif.
 */
import type { CompareDirsReport, JournalMarks } from './ipc/compare';
import type { SyncPlanItem } from './syncPlan';

export const BASELINE_VERSION = 1;

/** Toleransi mtime yang sama dengan compare.rs (resolusi FAT 2 detik). */
export const MTIME_TOLERANCE_MS = 2000;

export interface BaselineFacts {
  size: number;
  modifiedMs: number;
}

export interface BaselineEntry {
  left: BaselineFacts;
  right: BaselineFacts;
}

export interface SyncBaseline {
  version: typeof BASELINE_VERSION;
  leftRoot: string;
  rightRoot: string;
  entries: Record<string, BaselineEntry>;
  /** FER-477b: journal marks of the run that verified EVERY entry by content. Set only by
   *  `reconcileBaseline` after a fully clean settle compare; absent = next compare reads all. */
  journal?: JournalMarks;
}

declare const verifiedBrand: unique symbol;

/** Baseline hasil `reconcileBaseline` (terverifikasi). Satu-satunya bentuk yang boleh disimpan
 *  ke disk (FER-414): jalur lain tidak bisa mengisinya tanpa cast eksplisit. */
export type VerifiedBaseline = SyncBaseline & { readonly [verifiedBrand]: true };

export type SideChange = 'unchanged' | 'changed' | 'missing';

/** Hasil per item dari job nyata; diisi oleh executor, bukan ditebak. */
export type ItemOutcome = 'ok' | 'failed' | 'cancelled';

function normalizeRoot(root: string): string {
  return root.replace(/\\/g, '/').replace(/\/+$/, '');
}

export function emptyBaseline(leftRoot: string, rightRoot: string): SyncBaseline {
  return { version: BASELINE_VERSION, leftRoot, rightRoot, entries: {} };
}

/** Baseline hanya dipercaya bila bentuknya benar dan terikat pada pasangan
 *  (kiri, kanan) yang sama persis; panel yang ditukar = identitas lain. */
export function isBaselineTrusted(
  baseline: unknown,
  leftRoot: string,
  rightRoot: string,
): baseline is SyncBaseline {
  if (!baseline || typeof baseline !== 'object') return false;
  const b = baseline as Partial<SyncBaseline>;
  if (b.version !== BASELINE_VERSION) return false;
  if (typeof b.leftRoot !== 'string' || typeof b.rightRoot !== 'string') return false;
  if (!b.entries || typeof b.entries !== 'object') return false;
  return (
    normalizeRoot(b.leftRoot) === normalizeRoot(leftRoot) &&
    normalizeRoot(b.rightRoot) === normalizeRoot(rightRoot)
  );
}

export function factsEqual(a: BaselineFacts, b: BaselineFacts): boolean {
  return a.size === b.size && Math.abs(a.modifiedMs - b.modifiedMs) <= MTIME_TOLERANCE_MS;
}

export function sideChange(base: BaselineFacts, now: BaselineFacts | null): SideChange {
  if (!now) return 'missing';
  return factsEqual(base, now) ? 'unchanged' : 'changed';
}

/**
 * Baseline berikutnya setelah satu run. `after` adalah compare SEGAR sesudah
 * job selesai: sebuah item dicatat hanya bila hasilnya terverifikasi sama di
 * kedua sisi (state `same`) dan, untuk aksi tulis, outcome-nya `ok`. Item
 * gagal/cancel/konflik/tidak dijalankan tidak menggeser entri sebelumnya, jadi
 * partial failure tidak pernah menandai pasangan root "sudah sinkron".
 * Tidak memutasi `previous`.
 */
export function reconcileBaseline(
  previous: SyncBaseline | null,
  after: CompareDirsReport,
  plan: readonly SyncPlanItem[],
  outcomes: Readonly<Record<string, ItemOutcome>>,
): VerifiedBaseline {
  const next = emptyBaseline(after.leftRoot, after.rightRoot);
  if (previous && isBaselineTrusted(previous, after.leftRoot, after.rightRoot)) {
    for (const [name, entry] of Object.entries(previous.entries)) {
      next.entries[name] = {
        left: { ...entry.left },
        right: { ...entry.right },
      };
    }
  }
  const afterByName = new Map(after.entries.map((x) => [x.name, x]));

  for (const item of plan) {
    const now = afterByName.get(item.name);
    const outcome = outcomes[item.name];

    if (item.action === 'skip' && item.state === 'same') {
      // Sudah sama sebelum run: terverifikasi bila tetap sama sesudahnya.
      if (now?.state === 'same') next.entries[item.name] = toEntry(now);
      continue;
    }
    if (outcome !== 'ok') continue; // gagal / cancel / tidak dijalankan / konflik

    if (
      item.action === 'copyToRight' ||
      item.action === 'copyToLeft' ||
      item.action === 'updateRight' ||
      item.action === 'updateLeft'
    ) {
      if (now?.state === 'same') next.entries[item.name] = toEntry(now);
    } else if (item.action === 'deleteRight' || item.action === 'deleteLeft') {
      if (!now) delete next.entries[item.name]; // hilang di kedua sisi
    }
  }
  // FER-477b: the marks may only travel with a baseline that the settle compare proves to be
  // the complete, content-verified state of both trees at the mark. Never carried over.
  if (after.journal && provesFullSync(after, next)) next.journal = after.journal;
  return next as VerifiedBaseline;
}

/**
 * FER-477b: is `next` exactly the state `after` verified? Every entry `same` (by content), the
 * compare recursive, complete, uncancelled and error-free, and every file recorded with the facts
 * just verified. Only then can a later compare skip a content read of a file whose directory the
 * journal proves unchanged since the mark: anything kept from an older run (a failed copy, a
 * conflict, a skipped item) could hide a difference behind unchanged facts.
 */
export function provesFullSync(after: CompareDirsReport, next: SyncBaseline): boolean {
  if (after.recursive !== true || after.truncated || after.cancelled) return false;
  if ((after.errors?.length ?? 0) > 0) return false;
  for (const e of after.entries) {
    if (e.state !== 'same') return false;
    if (e.isDir) continue;
    const b = next.entries[e.name];
    if (
      !b ||
      b.left.size !== e.leftSize ||
      b.left.modifiedMs !== e.leftModifiedMs ||
      b.right.size !== e.rightSize ||
      b.right.modifiedMs !== e.rightModifiedMs
    )
      return false;
  }
  return true;
}

function toEntry(e: CompareDirsReport['entries'][number]): {
  left: BaselineFacts;
  right: BaselineFacts;
} {
  return {
    left: { size: e.leftSize, modifiedMs: e.leftModifiedMs },
    right: { size: e.rightSize, modifiedMs: e.rightModifiedMs },
  };
}
