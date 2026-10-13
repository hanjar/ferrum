export type EntityKind = 'file' | 'folder';
export type Availability = 'available' | 'offline' | 'missing' | 'inaccessible' | 'unknown';
export type MatchKind =
  'exactPath' | 'exactName' | 'prefix' | 'token' | 'path' | 'initialism' | 'fuzzy';
export type ReasonCode =
  | 'exactName'
  | 'similarName'
  | 'abbreviation'
  | 'recent'
  | 'frequent'
  | 'favorite'
  | 'sameWorkspace'
  | 'visitedInFerrum';
export type ActivityKind =
  'folderVisit' | 'fileOpenRequested' | 'filePreview' | 'operationCompleted';
export type ReceiptSource =
  'navigationCommit' | 'openAccepted' | 'previewCompleted' | 'jobCompleted';

export interface ActivityEntity {
  id: string;
  kind: EntityKind;
  provider: 'local';
  volumeId: string;
  locator: string;
  displayName: string;
  parentPath: string;
  availability: Availability;
}
export interface ActivityBounds {
  fromUtcMs: number;
  untilUtcMs: number;
}
export interface ActivityQuery {
  version: 1;
  requestId: string;
  privacyEpoch: number;
  view: 'recent' | 'frequent' | 'history';
  query: string;
  kind: EntityKind | null;
  bounds: ActivityBounds | null;
  limit: number;
  cursor: string | null;
  workspaceId: string | null;
}
export interface ActivityEntry {
  entity: ActivityEntity;
  lastAccessUtcMs: number;
  episodeCount: number;
  activeDays: number;
  frequentScore: number;
  matchKind: MatchKind;
  reasonCodes: ReasonCode[];
}
export interface ActivityCoverage {
  fromUtcMs: number;
  untilUtcMs: number;
  oldestRetainedUtcMs: number | null;
  truncated: boolean;
}
export interface ActivityResult {
  version: 1;
  requestId: string;
  privacyEpoch: number;
  rankingVersion: number;
  complete: boolean;
  nextCursor: string | null;
  coverage: ActivityCoverage;
  entries: ActivityEntry[];
}
export interface ActivityReceipt {
  version: 1;
  eventId: string;
  actionId: string;
  privacyEpoch: number;
  source: ReceiptSource;
  kind: ActivityKind;
  provenance: 'manual' | 'chat';
  entity: ActivityEntity;
  occurredUtcMs: number;
}
export interface ActivityPreferences {
  version: 1;
  privacyEpoch: number;
  recordingEnabled: boolean;
  privateSession: boolean;
  retentionDays: 7 | 30 | 90;
  excludedRoots: string[];
  hiddenEntityIds: string[];
}
export interface ActivityStats {
  version: 1;
  privacyEpoch: number;
  bounds: ActivityBounds;
  coverage: ActivityCoverage;
  episodeCount: number;
  uniqueEntities: number;
  daily: { utcDay: string; episodeCount: number }[];
}

