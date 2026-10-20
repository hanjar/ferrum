// AI-09 / FER-207..209: percakapan lanjutan (follow-up) untuk rencana yang sudah ada.
//
// Aturan:
//   - Pola sederhana (nama output, tujuan kiri/kanan, filter ekstensi, "hanya file", batal) ditangani
//     LOKAL dan deterministik: tidak memanggil provider dan tidak bisa disusupi lewat teks bebas.
//   - Frasa lain dikirim ke provider bersama ringkasan intent sebelumnya (envelope milik model sendiri,
//     TANPA path dan TANPA nama file/nama output). Hasilnya tetap lewat `parseIntent` + allowlist.
//   - Hasil apa pun adalah REVISION baru (riwayat immutable, dibatasi `MAX_REVISIONS`); approval dan
//     handle lama dicabut oleh executor, bukan oleh modul ini. Modul ini murni (tanpa efek samping).
import {
  ContractError,
  MAX_EXTENSIONS,
  isSafeExtension,
  isSafeOutputName,
  parseIntent,
  type IntentEnvelope,
  type ProposedIntent,
} from './contracts';

export const MAX_REVISIONS = 20;
/** Batas teks follow-up (byte); menyisakan ruang untuk ringkasan intent di request provider (8 KiB). */
export const MAX_FOLLOWUP_BYTES = 2000;

export type FollowUpEdit =
  | { kind: 'rename'; outputName: string }
  | { kind: 'defaultName' }
  | { kind: 'target'; target: 'left' | 'right' | 'source' }
  | { kind: 'filter'; extensions: string[] }
  | { kind: 'clearFilter' }
  | { kind: 'filesOnly' }
  | { kind: 'cancel' };

export type FollowUpParse =
  | { kind: 'local'; edit: FollowUpEdit }
  | { kind: 'rejected'; message: string }
  | { kind: 'provider' };

const SIDE: Record<string, 'left' | 'right'> = {
  kiri: 'left',
  kanan: 'right',
  left: 'left',
  right: 'right',
};

const CANCEL = /^(?:batal(?:kan)?|cancel|stop|hentikan)$/i;
const DEFAULT_NAME =
  /^(?:(?:pakai|gunakan|use)\s+)?(?:nama\s+(?:default|bawaan|standar)|default\s+name)$/i;
const RENAME =
  /^(?:(?:tolong|please)\s+)?(?:(?:ganti|ubah)\s+nama(?:nya)?(?:\s+(?:jadi|menjadi|ke|to))?|beri(?:kan)?\s+nama|kasih\s+nama|namai|nama(?:nya)?(?:\s+file)?(?:\s+zip(?:nya)?)?|rename(?:\s+(?:it|jadi|ke|to))?|name\s+it|call\s+it|named)\s*(?:jadi|menjadi|adalah|:|=|to|as)?\s+(.+)$/i;
const PREP = '(?:di|ke|dalam|in|to|on|into)';
const VERB =
  '(?:taruh|taro|simpan|letakkan|tempatkan|pindahkan|keluarkan|put|save|place|store|output)' +
  '(?:\\s+(?:hasil(?:nya)?|zip(?:-?nya)?|arsip(?:-?nya)?|file(?:-?nya)?|output(?:-?nya)?|it|the\\s+zip))?';
const LEAD = `(?:(?:tolong|please)\\s+)?(?:${VERB}\\s+${PREP}|tujuan(?:nya)?(?:\\s+(?:di|ke|adalah))?|${PREP})`;
const TARGET_SIDE = new RegExp(
  `^${LEAD}\\s+(?:panel\\s+|folder\\s+|sisi\\s+|the\\s+)?(kiri|kanan|left|right)(?:\\s+(?:panel|folder|side))?(?:\\s+(?:saja|aja|dong|ya|please))?$`,
  'i',
);
const TARGET_SOURCE = new RegExp(
  `^${LEAD}\\s+(?:folder\\s+)?(?:sumber|asal|yang\\s+sama|same\\s+folder|source(?:\\s+folder)?)(?:\\s+(?:saja|aja|ya))?$`,
  'i',
);
const FILES_ONLY =
  /^(?:hanya\s+file(?:\s+saja)?|file\s+saja|files?\s+only|only\s+files?|cuma\s+file|tanpa\s+(?:folder|subfolder|direktori)|without\s+(?:sub)?folders?)$/i;
const CLEAR_FILTER =
  /^(?:tanpa\s+filter|hapus\s+filter|all\s+types|semua\s+(?:jenis|tipe|ekstensi))$/i;
const FILTER_LEAD = /^(?:hanya|cuma|only|just)\s+(.+?)(?:\s+(?:saja|aja|only))?$/i;
const FILTER_TRAIL = /^(.+?)\s+(?:saja|aja|only)$/i;

