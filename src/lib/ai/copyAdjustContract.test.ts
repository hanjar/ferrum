// FER-471: TS mirror of the Rust `copy.adjust` control intent (src-tauri/src/ai/contract.rs).
// Same shared cases as the Rust tests; legacy `parseIntent` keeps rejecting the control intent.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import cases from '../../../contract-fixtures/ai/copy-adjust-cases.json';
import {
  COPY_ADJUST_CAPABILITY,
  MAX_COPY_ADJUST_STEP,
  MAX_COPY_THREADS,
  parseCopyAdjustRequest,
  parseIntent,
  parseOfferedIntent,
} from './contracts';
import {
  COPY_ADJUST_SCHEMA,
  CONTROL_CAPABILITY_GATES,
  describeCopyAdjust,
  offeredControlCapabilityIds,
  previewCopyAdjust,
  toCopyAdjust,
} from './copyAdjust';
import { parseModelReply } from './provider';

const OFFERED = ['archive.create', 'directories.sync', COPY_ADJUST_CAPABILITY] as const;
const MUTATING_ONLY = ['archive.create', 'directories.sync'] as const;
const here = dirname(fileURLToPath(import.meta.url));
const rustContract = readFileSync(join(here, '../../../src-tauri/src/ai/contract.rs'), 'utf-8');
const rustProvider = readFileSync(join(here, '../../../src-tauri/src/ai/provider.rs'), 'utf-8');

describe('copy.adjust contract (Rust parity)', () => {
  it('shared cases: accepted exactly when Rust accepts them, if offered', () => {
    expect(cases.length).toBeGreaterThanOrEqual(30);
    for (const c of cases) {
      const run = () => parseOfferedIntent(c.value, 'req_1', OFFERED);
      if (c.valid) expect(run().intent.capability, c.name).toBe('copy.adjust');
      else expect(run, c.name).toThrow();
    }
  });

  it('legacy parseIntent and a list without copy.adjust reject every case', () => {
    for (const c of cases) {
      expect(() => parseIntent(c.value, 'req_1'), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', MUTATING_ONLY), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', []), c.name).toThrow();
    }
  });

  it('the IPC-style request wire (intent minus capability) uses the same rules', () => {
    expect(parseCopyAdjustRequest({ action: 'set', threads: 8 })).toEqual({
      action: 'set',
      threads: 8,
    });
    for (const bad of [
      { action: 'set' },
      { action: 'set', threads: 17 },
      { action: 'auto', threads: 4 },
      { action: 'increase', by: 9 },
      { action: 'priority', priority: 'realtime' },
      { capability: 'copy.adjust', action: 'auto' },
      { action: 'auto', operationId: 'op_1' },
      null,
      [],
    ])
      expect(() => parseCopyAdjustRequest(bad), JSON.stringify(bad)).toThrow();
  });

  it('bounds and ids equal the Rust constants', () => {
    expect(rustContract).toContain(
      `pub const COPY_ADJUST_CAPABILITY: &str = "${COPY_ADJUST_CAPABILITY}";`,
    );
    expect(rustContract).toContain(`pub const MAX_COPY_ADJUST_STEP: u8 = ${MAX_COPY_ADJUST_STEP};`);
    expect(rustContract).toContain(`pub const MAX_COPY_THREADS: u8 = ${MAX_COPY_THREADS};`);
    expect(rustProvider).toContain(
      `const CONTROL_CAPABILITY_PROMPTS: [(&str, &str); 1] = [("${COPY_ADJUST_CAPABILITY}"`,
    );
  });

  it('model-facing schema examples parse and the schema is closed', () => {
    expect(COPY_ADJUST_SCHEMA.additionalProperties).toBe(false);
    expect(COPY_ADJUST_SCHEMA.required).toEqual(['capability', 'action']);
    expect(COPY_ADJUST_SCHEMA.examples.length).toBeGreaterThanOrEqual(5);
    for (const intent of COPY_ADJUST_SCHEMA.examples) {
      expect(() =>
        parseOfferedIntent({ schemaVersion: 1, requestId: 'req_1', intent }, 'req_1', OFFERED),
      ).not.toThrow();
    }
  });
});

describe('parseModelReply: copy.adjust', () => {
  const reply = {
    schemaVersion: 1,
    requestId: 'req_1',
    intent: { capability: 'copy.adjust', action: 'increase', by: 2 },
  };

  it('becomes a copyAdjust event (without the capability key) only when offered', () => {
    expect(parseModelReply(reply, 'req_1', OFFERED)).toEqual({
      kind: 'copyAdjust',
      request: { action: 'increase', by: 2 },
    });
    expect(() => parseModelReply(reply, 'req_1', MUTATING_ONLY)).toThrow();
    expect(() => parseModelReply(reply, 'req_1', undefined)).toThrow();
  });

  it('rejects an invalid request even when offered', () => {
    const bad = { ...reply, intent: { capability: 'copy.adjust', action: 'set', threads: 99 } };
    expect(() => parseModelReply(bad, 'req_1', OFFERED)).toThrow();
  });
});

describe('control capability gate', () => {
  it('is offered only while a copy runs and its gate is ready', () => {
    expect(offeredControlCapabilityIds({ copyRunning: true })).toEqual(['copy.adjust']);
    expect(offeredControlCapabilityIds({ copyRunning: false })).toEqual([]);
    expect(CONTROL_CAPABILITY_GATES['copy.adjust'].status).toBe('ready');
    expect(
      offeredControlCapabilityIds({
        copyRunning: true,
        gates: { 'copy.adjust': { status: 'planned' } as never },
      }),
    ).toEqual([]);
  });
});

describe('local preview mirrors the backend semantics', () => {
  const before = { threads: 4, auto: true, priority: 'normal' as const };

  it('thread changes clamp to 1..16 and turn auto off', () => {
    expect(previewCopyAdjust(before, { action: 'increase' })).toEqual({
      threads: 5,
      auto: false,
      priority: 'normal',
    });
    expect(previewCopyAdjust(before, { action: 'increase', by: 8 }).threads).toBe(12);
    expect(
      previewCopyAdjust({ ...before, threads: 15 }, { action: 'increase', by: 8 }).threads,
    ).toBe(16);
    expect(previewCopyAdjust(before, { action: 'decrease', by: 8 }).threads).toBe(1);
    expect(previewCopyAdjust(before, { action: 'set', threads: 16 }).threads).toBe(16);
  });

  it('auto and priority leave the other knobs alone', () => {
    const manual = { threads: 6, auto: false, priority: 'normal' as const };
    expect(previewCopyAdjust(manual, { action: 'auto' })).toEqual({ ...manual, auto: true });
    expect(previewCopyAdjust(manual, { action: 'priority', priority: 'background' })).toEqual({
      ...manual,
      priority: 'background',
    });
  });

  it('wire changes use the progress dialog vocabulary; descriptions are app text', () => {
    expect(toCopyAdjust({ action: 'increase' })).toEqual({ kind: 'up', by: 1 });
    expect(toCopyAdjust({ action: 'decrease', by: 3 })).toEqual({ kind: 'down', by: 3 });
    expect(toCopyAdjust({ action: 'set', threads: 2 })).toEqual({ kind: 'set', threads: 2 });
    expect(toCopyAdjust({ action: 'auto' })).toEqual({ kind: 'auto', enabled: true });
    expect(toCopyAdjust({ action: 'priority', priority: 'normal' })).toEqual({
      kind: 'priority',
      priority: 'normal',
    });
    const after = previewCopyAdjust(before, { action: 'increase' });
    expect(describeCopyAdjust({ action: 'increase' }, before, after)).toContain('4 → 5');
  });
});
