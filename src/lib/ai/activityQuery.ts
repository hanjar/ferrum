// ACT-08 R5-R7 v1: the read-only `activity.query` answer path (docs/activity/chat-integration.md).
//
// Rules kept here:
//   - The model only CHOOSES a strictly typed query. The app runs it locally through
//     `ActivityQueryPort` (IPC `activity_chat_query`) and renders the answer itself.
//   - The answer is NEVER sent to the provider: no second turn, nothing in OutboundContext.
//   - `parseActivityAnswer` pins the field set of the Rust `ChatResponse`/`ChatEntityRef`; any extra
//     field (locator, counts, scores, timestamps, ...) rejects the whole answer.
//   - "Open in panel" (R7 v1) only places an item in a pane through the existing pane API; there is
//     no mutation path. Follow-up operations go through the normal prepared plan + approval.
import { invoke } from '@tauri-apps/api/core';
import {
  ContractError,
  parseActivityQuery,
  type ActivityQuery,
  type HistoryEntity,
} from './contracts';
import type { PaneApi } from '../stores/panesRegistry.svelte';

export type AnswerAvailability = 'available' | 'offline' | 'missing' | 'inaccessible' | 'unknown';
/** Reasons the chat tool emits (subset of the activity `ReasonCode`). */
export type AnswerReason = 'recent' | 'frequent' | 'exactName' | 'similarName' | 'abbreviation';

/** Minimum disclosure per entity (Rust `ChatEntityRef`). Never a locator, counts, scores or times. */
export interface ActivityAnswerItem {
  id: string;
  displayName: string;
  /** Tail-truncated parent path (leading ellipsis), widened only to tell candidates apart. */
  parentHint: string;
  kind: HistoryEntity;
  availability: AnswerAvailability;
  reasons: AnswerReason[];
}

/** Rust `ChatResponse` (tag `status`). */
export type ActivityAnswer =
  | { status: 'entities'; entries: ActivityAnswerItem[]; truncated: boolean }
  | { status: 'resolved'; entry: ActivityAnswerItem }
  | { status: 'ambiguous'; candidates: ActivityAnswerItem[] }
  | { status: 'notFound' }
  | { status: 'withheld' }
  | { status: 'stale' }
  | { status: 'rejected'; reason: string }
  | { status: 'unavailable' };

/** Local location of one listed entity, for "Open in panel" only. Never leaves the renderer. */
export interface ActivityLocation {
  path: string;
  parentPath: string;
  kind: HistoryEntity;
}

/**
 * Port to the local activity service. `query` = IPC `activity_chat_query` (read-only, records
 * nothing); `locate` = IPC `activity_chat_locate` (read-only identity lookup).
 */
export interface ActivityQueryPort {
  /** True when the activity service is open (Rust `ActivityHandle` is Some). Gates the offer. */
  available(): boolean;
  query(request: ActivityQuery): Promise<unknown>;
  locate?(entityId: string): Promise<unknown>;
}

const MAX_ITEMS = 20;
const MAX_NAME_BYTES = 255;
/** 48 chars + leading ellipsis for listed/resolved entries (Rust `PARENT_HINT_MAX_CHARS`). */
const MAX_HINT_CHARS = 49;
/** Ambiguous candidates may widen the hint (255 chars) and append ` (id <id>)`. */
const MAX_WIDE_HINT_CHARS = 400;
const MAX_REASON_BYTES = 256;
const ID = /^[A-Za-z0-9_.:-]{1,128}$/;
const AVAILABILITY: readonly AnswerAvailability[] = [
  'available',
  'offline',
  'missing',
  'inaccessible',
  'unknown',
];
const REASONS: readonly AnswerReason[] = [
  'recent',
  'frequent',
  'exactName',
  'similarName',
  'abbreviation',
];
const ITEM_KEYS = ['id', 'displayName', 'parentHint', 'kind', 'availability', 'reasons'] as const;

function fail(message: string): never {
  throw new ContractError('invalid_payload', message);
}
function exact(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('Expected object');
  const record = value as Record<string, unknown>;
  const own = Object.keys(record);
  if (own.length !== keys.length || own.some((k) => !keys.includes(k)))
    fail('Unknown or missing field');
  return record;
}
const bytes = (s: string) => new TextEncoder().encode(s).length;
const CONTROL = /\p{Cc}/u;

