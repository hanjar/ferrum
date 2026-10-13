// FER-476: `indexer.status` and `indexer.setMode` — the AI may report on, and (after an explicit
// confirmation) switch the privacy mode of, the FerrumIndexer Windows service.
//
// Same shape as `copy.adjust` (FER-471): no plan, no ledger, no queue job, no path from the model.
// - `indexer.status` is read-only: the app reads `indexerStatus()` and shows an answer card. The
//   result never goes back to the provider and never contains a path (drive letters and codes only).
// - `indexer.setMode` ALWAYS waits for the confirmation card. The model only picks the target mode;
//   the card's text is written here (never by the model), and raising to `high_performance` also
//   needs the "PC ini hanya dipakai oleh saya" tick, exactly like the Settings dialog. The write is
//   `indexerSetMode(mode, true)`; the backend triggers UAC itself when raising.
//
// Offer rule (documented in the executor, mirrored by `gate_indexer_capabilities` in Rust): both
// capabilities are offered whenever the app runs as the Windows desktop app (the only place the
// service exists). In browser dev (no Tauri) they are not offered. They are NOT tied to the service
// being installed: "indeksnya sudah siap?" must get an honest "belum terpasang" instead of a
// generic refusal, and `indexer.setMode` re-checks the live status before it shows any card.
import {
  INDEXER_SET_MODE_CAPABILITY,
  INDEXER_STATUS_CAPABILITY,
  type IndexerCapabilityId,
} from './contracts';
import type { EngineGate, IntentSchema } from './commandsRegistry';
import {
  IndexerError,
  readiness,
  squattedNote,
  type IndexerMode,
  type IndexerReadiness,
  type IndexerSetModeResult,
  type IndexerStatus,
  type IndexerVolumeState,
} from '../ipc/indexer';

/** Executor port: live service status and the one privileged write. */
export interface IndexerControlPort {
  /** True when the Windows desktop app runs (the offer rule; folded with the Rust gate). */
  available(): boolean;
  status(): Promise<IndexerStatus>;
  /** `confirmed` is always true here: the card's button is the confirmation. */
  setMode(mode: IndexerMode, confirmed: true): Promise<IndexerSetModeResult>;
}

export const INDEXER_CAPABILITY_GATES: Record<IndexerCapabilityId, EngineGate> = {
  'indexer.status': { status: 'ready' },
  'indexer.setMode': { status: 'ready' },
};

export interface IndexerOfferInput {
  /** The Windows desktop app is running (Tauri). */
  desktopAvailable: boolean;
  gates?: Partial<Record<IndexerCapabilityId, EngineGate>>;
}

export function offeredIndexerCapabilityIds(input: IndexerOfferInput): IndexerCapabilityId[] {
  if (!input.desktopAvailable) return [];
  const gates = { ...INDEXER_CAPABILITY_GATES, ...input.gates };
  const ids: IndexerCapabilityId[] = [INDEXER_STATUS_CAPABILITY, INDEXER_SET_MODE_CAPABILITY];
  return ids.filter((id) => gates[id].status === 'ready');
}

/** Model-facing schemas; mirror `parseOfferedIntent` (examples are checked by tests). */
export const INDEXER_STATUS_SCHEMA: IntentSchema = {
  type: 'object',
  additionalProperties: false,
  required: ['capability'],
  properties: { capability: { const: INDEXER_STATUS_CAPABILITY } },
  examples: [{ capability: 'indexer.status' }],
};

export const INDEXER_SET_MODE_SCHEMA: IntentSchema = {
  type: 'object',
  additionalProperties: false,
  required: ['capability', 'mode'],
  properties: {
    capability: { const: INDEXER_SET_MODE_CAPABILITY },
    mode: { enum: ['high_performance', 'shared'] },
  },
  examples: [
    { capability: 'indexer.setMode', mode: 'high_performance' },
    { capability: 'indexer.setMode', mode: 'shared' },
  ],
};

export const MODE_LABEL: Record<IndexerMode, string> = {
  high_performance: 'High performance (PC dipakai sendiri)',
  shared: 'Bersama (privasi per pengguna)',
};

// ───────────────────────────── status answer ─────────────────────────────

