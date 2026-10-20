// AI-01 wire contracts. Model input is parsed only as IntentEnvelope; other contracts
// are app/backend-owned and must never be constructed from model output.
export type PaneRef = 'left' | 'right' | 'activeAtSubmit';
// AI-09: `extensions` (opsional) menyaring item ke file dengan ekstensi tersebut (huruf kecil, tanpa titik).
export type Selector =
  | { kind: 'selected'; extensions?: string[] }
  | { kind: 'allVisible'; entries: 'all' | 'filesOnly'; extensions?: string[] }
  | {
      kind: 'directoryContents';
      includeHidden: boolean;
      entries: 'all' | 'filesOnly';
      extensions?: string[];
    };
/**
 * ACT-08 R7 v2 / FER-433: pilihan satu item riwayat aktivitas. APP-ONLY: dibuat oleh tombol "Buat ZIP" di
 * kartu jawaban riwayat, tidak pernah oleh model (model tidak melihat entityId). `parseIntent` menolaknya
 * kecuali pemanggil lokal memberi `allowEntitySelector`; Rust `Intent` tetap tidak mengenalnya.
 */
export interface EntitySelector {
  kind: 'entity';
  entityId: string;
}
export type ArchiveSelector = Selector | EntitySelector;
/** FER-576: formats the model may ask `archive.create` for. RAR is never creatable from chat. */
export const ARCHIVE_INTENT_FORMATS = ['zip', '7z'] as const;
export type ArchiveIntentFormat = (typeof ARCHIVE_INTENT_FORMATS)[number];
/** FER-576: closed set of compression levels; `maximum` and `ultra` both map to the engine's strongest. */
export const ARCHIVE_INTENT_LEVELS = ['store', 'fast', 'normal', 'maximum', 'ultra'] as const;
export type ArchiveIntentLevel = (typeof ARCHIVE_INTENT_LEVELS)[number];
export type ProposedIntent =
  | {
      capability: 'archive.create';
      sourcePane: PaneRef;
      selector: ArchiveSelector;
      format: ArchiveIntentFormat;
      level?: ArchiveIntentLevel;
      target: { kind: 'sourceDirectory' } | { kind: 'pane'; pane: PaneRef };
      outputName?: string;
    }
  | { capability: 'directories.sync'; leftPane: 'left'; rightPane: 'right'; mode: 'bidirectional' };
export interface IntentEnvelope {
  schemaVersion: 1;
  requestId: string;
  intent: ProposedIntent;
}
/** Capabilities that produce a prepared plan and need approval (keys of BASE_RISK/ENGINE_GATES). */
export type MutatingCapabilityId = ProposedIntent['capability'];
/** ACT-08 read capability: the model only chooses a local history query; nothing is mutated and
 * the result is never sent back to the provider. Mirrors `Intent::ActivityQuery` in Rust. */
export const ACTIVITY_QUERY_CAPABILITY = 'activity.query';
export const DISK_USAGE_QUERY_CAPABILITY = 'diskUsage.query';
export type ReadCapabilityId =
  typeof ACTIVITY_QUERY_CAPABILITY | typeof DISK_USAGE_QUERY_CAPABILITY;
export const MAX_HISTORY_LIMIT = 20;
export const MAX_HISTORY_NAME_BYTES = 255;
export const HISTORY_PERIOD_DAYS = [7, 30, 90] as const;
export type HistoryEntity = 'file' | 'folder';
export type HistoryQuery =
  | {
      kind: 'recent' | 'frequent';
      periodDays: (typeof HISTORY_PERIOD_DAYS)[number];
      entity: HistoryEntity;
    }
  | { kind: 'findByName'; name: string; entity: HistoryEntity };
/** Wire of the `activity_chat_query` IPC command (same shape as the intent minus `capability`). */
export interface ActivityQuery {
  query: HistoryQuery;
  limit?: number;
}
export interface ActivityQueryIntent extends ActivityQuery {
  capability: typeof ACTIVITY_QUERY_CAPABILITY;
}
/**
 * FER-471 control capability: change threads / auto mode / priority of the copy job that is running
 * now. Performance only (no file, path or job id); runs only after the user confirms on the chat
 * card. Mirrors `Intent::CopyAdjust` in Rust.
 */
export const COPY_ADJUST_CAPABILITY = 'copy.adjust';
export type ControlCapabilityId = typeof COPY_ADJUST_CAPABILITY;
export const MAX_COPY_ADJUST_STEP = 8;
export const MAX_COPY_THREADS = 16;
export type CopyAdjustRequest =
  | { action: 'increase' | 'decrease'; by?: number }
  | { action: 'set'; threads: number }
  | { action: 'auto' }
  | { action: 'priority'; priority: 'normal' | 'background' };
