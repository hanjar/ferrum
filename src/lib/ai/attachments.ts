// FER-486 "Add to chat": lampiran file untuk pertanyaan bebas ke AI.
// Webview hanya memegang REFERENSI (path + metadata + flag). Isi file dibaca dan dimasukkan ke prompt
// di backend Rust (ai_ask) saat pengguna menekan Kirim; isi tidak pernah melewati IPC ke sini.
// Jawaban AI hanya teks: tidak pernah di-parse sebagai intent, jadi tidak ada jalur eksekusi baru.
import { invoke } from '@tauri-apps/api/core';
import { endpointHostPort } from './providerErrors';
import type { RemoteProviderConfig } from './remote-provider';

export const MAX_ATTACHMENTS = 10;
/** Batas teks per file dan total (harus sama dengan src-tauri/src/ai/attach.rs). */
export const MAX_FILE_BYTES = 100 * 1024;
export const MAX_TOTAL_BYTES = 300 * 1024;
export const MAX_HISTORY_TURNS = 8;
const MAX_HISTORY_TURN_BYTES = 8192;
const MAX_HISTORY_BYTES = 32 * 1024;

export type AttachmentKind =
  'text' | 'folder' | 'image' | 'pdf' | 'document' | 'media' | 'archive' | 'binary' | 'link';
export type AttachmentSupport = 'content' | 'listing' | 'metadata';

/** Metadata dari backend (`ai_attachment_inspect`). Tidak pernah berisi isi file. */
export interface AttachmentInfo {
  path: string;
  name: string;
  kind: AttachmentKind;
  size: number;
  modifiedMs: number;
  inArchive: boolean;
  support: AttachmentSupport;
  sensitive?: 'name' | 'content';
  note?: string;
}

export interface InspectResult {
  path: string;
  info?: AttachmentInfo;
  error?: string;
}

/** Ringkasan apa yang benar-benar dikirim untuk satu lampiran (tanpa isi). */
export interface SentAttachment {
  path: string;
  name: string;
  kind: AttachmentKind;
  support: AttachmentSupport;
  size: number;
  sentBytes: number;
  truncated: boolean;
  note?: string;
}

export interface AskTurn {
  role: 'user' | 'assistant';
  text: string;
}

export interface AskInput {
  requestId: string;
  question: string;
  history: AskTurn[];
  attachments: { path: string; confirmedSensitive: boolean }[];
  signal: AbortSignal;
}

export interface AskOutcome {
  answer: string;
  attachments: SentAttachment[];
}

/** Galat `ai_ask`: kode provider (sama dengan ai_plan_intent) atau penjaga lampiran. */
export class AskError extends Error {
  constructor(
    public readonly kind: string,
    public readonly path?: string,
  ) {
    super(kind);
    this.name = 'AskError';
  }
}

/** Tujuan isi lampiran: host:port provider dan apakah host-nya loopback. */
export interface AttachmentDestination {
  hostPort: string;
  /** Full provider endpoint; a change resets the question/answer history (FER-486 A4). */
  endpoint?: string;
  local: boolean;
}

/** Port yang dipakai ChatStore. Disuntikkan agar store tetap bisa diuji tanpa Tauri. */
export interface AttachmentAsker {
  /** FER-493 O4: `requestId` membuat path ke-i bisa dibatalkan lewat `cancelInspect(`${id}:${i}`)`. */
  inspect(paths: string[], requestId?: string): Promise<InspectResult[]>;
  /** FER-493 O4: hentikan pemeriksaan (ekstraksi Office) satu chip yang dihapus. */
  cancelInspect?(id: string): void;
  ask(input: AskInput): Promise<AskOutcome>;
  destination(): AttachmentDestination | undefined;
}

export function isLoopbackHost(hostPort: string): boolean {
  const host = hostPort.replace(/:\d+$/, '').toLowerCase();
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]';
}

export function destinationOf(endpoint: string | undefined): AttachmentDestination | undefined {
  const hostPort = endpointHostPort(endpoint);
  return hostPort && endpoint ? { hostPort, endpoint, local: isLoopbackHost(hostPort) } : undefined;
}

// ── Disclosure sekali per host (provider di luar PC) ──────────────────────────────────────────
// Diakui sekali per host per sesi aplikasi. Sengaja hanya di memori: modul AI tidak menyimpan apa
// pun di storage browser (jaminan statis adversarial.test.ts; hanya providerSetupStore yang boleh).

const acked = new Set<string>();

export function disclosureAcked(hostPort: string): boolean {
  return acked.has(hostPort.toLowerCase());
}

export function ackDisclosure(hostPort: string): void {
  acked.add(hostPort.toLowerCase());
}

