import { describe, expect, it } from 'vitest';
import cases from '../../../contract-fixtures/ai/cases.json';
import trustedCases from '../../../contract-fixtures/ai/trusted-cases.json';
import {
  ContractError,
  parseIntent,
  parsePaneSnapshot,
  parseResolvedPlan,
  parsePreparedHandle,
  assertPreparedForPlan,
  parseExecutionReceipt,
} from './contracts';

const sync = {
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'directories.sync',
    leftPane: 'left',
    rightPane: 'right',
    mode: 'bidirectional',
  },
};

const snapshot = {
  schemaVersion: 1,
  requestId: 'req_1',
  activePaneAtSubmit: 'left',
  panes: [
    {
      paneId: 'left',
      tabId: 'tab_left',
      location: { kind: 'disk', path: 'C:\\left' },
      contextRevision: 2,
      selectionRevision: 3,
      selectionHandle: 'sel_left',
      filter: '',
      showHidden: false,
      sort: { key: 'name', dir: 1 },
    },
    {
      paneId: 'right',
      tabId: 'tab_right',
      location: { kind: 'disk', path: 'C:\\right' },
      contextRevision: 4,
      selectionRevision: 1,
      selectionHandle: 'sel_right',
      filter: '',
      showHidden: false,
      sort: { key: 'name', dir: 1 },
    },
  ],
};

const plan = {
  schemaVersion: 1,
  planId: 'plan_1',
  requestId: 'req_1',
  revision: 1,
  capability: 'directories.sync',
  sourceSnapshot: snapshot,
  previewDigest: 'digest_1',
  risk: 'mutating',
  requiresApproval: true,
  stepCount: 2,
  operation: {
    kind: 'directories.sync',
    leftRoot: 'C:\\left',
    rightRoot: 'C:\\right',
    leftManifest: 'manifest_left',
    rightManifest: 'manifest_right',
  },
  preview: { itemCount: 2, conflictCount: 0, totalBytes: 1024 },
  steps: [
    { id: 'step_1', capability: 'directories.sync', dependsOn: [] },
    { id: 'step_2', capability: 'directories.sync', dependsOn: ['step_1'] },
  ],
};
const handle = {
  schemaVersion: 1,
  handleId: 'handle_1',
  planId: 'plan_1',
  requestId: 'req_1',
  revision: 1,
  planDigest: 'digest_1',
  expiresAtMs: 2_000_000_000_000,
};
const receipt = {
  schemaVersion: 1,
  executionId: 'exec_1',
  planId: 'plan_1',
  jobIds: ['job_1'],
  state: 'queued',
  completed: 0,
  failed: 0,
  errors: [],
};

