/** FER-460 — the "Scan lebih cepat?" decision before a Disk usage scan.
 *
 *  Pure orchestration with injected effects so the rules are unit-tested:
 *  - the offer appears only when the preference allows it AND the root is
 *    on a local NTFS drive with the helper installed (cheap backend check,
 *    no elevation);
 *  - "Jangan tanya lagi" remembers only the OFFER preference (switching it
 *    off = always the regular scan, no question). Elevation is never
 *    remembered or automatic: every fast scan goes through UAC again;
 *  - closing the offer without choosing starts nothing.
 */
import type { DiskUsageFastEligibility } from '../ipc/types';

/** 'index' = FerrumIndexer (FER-476, no UAC); 'fast' = elevated MFT helper. */
export type ScanMode = 'fast' | 'normal' | 'index';

export interface OfferAnswer {
  /** null = the dialog was dismissed: no scan. */
  mode: ScanMode | null;
  dontAskAgain: boolean;
}

export interface OfferDeps {
  offerEnabled: () => boolean;
  eligibility: (root: string) => Promise<DiskUsageFastEligibility>;
  /** FER-476 (optional): is the FerrumIndexer's index ready for `root`? */
  indexEligibility?: (root: string) => Promise<DiskUsageFastEligibility>;
  /** `index` = whether "Instan (indeks)" is offered too. */
  ask: (
    eligibility: DiskUsageFastEligibility,
    index?: DiskUsageFastEligibility,
  ) => Promise<OfferAnswer>;
  disableOffer: () => void;
  /** FER-464 (Disk usage only): when the FerrumIndexer covers the root, go
   *  straight to "Instan (indeks)" — no question, no UAC, whatever the
   *  "Jangan tanya lagi" preference says (that preference only silences the
   *  OFFER, which this path never shows). */
  instantDefault?: boolean;
  /** FER-464: the user explicitly asked to choose the scan kind (the manual
   *  "Pilihan scan…" button): show the offer even if it was switched off. */
  forceOffer?: boolean;
}

async function safe(
  f: ((root: string) => Promise<DiskUsageFastEligibility>) | undefined,
  root: string,
): Promise<DiskUsageFastEligibility> {
  if (!f) return { eligible: false };
  try {
    return await f(root);
  } catch {
    return { eligible: false };
  }
}

/** The offer appears when the elevated MFT scan OR the index (FER-476) can
 *  serve `root`; "Jangan tanya lagi" keeps meaning "always the regular scan".
 *  The index is never chosen silently: it is one button of the offer. */
export async function chooseScanMode(root: string, deps: OfferDeps): Promise<ScanMode | null> {
  if (deps.instantDefault && !deps.forceOffer) {
    const index = await safe(deps.indexEligibility, root);
    if (index.eligible) return 'index';
  }
  if (!deps.offerEnabled() && !deps.forceOffer) return 'normal';
  const [elig, index] = await Promise.all([
    safe(deps.eligibility, root),
    safe(deps.indexEligibility, root),
  ]);
  if (!elig.eligible && !index.eligible) return 'normal';
  const answer = await deps.ask(elig, index.eligible ? index : undefined);
  if (answer.dontAskAgain && !deps.forceOffer) deps.disableOffer();
  // Never return a mode the backend said it cannot serve.
  if (answer.mode === 'fast' && !elig.eligible) return 'normal';
  if (answer.mode === 'index' && !index.eligible) return 'normal';
  return answer.mode;
}
