// ACT-08 R4: TS mirror of the Rust `activity.query` read intent (src-tauri/src/ai/contract.rs).
// Same shared cases as the Rust tests; legacy `parseIntent` keeps rejecting the read intent.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import cases from '../../../contract-fixtures/ai/activity-query-cases.json';
import {
  ACTIVITY_QUERY_CAPABILITY,
  HISTORY_PERIOD_DAYS,
  MAX_HISTORY_LIMIT,
  MAX_HISTORY_NAME_BYTES,
  parseActivityQuery,
  parseIntent,
  parseOfferedIntent,
} from './contracts';
import {
  ACTIVITY_QUERY_SCHEMA,
  READ_CAPABILITY_GATES,
  offeredReadCapabilityIds,
} from './readCapabilities';

const OFFERED = ['archive.create', 'directories.sync', ACTIVITY_QUERY_CAPABILITY] as const;
const MUTATING_ONLY = ['archive.create', 'directories.sync'] as const;
const here = dirname(fileURLToPath(import.meta.url));
const rustContract = readFileSync(join(here, '../../../src-tauri/src/ai/contract.rs'), 'utf-8');
const rustProvider = readFileSync(join(here, '../../../src-tauri/src/ai/provider.rs'), 'utf-8');

const archive = {
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'archive.create',
    sourcePane: 'left',
    selector: { kind: 'selected' },
    format: 'zip',
    target: { kind: 'sourceDirectory' },
  },
};

describe('activity.query contract (Rust parity)', () => {
  it('shared cases: accepted exactly when Rust accepts them, if offered', () => {
    expect(cases.length).toBeGreaterThanOrEqual(20);
    for (const c of cases) {
      const run = () => parseOfferedIntent(c.value, 'req_1', OFFERED);
      if (c.valid) expect(run().intent.capability, c.name).toBe('activity.query');
      else expect(run, c.name).toThrow();
    }
  });

  it('legacy parseIntent and a list without activity.query reject every case', () => {
    for (const c of cases) {
      expect(() => parseIntent(c.value, 'req_1'), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', MUTATING_ONLY), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', []), c.name).toThrow();
    }
  });

  it('mutating intents parse as before and still need to be offered', () => {
    expect(parseOfferedIntent(archive, 'req_1', OFFERED)).toEqual(parseIntent(archive, 'req_1'));
    expect(parseOfferedIntent(archive, 'req_1', MUTATING_ONLY)).toEqual(archive);
    expect(() => parseOfferedIntent(archive, 'req_1', [ACTIVITY_QUERY_CAPABILITY])).toThrow();
  });

  it('IPC wire (query + limit) uses the same strict rules', () => {
    expect(
      parseActivityQuery({
        query: { kind: 'findByName', name: 'Invoice', entity: 'file' },
        limit: 3,
      }),
    ).toEqual({ query: { kind: 'findByName', name: 'Invoice', entity: 'file' }, limit: 3 });
    for (const bad of [
      { query: { kind: 'recent', periodDays: 7, entity: 'folder' }, workspaceRoot: 'C:\\' },
      { query: { kind: 'recent', periodDays: 7, entity: 'folder' }, limit: 21 },
      { query: { kind: 'recent', periodDays: 60, entity: 'folder' } },
      { query: { kind: 'findByName', name: 'a/b', entity: 'folder' } },
      { capability: 'activity.query', query: { kind: 'recent', periodDays: 7, entity: 'folder' } },
    ])
      expect(() => parseActivityQuery(bad), JSON.stringify(bad)).toThrow();
  });

  it('bounds equal the Rust constants', () => {
    expect(rustContract).toContain(`pub const MAX_HISTORY_LIMIT: u8 = ${MAX_HISTORY_LIMIT};`);
    expect(rustContract).toContain(
      `pub const MAX_HISTORY_NAME_BYTES: usize = ${MAX_HISTORY_NAME_BYTES};`,
    );
    expect(rustContract).toContain(
      `pub const HISTORY_PERIOD_DAYS: [u16; 3] = [${HISTORY_PERIOD_DAYS.join(', ')}];`,
    );
    expect(rustContract).toContain(
      `pub const ACTIVITY_QUERY_CAPABILITY: &str = "${ACTIVITY_QUERY_CAPABILITY}";`,
    );
    expect(rustProvider).toContain(
      `const READ_CAPABILITY_PROMPTS: [(&str, &str); 1] = [("${ACTIVITY_QUERY_CAPABILITY}"`,
    );
  });

  it('model-facing schema examples parse and the schema is closed', () => {
    expect(ACTIVITY_QUERY_SCHEMA.additionalProperties).toBe(false);
    expect(ACTIVITY_QUERY_SCHEMA.required).toEqual(['capability', 'query']);
    expect(ACTIVITY_QUERY_SCHEMA.examples.length).toBeGreaterThanOrEqual(3);
    for (const intent of ACTIVITY_QUERY_SCHEMA.examples) {
      expect(() =>
        parseOfferedIntent({ schemaVersion: 1, requestId: 'req_1', intent }, 'req_1', OFFERED),
      ).not.toThrow();
    }
  });
});

describe('read capability gate', () => {
  it('is open since R5-R7 v1, but only while the activity service is available', () => {
    expect(READ_CAPABILITY_GATES['activity.query'].status).toBe('ready');
    expect(offeredReadCapabilityIds({ activityAvailable: true })).toEqual(['activity.query']);
    expect(offeredReadCapabilityIds({ activityAvailable: false })).toEqual([]);
    const pending = { 'activity.query': { status: 'pending' as const, reason: 'off' } };
    expect(offeredReadCapabilityIds({ activityAvailable: true, gates: pending })).toEqual([]);
  });

  it('when opened, is offered only while the activity service is available', () => {
    const gates = { 'activity.query': { status: 'ready' as const } };
    expect(offeredReadCapabilityIds({ activityAvailable: true, gates })).toEqual([
      'activity.query',
    ]);
    expect(offeredReadCapabilityIds({ activityAvailable: false, gates })).toEqual([]);
  });
});
