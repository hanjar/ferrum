import { describe, expect, it } from 'vitest';
import { FsError } from './types';
import { asEntryProperties, asHostInfo, asOccupiedReport, asVolumeInfo } from './properties';

const entryFixture = {
  path: 'C:\\d\\notes.txt',
  name: 'notes.txt',
  kind: 'file',
  size: 12,
  allocatedBytes: 4096,
  allocatedSupported: true,
  createdMs: 1790500000000,
  modifiedMs: 1790512000000,
  accessedMs: 1790512000001,
  readOnly: true,
  hidden: false,
  system: false,
  attributesEditable: true,
};

const occupiedFixture = {
  files: 3,
  dirs: 2,
  symlinks: 1,
  logicalBytes: 1500,
  allocatedBytes: 12288,
  allocatedSupported: true,
  failed: [{ path: 'C:\\d\\locked', code: 'permissionDenied', message: 'nope' }],
  cancelled: false,
};

const volumeFixture = {
  root: 'C:\\',
  label: 'System',
  fsName: 'NTFS',
  totalBytes: 512 * 1024 ** 3,
  freeBytes: 82 * 1024 ** 3,
};

describe('properties IPC payload guards', () => {
  it('accepts camelCase payloads shaped like the Rust contract', () => {
    expect(asEntryProperties(entryFixture)).toEqual(entryFixture);
    expect(asOccupiedReport(occupiedFixture)).toEqual(occupiedFixture);
    expect(asVolumeInfo(volumeFixture)).toEqual(volumeFixture);
  });

  it('treats symlinkTarget as optional and preserves it when present', () => {
    expect(asEntryProperties(entryFixture).symlinkTarget).toBeUndefined();
    expect(
      asEntryProperties({ ...entryFixture, symlinkTarget: 'D:\\real.txt' }).symlinkTarget,
    ).toBe('D:\\real.txt');
  });

  it('reads POSIX mode when reported and treats a missing editable flag as not editable', () => {
    expect(asEntryProperties({ ...entryFixture, mode: 0o640 }).mode).toBe(0o640);
    expect(asEntryProperties(entryFixture).mode).toBeUndefined();
    const legacy: Record<string, unknown> = { ...entryFixture };
    delete legacy.attributesEditable;
    expect(asEntryProperties(legacy).attributesEditable).toBe(false);
  });

  it('defaults cancelled to false and preserves a cancelled report', () => {
    const legacy: Record<string, unknown> = { ...occupiedFixture };
    delete legacy.cancelled;
    expect(asOccupiedReport(legacy).cancelled).toBe(false);
    expect(asOccupiedReport({ ...occupiedFixture, cancelled: true }).cancelled).toBe(true);
  });

  it('guards the host info payload', () => {
    const host = { os: 'linux', arch: 'x86_64', family: 'unix', hostName: 'box' };
    expect(asHostInfo(host)).toEqual(host);
    expect(asHostInfo({ os: 'linux', arch: 'x', family: 'unix' }).hostName).toBeUndefined();
    expect(() => asHostInfo({ os: 'linux' })).toThrowError(FsError);
  });

  it('throws a coded FsError on malformed or incomplete payloads', () => {
    for (const raw of [null, 'x', 42, {}, { ...entryFixture, size: 'big' }]) {
      expect(() => asEntryProperties(raw)).toThrowError(FsError);
    }
    for (const raw of [
      null,
      {},
      { ...occupiedFixture, files: 'many' },
      { ...occupiedFixture, failed: [{ path: 1, code: 'x', message: 'y' }] },
    ]) {
      expect(() => asOccupiedReport(raw)).toThrowError(FsError);
    }
    for (const raw of [null, {}, { ...volumeFixture, freeBytes: null }]) {
      expect(() => asVolumeInfo(raw)).toThrowError(FsError);
    }
  });
});