export type CopyAdjustIntent = CopyAdjustRequest & { capability: ControlCapabilityId };
/**
 * FER-476 indexer capabilities (FerrumIndexer, the optional Windows service). `indexer.status` is a
 * read-only answer card; `indexer.setMode` always waits for a confirmation card that spells out the
 * privacy consequence. The model only picks the target mode: no path, no free text is executed.
 * Mirrors `Intent::IndexerStatus` / `Intent::IndexerSetMode` in Rust.
 */
export const INDEXER_STATUS_CAPABILITY = 'indexer.status';
export const INDEXER_SET_MODE_CAPABILITY = 'indexer.setMode';
export type IndexerCapabilityId =
  typeof INDEXER_STATUS_CAPABILITY | typeof INDEXER_SET_MODE_CAPABILITY;
export const INDEXER_MODE_CHOICES = ['high_performance', 'shared'] as const;
export interface IndexerSetModeRequest {
  mode: (typeof INDEXER_MODE_CHOICES)[number];
}
export type IndexerStatusIntent = { capability: typeof INDEXER_STATUS_CAPABILITY };
export type IndexerSetModeIntent = IndexerSetModeRequest & {
  capability: typeof INDEXER_SET_MODE_CAPABILITY;
};
/**
 * FER-466 read capability: the model only picks the kind of question about the Disk Usage result on
 * screen (plus a scope / category / limit from closed sets). The app runs it locally on that snapshot
 * and answers on a card; no scan data is sent to the provider. Mirrors `Intent::DiskUsageQuery`.
 */
export const MAX_DISK_USAGE_LIMIT = 20;
export const DISK_USAGE_SCOPES = ['view', 'root'] as const;
export const DISK_USAGE_CATEGORIES = [
  'video',
  'image',
  'audio',
  'document',
  'archive',
  'other',
] as const;
export type DiskUsageScope = (typeof DISK_USAGE_SCOPES)[number];
export type DiskUsageCategory = (typeof DISK_USAGE_CATEGORIES)[number];
export type DiskUsageQueryKind =
  | { kind: 'largestFiles'; scope?: DiskUsageScope; category?: DiskUsageCategory; limit?: number }
  | { kind: 'largestFolders'; scope?: DiskUsageScope; limit?: number }
  | { kind: 'summary' };
export interface DiskUsageQueryRequest {
  query: DiskUsageQueryKind;
}
export type DiskUsageQueryIntent = DiskUsageQueryRequest & {
  capability: typeof DISK_USAGE_QUERY_CAPABILITY;
};
export type ModelIntent =
  | DiskUsageQueryIntent
  | ProposedIntent
  | ActivityQueryIntent
  | CopyAdjustIntent
  | IndexerStatusIntent
  | IndexerSetModeIntent;
export interface OfferedIntentEnvelope {
  schemaVersion: 1;
  requestId: string;
  intent: ModelIntent;
}
export interface PaneStateSnapshot {
  paneId: 'left' | 'right';
  tabId: string;
  location: { kind: 'disk' | 'archive'; path: string };
  contextRevision: number;
  selectionRevision: number;
  selectionHandle: string;
  filter: string;
  showHidden: boolean;
  sort: { key: 'name' | 'size' | 'modified'; dir: 1 | -1 };
}
export interface PaneSnapshot {
  schemaVersion: 1;
  requestId: string;
  activePaneAtSubmit: 'left' | 'right';
  panes: [PaneStateSnapshot, PaneStateSnapshot];
}
export type ResolvedOperation =
  | { kind: 'archive.create'; sourceManifest: string; targetDirectory: string; outputName: string }
  | {
      kind: 'directories.sync';
      leftRoot: string;
      rightRoot: string;
      leftManifest: string;
      rightManifest: string;
    };
export interface PlanStep {
  id: string;
  capability: ProposedIntent['capability'];
  dependsOn: string[];
}
export interface ResolvedPlan {
  schemaVersion: 1;
  planId: string;
  requestId: string;
  revision: number;
  capability: ProposedIntent['capability'];
  sourceSnapshot: PaneSnapshot;
  operation: ResolvedOperation;
  preview: { itemCount: number; conflictCount: number; totalBytes: number };
  steps: PlanStep[];
  previewDigest: string;
  risk: 'mutating';
  requiresApproval: true;
  stepCount: number;
}
export interface PreparedHandle {
  schemaVersion: 1;
  handleId: string;
  planId: string;
  requestId: string;
  revision: number;
  planDigest: string;
  expiresAtMs: number;
}
export interface ItemError {
  code: ContractErrorCode;
  message: string;
  itemRef?: string;
}
export interface ExecutionReceipt {
  schemaVersion: 1;
  executionId: string;
  planId: string;
  jobIds: string[];
  state: 'queued' | 'running' | 'completed' | 'partial' | 'failed' | 'cancelled' | 'unknownOutcome';
  completed: number;
  failed: number;
  errors: ItemError[];
}
export type ContractErrorCode =
  | 'invalid_payload'
  | 'unsupported_version'
  | 'request_mismatch'
  | 'invalid_reference'
  | 'limit_exceeded';