/** Kata pengisi yang diabaikan saat membaca daftar ekstensi. */
const NOISE = new Set([
  'file',
  'files',
  'berkas',
  'yang',
  'berformat',
  'berekstensi',
  'ekstensi',
  'format',
  'bertipe',
  'tipe',
  'type',
  'types',
  'dengan',
  'with',
  'the',
  'ber',
  'saja',
  'aja',
  'only',
  'dan',
  'and',
  'atau',
  'or',
  'serta',
]);
/** Ekstensi yang dikenali tanpa titik ("hanya PDF"). Selain itu tanpa titik -> provider (bukan tebakan lokal). */
const KNOWN_EXT = new Set([
  'pdf',
  'doc',
  'docx',
  'xls',
  'xlsx',
  'ppt',
  'pptx',
  'odt',
  'ods',
  'txt',
  'rtf',
  'csv',
  'json',
  'xml',
  'html',
  'htm',
  'md',
  'log',
  'ini',
  'cfg',
  'yaml',
  'yml',
  'jpg',
  'jpeg',
  'png',
  'gif',
  'webp',
  'bmp',
  'svg',
  'tif',
  'tiff',
  'heic',
  'mp3',
  'wav',
  'flac',
  'mp4',
  'mkv',
  'avi',
  'mov',
  'zip',
  'rar',
  '7z',
  'tar',
  'gz',
  'exe',
  'msi',
  'iso',
  'js',
  'ts',
  'py',
  'rs',
  'cs',
  'java',
  'sql',
  'psd',
  'ai',
  'dwg',
]);