export interface IndexerVolumeView {
  /** Drive letter `A`..`Z` (never a path). */
  letter: string;
  state: IndexerVolumeState;
  /** 0..100, floor of permille / 10. */
  percent: number;
  /** Stable error code of the service when `state === 'error'`. */
  error: string | null;
  live: boolean;
}

export interface IndexerStatusView {
  kind: 'status';
  readiness: IndexerReadiness;
  /** Index work is paused (service paused). */
  paused: boolean;
  mode: IndexerMode | null;
  /** App-built sentence answering "is the index ready?". */
  summary: string;
  volumes: IndexerVolumeView[];
  /** FER-477: what the index offers in the current mode (app facts, never model text). */
  features: { folderSizes: boolean; incrementalSync: boolean };
}

const CODE = /^[a-z0-9_]{1,40}$/;

export function describeIndexerStatus(status: IndexerStatus): IndexerStatusView {
  const state = readiness(status);
  const volumes: IndexerVolumeView[] = status.volumes.map((v) => ({
    letter: v.letter,
    state: v.state,
    percent: v.state === 'ready' ? 100 : Math.min(100, Math.floor(v.progressPermille / 10)),
    error: v.state === 'error' ? (v.error && CODE.test(v.error) ? v.error : 'internal') : null,
    live: v.live,
  }));
  const paused = status.reachable && status.indexPaused;
  let summary: string;
  switch (state) {
    case 'not_installed':
      summary =
        'FerrumIndexer belum terpasang. Ferrum tetap bekerja dengan scan biasa, tetapi pencarian tidak instan.';
      break;
    case 'stopped':
      summary =
        squattedNote(status) ??
        'FerrumIndexer terpasang tetapi tidak berjalan atau tidak menjawab. Ferrum memakai scan biasa sampai service berjalan lagi.';
      break;
    case 'degraded':
      summary =
        'FerrumIndexer berjalan dalam mode terbatas (degraded): token service tidak dibatasi, jadi pencarian dan Disk Usage dari indeks dimatikan demi keamanan. Ferrum memakai scan biasa. Perbaikannya: pasang ulang Ferrum atau jadikan service SID Restricted lagi.';
      break;
    case 'building': {
      const active = volumes.filter((v) => v.state !== 'error');
      const avg = active.length
        ? Math.floor(active.reduce((sum, v) => sum + v.percent, 0) / active.length)
        : null;
      summary =
        avg === null
          ? 'Indeks sedang disusun; belum siap dipakai sepenuhnya.'
          : `Indeks belum siap: sedang disusun (sekitar ${avg}%).`;
      break;
    }
    case 'ready':
      summary = 'Indeks siap dipakai.';
      break;
    case 'error':
      summary = 'Indeks bermasalah dan belum bisa dipakai.';
      break;
  }
  if (paused) summary += ' Pekerjaan indeks sedang dijeda.';
  if (state === 'ready' || state === 'building') {
    const failed = volumes.filter((v) => v.state === 'error');
    if (failed.length)
      summary += ` ${failed.length} drive gagal diindeks (${failed.map((v) => `${v.letter}: ${v.error}`).join(', ')}).`;
  }
  // FER-477: folder sizes only in High performance (the service refuses them in the shared mode);
  // incremental sync works in both modes once the index is ready.
  const usable = state === 'ready';
  const mode = status.reachable ? status.mode : null;
  const features = {
    folderSizes: usable && mode === 'high_performance',
    incrementalSync: usable,
  };
  if (usable && mode === 'high_performance')
    summary +=
      ' Ukuran folder dari indeks tersedia (nyalakan di Pengaturan > Indexer). "Sync cepat dengan journal indexer" bisa dinyalakan di tempat yang sama (opsional, mati secara default).';
  else if (usable && mode === 'shared')
    summary +=
      ' Ukuran folder dari indeks tidak tersedia di mode Bersama (demi privasi). "Sync cepat dengan journal indexer" (opsional, mati secara default) tidak berlaku untuk folder di dalam profil pengguna.';
  return {
    kind: 'status',
    readiness: state,
    paused,
    mode,
    summary,
    volumes,
    features,
  };
}

// ───────────────────────────── mode change ─────────────────────────────