function item(value: unknown, maxHint: number): ActivityAnswerItem {
  const r = exact(value, ITEM_KEYS);
  if (typeof r.id !== 'string' || !ID.test(r.id)) fail('Invalid entity id');
  if (
    typeof r.displayName !== 'string' ||
    !r.displayName.trim() ||
    bytes(r.displayName) > MAX_NAME_BYTES ||
    CONTROL.test(r.displayName) ||
    /[\\/]/.test(r.displayName)
  ) {
    fail('Display name must be a plain name');
  }
  if (
    typeof r.parentHint !== 'string' ||
    [...r.parentHint].length > maxHint ||
    CONTROL.test(r.parentHint)
  ) {
    fail('Invalid parent hint');
  }
  if (r.kind !== 'file' && r.kind !== 'folder') fail('Invalid kind');
  if (
    typeof r.availability !== 'string' ||
    !AVAILABILITY.includes(r.availability as AnswerAvailability)
  ) {
    fail('Invalid availability');
  }
  if (
    !Array.isArray(r.reasons) ||
    r.reasons.length > REASONS.length ||
    new Set(r.reasons).size !== r.reasons.length ||
    r.reasons.some((x) => typeof x !== 'string' || !REASONS.includes(x as AnswerReason))
  ) {
    fail('Invalid reasons');
  }
  return {
    id: r.id,
    displayName: r.displayName,
    parentHint: r.parentHint,
    kind: r.kind,
    availability: r.availability as AnswerAvailability,
    reasons: [...(r.reasons as AnswerReason[])],
  };
}
function list(value: unknown, min: number, maxHint: number): ActivityAnswerItem[] {
  if (!Array.isArray(value) || value.length < min || value.length > MAX_ITEMS)
    fail('Invalid item list');
  const out = value.map((v) => item(v, maxHint));
  if (new Set(out.map((i) => i.id)).size !== out.length) fail('Duplicate entity id');
  return out;
}

/**
 * Strict parser for the `activity_chat_query` response. Returns a fresh object holding only the
 * pinned fields; anything else (unknown status, extra or missing keys, a locator, counts, scores,
 * timestamps, oversized lists) throws `ContractError`.
 */
export function parseActivityAnswer(value: unknown): ActivityAnswer {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('Expected object');
  const status = (value as Record<string, unknown>).status;
  switch (status) {
    case 'entities': {
      const r = exact(value, ['status', 'entries', 'truncated']);
      if (typeof r.truncated !== 'boolean') fail('Invalid truncated flag');
      return { status, entries: list(r.entries, 0, MAX_HINT_CHARS), truncated: r.truncated };
    }
    case 'resolved': {
      const r = exact(value, ['status', 'entry']);
      return { status, entry: item(r.entry, MAX_HINT_CHARS) };
    }
    case 'ambiguous': {
      const r = exact(value, ['status', 'candidates']);
      const candidates = list(r.candidates, 2, MAX_WIDE_HINT_CHARS);
      if (new Set(candidates.map((c) => c.parentHint)).size !== candidates.length) {
        fail('Ambiguous candidates must be distinguishable');
      }
      return { status, candidates };
    }
    case 'rejected': {
      const r = exact(value, ['status', 'reason']);
      if (
        typeof r.reason !== 'string' ||
        bytes(r.reason) > MAX_REASON_BYTES ||
        CONTROL.test(r.reason)
      ) {
        fail('Invalid reason');
      }
      return { status, reason: r.reason };
    }
    case 'notFound':
    case 'withheld':
    case 'stale':
    case 'unavailable':
      exact(value, ['status']);
      return { status };
    default:
      fail('Unknown answer status');
  }
}

/** Every item the answer lists (entries, the resolved entry or the candidates). */
export function answerItems(answer: ActivityAnswer): ActivityAnswerItem[] {
  switch (answer.status) {
    case 'entities':
      return answer.entries;
    case 'resolved':
      return [answer.entry];
    case 'ambiguous':
      return answer.candidates;
    default:
      return [];
  }
}

/** Missing/offline items have no "Open in panel" button. */
export function isOpenable(item: Pick<ActivityAnswerItem, 'availability'>): boolean {
  return item.availability !== 'missing' && item.availability !== 'offline';
}

/** Strict parser for `activity_chat_locate`; `null` = not locatable. */
export function parseActivityLocation(value: unknown): ActivityLocation | null {
  if (value === null || value === undefined) return null;
  const r = exact(value, ['path', 'parentPath', 'kind']);
  const path = (v: unknown) =>
    typeof v === 'string' &&
    v.length > 0 &&
    bytes(v) <= 4096 &&
    !CONTROL.test(v) &&
    !v.split(/[\\/]/).includes('..');
  if (!path(r.path) || !path(r.parentPath)) fail('Invalid location');
  if (r.kind !== 'file' && r.kind !== 'folder') fail('Invalid kind');
  return { path: r.path as string, parentPath: r.parentPath as string, kind: r.kind };
}

// ───────────────────────── user-facing copy (Indonesian, app text) ─────────────────────────

const PERIOD_UNIT: Record<'file' | 'folder', string> = { file: 'file', folder: 'folder' };

/** FER-432: longest model-chosen text shown verbatim (code points, ellipsis included). */
export const MAX_MODEL_TEXT_CHARS = 60;

/** Cut model-chosen text to `max` code points, the last one an ellipsis when cut. */
export function truncateModelText(text: string, max = MAX_MODEL_TEXT_CHARS): string {
  const chars = [...text];
  return chars.length <= max ? text : `${chars.slice(0, max - 1).join('')}…`;
}

/**
 * One-line summary of the local query (from the parsed intent, never model prose). The only
 * model-chosen free text, the findByName `name`, is quoted and cut to 60 characters; the card
 * labels the whole line as the model's request (FER-432).
 */