/** Test hook. */
export function resetDisclosuresForTests(): void {
  acked.clear();
}

// ── Tampilan ──────────────────────────────────────────────────────────────────────────────────

const KIND_LABEL: Record<AttachmentKind, string> = {
  text: 'teks',
  folder: 'folder',
  image: 'gambar',
  pdf: 'PDF',
  document: 'dokumen',
  media: 'media',
  archive: 'arsip',
  binary: 'biner',
  link: 'tautan',
};

export function kindLabel(kind: AttachmentKind): string {
  return KIND_LABEL[kind];
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

/** History tanya-jawab yang dikirim ulang (teks saja), dibatasi seperti di backend. */
export function boundHistory(turns: AskTurn[]): AskTurn[] {
  const out: AskTurn[] = [];
  let total = 0;
  const enc = new TextEncoder();
  for (let i = turns.length - 1; i >= 0 && out.length < MAX_HISTORY_TURNS; i--) {
    let text = turns[i].text;
    while (enc.encode(text).length > MAX_HISTORY_TURN_BYTES) {
      text = text.slice(0, -256);
      // FER-486 A4: never leave half of a surrogate pair at the cut.
      if (/[\uD800-\uDBFF]$/.test(text)) text = text.slice(0, -1);
    }
    const size = enc.encode(text).length;
    if (total + size > MAX_HISTORY_BYTES) break;
    total += size;
    out.unshift({ role: turns[i].role, text });
  }
  // Riwayat dimulai dari pertanyaan pengguna.
  while (out.length && out[0].role !== 'user') out.shift();
  return out;
}

// ── IPC ───────────────────────────────────────────────────────────────────────────────────────

function parseInspect(raw: unknown): InspectResult[] {
  if (!Array.isArray(raw)) throw new AskError('malformed');
  return raw.map((r: Record<string, unknown>) => {
    const info = r.info as Record<string, unknown> | undefined;
    return {
      path: String(r.path ?? ''),
      error: typeof r.error === 'string' ? r.error : undefined,
      info:
        info && typeof info.name === 'string'
          ? {
              path: String(info.path),
              name: info.name,
              kind: info.kind as AttachmentKind,
              size: Number(info.size) || 0,
              modifiedMs: Number(info.modifiedMs) || 0,
              inArchive: info.inArchive === true,
              support: info.support as AttachmentSupport,
              sensitive:
                info.sensitive === 'name' || info.sensitive === 'content'
                  ? info.sensitive
                  : undefined,
              note: typeof info.note === 'string' ? info.note : undefined,
            }
          : undefined,
    };
  });
}

function backendKind(error: unknown): { kind: string; path?: string } {
  if (error && typeof error === 'object' && 'kind' in error) {
    const e = error as { kind: unknown; path?: unknown };
    if (typeof e.kind === 'string' && /^[a-z_]{1,40}$/.test(e.kind))
      return { kind: e.kind, path: typeof e.path === 'string' ? e.path : undefined };
  }
  return { kind: 'backend_failure' };
}

/** Port produksi: perintah Tauri. `config` dibaca saat dipakai (setup bisa berubah). */
export function tauriAttachmentAsker(
  config: () => RemoteProviderConfig | undefined,
): AttachmentAsker {
  return {
    async inspect(paths, requestId) {
      return parseInspect(await invoke<unknown>('ai_attachment_inspect', { paths, requestId }));
    },
    cancelInspect(id) {
      void invoke('ai_cancel_plan', { requestId: id }).catch(() => undefined);
    },
    async ask(input) {
      const setup = config();
      if (!setup) throw new AskError('invalid_config');
      const onAbort = () => {
        void invoke('ai_cancel_plan', { requestId: input.requestId }).catch(() => undefined);
      };
      input.signal.addEventListener('abort', onAbort, { once: true });
      try {
        const raw = await invoke<Record<string, unknown>>('ai_ask', {
          config: setup,
          request: {
            requestId: input.requestId,
            question: input.question,
            history: input.history,
            attachments: input.attachments,
          },
        });
        if (input.signal.aborted) throw new AskError('cancelled');
        if (!raw || typeof raw.answer !== 'string' || !Array.isArray(raw.attachments))
          throw new AskError('malformed');
        return { answer: raw.answer, attachments: raw.attachments as SentAttachment[] };
      } catch (error) {
        if (error instanceof AskError) throw error;
        const { kind, path } = backendKind(error);
        throw new AskError(input.signal.aborted ? 'cancelled' : kind, path);
      } finally {
        input.signal.removeEventListener('abort', onAbort);
      }
    },
    destination: () => destinationOf(config()?.endpoint),
  };
}
