import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { asFileEntry, FsError, type OperationProgress } from './types';

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, '../../../contract-fixtures');
const fixture = (name: string): string => readFileSync(join(fixtureDir, name), 'utf-8');

describe('IPC contract fixtures (shared with Rust tests)', () => {
  it('parses file-entry.json into a FileEntry', () => {
    const entry = asFileEntry(JSON.parse(fixture('file-entry.json')));
    expect(entry.name).toBe('hélix-eñgine');
    expect(entry.isDir).toBe(true);
    expect(entry.isSymlink).toBe(false);
    expect(entry.size).toBe(0);
    expect(entry.modifiedMs).toBe(1790512000000);
    expect(entry.hidden).toBe(false);
  });

  it('parses fs-error.json into an FsError', () => {
    const err = FsError.fromIPC(JSON.parse(fixture('fs-error.json')));
    expect(err).toBeInstanceOf(FsError);
    expect(err.code).toBe('permissionDenied');
    expect(err.message).toContain('os error 5');
    expect(err.path).toBe('C:\\Windows\\CSC');
  });

  it('operation-progress.json matches the OperationProgress shape', () => {
    const p: OperationProgress = JSON.parse(fixture('operation-progress.json'));
    expect(p.operationId).toMatch(/^op_[0-9A-Z]{10,}$/);
    expect(p.kind).toBe('copy');
    expect(p.state).toBe('running');
    expect(p.totalBytes).toBeGreaterThan(p.processedBytes);
    expect(p.totalFiles).toBeGreaterThan(p.processedFiles);
    expect(p.rateBytesPerSec).toBeGreaterThan(0);
    // Both numbers must stay JS-safe (< 2^53)
    for (const n of [p.processedBytes, p.totalBytes ?? 0]) {
      expect(n).toBeLessThan(Number.MAX_SAFE_INTEGER);
    }
  });
});

describe('FsError.fromIPC', () => {
  it('wraps non-object rejections without throwing', () => {
    const err = FsError.fromIPC('boom');
    expect(err.code).toBe('io');
    expect(err.message).toBe('boom');
  });

  it('rejects shapes missing code/message', () => {
    const err = FsError.fromIPC({ code: 42 });
    expect(err.code).toBe('io');
  });
});

describe('asFileEntry guard', () => {
  it('rejects malformed entries with a structured error', () => {
    expect(() => asFileEntry({ name: 'x' })).toThrow(FsError);
    expect(() => asFileEntry(null)).toThrow(FsError);
    expect(() => asFileEntry('str')).toThrow(FsError);
    // wrong-typed fields must not slip through
    expect(() => asFileEntry({ ...JSON.parse(fixture('file-entry.json')), size: '12' })).toThrow(
      FsError,
    );
  });
});
