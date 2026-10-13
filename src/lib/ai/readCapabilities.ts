// ACT-08 R3/R4: read capabilities (no plan, no approval, no mutation). Kept apart from the
// mutating capability registry so BASE_RISK / ENGINE_GATES / facade stay untouched.
//
// Offer rule (documented in docs/activity/chat-integration.md): `activity.query` is offered to the
// provider only when its gate is ready AND the activity service is available AND the first-run
// disclosure was acknowledged (FER-432; the port's `available()` folds the ack in). A paused history is
// still "available": the local query answers `withheld`. The backend applies the same availability
// rule again (`gate_read_capabilities` in src-tauri/src/ai/provider.rs).
//
// The gate is READY since R5-R7 v1 (executor answer path + answer card, ACT-08). It is still only
// offered when the executor has an `ActivityQueryPort` (aiRuntime wires it) AND the service is open.
import {
  DISK_USAGE_CATEGORIES,
  DISK_USAGE_SCOPES,
  HISTORY_PERIOD_DAYS,
  MAX_DISK_USAGE_LIMIT,
  MAX_HISTORY_LIMIT,
  MAX_HISTORY_NAME_BYTES,
  type ReadCapabilityId,
} from './contracts';
import type { EngineGate, IntentSchema } from './commandsRegistry';

export const READ_CAPABILITY_GATES: Record<ReadCapabilityId, EngineGate> = {
  'activity.query': { status: 'ready' },
  'diskUsage.query': { status: 'ready' },
};

export interface ReadOfferInput {
  /**
   * True when the activity service opened (Rust `ActivityHandle` is Some) AND the first-run
   * disclosure was acknowledged (FER-432; folded in by the port's `available()`).
   */
  activityAvailable: boolean;
  /**
   * FER-466: a Disk Usage result is on screen (workspace open) or still held by the workspace.
   * Absent = false, so callers that predate `diskUsage.query` never offer it.
   */
  diskUsageAvailable?: boolean;
  /** Test/integration override of the gate table. */
  gates?: Partial<Record<ReadCapabilityId, EngineGate>>;
}

export function offeredReadCapabilityIds(input: ReadOfferInput): ReadCapabilityId[] {
  const gates = { ...READ_CAPABILITY_GATES, ...input.gates };
  const ids: ReadCapabilityId[] = [];
  if (input.activityAvailable && gates['activity.query'].status === 'ready') {
    ids.push('activity.query');
  }
  if (input.diskUsageAvailable === true && gates['diskUsage.query'].status === 'ready') {
    ids.push('diskUsage.query');
  }
  return ids;
}

const ENTITY = { enum: ['file', 'folder'] };

/** Model-facing schema; mirrors `parseOfferedIntent` (examples are checked by tests). */
export const ACTIVITY_QUERY_SCHEMA: IntentSchema = {
  type: 'object',
  additionalProperties: false,
  required: ['capability', 'query'],
  properties: {
    capability: { const: 'activity.query' },
    query: {
      oneOf: [
        {
          type: 'object',
          additionalProperties: false,
          required: ['kind', 'periodDays', 'entity'],
          properties: {
            kind: { enum: ['recent', 'frequent'] },
            periodDays: { enum: [...HISTORY_PERIOD_DAYS] },
            entity: ENTITY,
          },
        },
        {
          type: 'object',
          additionalProperties: false,
          required: ['kind', 'name', 'entity'],
          properties: {
            kind: { const: 'findByName' },
            name: {
              type: 'string',
              minLength: 1,
              maxLength: MAX_HISTORY_NAME_BYTES,
              pattern: '^[^\\\\/:]+$',
            },
            entity: ENTITY,
          },
        },
      ],
    },
    limit: { type: 'integer', minimum: 1, maximum: MAX_HISTORY_LIMIT },
  },
  examples: [
    { capability: 'activity.query', query: { kind: 'recent', periodDays: 7, entity: 'file' } },
    {
      capability: 'activity.query',
      query: { kind: 'frequent', periodDays: 30, entity: 'folder' },
      limit: 5,
    },
    {
      capability: 'activity.query',
      query: { kind: 'findByName', name: 'invoice 2026', entity: 'folder' },
    },
  ],
};

/** FER-466 model-facing schema; mirrors `parseOfferedIntent` (examples are checked by tests). */
export const DISK_USAGE_QUERY_SCHEMA: IntentSchema = {
  type: 'object',
  additionalProperties: false,
  required: ['capability', 'query'],
  properties: {
    capability: { const: 'diskUsage.query' },
    query: {
      oneOf: [
        {
          type: 'object',
          additionalProperties: false,
          required: ['kind'],
          properties: {
            kind: { const: 'largestFiles' },
            scope: { enum: [...DISK_USAGE_SCOPES] },
            category: { enum: [...DISK_USAGE_CATEGORIES] },
            limit: { type: 'integer', minimum: 1, maximum: MAX_DISK_USAGE_LIMIT },
          },
        },
        {
          type: 'object',
          additionalProperties: false,
          required: ['kind'],
          properties: {
            kind: { const: 'largestFolders' },
            scope: { enum: [...DISK_USAGE_SCOPES] },
            limit: { type: 'integer', minimum: 1, maximum: MAX_DISK_USAGE_LIMIT },
          },
        },
        {
          type: 'object',
          additionalProperties: false,
          required: ['kind'],
          properties: { kind: { const: 'summary' } },
        },
      ],
    },
  },
  examples: [
    { capability: 'diskUsage.query', query: { kind: 'largestFiles', limit: 5 } },
    {
      capability: 'diskUsage.query',
      query: { kind: 'largestFiles', scope: 'root', category: 'video' },
    },
    { capability: 'diskUsage.query', query: { kind: 'largestFolders', limit: 10 } },
    { capability: 'diskUsage.query', query: { kind: 'summary' } },
  ],
};
