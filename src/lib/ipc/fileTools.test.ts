import { describe, expect, it } from 'vitest';
import { asArchiveMemberChecksum, asVerifyReport } from './fileTools';

describe('fileTools IPC guards', () => {
  it('keeps the cancelled flag of a verify report only when it is true', () => {
    const base = { requested: 1, ok: 0, mismatched: 0, outcomes: [] };
    expect(asVerifyReport({ ...base, cancelled: true }).cancelled).toBe(true);
    expect(asVerifyReport({ ...base, cancelled: false }).cancelled).toBeUndefined();
    expect(asVerifyReport(base).cancelled).toBeUndefined();
  });

  it('accepts the skipped outcome status', () => {
    const report = asVerifyReport({
      requested: 1,
      ok: 0,
      mismatched: 0,
      outcomes: [{ path: 'C:\\d\\a.txt.sha256', status: 'skipped', message: 'exists' }],
    });
    expect(report.outcomes[0].status).toBe('skipped');
  });

  it('validates the archive member checksum payload', () => {
    const good = {
      archivePath: 'C:\\d\\p.zip',
      memberPath: 'a/b.txt',
      algorithm: 'sha256',
      hash: 'ab',
    };
    expect(asArchiveMemberChecksum(good)).toEqual(good);
    expect(() => asArchiveMemberChecksum({ ...good, algorithm: 'whirlpool' })).toThrow();
    expect(() => asArchiveMemberChecksum({ ...good, hash: '' })).toThrow();
    expect(() => asArchiveMemberChecksum(null)).toThrow();
  });
});