function bytes(s: string): number {
  return new TextEncoder().encode(s).length;
}
function clip(s: string, n = 40): string {
  const clean = s.replace(/[\p{Cc}\p{Cf}]/gu, '?');
  return clean.length > n ? `${clean.slice(0, n)}…` : clean;
}
function unquote(raw: string): { text: string; quoted: boolean } {
  const m = /^(["'`‘’“”])(.*)(["'`‘’“”])$/s.exec(raw);
  return m ? { text: m[2], quoted: true } : { text: raw, quoted: false };
}

function parseRename(payload: string): FollowUpParse {
  const { text, quoted } = unquote(payload.trim());
  // Beberapa kata tanpa kutip = kalimat bebas ("yang bagus ya"), bukan nama: serahkan ke provider.
  if (!quoted && /\s/.test(text)) return { kind: 'provider' };
  if (!isSafeOutputName(text)) {
    return {
      kind: 'rejected',
      message:
        `Nama "${clip(text)}" tidak aman untuk nama arsip (tanpa pemisah folder, ":", titik di awal/akhir, ` +
        'atau nama perangkat Windows seperti CON). Rencana tidak diubah.',
    };
  }
  return { kind: 'local', edit: { kind: 'rename', outputName: text } };
}

function parseFilter(payload: string): FollowUpParse {
  const tokens = payload.split(/[\s,&/+]+/).filter(Boolean);
  const exts: string[] = [];
  for (const raw of tokens) {
    const lower = raw.toLowerCase();
    if (NOISE.has(lower)) continue;
    const explicit = /^(?:\*?\.)(.*)$/.exec(lower);
    // Token berbentuk wildcard/path/titik ("*", "p*", "pdf.exe") bukan ekstensi: tolak, jangan tebak.
    if (!explicit && /[*?.\\:<>|"]/.test(lower)) {
      return {
        kind: 'rejected',
        message: `Ekstensi "${clip(raw)}" tidak valid (huruf/angka saja, tanpa wildcard atau path). Rencana tidak diubah.`,
      };
    }
    if (explicit) {
      if (!isSafeExtension(explicit[1])) {
        return {
          kind: 'rejected',
          message: `Ekstensi "${clip(raw)}" tidak valid (huruf/angka saja, tanpa wildcard atau path). Rencana tidak diubah.`,
        };
      }
      if (!exts.includes(explicit[1])) exts.push(explicit[1]);
      continue;
    }
    if (!KNOWN_EXT.has(lower)) return { kind: 'provider' }; // bukan ekstensi yang dikenali: bukan tebakan lokal
    if (!exts.includes(lower)) exts.push(lower);
  }
  if (exts.length === 0) return { kind: 'provider' };
  if (exts.length > MAX_EXTENSIONS) {
    return {
      kind: 'rejected',
      message: `Maksimal ${MAX_EXTENSIONS} ekstensi sekaligus. Rencana tidak diubah.`,
    };
  }
  return { kind: 'local', edit: { kind: 'filter', extensions: exts } };
}

/** Klasifikasikan teks follow-up. Tidak pernah melempar. */
export function parseFollowUp(raw: string): FollowUpParse {
  if (typeof raw !== 'string') return { kind: 'rejected', message: 'Pesan tidak valid.' };
  const text = raw
    .normalize('NFKC')
    .trim()
    .replace(/[.!?]+$/, '')
    .replace(/\s+/g, ' ');
  if (!text) return { kind: 'rejected', message: 'Pesan kosong.' };
  if (bytes(raw) > MAX_FOLLOWUP_BYTES) {
    return {
      kind: 'rejected',
      message: 'Pesan terlalu panjang untuk mengubah rencana. Ringkas, atau mulai permintaan baru.',
    };
  }
  if (CANCEL.test(text)) return { kind: 'local', edit: { kind: 'cancel' } };
  if (DEFAULT_NAME.test(text)) return { kind: 'local', edit: { kind: 'defaultName' } };
  const side = TARGET_SIDE.exec(text);
  if (side) return { kind: 'local', edit: { kind: 'target', target: SIDE[side[1].toLowerCase()] } };
  if (TARGET_SOURCE.test(text))
    return { kind: 'local', edit: { kind: 'target', target: 'source' } };
  if (FILES_ONLY.test(text)) return { kind: 'local', edit: { kind: 'filesOnly' } };
  if (CLEAR_FILTER.test(text)) return { kind: 'local', edit: { kind: 'clearFilter' } };
  const rename = RENAME.exec(text);
  if (rename) return parseRename(rename[1]);
  const filter = FILTER_LEAD.exec(text) ?? FILTER_TRAIL.exec(text);
  if (filter) return parseFilter(filter[1]);
  return { kind: 'provider' };
}

/** Catatan revisi untuk perubahan lewat provider (teks pengguna dipotong; dirender sebagai teks). */
export function providerNote(text: string): string {
  return `Diubah lewat instruksi: "${clip(text.trim(), 50)}"`;
}

/** Teks singkat untuk catatan revisi di UI (dibuat lokal, bukan dari model). */
export function describeEdit(edit: FollowUpEdit): string {
  switch (edit.kind) {
    case 'rename':
      return `Nama output: ${clip(edit.outputName, 60)}`;
    case 'defaultName':
      return 'Nama output: bawaan';
    case 'target':
      return edit.target === 'source'
        ? 'Tujuan: folder sumber'
        : `Tujuan: panel ${edit.target === 'left' ? 'kiri' : 'kanan'}`;
    case 'filter':
      return `Hanya file ${edit.extensions.map((e) => `.${e}`).join(', ')}`;
    case 'clearFilter':
      return 'Filter ekstensi dihapus';
    case 'filesOnly':
      return 'Hanya file (tanpa folder)';
    case 'cancel':
      return 'Dibatalkan';
  }
}

export type ApplyResult = { ok: true; envelope: IntentEnvelope } | { ok: false; message: string };

/** FER-433: selector `entity` = satu item riwayat yang eksplisit; filter/"hanya file" tidak bermakna. */
const ENTITY_EDIT_REJECTED: ApplyResult = {
  ok: false,
  message:
    'Item dari riwayat adalah satu item eksplisit; filter ekstensi dan "hanya file" tidak berlaku. Rencana tidak diubah.',
};

/** Terapkan edit lokal pada intent (salinan); hasil divalidasi ulang oleh `parseIntent`. */
export function applyEdit(prev: IntentEnvelope, edit: FollowUpEdit): ApplyResult {
  const intent = structuredClone(prev.intent) as ProposedIntent;
  if (edit.kind === 'cancel') return { ok: false, message: 'Batal ditangani oleh executor.' };
  if (intent.capability !== 'archive.create') {
    return {
      ok: false,
      message:
        'Sinkronisasi tidak punya nama, tujuan, atau filter yang bisa diubah. Ketik "batal" atau buat permintaan baru.',
    };
  }
  switch (edit.kind) {
    case 'rename':
      intent.outputName = edit.outputName;
      break;
    case 'defaultName':
      delete intent.outputName;
      break;
    case 'target':
      intent.target =
        edit.target === 'source'
          ? { kind: 'sourceDirectory' }
          : { kind: 'pane', pane: edit.target };
      break;
    case 'filter':
      if (intent.selector.kind === 'entity') return ENTITY_EDIT_REJECTED;
      intent.selector = { ...intent.selector, extensions: [...edit.extensions] };
      break;
    case 'clearFilter': {
      if (intent.selector.kind === 'entity') return ENTITY_EDIT_REJECTED;
      const { extensions: _drop, ...rest } = intent.selector;
      void _drop;
      intent.selector = rest as typeof intent.selector;
      break;
    }
    case 'filesOnly':
      if (intent.selector.kind === 'entity') return ENTITY_EDIT_REJECTED;
      if (intent.selector.kind === 'selected') {
        return {
          ok: false,
          message:
            'Item yang dipilih sudah eksplisit; "hanya file" tidak berlaku. Rencana tidak diubah.',
        };
      }
      intent.selector = { ...intent.selector, entries: 'filesOnly' };
      break;
  }
  return validated({ ...prev, intent });
}

function validated(candidate: IntentEnvelope): ApplyResult {
  try {
    const allowEntitySelector =
      candidate.intent.capability === 'archive.create' &&
      candidate.intent.selector.kind === 'entity';
    return {
      ok: true,
      envelope: parseIntent(structuredClone(candidate), candidate.requestId, {
        allowEntitySelector,
      }),
    };
  } catch (e) {
    return {
      ok: false,
      message:
        e instanceof ContractError
          ? 'Perubahan menghasilkan rencana yang tidak valid. Rencana tidak diubah.'
          : 'Perubahan gagal.',
    };
  }
}

/**
 * Ringkasan intent sebelumnya untuk provider: hanya bentuk perintah (capability, pane, selector, target).
 * Tidak ada path, nama file, atau nama output (hanya penanda `hasOutputName`).
 */
export function previousIntentSummary(prev: IntentEnvelope): Record<string, unknown> {
  const i = prev.intent;
  if (i.capability === 'directories.sync') return { capability: i.capability, mode: i.mode };
  // FER-576: a delete has no format/target/name; the follow-up only keeps the shape.
  if (i.capability === 'items.delete')
    return { capability: i.capability, sourcePane: i.sourcePane, selector: i.selector };
  return {
    capability: i.capability,
    sourcePane: i.sourcePane,
    // FER-433: id entity opak tidak perlu sampai ke provider.
    selector: i.selector.kind === 'entity' ? { kind: 'entity' } : i.selector,
    format: i.format,
    ...(i.level !== undefined ? { level: i.level } : {}),
    target: i.target,
    hasOutputName: i.outputName !== undefined,
  };
}

/** Teks request ke provider: ringkasan rencana lama + instruksi lanjutan (data, bukan instruksi sistem). */
export function composeProviderText(prev: IntentEnvelope, text: string): string {
  return (
    `Follow-up to the previous plan. previousPlan=${JSON.stringify(previousIntentSummary(prev))}\n` +
    `User follow-up: ${text.trim()}`
  );
}

/**
 * Gabungkan balasan provider: provider tidak melihat nama output, jadi nama sebelumnya dipertahankan
 * bila balasan tidak menyebut nama baru. Hasil divalidasi ulang.
 */
export function mergeProviderIntent(prev: IntentEnvelope, next: IntentEnvelope): ApplyResult {
  const p = prev.intent;
  const n = structuredClone(next.intent) as ProposedIntent;
  if (
    p.capability === 'archive.create' &&
    n.capability === 'archive.create' &&
    n.outputName === undefined &&
    p.outputName !== undefined
  ) {
    n.outputName = p.outputName;
  }
  return validated({ ...next, intent: n });
}

// ───────────────────────── riwayat intent ─────────────────────────

export type RevisionCause = 'initial' | 'local' | 'provider';
export interface IntentRevision {
  readonly revision: number;
  readonly envelope: IntentEnvelope;
  readonly cause: RevisionCause;
  readonly note: string;
}

function deepFreeze<T>(value: T): T {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const v of Object.values(value as object)) deepFreeze(v);
  }
  return value;
}

/** Riwayat revision immutable dan dibatasi. Entri lama tidak pernah diubah; `append` hanya menambah. */
export class IntentHistory {
  private readonly items: IntentRevision[] = [];

  constructor(initial: IntentEnvelope) {
    this.items.push(this.make(1, initial, 'initial', 'Permintaan awal'));
  }

  private make(
    revision: number,
    envelope: IntentEnvelope,
    cause: RevisionCause,
    note: string,
  ): IntentRevision {
    return deepFreeze({ revision, envelope: structuredClone(envelope), cause, note });
  }

  get length(): number {
    return this.items.length;
  }
  get revision(): number {
    return this.items[this.items.length - 1].revision;
  }
  get current(): IntentRevision {
    return this.items[this.items.length - 1];
  }
  get full(): boolean {
    return this.items.length >= MAX_REVISIONS;
  }
  entries(): readonly IntentRevision[] {
    return Object.freeze([...this.items]);
  }

  append(envelope: IntentEnvelope, cause: RevisionCause, note: string): IntentRevision | undefined {
    if (this.full) return undefined;
    const rev = this.make(this.revision + 1, envelope, cause, note);
    this.items.push(rev);
    return rev;
  }
}