export function describeQuery(request: ActivityQuery): string {
  const q = request.query;
  const unit = PERIOD_UNIT[q.entity];
  switch (q.kind) {
    case 'recent':
      return `${unit} terakhir dibuka (${q.periodDays} hari)`;
    case 'frequent':
      return `${unit} yang sering dibuka (${q.periodDays} hari)`;
    case 'findByName':
      return `cari ${unit} bernama "${truncateModelText(q.name)}"`;
  }
}

export const REASON_LABEL: Record<AnswerReason, string> = {
  recent: 'Baru dibuka',
  frequent: 'Sering dibuka',
  exactName: 'Nama sama persis',
  similarName: 'Nama mirip',
  abbreviation: 'Singkatan',
};
export const AVAILABILITY_LABEL: Record<AnswerAvailability, string> = {
  available: 'Tersedia',
  inaccessible: 'Tidak dapat diakses',
  unknown: 'Belum diperiksa',
  offline: 'Drive offline',
  missing: 'Sudah tidak ada',
};

/** Status line + tone for each answer; text is app copy, never from the model or the backend. */
export function describeAnswer(answer: ActivityAnswer): {
  text: string;
  tone: 'good' | 'warn' | 'bad' | 'info';
} {
  switch (answer.status) {
    case 'entities':
      return answer.entries.length
        ? {
            text: answer.truncated
              ? `${answer.entries.length} item teratas dari riwayat aktivitas. Masih ada item lain.`
              : `${answer.entries.length} item dari riwayat aktivitas.`,
            tone: 'good',
          }
        : { text: 'Tidak ada item di riwayat aktivitas untuk periode ini.', tone: 'info' };
    case 'resolved':
      return { text: 'Ditemukan satu item di riwayat aktivitas.', tone: 'good' };
    case 'ambiguous':
      return {
        text: `Ada ${answer.candidates.length} item dengan nama yang sama. Pilih yang Anda maksud.`,
        tone: 'info',
      };
    case 'notFound':
      return { text: 'Tidak ditemukan di riwayat aktivitas.', tone: 'info' };
    case 'withheld':
      return {
        text: 'Riwayat aktivitas sedang dijeda atau sesi privat aktif; tidak ada riwayat yang ditampilkan.',
        tone: 'warn',
      };
    case 'stale':
      return {
        text: 'Riwayat berubah saat dibaca (dihapus atau disembunyikan). Tanyakan lagi untuk hasil terbaru.',
        tone: 'warn',
      };
    case 'rejected':
      return { text: 'Pertanyaan riwayat tidak valid; tidak ada yang dibaca.', tone: 'bad' };
    case 'unavailable':
      return { text: 'Riwayat aktivitas tidak tersedia di jendela ini.', tone: 'bad' };
  }
}

// ───────────────────────── R7 v1: place an item in a pane ─────────────────────────

/**
 * "Open in panel": folder -> navigate the pane to it; file -> reveal (open the parent and select the
 * entry), falling back to opening the parent. Pane API only: nothing is opened with the OS, copied,
 * archived or synced. Returns false when the pane cannot navigate.
 */
export function revealInPane(location: ActivityLocation, pane: PaneApi | undefined): boolean {
  if (!pane) return false;
  if (location.kind === 'folder') {
    if (!pane.openPath) return false;
    pane.openPath(location.path);
    return true;
  }
  if (pane.revealPath) {
    pane.revealPath(location.path);
    return true;
  }
  if (!pane.openPath) return false;
  pane.openPath(location.parentPath);
  return true;
}

// ───────────────────────── Tauri port ─────────────────────────

/** Rust `FsError` for an invalid query is `{ code: 'invalidPath', ... }`. */
function isInvalidQueryError(error: unknown): boolean {
  return (
    !!error && typeof error === 'object' && (error as { code?: unknown }).code === 'invalidPath'
  );
}

/** Map a failed port call to an answer (no backend message is ever shown). */
export function answerForError(error: unknown): ActivityAnswer {
  return isInvalidQueryError(error)
    ? { status: 'rejected', reason: 'invalid query' }
    : { status: 'unavailable' };
}

export interface TauriActivityPortOptions {
  /** Activity service open (activityStore.available). */
  available: () => boolean;
  /**
   * FER-432: the first-run recording disclosure was acknowledged
   * (activityStore.disclosureAcked). Required: `activity.query` is never offered before it.
   */
  disclosureAcked: () => boolean;
  /** This window's private session: answered `withheld` locally, the IPC is not called. */
  privateSession?: () => boolean;
}

/** Thin IPC port. `query` only ever invokes `activity_chat_query`; `locate` only `activity_chat_locate`. */
export function tauriActivityQueryPort(options: TauriActivityPortOptions): ActivityQueryPort {
  return {
    available: () => options.available() === true && options.disclosureAcked() === true,
    async query(request) {
      if (options.privateSession?.()) return { status: 'withheld' };
      const wire = parseActivityQuery(
        request.limit === undefined
          ? { query: request.query }
          : { query: request.query, limit: request.limit },
      );
      return invoke('activity_chat_query', { request: wire });
    },
    async locate(entityId) {
      if (options.privateSession?.() || !ID.test(entityId)) return null;
      return invoke('activity_chat_locate', { entityId });
    },
  };
}
