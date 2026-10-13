// AI-05 / FER-191: kebijakan lokal. Risiko, availability, dan keputusan approval
// dihitung di sini dari plan tepercaya (hasil facade) + preferensi pengguna.
// Tidak ada bidang dari model (approved/risk/requiresApproval) yang dibaca.
import type { Availability, MutatingCapabilityId } from './commands';
import type { ResolvedPlan } from './contracts';

/** read: tanpa tulis; selection: hanya sorotan; create: output baru tanpa overwrite; mutating: mengubah isi yang ada. */
export type Risk = 'read' | 'selection' | 'create' | 'mutating';

export interface PolicyPrefs {
  /** Auto-create eksplisit: output baru tanpa overwrite boleh jalan tanpa klik Jalankan. */
  autoCreate: boolean;
}
/** Default produk: mode assisted. */
export const DEFAULT_PREFS: Readonly<PolicyPrefs> = Object.freeze({ autoCreate: false });

/** Risiko dasar per capability; hanya capability yang ada di registry. */
const BASE_RISK: Record<MutatingCapabilityId, Risk> = {
  'archive.create': 'create',
  'directories.sync': 'mutating',
};
/** Capability yang boleh auto-approve bila pengguna mengaktifkan auto-create. */
const AUTO_CREATE_ELIGIBLE = new Set<MutatingCapabilityId>(['archive.create']);

export type PolicyDecision =
  | { kind: 'blocked'; reason: string }
  | { kind: 'needsApproval'; risk: Risk; reasons: string[] }
  | { kind: 'autoApproved'; risk: Risk; reasons: string[] };

export interface PolicyInput {
  plan: ResolvedPlan;
  availability: Availability;
  prefs: PolicyPrefs;
}

const issued = new WeakSet<object>();
const issue = (d: PolicyDecision): PolicyDecision => {
  issued.add(d);
  return Object.freeze(d);
};
/** Hanya keputusan yang dibuat evaluatePolicy yang sah (mencegah decision palsu). */
export function isIssuedDecision(value: unknown): value is PolicyDecision {
  return typeof value === 'object' && value !== null && issued.has(value);
}

export function classifyRisk(plan: ResolvedPlan): Risk {
  const base = BASE_RISK[plan.capability] ?? 'mutating';
  // Konflik berarti ada item yang sudah ada di tujuan: bukan lagi sekadar "buat baru".
  return plan.preview.conflictCount > 0 ? 'mutating' : base;
}

export function evaluatePolicy(input: PolicyInput): PolicyDecision {
  const { plan, availability, prefs } = input;
  if (!availability.available) return issue({ kind: 'blocked', reason: availability.reason });
  const risk = classifyRisk(plan);
  const reasons: string[] = [];
  if (risk === 'mutating')
    reasons.push('Operasi mengubah isi yang sudah ada atau memiliki konflik.');
  const eligible =
    prefs.autoCreate === true &&
    risk === 'create' &&
    AUTO_CREATE_ELIGIBLE.has(plan.capability) &&
    plan.preview.conflictCount === 0;
  if (eligible) {
    return issue({
      kind: 'autoApproved',
      risk,
      reasons: ['Auto-create aktif: output baru tanpa overwrite.'],
    });
  }
  if (plan.capability === 'directories.sync')
    reasons.push('Sinkronisasi selalu butuh persetujuan atas dry-run.');
  return issue({ kind: 'needsApproval', risk, reasons });
}