function object(value: unknown, keys: string[]): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value))
    throw new Error('Expected object');
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).some((key) => !keys.includes(key)) ||
    keys.some((key) => !(key in record))
  ) {
    throw new Error('Unexpected or missing field');
  }
  return record;
}
function text(value: unknown, max = 4096): string {
  if (typeof value !== 'string' || value.length > max || value.includes('\0'))
    throw new Error('Invalid text');
  return value;
}
function id(value: unknown): string {
  const result = text(value, 128);
  if (!result.trim()) throw new Error('Empty ID');
  return result;
}
function integer(value: unknown): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0)
    throw new Error('Invalid integer');
  return value;
}
function finite(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0)
    throw new Error('Invalid score');
  return value;
}
function boolean(value: unknown): boolean {
  if (typeof value !== 'boolean') throw new Error('Invalid boolean');
  return value;
}
function choice<T extends string>(value: unknown, choices: readonly T[]): T {
  if (typeof value !== 'string' || !choices.includes(value as T))
    throw new Error('Invalid discriminator');
  return value as T;
}
function nullable<T>(value: unknown, parse: (value: unknown) => T): T | null {
  return value === null ? null : parse(value);
}
function version(value: unknown): 1 {
  if (value !== 1) throw new Error('Unsupported contract version');
  return 1;
}
function locator(value: unknown): string {
  const path = text(value);
  if (
    !(/^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\') || path.startsWith('/')) ||
    path.split(/[\\/]/).includes('..')
  ) {
    throw new Error('Expected absolute local path');
  }
  return path;
}
function bounds(value: unknown): ActivityBounds {
  const row = object(value, ['fromUtcMs', 'untilUtcMs']);
  const fromUtcMs = integer(row.fromUtcMs);
  const untilUtcMs = integer(row.untilUtcMs);
  if (fromUtcMs >= untilUtcMs) throw new Error('Invalid time bounds');
  return { fromUtcMs, untilUtcMs };
}
function entity(value: unknown): ActivityEntity {
  const row = object(value, [
    'id',
    'kind',
    'provider',
    'volumeId',
    'locator',
    'displayName',
    'parentPath',
    'availability',
  ]);
  return {
    id: id(row.id),
    kind: choice(row.kind, ['file', 'folder']),
    provider: choice(row.provider, ['local']),
    volumeId: id(row.volumeId),
    locator: locator(row.locator),
    displayName: text(row.displayName, 255),
    parentPath: locator(row.parentPath),
    availability: choice(row.availability, [
      'available',
      'offline',
      'missing',
      'inaccessible',
      'unknown',
    ]),
  };
}
function coverage(value: unknown): ActivityCoverage {
  const row = object(value, ['fromUtcMs', 'untilUtcMs', 'oldestRetainedUtcMs', 'truncated']);
  const fromUtcMs = integer(row.fromUtcMs);
  const untilUtcMs = integer(row.untilUtcMs);
  if (fromUtcMs >= untilUtcMs) throw new Error('Invalid coverage');
  return {
    fromUtcMs,
    untilUtcMs,
    oldestRetainedUtcMs: nullable(row.oldestRetainedUtcMs, integer),
    truncated: boolean(row.truncated),
  };
}

export function parseActivityPreferences(value: unknown): ActivityPreferences {
  const row = object(value, [
    'version',
    'privacyEpoch',
    'recordingEnabled',
    'privateSession',
    'retentionDays',
    'excludedRoots',
    'hiddenEntityIds',
  ]);
  const retentionDays = integer(row.retentionDays);
  if (retentionDays !== 7 && retentionDays !== 30 && retentionDays !== 90)
    throw new Error('Invalid retention');
  if (
    !Array.isArray(row.excludedRoots) ||
    !Array.isArray(row.hiddenEntityIds) ||
    row.excludedRoots.length > 1000 ||
    row.hiddenEntityIds.length > 1000
  ) {
    throw new Error('Invalid privacy lists');
  }
  return {
    version: version(row.version),
    privacyEpoch: integer(row.privacyEpoch),
    recordingEnabled: boolean(row.recordingEnabled),
    privateSession: boolean(row.privateSession),
    retentionDays,
    excludedRoots: row.excludedRoots.map(locator),
    hiddenEntityIds: row.hiddenEntityIds.map(id),
  };
}

function utcDay(value: unknown): string {
  const day = text(value, 10);
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!match) throw new Error('Invalid UTC day');
  const year = Number(match[1]);
  const month = Number(match[2]);
  const date = Number(match[3]);
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (month < 1 || month > 12 || date < 1 || date > days[month - 1])
    throw new Error('Invalid UTC day');
  return day;
}

export function parseActivityStats(value: unknown): ActivityStats {
  const row = object(value, [
    'version',
    'privacyEpoch',
    'bounds',
    'coverage',
    'episodeCount',
    'uniqueEntities',
    'daily',
  ]);
  const period = bounds(row.bounds);
  const dataCoverage = coverage(row.coverage);
  const episodeCount = integer(row.episodeCount);
  const uniqueEntities = integer(row.uniqueEntities);
  if (!Array.isArray(row.daily) || row.daily.length > 1000) throw new Error('Invalid daily counts');
  const daily = row.daily.map((value) => {
    const item = object(value, ['utcDay', 'episodeCount']);
    return { utcDay: utcDay(item.utcDay), episodeCount: integer(item.episodeCount) };
  });
  if (
    dataCoverage.fromUtcMs !== period.fromUtcMs ||
    dataCoverage.untilUtcMs !== period.untilUtcMs ||
    uniqueEntities > episodeCount ||
    daily.reduce((total, day) => total + day.episodeCount, 0) !== episodeCount ||
    daily.some((day, index) => index > 0 && day.utcDay <= daily[index - 1].utcDay)
  ) {
    throw new Error('Inconsistent activity statistics');
  }
  return {
    version: version(row.version),
    privacyEpoch: integer(row.privacyEpoch),
    bounds: period,
    coverage: dataCoverage,
    episodeCount,
    uniqueEntities,
    daily,
  };
}