describe('AI-01 model intent schema', () => {
  it.each(cases)('$name', ({ valid, value }) => {
    if (valid) expect(parseIntent(value, 'req_1')).toEqual(value);
    else expect(() => parseIntent(value, 'req_1')).toThrow(ContractError);
  });

  it('rejects circular and extreme-depth model input with typed errors', () => {
    let deeplyNested: unknown = null;
    for (let i = 0; i < 20_000; i++) deeplyNested = [deeplyNested];
    let error: unknown;
    try {
      parseIntent({ ...sync, extra: deeplyNested }, 'req_1');
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(ContractError);
    expect((error as ContractError).code).toBe('limit_exceeded');
    const cyclic: { child?: unknown } = {};
    cyclic.child = cyclic;
    expect(() => parseIntent({ ...sync, extra: cyclic }, 'req_1')).toThrowError(
      expect.objectContaining({ code: 'invalid_payload' }),
    );
    const shared = { kind: 'selected' };
    expect(() => parseIntent({ ...sync, extra: [shared, shared] }, 'req_1')).toThrowError(
      expect.objectContaining({ code: 'invalid_payload' }),
    );
  });

  it('rejects oversized and deeply nested model payloads with typed errors', () => {
    const oversized = { ...sync, extra: 'x'.repeat(33 * 1024) };
    expect(() => parseIntent(oversized, 'req_1')).toThrowError(
      expect.objectContaining({ code: 'limit_exceeded' }),
    );
    let nested: unknown = null;
    for (let i = 0; i < 30; i++) nested = [nested];
    expect(() => parseIntent({ ...sync, extra: nested }, 'req_1')).toThrowError(
      expect.objectContaining({ code: 'limit_exceeded' }),
    );
  });

  it('rejects mismatched request, oversized names, and deep payload before resolution', () => {
    expect(() => parseIntent(sync, 'req_2')).toThrow(ContractError);
    expect(
      parseIntent(
        {
          ...cases[0].value,
          intent: { ...cases[0].value.intent, target: { kind: 'pane', pane: 'activeAtSubmit' } },
        },
        'req_1',
      ),
    ).toBeTruthy();
    const archive = cases[0].value;
    expect(() =>
      parseIntent(
        { ...archive, intent: { ...archive.intent, outputName: 'x'.repeat(129) } },
        'req_1',
      ),
    ).toThrow(ContractError);
    expect(() =>
      parseIntent({ ...archive, intent: { ...archive.intent, outputName: 'CON.zip' } }, 'req_1'),
    ).toThrow(ContractError);
    expect(() =>
      parseIntent({ ...archive, intent: { ...archive.intent, outputName: 'CON .zip' } }, 'req_1'),
    ).toThrow(ContractError);
    expect(() =>
      parseIntent(
        { ...sync, intent: { ...sync.intent, payload: { deeply: { nested: [] } } } },
        'req_1',
      ),
    ).toThrow(ContractError);
    expect(() => parseIntent({ ...sync, intent: { ...sync.intent, count: -1 } }, 'req_1')).toThrow(
      ContractError,
    );
  });
});

describe('AI-01 trusted contract boundaries', () => {
  it.each(trustedCases)('$name', ({ kind, valid, value }) => {
    const parse = {
      snapshot: parsePaneSnapshot,
      plan: parseResolvedPlan,
      handle: parsePreparedHandle,
      receipt: parseExecutionReceipt,
    }[kind];
    if (!parse) throw new Error(`Unknown fixture kind: ${kind}`);
    if (valid) expect(parse(value)).toEqual(value);
    else expect(() => parse(value)).toThrow(ContractError);
  });

  it('validates immutable snapshot and rejects duplicate tabs or invalid revisions', () => {
    expect(parsePaneSnapshot(snapshot)).toEqual(snapshot);
    expect(() =>
      parsePaneSnapshot({ ...snapshot, panes: [snapshot.panes[0], snapshot.panes[0]] }),
    ).toThrow(ContractError);
    expect(() =>
      parsePaneSnapshot({
        ...snapshot,
        panes: [{ ...snapshot.panes[0], contextRevision: -1 }, snapshot.panes[1]],
      }),
    ).toThrow(ContractError);
    expect(() =>
      parsePaneSnapshot({
        ...snapshot,
        panes: [{ ...snapshot.panes[0], selectionHandle: '../bad' }, snapshot.panes[1]],
      }),
    ).toThrow(ContractError);
    expect(() =>
      parsePaneSnapshot({
        ...snapshot,
        panes: [{ ...snapshot.panes[0], extra: true }, snapshot.panes[1]],
      }),
    ).toThrow(ContractError);
    expect(() =>
      parsePaneSnapshot({
        ...snapshot,
        panes: [{ ...snapshot.panes[0], filter: 'é'.repeat(200) }, snapshot.panes[1]],
      }),
    ).toThrow(ContractError);
    expect(
      parsePaneSnapshot({
        ...snapshot,
        panes: [
          { ...snapshot.panes[0], sort: { key: 'modified', dir: -1 } },
          { ...snapshot.panes[1], sort: { key: 'name', dir: 1 } },
        ],
      }),
    ).toBeTruthy();
  });

  it('keeps risk and approval app-owned, bound to plan and revision', () => {
    expect(parseResolvedPlan(plan)).toEqual(plan);
    expect(parsePreparedHandle(handle)).toEqual(handle);
    expect(() => parseResolvedPlan({ ...plan, risk: 'safe' })).toThrow(ContractError);
    expect(() => parseResolvedPlan({ ...plan, stepCount: 10001 })).toThrow(ContractError);
    expect(() =>
      parseResolvedPlan({ ...plan, operation: { ...plan.operation, rightRoot: '../made-up' } }),
    ).toThrow(ContractError);
    expect(() =>
      parseResolvedPlan({
        ...plan,
        operation: { ...plan.operation, rightRoot: 'C:\\right\\..\\other' },
      }),
    ).toThrow(ContractError);
    expect(() =>
      parseResolvedPlan({ ...plan, operation: { ...plan.operation, leftRoot: 'C:\\left\\CON' } }),
    ).toThrow(ContractError);
    expect(() =>
      parseResolvedPlan({
        ...plan,
        steps: [
          { id: 'step_1', capability: 'directories.sync', dependsOn: ['step_2'] },
          plan.steps[1],
        ],
      }),
    ).toThrow(ContractError);
    expect(() => parseResolvedPlan({ ...plan, steps: [plan.steps[0]] })).toThrow(ContractError);
    expect(() => parsePreparedHandle({ ...handle, revision: 0 })).toThrow(ContractError);
    expect(() => parsePreparedHandle({ ...handle, planDigest: '../escape' })).toThrow(
      ContractError,
    );
    expect(() => parsePreparedHandle({ ...handle, approval: 'model' })).toThrow(ContractError);
  });

  it('validates archive destination selected outside panes without accepting unsafe Windows paths', () => {
    const archivePlan = {
      ...plan,
      capability: 'archive.create',
      operation: {
        kind: 'archive.create',
        sourceManifest: 'manifest_src',
        targetDirectory: 'C:\\archives',
        outputName: 'backup.zip',
      },
      steps: [{ id: 'step_1', capability: 'archive.create', dependsOn: [] }],
      stepCount: 1,
    };
    expect(parseResolvedPlan(archivePlan)).toEqual(archivePlan);
    for (const targetDirectory of [
      'C:relative',
      'C:\\archives\\..\\secret',
      'C:\\archives\\CON',
      'C:\\archives\\a?b',
      '/tmp/archives',
    ]) {
      expect(() =>
        parseResolvedPlan({
          ...archivePlan,
          operation: { ...archivePlan.operation, targetDirectory },
        }),
      ).toThrow(ContractError);
    }
    expect(() =>
      parseResolvedPlan({
        ...archivePlan,
        operation: { ...archivePlan.operation, outputName: 'CON.zip' },
      }),
    ).toThrow(ContractError);
  });

  it('rejects stale handle or digest after plan revision', () => {
    expect(() => assertPreparedForPlan(handle, plan, 1_900_000_000_000)).not.toThrow();
    expect(() =>
      assertPreparedForPlan(handle, { ...plan, revision: 2 }, 1_900_000_000_000),
    ).toThrow(ContractError);
    expect(() =>
      assertPreparedForPlan(handle, { ...plan, previewDigest: 'digest_2' }, 1_900_000_000_000),
    ).toThrow(ContractError);
    expect(() => assertPreparedForPlan(handle, plan, 2_000_000_000_000)).toThrow(ContractError);
    expect(() =>
      assertPreparedForPlan({ ...handle, requestId: 'req_2' }, plan, 1_900_000_000_000),
    ).toThrow(ContractError);
  });

  it('distinguishes queued receipt from success and bounds jobs/errors', () => {
    expect(parseExecutionReceipt(receipt)).toEqual(receipt);
    expect(() => parseExecutionReceipt({ ...receipt, state: 'succeeded' })).toThrow(ContractError);
    expect(() => parseExecutionReceipt({ ...receipt, completed: -1 })).toThrow(ContractError);
    expect(() => parseExecutionReceipt({ ...receipt, jobIds: Array(257).fill('job_1') })).toThrow(
      ContractError,
    );
    expect(() =>
      parseExecutionReceipt({ ...receipt, error: { code: 'oops', message: 'bad' } }),
    ).toThrow(ContractError);
    expect(() => parseExecutionReceipt({ ...receipt, completed: 1 })).toThrow(ContractError);
    expect(() => parseExecutionReceipt({ ...receipt, state: 'completed', failed: 1 })).toThrow(
      ContractError,
    );
    expect(() =>
      parseExecutionReceipt({
        ...receipt,
        errors: [{ code: 'invalid_payload', message: 'x', itemRef: null }],
      }),
    ).toThrow(ContractError);
    expect(() =>
      parseExecutionReceipt({
        ...receipt,
        state: 'partial',
        errors: [{ code: 'invalid_payload', message: 'é'.repeat(300) }],
      }),
    ).toThrow(ContractError);
  });
});