export const ACK_LABEL = 'Saya mengerti, PC ini hanya dipakai oleh saya.';

export interface IndexerModeView {
  kind: 'setMode';
  target: IndexerMode;
  current: IndexerMode | null;
  /** App-built sentence (never model text). */
  summary: string;
  /** Privacy consequence of the target mode, one bullet per line (app text). */
  consequences: string[];
  /** Raising to high_performance needs the checkbox before the confirm button is enabled. */
  needsAck: boolean;
  ackLabel: string;
  /** Set once the change ended (no-op, refusal, success or failure). */
  result?: { ok: boolean; message: string; code?: string };
}

export function describeModeChange(
  target: IndexerMode,
  current: IndexerMode | null,
  canLowerPrivacy: boolean,
): IndexerModeView {
  const raising = target === 'high_performance';
  const consequences = raising
    ? [
        'Nama file dari SEMUA akun di PC ini bisa dicari oleh siapa pun yang memakai Ferrum di PC ini, termasuk file di profil pengguna lain.',
        'Ukuran folder dari indeks (kolom Size) ikut tersedia dan menghitung isi semua folder, termasuk yang tidak bisa Anda buka.',
        'Pilih ini hanya kalau PC ini dipakai oleh satu orang.',
        ...(canLowerPrivacy ? [] : ['Windows akan meminta persetujuan administrator (UAC).']),
      ]
    : [
        'Setiap pengguna hanya melihat file yang boleh dibuka oleh akun Windows-nya sendiri.',
        'Profil akun lain tidak diindeks.',
        'Tidak perlu persetujuan administrator (UAC).',
      ];
  return {
    kind: 'setMode',
    target,
    current,
    summary: raising
      ? 'Aktifkan mode High performance untuk FerrumIndexer? Ini mengubah siapa yang bisa mencari nama file di PC ini.'
      : 'Kembali ke mode Bersama untuk FerrumIndexer? Pencarian dibatasi per pengguna Windows.',
    consequences,
    needsAck: raising,
    ackLabel: ACK_LABEL,
  };
}

export function alreadyInMode(view: IndexerModeView): IndexerModeView {
  return {
    ...view,
    needsAck: false,
    result: {
      ok: true,
      message: `Sudah dalam mode ${MODE_LABEL[view.target]}; tidak ada yang diubah.`,
    },
  };
}

const ERROR_TEXT: Record<string, string> = {
  declined: 'Persetujuan administrator (UAC) ditolak. Mode tidak berubah.',
  not_running: 'FerrumIndexer tidak berjalan, jadi mode tidak bisa diubah. Mode tidak berubah.',
  refused: 'FerrumIndexer menolak perubahan ini. Mode tidak berubah.',
  failed: 'Perubahan mode gagal. Mode tidak berubah; periksa di Pengaturan > Indexer.',
  unsupported: 'Mengganti mode butuh aplikasi desktop Ferrum di Windows. Mode tidak berubah.',
  not_confirmed: 'Konfirmasi belum diberikan. Mode tidak berubah.',
};

/** Result message of a failed change (stable codes only; the error's own message is never shown). */
export function modeChangeFailure(error: unknown): { message: string; code: string } {
  const code = error instanceof IndexerError ? error.code : 'failed';
  return { code, message: ERROR_TEXT[code] ?? ERROR_TEXT.failed };
}

export function modeChangeSuccess(r: IndexerSetModeResult): { ok: true; message: string } {
  if (!r.changed)
    return { ok: true, message: `Mode sudah ${MODE_LABEL[r.mode]}; tidak ada yang diubah.` };
  return {
    ok: true,
    message: `Mode diganti ke ${MODE_LABEL[r.mode]}${r.elevated ? ' (dengan persetujuan administrator)' : ''}.`,
  };
}

export const NOT_INSTALLED_FOR_CHANGE =
  'FerrumIndexer belum terpasang, jadi tidak ada mode yang bisa diubah.';
export const NOT_RUNNING_FOR_CHANGE =
  'FerrumIndexer tidak berjalan atau tidak menjawab, jadi mode tidak bisa diubah. Mode tidak berubah.';

export type IndexerView = IndexerStatusView | IndexerModeView;