export class ContractError extends Error {
  constructor(
    public readonly code: ContractErrorCode,
    message: string,
  ) {
    super(message);
    this.name = 'ContractError';
  }
}

const MAX_JSON_BYTES = 32 * 1024;
const MAX_DEPTH = 12;
const MAX_STEPS = 10_000;
// Item counts (a preview over a whole folder, a receipt of completed items) scale with the panel, not with the
// number of plan steps; the model-facing context already allows panels up to 1,000,000 entries.
const MAX_ITEMS = 1_000_000;
const MAX_JOBS = 256;
const REF = /^[A-Za-z0-9_-]{1,128}$/;
// Parentheses are allowed so the collision rename `name (2).zip` (local, contextTarget) is valid.
const BASENAME = /^[A-Za-z0-9][A-Za-z0-9 _.()-]{0,127}$/;
export const MAX_EXTENSIONS = 8;
const EXTENSION = /^[a-z0-9]{1,16}$/;

/** Basename keluaran yang aman (tanpa separator/ADS/traversal/nama perangkat Windows). */
export function isSafeOutputName(name: unknown): name is string {
  return (
    typeof name === 'string' &&
    BASENAME.test(name) &&
    name !== '.' &&
    name !== '..' &&
    !/[. ]$/.test(name) &&
    !/^(con|prn|aux|nul|com[1-9]|lpt[1-9]) *(?:\.|$)/i.test(name)
  );
}
/** Satu ekstensi filter: huruf kecil/angka, tanpa titik, wildcard atau pemisah path. */
export function isSafeExtension(ext: unknown): ext is string {
  return typeof ext === 'string' && EXTENSION.test(ext);
}

