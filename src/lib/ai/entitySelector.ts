// ACT-08 R7 v2 / FER-433: selector `{kind:'entity', entityId}` untuk "zip item riwayat itu".
//
// Sifat:
//   - APP-ONLY. Dibuat dari tombol "Buat ZIP" di kartu jawaban riwayat; model tidak pernah melihat atau
//     mengusulkan entityId (tidak ada giliran provider kedua). `parseIntent` menolaknya untuk output model.
//   - Diselesaikan LOKAL saat prepare (`activity_chat_locate`, read-only): path tidak keluar renderer dan
//     hasilnya hanya manifest satu item. Sesudah itu jalurnya sama dengan archive.create biasa: preview,
//     persetujuan, backend guard (fingerprint sourcePaths), ledger, queue. Tidak ada jalur mutasi baru.
import type { IntentEnvelope } from './contracts';
import { sanitizeOutputName } from './contextTarget';
import type { ActivityLocation } from './activityQuery';

export function entityArchiveEnvelope(requestId: string, entityId: string): IntentEnvelope {
  return {
    schemaVersion: 1,
    requestId,
    intent: {
      capability: 'archive.create',
      sourcePane: 'activeAtSubmit',
      selector: { kind: 'entity', entityId },
      format: 'zip',
      // ZIP selalu diletakkan di folder panel aktif; item riwayat bisa berada di mana saja.
      target: { kind: 'pane', pane: 'activeAtSubmit' },
    },
  };
}

const lastSegment = (path: string): string => {
  const trimmed = path.replace(/[\\/]+$/, '');
  return trimmed.slice(Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\')) + 1);
};

/** Nama item (basename) dari lokasi lokal. */
export function entityBaseName(location: Pick<ActivityLocation, 'path'>): string {
  return lastSegment(location.path);
}

/**
 * Nama ZIP bawaan: folder memakai nama folder; file membuang ekstensi terakhir ("laporan.pdf" -> "laporan.zip").
 * `undefined` bila nama tidak bisa dipakai; pemanggil lalu jatuh ke nama bawaan target.
 */
export function entityArchiveName(location: ActivityLocation): string | undefined {
  const base = entityBaseName(location);
  const dot = base.lastIndexOf('.');
  const stem = location.kind === 'file' && dot > 0 ? base.slice(0, dot) : base;
  const named = sanitizeOutputName(stem);
  return named.ok ? named.name : undefined;
}
