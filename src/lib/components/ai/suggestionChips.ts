// FER-428: suggestion chip panel chat (keputusan owner atas analisis FER-415).
// Chip hanya mengirim/mengisi teks biasa; eksekusi tetap lewat rencana + Run. Frasa memakai
// kalimat yang sudah terbukti di desktop smoke (ZIP, sync, follow-up AI-09).
import type { RunView } from '../../ai/chatStore.svelte';

export interface Chip {
  id: string;
  /** Teks yang dikirim (mode send) atau diisikan ke composer (mode fill). */
  text: string;
  mode: 'send' | 'fill';
}

/** Keadaan panel yang relevan untuk chip starter (dibaca dari registry pane). */
export interface PaneHint {
  leftPath?: string;
  rightPath?: string;
  leftSelected: number;
}

const samePath = (a: string, b: string) =>
  a.replace(/[\\/]+$/, '').toLowerCase() === b.replace(/[\\/]+$/, '').toLowerCase();

export function starterChips(h: PaneHint): Chip[] {
  const chips: Chip[] = [
    { id: 'zip-left', text: 'Arsipkan semua file di panel kiri jadi ZIP', mode: 'send' },
  ];
  if (h.leftPath && h.rightPath && !samePath(h.leftPath, h.rightPath))
    chips.push({ id: 'sync', text: 'Sinkronkan kedua panel dua arah', mode: 'send' });
  // Hanya bila ada pilihan: mencegah permintaan "yang dipilih" pada pilihan kosong (smoke S1).
  if (h.leftSelected > 0)
    chips.push({
      id: 'zip-selected',
      text: 'Arsipkan item yang dipilih di panel kiri jadi ZIP',
      mode: 'send',
    });
  return chips;
}

/** Chip perubahan rencana: hanya saat rencana tampil dan belum disetujui. */
export function followUpChips(run: RunView | undefined): Chip[] {
  if (!run?.plan || run.approvalSent) return [];
  if (run.state !== 'preview' && run.state !== 'awaitingApproval') return [];
  const cancel: Chip = { id: 'cancel', text: 'batal', mode: 'send' };
  const op = run.plan.operation;
  if (op.kind !== 'archive.create') return [cancel];
  const right = run.plan.sourceSnapshot.panes[1].location.path;
  const toRight = !samePath(op.targetDirectory, right);
  return [
    { id: 'rename', text: 'namanya ', mode: 'fill' },
    toRight
      ? { id: 'to-right', text: 'taruh di kanan', mode: 'send' }
      : { id: 'to-left', text: 'taruh di kiri', mode: 'send' },
    { id: 'only-pdf', text: 'hanya PDF', mode: 'send' },
    cancel,
  ];
}
