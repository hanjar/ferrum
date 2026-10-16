import { describe, expect, it } from 'vitest';
import {
  emptyForm,
  excludeExample,
  fieldMessage,
  fromPolicy,
  ipcErrorMessage,
  modeSentence,
  patternsOf,
  recycleBinBlockReason,
  rootCaption,
  toDraft,
  usedByMessage,
} from './syncProfileModel';

describe('toDraft', () => {
  it('drops fields that do not apply to the mode and policy', () => {
    const f = {
      ...emptyForm(),
      name: 'a',
      left: 'D:\\A',
      right: 'E:\\B',
      excludeText: ' *.tmp \n\n  node_modules\\ ',
    };
    const one = toDraft(f);
    expect(one.from).toBe('left');
    expect(one).not.toHaveProperty('propagateDeletes');
    expect(one).not.toHaveProperty('versioningRoot');
    expect(one.exclude).toEqual(['*.tmp', 'node_modules\\']);

    const two = toDraft({
      ...f,
      mode: 'twoWay',
      propagateDeletes: true,
      deletePolicy: 'versioning',
      versioningRoot: 'F:\\V',
    });
    expect(two).not.toHaveProperty('from');
    expect(two.propagateDeletes).toBe(true);
    expect(two.versioningRoot).toBe('F:\\V');
  });

  it('sends 0 for a cleared guard number so the backend reports the range error', () => {
    expect(toDraft({ ...emptyForm(), guardMaxCount: NaN }).guard.maxCount).toBe(0);
  });

  it('round-trips a stored policy', () => {
    const f = fromPolicy('P', {
      left: 'D:\\A',
      right: 'E:\\B',
      mode: 'mirror',
      from: 'right',
      deletePolicy: 'recycleBin',
      compare: 'content',
      include: ['*.jpg'],
      exclude: [],
      guard: { enabled: false, maxCount: 9, maxPercent: 5 },
    });
    expect(f.from).toBe('right');
    expect(toDraft(f)).toMatchObject({
      name: 'P',
      mode: 'mirror',
      from: 'right',
      include: ['*.jpg'],
    });
  });
});

describe('wording', () => {
  it('has one sentence per mode and side (SP-2.4)', () => {
    expect(modeSentence('mirror', 'left')).toMatch(/Left always wins.*right becomes an exact copy/);
    expect(modeSentence('oneWay', 'right')).toMatch(/^Right always wins/);
    expect(modeSentence('twoWay', 'left')).toMatch(/Conflicts are held/);
  });

  it('builds the filter example (SP-4.5)', () => {
    expect(excludeExample(patternsOf('node_modules\\\n*.tmp\nx'))).toBe(
      'Excluded: 3 patterns · e.g. `node_modules\\`, `*.tmp`',
    );
    expect(excludeExample([])).toBe('Excluded: no patterns');
  });

  it('captions a root by kind and file system (SP-9.2)', () => {
    expect(
      rootCaption(
        { kind: 'local', volumeSerial: '1', fileId: '2', fsName: 'NTFS', drive: 'fixed' },
        'fixed',
      ),
    ).toBe('Local disk · NTFS');
    expect(rootCaption({ kind: 'network', uncPrefix: '\\\\nas\\x' }, 'network')).toBe(
      'Network share',
    );
  });

  it('names the folder in the Recycle Bin message (SP-3.1)', () => {
    expect(
      fieldMessage(
        { field: 'right', code: 'recycle_bin_unavailable' },
        { left: 'D:\\A', right: '\\\\nas\\b' },
      ),
    ).toBe(
      "Recycle Bin isn't available on \\\\nas\\b — choose a versioning folder or permanent delete",
    );
  });

  it('uses the singular and plural of SP-9.5', () => {
    expect(usedByMessage(2)).toBe('Used by 2 automations — delete or change them first.');
    expect(usedByMessage(1)).toBe('Used by 1 automation — delete or change it first.');
  });

  it('has a message for every closed-set code', () => {
    for (const c of [
      'stale',
      'canceled',
      'busy',
      'lease_busy',
      'not_ready',
      'held',
      'in_use',
      'not_found',
      'store_read_only',
    ]) {
      expect(ipcErrorMessage(c, 'run')).not.toMatch(/Something went wrong/);
    }
  });
});

describe('recycleBinBlockReason', () => {
  const base = {
    mode: 'mirror' as const,
    from: 'left' as const,
    propagateDeletes: false,
    left: 'D:\\A',
    right: 'F:\\B',
  };
  it('is null when the receiving side is a fixed drive or unknown', () => {
    expect(recycleBinBlockReason(base, { left: 'fixed', right: 'fixed' })).toBeNull();
    expect(recycleBinBlockReason(base, {})).toBeNull();
  });
  it('blocks only the sides that can receive deletes', () => {
    // Mirror left -> right deletes on the right only.
    expect(recycleBinBlockReason(base, { left: 'network', right: 'fixed' })).toBeNull();
    expect(recycleBinBlockReason(base, { left: 'fixed', right: 'removable' })).toContain('F:\\B');
    expect(recycleBinBlockReason({ ...base, mode: 'oneWay' }, { right: 'network' })).toBeNull();
    expect(
      recycleBinBlockReason(
        { ...base, mode: 'twoWay', propagateDeletes: true },
        { left: 'network' },
      ),
    ).toContain('D:\\A');
    expect(recycleBinBlockReason({ ...base, mode: 'twoWay' }, { left: 'network' })).toBeNull();
  });
});