function fail(code: ContractErrorCode, message: string): never {
  throw new ContractError(code, message);
}
function obj(
  value: unknown,
  keys: readonly string[],
  required: readonly string[],
): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    fail('invalid_payload', 'Expected object');
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).some((key) => !keys.includes(key)) ||
    required.some((key) => !Object.hasOwn(record, key))
  ) {
    fail('invalid_payload', 'Unknown or missing field');
  }
  return record;
}
function literal<T extends string | number | boolean>(value: unknown, expected: T): T {
  if (value !== expected) fail('invalid_payload', `Expected ${expected}`);
  return expected;
}
function choice<T extends string>(value: unknown, options: readonly T[]): T {
  if (typeof value !== 'string' || !options.includes(value as T))
    fail('invalid_payload', 'Invalid enum');
  return value as T;
}
function ref(value: unknown): string {
  if (typeof value !== 'string' || !REF.test(value))
    fail('invalid_reference', 'Invalid opaque reference');
  return value;
}
function text(value: unknown, limit: number): string {
  if (typeof value !== 'string' || new TextEncoder().encode(value).length > limit)
    fail('limit_exceeded', 'Invalid text length');
  return value;
}
function count(value: unknown, max = MAX_STEPS): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0 || value > max)
    fail('limit_exceeded', 'Invalid count');
  return value;
}
function positive(value: unknown): number {
  const n = count(value);
  if (!n) fail('invalid_payload', 'Revision must be positive');
  return n;
}
function bounded(value: unknown): void {
  const pending: Array<{ item: unknown; depth: number; exit?: boolean }> = [
    { item: value, depth: 0 },
  ];
  const ancestors = new WeakSet<object>();
  let visited = 0;
  try {
    while (pending.length) {
      const { item, depth, exit } = pending.pop()!;
      if (exit) {
        ancestors.delete(item as object);
        continue;
      }
      if (++visited > MAX_JSON_BYTES) fail('limit_exceeded', 'Too many payload values');
      if (depth > MAX_DEPTH) fail('limit_exceeded', 'Payload too deep');
      if (item && typeof item === 'object') {
        if (ancestors.has(item)) fail('invalid_payload', 'Cyclic payload');
        ancestors.add(item);
        pending.push({ item, depth, exit: true });
        for (const child of Object.values(item)) pending.push({ item: child, depth: depth + 1 });
      }
    }
  } catch (error) {
    if (error instanceof ContractError) throw error;
    fail('invalid_payload', 'Uninspectable payload');
  }
  let json: string | undefined;
  try {
    json = JSON.stringify(value);
    if (!json || new TextEncoder().encode(json).length > MAX_JSON_BYTES) {
      fail('limit_exceeded', 'Payload too large');
    }
  } catch (error) {
    if (error instanceof ContractError) throw error;
    fail('invalid_payload', 'Unserializable payload');
  }
}
function safeWindowsPath(value: unknown): value is string {
  if (
    typeof value !== 'string' ||
    new TextEncoder().encode(value).length > 4096 ||
    !/^[A-Za-z]:\\/.test(value) ||
    value.includes('/') ||
    value.includes('\0')
  )
    return false;
  return value
    .slice(3)
    .split('\\')
    .every((segment) => {
      if (
        !segment ||
        segment === '.' ||
        segment === '..' ||
        /[. ]$/.test(segment) ||
        /[<>:"|?*\p{Cc}]/u.test(segment)
      )
        return false;
      const stem = segment.split('.')[0].trimEnd();
      return !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i.test(stem);
    });
}
function version(value: unknown): void {
  if (value !== 1) fail('unsupported_version', 'Expected schema version 1');
}
function paneRef(value: unknown): PaneRef {
  return choice(value, ['left', 'right', 'activeAtSubmit']);
}
function extensions(value: unknown): void {
  if (!Array.isArray(value) || value.length < 1 || value.length > MAX_EXTENSIONS) {
    fail('limit_exceeded', 'Invalid extension list');
  }
  const seen = new Set<string>();
  for (const ext of value) {
    if (!isSafeExtension(ext) || seen.has(ext)) fail('invalid_payload', 'Invalid extension');
    seen.add(ext);
  }
}
/** Sama dengan `ID` di activityQuery.ts (id entity opak dari layanan riwayat lokal). */
const ENTITY_ID = /^[A-Za-z0-9_.:-]{1,128}$/;
function selector(value: unknown, allowEntity = false): void {
  const s = obj(value, ['kind', 'entries', 'includeHidden', 'extensions', 'entityId'], ['kind']);
  switch (s.kind) {
    case 'entity':
      if (!allowEntity) fail('invalid_payload', 'Unknown selector');
      obj(s, ['kind', 'entityId'], ['kind', 'entityId']);
      if (typeof s.entityId !== 'string' || !ENTITY_ID.test(s.entityId))
        fail('invalid_payload', 'Invalid entity id');
      break;
    case 'selected':
      obj(s, ['kind', 'extensions'], ['kind']);
      break;
    case 'allVisible':
      obj(s, ['kind', 'entries', 'extensions'], ['kind', 'entries']);
      choice(s.entries, ['all', 'filesOnly']);
      break;
    case 'directoryContents':
      obj(
        s,
        ['kind', 'entries', 'includeHidden', 'extensions'],
        ['kind', 'entries', 'includeHidden'],
      );
      choice(s.entries, ['all', 'filesOnly']);
      if (typeof s.includeHidden !== 'boolean') fail('invalid_payload', 'Invalid hidden flag');
      break;
    default:
      fail('invalid_payload', 'Unknown selector');
  }
  if (s.extensions !== undefined) extensions(s.extensions);
}
function target(value: unknown): void {
  const t = obj(value, ['kind', 'pane'], ['kind']);
  if (t.kind === 'sourceDirectory') obj(t, ['kind'], ['kind']);
  else if (t.kind === 'pane') {
    obj(t, ['kind', 'pane'], ['kind', 'pane']);
    paneRef(t.pane);
  } else fail('invalid_payload', 'Unknown target');
}

export interface ParseIntentOptions {
  /** HANYA untuk envelope buatan aplikasi (FER-433); output model tidak pernah memakainya. */
  allowEntitySelector?: boolean;
}

export function parseIntent(
  value: unknown,
  activeRequestId: string,
  options: ParseIntentOptions = {},
): IntentEnvelope {
  bounded(value);
  const envelope = obj(
    value,
    ['schemaVersion', 'requestId', 'intent'],
    ['schemaVersion', 'requestId', 'intent'],
  );
  version(envelope.schemaVersion);
  ref(envelope.requestId);
  if (envelope.requestId !== activeRequestId) fail('request_mismatch', 'Request no longer active');
  const intent = obj(
    envelope.intent,
    [
      'capability',
      'sourcePane',
      'selector',
      'format',
      'level',
      'target',
      'outputName',
      'leftPane',
      'rightPane',
      'mode',
    ],
    ['capability'],
  );
  if (intent.capability === 'archive.create') {
    obj(
      intent,
      ['capability', 'sourcePane', 'selector', 'format', 'level', 'target', 'outputName'],
      ['capability', 'sourcePane', 'selector', 'format', 'target'],
    );
    paneRef(intent.sourcePane);
    selector(intent.selector, options.allowEntitySelector === true);
    if (!ARCHIVE_INTENT_FORMATS.includes(intent.format as ArchiveIntentFormat)) {
      fail('invalid_payload', 'Archive format must be zip or 7z');
    }
    if (
      intent.level !== undefined &&
      !ARCHIVE_INTENT_LEVELS.includes(intent.level as ArchiveIntentLevel)
    ) {
      fail('invalid_payload', 'Archive level is not allowed');
    }
    target(intent.target);
    if (intent.outputName !== undefined && !isSafeOutputName(intent.outputName)) {
      fail('invalid_payload', 'Output name must be safe basename');
    }
  } else if (intent.capability === 'directories.sync') {
    obj(
      intent,
      ['capability', 'leftPane', 'rightPane', 'mode'],
      ['capability', 'leftPane', 'rightPane', 'mode'],
    );
    literal(intent.leftPane, 'left');
    literal(intent.rightPane, 'right');
    literal(intent.mode, 'bidirectional');
  } else fail('invalid_payload', 'Unknown capability');
  return value as IntentEnvelope;
}

function historyQuery(value: unknown): void {
  const q = obj(value, ['kind', 'periodDays', 'name', 'entity'], ['kind', 'entity']);
  if (q.kind === 'recent' || q.kind === 'frequent') {
    obj(q, ['kind', 'periodDays', 'entity'], ['kind', 'periodDays', 'entity']);
    if (!HISTORY_PERIOD_DAYS.includes(q.periodDays as 7))
      fail('invalid_payload', 'History period must be 7, 30 or 90');
  } else if (q.kind === 'findByName') {
    obj(q, ['kind', 'name', 'entity'], ['kind', 'name', 'entity']);
    if (
      typeof q.name !== 'string' ||
      !q.name.trim() ||
      new TextEncoder().encode(q.name).length > MAX_HISTORY_NAME_BYTES ||
      /[\p{Cc}\\/:]/u.test(q.name)
    ) {
      fail('invalid_payload', 'History name must be a plain name');
    }
  } else fail('invalid_payload', 'Unknown history query');
  choice(q.entity, ['file', 'folder']);
}
/** Strict parse of the `query` object of `diskUsage.query` (Rust `DiskUsageQueryKind`). */
export function parseDiskUsageQueryKind(value: unknown): DiskUsageQueryKind {
  const q = obj(value, ['kind', 'scope', 'category', 'limit'], ['kind']);
  const kind = choice(q.kind, ['largestFiles', 'largestFolders', 'summary']);
  const has = (key: string) => Object.hasOwn(q, key);
  if (kind === 'summary') {
    obj(q, ['kind'], ['kind']);
  } else {
    obj(
      q,
      kind === 'largestFiles' ? ['kind', 'scope', 'category', 'limit'] : ['kind', 'scope', 'limit'],
      ['kind'],
    );
    if (has('scope')) choice(q.scope, DISK_USAGE_SCOPES);
    if (has('category')) choice(q.category, DISK_USAGE_CATEGORIES);
    if (has('limit')) {
      if (
        typeof q.limit !== 'number' ||
        !Number.isInteger(q.limit) ||
        q.limit < 1 ||
        q.limit > MAX_DISK_USAGE_LIMIT
      ) {
        fail('limit_exceeded', 'Disk usage limit must be 1..20');
      }
    }
  }
  return value as DiskUsageQueryKind;
}

function historyLimit(value: unknown): void {
  if (value === undefined) return;
  if (
    typeof value !== 'number' ||
    !Number.isInteger(value) ||
    value < 1 ||
    value > MAX_HISTORY_LIMIT
  ) {
    fail('limit_exceeded', 'History limit must be 1..20');
  }
}

const COPY_ADJUST_KEYS = ['action', 'by', 'threads', 'priority'] as const;

function copyAdjustFields(value: Record<string, unknown>): void {
  const action = choice(value.action, ['increase', 'decrease', 'set', 'auto', 'priority']);
  const has = (key: (typeof COPY_ADJUST_KEYS)[number]) => Object.hasOwn(value, key);
  const int = (n: unknown, min: number, max: number, what: string) => {
    if (typeof n !== 'number' || !Number.isInteger(n) || n < min || n > max)
      fail('limit_exceeded', what);
  };
  if (action === 'increase' || action === 'decrease') {
    if (has('threads') || has('priority')) fail('invalid_payload', 'Unexpected copy.adjust field');
    if (has('by')) int(value.by, 1, MAX_COPY_ADJUST_STEP, 'Copy step must be 1..8');
  } else if (action === 'set') {
    if (has('by') || has('priority')) fail('invalid_payload', 'Unexpected copy.adjust field');
    if (!has('threads')) fail('invalid_payload', 'Missing threads');
    int(value.threads, 1, MAX_COPY_THREADS, 'Copy threads must be 1..16');
  } else if (action === 'auto') {
    if (has('by') || has('threads') || has('priority'))
      fail('invalid_payload', 'Unexpected copy.adjust field');
  } else {
    if (has('by') || has('threads')) fail('invalid_payload', 'Unexpected copy.adjust field');
    if (!has('priority')) fail('invalid_payload', 'Missing priority');
    choice(value.priority, ['normal', 'background']);
  }
}

/** Strict wire of a copy tuning request (the intent minus `capability`). */
export function parseCopyAdjustRequest(value: unknown): CopyAdjustRequest {
  bounded(value);
  if (!value || typeof value !== 'object' || Array.isArray(value))
    fail('invalid_payload', 'Expected object');
  const record = obj(value, COPY_ADJUST_KEYS, ['action']);
  copyAdjustFields(record);
  return value as CopyAdjustRequest;
}

/** Strict wire of an indexer mode request (the intent minus `capability`): exactly one `mode`. */
export function parseIndexerSetModeRequest(value: unknown): IndexerSetModeRequest {
  bounded(value);
  if (!value || typeof value !== 'object' || Array.isArray(value))
    fail('invalid_payload', 'Expected object');
  const record = obj(value, ['mode'], ['mode']);
  choice(record.mode, [...INDEXER_MODE_CHOICES]);
  return value as IndexerSetModeRequest;
}

/** Strict wire of the `activity_chat_query` IPC (Rust `ActivityQuery` + `validate_activity_query`). */
export function parseActivityQuery(value: unknown): ActivityQuery {
  bounded(value);
  const q = obj(value, ['query', 'limit'], ['query']);
  historyQuery(q.query);
  historyLimit(q.limit);
  return value as ActivityQuery;
}

/**
 * Strict parser for a model reply (mirror of Rust `parse_intent_offered`): the capability must be in
 * `offered` (the list the app sent for this request). Mutating intents then go through `parseIntent`
 * unchanged; `activity.query` is only reachable here, never through `parseIntent`.
 */
export function parseOfferedIntent(
  value: unknown,
  activeRequestId: string,
  offered: readonly string[],
): OfferedIntentEnvelope {
  bounded(value);
  const envelope = obj(
    value,
    ['schemaVersion', 'requestId', 'intent'],
    ['schemaVersion', 'requestId', 'intent'],
  );
  const intent = obj(envelope.intent, Object.keys(envelope.intent ?? {}), ['capability']);
  if (typeof intent.capability !== 'string' || !offered.includes(intent.capability)) {
    fail('invalid_payload', 'Capability not offered');
  }
  if (
    intent.capability !== ACTIVITY_QUERY_CAPABILITY &&
    intent.capability !== COPY_ADJUST_CAPABILITY &&
    intent.capability !== INDEXER_STATUS_CAPABILITY &&
    intent.capability !== INDEXER_SET_MODE_CAPABILITY &&
    intent.capability !== DISK_USAGE_QUERY_CAPABILITY
  )
    return parseIntent(value, activeRequestId);
  version(envelope.schemaVersion);
  ref(envelope.requestId);
  if (envelope.requestId !== activeRequestId) fail('request_mismatch', 'Request no longer active');
  if (intent.capability === DISK_USAGE_QUERY_CAPABILITY) {
    const record = obj(intent, ['capability', 'query'], ['capability', 'query']);
    parseDiskUsageQueryKind(record.query);
    return value as OfferedIntentEnvelope;
  }
  if (intent.capability === INDEXER_STATUS_CAPABILITY) {
    obj(intent, ['capability'], ['capability']);
    return value as OfferedIntentEnvelope;
  }
  if (intent.capability === INDEXER_SET_MODE_CAPABILITY) {
    const record = obj(intent, ['capability', 'mode'], ['capability', 'mode']);
    choice(record.mode, [...INDEXER_MODE_CHOICES]);
    return value as OfferedIntentEnvelope;
  }
  if (intent.capability === COPY_ADJUST_CAPABILITY) {
    const record = obj(intent, ['capability', ...COPY_ADJUST_KEYS], ['capability', 'action']);
    copyAdjustFields(record);
    return value as OfferedIntentEnvelope;
  }
  obj(intent, ['capability', 'query', 'limit'], ['capability', 'query']);
  historyQuery(intent.query);
  historyLimit(intent.limit);
  return value as OfferedIntentEnvelope;
}

export function parsePaneSnapshot(value: unknown): PaneSnapshot {
  bounded(value);
  const s = obj(
    value,
    ['schemaVersion', 'requestId', 'activePaneAtSubmit', 'panes'],
    ['schemaVersion', 'requestId', 'activePaneAtSubmit', 'panes'],
  );
  version(s.schemaVersion);
  ref(s.requestId);
  choice(s.activePaneAtSubmit, ['left', 'right']);
  if (!Array.isArray(s.panes) || s.panes.length !== 2)
    fail('invalid_payload', 'Expected exactly two panes');
  for (const [i, entry] of s.panes.entries()) {
    const p = obj(
      entry,
      [
        'paneId',
        'tabId',
        'location',
        'contextRevision',
        'selectionRevision',
        'selectionHandle',
        'filter',
        'showHidden',
        'sort',
      ],
      [
        'paneId',
        'tabId',
        'location',
        'contextRevision',
        'selectionRevision',
        'selectionHandle',
        'filter',
        'showHidden',
        'sort',
      ],
    );
    literal(p.paneId, i === 0 ? 'left' : 'right');
    ref(p.tabId);
    ref(p.selectionHandle);
    count(p.contextRevision);
    count(p.selectionRevision);
    const loc = obj(p.location, ['kind', 'path'], ['kind', 'path']);
    choice(loc.kind, ['disk', 'archive']);
    if (!text(loc.path, 4096)) fail('invalid_payload', 'Empty location');
    text(p.filter, 256);
    if (typeof p.showHidden !== 'boolean') fail('invalid_payload', 'Invalid hidden flag');
    const sort = obj(p.sort, ['key', 'dir'], ['key', 'dir']);
    choice(sort.key, ['name', 'size', 'modified']);
    if (sort.dir !== 1 && sort.dir !== -1) fail('invalid_payload', 'Invalid sort direction');
  }
  if ((s.panes[0] as PaneStateSnapshot).tabId === (s.panes[1] as PaneStateSnapshot).tabId)
    fail('invalid_reference', 'Duplicate tab ID');
  return value as PaneSnapshot;
}
export function parseResolvedPlan(value: unknown): ResolvedPlan {
  bounded(value);
  const p = obj(
    value,
    [
      'schemaVersion',
      'planId',
      'requestId',
      'revision',
      'capability',
      'sourceSnapshot',
      'operation',
      'preview',
      'steps',
      'previewDigest',
      'risk',
      'requiresApproval',
      'stepCount',
    ],
    [
      'schemaVersion',
      'planId',
      'requestId',
      'revision',
      'capability',
      'sourceSnapshot',
      'operation',
      'preview',
      'steps',
      'previewDigest',
      'risk',
      'requiresApproval',
      'stepCount',
    ],
  );
  version(p.schemaVersion);
  ref(p.planId);
  ref(p.requestId);
  positive(p.revision);
  choice(p.capability, ['archive.create', 'directories.sync']);
  const snapshot = parsePaneSnapshot(p.sourceSnapshot);
  if (snapshot.requestId !== p.requestId)
    fail('request_mismatch', 'Plan snapshot request mismatch');
  const operation = obj(
    p.operation,
    [
      'kind',
      'sourceManifest',
      'targetDirectory',
      'outputName',
      'leftRoot',
      'rightRoot',
      'leftManifest',
      'rightManifest',
    ],
    ['kind'],
  );
  if (operation.kind !== p.capability) fail('invalid_payload', 'Operation capability mismatch');
  if (operation.kind === 'archive.create') {
    obj(
      operation,
      ['kind', 'sourceManifest', 'targetDirectory', 'outputName'],
      ['kind', 'sourceManifest', 'targetDirectory', 'outputName'],
    );
    ref(operation.sourceManifest);
    if (!safeWindowsPath(operation.targetDirectory))
      fail('invalid_reference', 'Unsafe target directory');
    if (
      typeof operation.outputName !== 'string' ||
      !BASENAME.test(operation.outputName) ||
      operation.outputName === '.' ||
      operation.outputName === '..' ||
      /[. ]$/.test(operation.outputName) ||
      /^(con|prn|aux|nul|com[1-9]|lpt[1-9]) *(?:\.|$)/i.test(operation.outputName)
    ) {
      fail('invalid_payload', 'Invalid resolved output name');
    }
  } else {
    obj(
      operation,
      ['kind', 'leftRoot', 'rightRoot', 'leftManifest', 'rightManifest'],
      ['kind', 'leftRoot', 'rightRoot', 'leftManifest', 'rightManifest'],
    );
    if (!safeWindowsPath(operation.leftRoot) || !safeWindowsPath(operation.rightRoot)) {
      fail('invalid_reference', 'Unsafe sync root');
    }
    ref(operation.leftManifest);
    ref(operation.rightManifest);
    if (
      snapshot.panes.some((pane) => pane.location.kind !== 'disk') ||
      operation.leftRoot !== snapshot.panes[0].location.path ||
      operation.rightRoot !== snapshot.panes[1].location.path
    ) {
      fail('invalid_reference', 'Sync roots not in disk snapshot');
    }
  }
  const preview = obj(
    p.preview,
    ['itemCount', 'conflictCount', 'totalBytes'],
    ['itemCount', 'conflictCount', 'totalBytes'],
  );
  count(preview.itemCount, MAX_ITEMS);
  count(preview.conflictCount, MAX_ITEMS);
  count(preview.totalBytes, Number.MAX_SAFE_INTEGER);
  if ((preview.conflictCount as number) > (preview.itemCount as number))
    fail('invalid_payload', 'Invalid conflict count');
  const stepCount = positive(p.stepCount);
  if (!Array.isArray(p.steps) || p.steps.length !== stepCount)
    fail('invalid_payload', 'Plan step count mismatch');
  const prior = new Set<string>();
  for (const value of p.steps) {
    const step = obj(value, ['id', 'capability', 'dependsOn'], ['id', 'capability', 'dependsOn']);
    const id = ref(step.id);
    if (prior.has(id)) fail('invalid_reference', 'Duplicate plan step');
    literal(step.capability, p.capability as ProposedIntent['capability']);
    if (!Array.isArray(step.dependsOn) || step.dependsOn.length > MAX_STEPS)
      fail('limit_exceeded', 'Invalid dependencies');
    const deps = new Set<string>();
    for (const dependency of step.dependsOn) {
      const dep = ref(dependency);
      if (!prior.has(dep) || deps.has(dep))
        fail('invalid_reference', 'Step dependency must precede step');
      deps.add(dep);
    }
    prior.add(id);
  }
  ref(p.previewDigest);
  literal(p.risk, 'mutating');
  literal(p.requiresApproval, true);
  return value as ResolvedPlan;
}
export function parsePreparedHandle(value: unknown): PreparedHandle {
  bounded(value);
  const h = obj(
    value,
    ['schemaVersion', 'handleId', 'planId', 'requestId', 'revision', 'planDigest', 'expiresAtMs'],
    ['schemaVersion', 'handleId', 'planId', 'requestId', 'revision', 'planDigest', 'expiresAtMs'],
  );
  version(h.schemaVersion);
  ref(h.handleId);
  ref(h.planId);
  ref(h.requestId);
  positive(h.revision);
  ref(h.planDigest);
  count(h.expiresAtMs, Number.MAX_SAFE_INTEGER);
  return value as PreparedHandle;
}
export function assertPreparedForPlan(
  handleValue: unknown,
  planValue: unknown,
  nowMs: number,
): void {
  const handle = parsePreparedHandle(handleValue);
  const plan = parseResolvedPlan(planValue);
  if (
    !Number.isSafeInteger(nowMs) ||
    nowMs < 0 ||
    handle.expiresAtMs <= nowMs ||
    handle.planId !== plan.planId ||
    handle.requestId !== plan.requestId ||
    handle.revision !== plan.revision ||
    handle.planDigest !== plan.previewDigest
  ) {
    fail('invalid_reference', 'Prepared handle does not match current plan');
  }
}

export function parseExecutionReceipt(value: unknown): ExecutionReceipt {
  bounded(value);
  const r = obj(
    value,
    ['schemaVersion', 'executionId', 'planId', 'jobIds', 'state', 'completed', 'failed', 'errors'],
    ['schemaVersion', 'executionId', 'planId', 'jobIds', 'state', 'completed', 'failed', 'errors'],
  );
  version(r.schemaVersion);
  ref(r.executionId);
  ref(r.planId);
  if (!Array.isArray(r.jobIds) || r.jobIds.length > MAX_JOBS)
    fail('limit_exceeded', 'Too many jobs');
  r.jobIds.forEach(ref);
  choice(r.state, [
    'queued',
    'running',
    'completed',
    'partial',
    'failed',
    'cancelled',
    'unknownOutcome',
  ]);
  count(r.completed, MAX_ITEMS);
  count(r.failed, MAX_ITEMS);
  if (!Array.isArray(r.errors) || r.errors.length > MAX_JOBS)
    fail('limit_exceeded', 'Too many errors');
  if (
    (r.state === 'queued' && (r.completed !== 0 || r.failed !== 0 || r.errors.length !== 0)) ||
    (r.state === 'completed' && (r.failed !== 0 || r.errors.length !== 0))
  ) {
    fail('invalid_payload', 'Receipt state conflicts with results');
  }
  for (const error of r.errors) {
    const e = obj(error, ['code', 'message', 'itemRef'], ['code', 'message']);
    choice(e.code, [
      'invalid_payload',
      'unsupported_version',
      'request_mismatch',
      'invalid_reference',
      'limit_exceeded',
    ]);
    text(e.message, 512);
    if (e.itemRef !== undefined) ref(e.itemRef);
  }
  return value as ExecutionReceipt;
}