export function parseActivityQuery(value: unknown): ActivityQuery {
  const row = object(value, [
    'version',
    'requestId',
    'privacyEpoch',
    'view',
    'query',
    'kind',
    'bounds',
    'limit',
    'cursor',
    'workspaceId',
  ]);
  const limit = integer(row.limit);
  if (limit < 1 || limit > 100) throw new Error('Invalid page limit');
  return {
    version: version(row.version),
    requestId: id(row.requestId),
    privacyEpoch: integer(row.privacyEpoch),
    view: choice(row.view, ['recent', 'frequent', 'history']),
    query: text(row.query, 256),
    kind: nullable(row.kind, (v) => choice(v, ['file', 'folder'])),
    bounds: nullable(row.bounds, bounds),
    limit,
    cursor: nullable(row.cursor, id),
    workspaceId: nullable(row.workspaceId, id),
  };
}
export function parseActivityResult(value: unknown): ActivityResult {
  const row = object(value, [
    'version',
    'requestId',
    'privacyEpoch',
    'rankingVersion',
    'complete',
    'nextCursor',
    'coverage',
    'entries',
  ]);
  if (!Array.isArray(row.entries) || row.entries.length > 100) throw new Error('Invalid entries');
  return {
    version: version(row.version),
    requestId: id(row.requestId),
    privacyEpoch: integer(row.privacyEpoch),
    rankingVersion: integer(row.rankingVersion),
    complete: boolean(row.complete),
    nextCursor: nullable(row.nextCursor, id),
    coverage: coverage(row.coverage),
    entries: row.entries.map((entry): ActivityEntry => {
      const item = object(entry, [
        'entity',
        'lastAccessUtcMs',
        'episodeCount',
        'activeDays',
        'frequentScore',
        'matchKind',
        'reasonCodes',
      ]);
      if (!Array.isArray(item.reasonCodes)) throw new Error('Invalid reasons');
      return {
        entity: entity(item.entity),
        lastAccessUtcMs: integer(item.lastAccessUtcMs),
        episodeCount: integer(item.episodeCount),
        activeDays: integer(item.activeDays),
        frequentScore: finite(item.frequentScore),
        matchKind: choice(item.matchKind, [
          'exactPath',
          'exactName',
          'prefix',
          'token',
          'path',
          'initialism',
          'fuzzy',
        ]),
        reasonCodes: item.reasonCodes.map((reason) =>
          choice(reason, [
            'exactName',
            'similarName',
            'abbreviation',
            'recent',
            'frequent',
            'favorite',
            'sameWorkspace',
            'visitedInFerrum',
          ]),
        ),
      };
    }),
  };
}
export function parseActivityReceipt(value: unknown): ActivityReceipt {
  const row = object(value, [
    'version',
    'eventId',
    'actionId',
    'privacyEpoch',
    'source',
    'kind',
    'provenance',
    'entity',
    'occurredUtcMs',
  ]);
  const source = choice(row.source, [
    'navigationCommit',
    'openAccepted',
    'previewCompleted',
    'jobCompleted',
  ]);
  const kind = choice(row.kind, [
    'folderVisit',
    'fileOpenRequested',
    'filePreview',
    'operationCompleted',
  ]);
  const item = entity(row.entity);
  const expected: Record<ReceiptSource, ActivityKind> = {
    navigationCommit: 'folderVisit',
    openAccepted: 'fileOpenRequested',
    previewCompleted: 'filePreview',
    jobCompleted: 'operationCompleted',
  };
  if (
    kind !== expected[source] ||
    (kind === 'folderVisit' && item.kind !== 'folder') ||
    (kind === 'fileOpenRequested' && item.kind !== 'file')
  ) {
    throw new Error('Receipt source and entity mismatch');
  }
  return {
    version: version(row.version),
    eventId: id(row.eventId),
    actionId: id(row.actionId),
    privacyEpoch: integer(row.privacyEpoch),
    source,
    kind,
    provenance: choice(row.provenance, ['manual', 'chat']),
    entity: item,
    occurredUtcMs: integer(row.occurredUtcMs),
  };
}
