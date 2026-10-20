// FER-576: the TypeScript half of the `pane.navigate` / `items.delete` / chain contract. The cases are shared with
// the Rust parser (src-tauri/src/ai/contract_chain_tests.rs): both must agree on every one of them.
import { describe, expect, it } from 'vitest';
import chainCases from '../../../contract-fixtures/ai/chain-cases.json';
import navigateCases from '../../../contract-fixtures/ai/navigate-cases.json';
import deleteVerbCases from '../../../contract-fixtures/ai/delete-verb-cases.json';
import {
  ContractError,
  isSafeNavigatePath,
  parseChain,
  parseOfferedIntent,
  parseResolvedPlan,
} from './contracts';
import { parseModelReply } from './provider';
import { drivePathCount, parentFolder, pathTypedByUser, userAskedToDelete } from './paneNavigate';

describe('shared chain / navigate / delete cases (Rust parity)', () => {
  it.each(chainCases)('$name', (c) => {
    const parse = () =>
      c.kind === 'chain'
        ? parseChain(c.value, 'req_1', c.offered, c.userText ?? '')
        : parseOfferedIntent(c.value, 'req_1', c.offered);
    if (c.valid) expect(parse()).toEqual(c.value);
    else expect(parse).toThrow(ContractError);
  });

  it.each(navigateCases)('typed path: $name', (c) => {
    expect(pathTypedByUser(c.path, c.text)).toBe(c.typed);
  });
});

describe('delete request gate (Rust parity)', () => {
  it.each(deleteVerbCases)('asked to delete: $name', (c) => {
    expect(userAskedToDelete(c.text)).toBe(c.asked);
  });
  it('counts absolute drive paths by their drive prefix', () => {
    expect(
      drivePathCount('a D:' + String.fromCharCode(92) + 'x' + String.fromCharCode(92) + 'y b'),
    ).toBe(1);
    expect(drivePathCount('d:/x dan E:/y')).toBe(2);
    expect(drivePathCount('tidak ada')).toBe(0);
    expect(drivePathCount('abcD:/x')).toBe(0);
    expect(drivePathCount('')).toBe(0);
  });
});

describe('navigation paths', () => {
  it.each(['D:\\a\\b.csv', 'd:/a/b', 'C:', 'C:\\', 'E:\\Folder With Spaces\\x'])(
    'accepts %s',
    (p) => {
      expect(isSafeNavigatePath(p)).toBe(true);
    },
  );
  it.each([
    '',
    'relative\\path',
    '\\\\server\\share\\x',
    '\\\\?\\C:\\x',
    'C:\\x\\..\\y',
    'C:\\x:ads',
    'C:\\nul',
    'C:\\a\\con.txt',
    'C:\\a\\b ',
    'C:\\a\\\\b',
    'C:x',
    `C:\\${'a'.repeat(2000)}`,
  ])('rejects %s', (p) => {
    expect(isSafeNavigatePath(p)).toBe(false);
  });
  it('finds the folder of a file and keeps a drive root', () => {
    expect(parentFolder('D:\\a\\b.csv')).toBe('D:\\a');
    expect(parentFolder('D:\\b.csv')).toBe('D:\\');
    expect(parentFolder('D:\\')).toBeUndefined();
  });
});

describe('provider replies (parseModelReply)', () => {
  const offered = ['archive.create', 'pane.navigate', 'items.delete'];
  it('turns a typed-path navigation into a paneNavigate event', () => {
    const reply = {
      schemaVersion: 1,
      requestId: 'req_1',
      intent: { capability: 'pane.navigate', pane: 'left', path: 'D:\\x\\a.csv' },
    };
    expect(parseModelReply(reply, 'req_1', offered)).toEqual({
      kind: 'paneNavigate',
      pane: 'left',
      path: 'D:\\x\\a.csv',
    });
    expect(() => parseModelReply(reply, 'req_1', ['archive.create'])).toThrow();
  });
  it('turns steps into a chain event only when the path was typed', () => {
    const reply = {
      schemaVersion: 1,
      requestId: 'req_1',
      steps: [
        { capability: 'pane.navigate', pane: 'left', path: 'D:\\x\\a.csv' },
        { capability: 'items.delete', sourcePane: 'left', selector: { kind: 'selected' } },
      ],
    };
    expect(parseModelReply(reply, 'req_1', offered, 'hapus D:\\x\\a.csv').kind).toBe('chain');
    expect(() => parseModelReply(reply, 'req_1', offered, 'hapus file itu')).toThrow(ContractError);
  });
  it('never turns items.delete into a plan when it was not offered', () => {
    const reply = {
      schemaVersion: 1,
      requestId: 'req_1',
      intent: { capability: 'items.delete', sourcePane: 'left', selector: { kind: 'selected' } },
    };
    expect(() => parseModelReply(reply, 'req_1', ['archive.create'])).toThrow(ContractError);
    expect(() => parseModelReply(reply, 'req_1', undefined)).toThrow(ContractError);
    expect(parseModelReply(reply, 'req_1', offered, 'tolong hapus').kind).toBe('intent');
    // Review R1 S5: no delete word in the request, no delete plan.
    expect(() => parseModelReply(reply, 'req_1', offered, 'jelaskan file ini')).toThrow(
      ContractError,
    );
  });
});

describe('items.delete in a resolved plan', () => {
  const snapshot = (id: string) => ({
    schemaVersion: 1,
    requestId: 'req_1',
    activePaneAtSubmit: 'left',
    panes: ['left', 'right'].map((paneId, i) => ({
      paneId,
      tabId: `tab_${id}_${paneId}`,
      location: { kind: 'disk', path: i === 0 ? 'C:\\left' : 'C:\\right' },
      contextRevision: 1,
      selectionRevision: 1,
      selectionHandle: `sel_${paneId}`,
      filter: '',
      showHidden: false,
      sort: { key: 'name', dir: 1 },
    })),
  });
  const plan = (over: Record<string, unknown> = {}, conflicts = 0) => ({
    schemaVersion: 1,
    planId: 'plan_1',
    requestId: 'req_1',
    revision: 1,
    capability: 'items.delete',
    sourceSnapshot: snapshot('a'),
    operation: { kind: 'items.delete', sourceManifest: 'manifest_1', sourceDirectory: 'C:\\left' },
    preview: { itemCount: 2, conflictCount: conflicts, totalBytes: 0 },
    steps: [{ id: 'step_1', capability: 'items.delete', dependsOn: [] }],
    previewDigest: 'digest_1',
    risk: 'mutating',
    requiresApproval: true,
    stepCount: 1,
    ...over,
  });
  it('round trips, and refuses an unsafe folder, a conflict or a missing approval', () => {
    expect(parseResolvedPlan(plan())).toEqual(plan());
    expect(() => parseResolvedPlan(plan({ requiresApproval: false }))).toThrow(ContractError);
    expect(() => parseResolvedPlan(plan({}, 1))).toThrow(ContractError);
    expect(() =>
      parseResolvedPlan(
        plan({
          operation: { kind: 'items.delete', sourceManifest: 'm', sourceDirectory: '..\\x' },
        }),
      ),
    ).toThrow(ContractError);
    expect(() =>
      parseResolvedPlan(
        plan({
          operation: {
            kind: 'items.delete',
            sourceManifest: 'm',
            sourceDirectory: 'C:\\x',
            permanent: true,
          },
        }),
      ),
    ).toThrow(ContractError);
    expect(() => parseResolvedPlan(plan({ capability: 'archive.create' }))).toThrow(ContractError);
  });
});
