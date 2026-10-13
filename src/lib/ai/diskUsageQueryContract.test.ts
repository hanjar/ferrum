// FER-466: TS mirror of the Rust `diskUsage.query` read intent (src-tauri/src/ai/contract.rs).
// Same shared cases as the Rust tests; legacy `parseIntent` keeps rejecting the read intent.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import cases from '../../../contract-fixtures/ai/disk-usage-cases.json';
import {
  DISK_USAGE_QUERY_CAPABILITY,
  MAX_DISK_USAGE_LIMIT,
  parseDiskUsageQueryKind,
  parseIntent,
  parseOfferedIntent,
} from './contracts';
import { FakeIntentProvider, parseModelReply, type ProviderEvent } from './provider';
import {
  DISK_USAGE_QUERY_SCHEMA,
  READ_CAPABILITY_GATES,
  offeredReadCapabilityIds,
} from './readCapabilities';

const OFFERED = ['archive.create', 'directories.sync', DISK_USAGE_QUERY_CAPABILITY] as const;
const MUTATING_ONLY = ['archive.create', 'directories.sync'] as const;
const here = dirname(fileURLToPath(import.meta.url));
const rustContract = readFileSync(join(here, '../../../src-tauri/src/ai/contract.rs'), 'utf-8');
const rustProvider = readFileSync(join(here, '../../../src-tauri/src/ai/provider.rs'), 'utf-8');

describe('diskUsage.query contract (Rust parity)', () => {
  it('shared cases: accepted exactly when Rust accepts them, if offered', () => {
    expect(cases.length).toBeGreaterThanOrEqual(30);
    for (const c of cases) {
      const run = () => parseOfferedIntent(c.value, 'req_1', OFFERED);
      if (c.valid) expect(run().intent.capability, c.name).toBe('diskUsage.query');
      else expect(run, c.name).toThrow();
    }
  });

  it('legacy parseIntent and a list without diskUsage.query reject every case', () => {
    for (const c of cases) {
      expect(() => parseIntent(c.value, 'req_1'), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', MUTATING_ONLY), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', []), c.name).toThrow();
    }
  });

  it('the query object alone is parsed with the same strict rules', () => {
    expect(parseDiskUsageQueryKind({ kind: 'largestFiles', limit: 3 })).toEqual({
      kind: 'largestFiles',
      limit: 3,
    });
    for (const bad of [
      { kind: 'summary', limit: 1 },
      { kind: 'largestFolders', category: 'video' },
      { kind: 'largestFiles', limit: 21 },
      { kind: 'largestFiles', scope: null },
      { kind: 'largestFiles', path: 'C:\\' },
      { kind: 'wipe' },
      'summary',
      null,
    ])
      expect(() => parseDiskUsageQueryKind(bad), JSON.stringify(bad)).toThrow();
  });

  it('bounds and ids equal the Rust constants', () => {
    expect(rustContract).toContain(`pub const MAX_DISK_USAGE_LIMIT: u8 = ${MAX_DISK_USAGE_LIMIT};`);
    expect(rustContract).toContain(
      `pub const DISK_USAGE_QUERY_CAPABILITY: &str = "${DISK_USAGE_QUERY_CAPABILITY}";`,
    );
    expect(rustProvider).toContain(`("${DISK_USAGE_QUERY_CAPABILITY}", DISK_USAGE_QUERY_PROMPT)`);
    for (const word of [
      'View',
      'Root',
      'Video',
      'Image',
      'Audio',
      'Document',
      'Archive',
      'Other',
    ]) {
      expect(rustContract).toContain(`    ${word},`);
    }
  });

  it('model-facing schema is closed and its examples parse', () => {
    expect(DISK_USAGE_QUERY_SCHEMA.additionalProperties).toBe(false);
    expect(DISK_USAGE_QUERY_SCHEMA.required).toEqual(['capability', 'query']);
    expect(DISK_USAGE_QUERY_SCHEMA.examples.length).toBeGreaterThanOrEqual(3);
    for (const intent of DISK_USAGE_QUERY_SCHEMA.examples) {
      expect(() =>
        parseOfferedIntent({ schemaVersion: 1, requestId: 'req_1', intent }, 'req_1', OFFERED),
      ).not.toThrow();
    }
  });
});

describe('provider mapping', () => {
  const reply = {
    schemaVersion: 1,
    requestId: 'req_1',
    intent: { capability: 'diskUsage.query', query: { kind: 'largestFiles', limit: 5 } },
  };

  it('maps an offered reply to a diskUsageQuery event', () => {
    expect(parseModelReply(reply, 'req_1', OFFERED)).toEqual({
      kind: 'diskUsageQuery',
      query: { kind: 'largestFiles', limit: 5 },
    });
  });

  it('an unoffered reply is refused (falls to the strict mutating parser)', () => {
    expect(() => parseModelReply(reply, 'req_1', MUTATING_ONLY)).toThrow();
    expect(() => parseModelReply(reply, 'req_1', undefined)).toThrow();
  });

  it('the fake provider yields the event for an offered request', async () => {
    const provider = new FakeIntentProvider({ req_1: reply });
    const events: ProviderEvent[] = [];
    for await (const e of provider.plan({
      requestId: 'req_1',
      userText: 'paling gede apa',
      context: {
        activePane: 'left',
        leftLocation: 'disk',
        rightLocation: 'disk',
        leftCount: 1,
        rightCount: 1,
        showHidden: false,
        filterActive: false,
      },
      capabilities: OFFERED,
      signal: new AbortController().signal,
    }))
      events.push(e);
    expect(events).toEqual([{ kind: 'diskUsageQuery', query: { kind: 'largestFiles', limit: 5 } }]);
  });
});

describe('read capability gate', () => {
  it('is ready, but offered only while a Disk Usage result is available', () => {
    expect(READ_CAPABILITY_GATES['diskUsage.query'].status).toBe('ready');
    expect(
      offeredReadCapabilityIds({ activityAvailable: false, diskUsageAvailable: true }),
    ).toEqual(['diskUsage.query']);
    expect(offeredReadCapabilityIds({ activityAvailable: false })).toEqual([]);
    expect(
      offeredReadCapabilityIds({ activityAvailable: false, diskUsageAvailable: false }),
    ).toEqual([]);
    expect(offeredReadCapabilityIds({ activityAvailable: true, diskUsageAvailable: true })).toEqual(
      ['activity.query', 'diskUsage.query'],
    );
    const pending = { 'diskUsage.query': { status: 'pending' as const, reason: 'off' } };
    expect(
      offeredReadCapabilityIds({
        activityAvailable: false,
        diskUsageAvailable: true,
        gates: pending,
      }),
    ).toEqual([]);
  });

  it('old callers (activity only) keep their exact result', () => {
    expect(offeredReadCapabilityIds({ activityAvailable: true })).toEqual(['activity.query']);
  });
});
