import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
  ArchiveListingGate,
  asArchiveCapabilities,
  asArchiveEntry,
  asArchiveListResponse,
  asLocation,
  archiveParent,
  childArchiveLocation,
  type ArchiveContractFixture,
} from './types';

const fixture: ArchiveContractFixture = JSON.parse(
  readFileSync(
    join(
      dirname(fileURLToPath(import.meta.url)),
      '../../../contract-fixtures/archive-contract.json',
    ),
    'utf8',
  ),
);

describe('archive IPC contract', () => {
  it('roundtrips shared fixture values, preserving disk path identity and archive member identity', () => {
    expect(asLocation(JSON.parse(JSON.stringify(fixture.diskLocation)))).toEqual(
      fixture.diskLocation,
    );
    expect(asLocation(JSON.parse(JSON.stringify(fixture.location)))).toEqual(fixture.location);
    expect(asArchiveEntry(JSON.parse(JSON.stringify(fixture.entry)))).toEqual(fixture.entry);
    expect(() =>
      asArchiveCapabilities({ ...fixture.capabilities, format: 'rar', create: true }),
    ).toThrow();
    expect(asArchiveCapabilities(JSON.parse(JSON.stringify(fixture.capabilities)))).toEqual(
      fixture.capabilities,
    );
    expect(asArchiveListResponse(JSON.parse(JSON.stringify(fixture.listResponse)))).toEqual(
      fixture.listResponse,
    );
    expect(fixture.capabilities.format).toBe('zip');
    expect(fixture.listRequest.location).toEqual(fixture.location);
    expect(fixture.readRequest.location.memberPath).toBe(fixture.entry.memberPath);
    expect(fixture.extractRequest.memberPaths).toContain(fixture.entry.memberPath);
    expect(fixture.createRequest.format).toBe('zip');
    expect(fixture.testRequest.archivePath).toBe(fixture.location.archivePath);
    expect(fixture.readResponse.bytes).toEqual([72, 105]);
    expect(fixture.operationResponse.operationId).toBe(fixture.progress.operationId);
    expect(fixture.testResponse.entries[0].memberPath).toBe(fixture.entry.memberPath);
    expect(fixture.progress.currentMemberPath).toBe(fixture.entry.memberPath);
    expect(fixture.error.code).toBe('unsafeEntry');
  });

  it('rejects unsafe or noncanonical member paths at IPC boundary', () => {
    for (const memberPath of [
      '../secret',
      'a/../b',
      '/root',
      'C:/root',
      '//server/share',
      'a\\b',
      'a//b',
      'a/./b',
      'a/',
      'a:\\stream',
      'NUL',
      'a/CON.txt',
      'a\0b',
    ]) {
      expect(() => asLocation({ ...fixture.location, memberPath })).toThrow();
      expect(() => asArchiveEntry({ ...fixture.entry, memberPath })).toThrow();
    }
    expect(() => asLocation({ kind: 'archive', archivePath: '', memberPath: '' })).toThrow();
    expect(() => asLocation({ kind: 'disk', path: '' })).toThrow();
    expect(() => asArchiveEntry({ ...fixture.entry, name: 'different.jpg' })).toThrow();
  });

  it('navigates parent and child without treating member paths as OS paths or nesting archives', () => {
    const child = childArchiveLocation(fixture.location, 'winter');
    expect(child).toEqual({ ...fixture.location, memberPath: 'albums/été/winter' });
    expect(archiveParent(child)).toEqual(fixture.location);
    expect(archiveParent({ ...fixture.location, memberPath: '' })).toEqual({
      kind: 'disk',
      path: 'C:\\Work',
    });
    expect(() => childArchiveLocation(fixture.location, 'another.zip/inside')).toThrow();
  });

  it('rejects old responses after path change, tab switch, and newer request on same path', () => {
    const gate = new ArchiveListingGate();
    gate.activate('left', fixture.location);
    const first = gate.request('left');
    const later = gate.request('left');
    expect(gate.accept('left', { ...fixture.listResponse, requestToken: first })).toBe(false);
    expect(gate.accept('left', { ...fixture.listResponse, requestToken: later })).toBe(true);

    gate.activate('left', childArchiveLocation(fixture.location, 'winter'));
    expect(gate.accept('left', { ...fixture.listResponse, requestToken: later })).toBe(false);
    gate.activate('right', fixture.location);
    expect(gate.accept('left', { ...fixture.listResponse, requestToken: later })).toBe(false);
    const right = gate.request('right');
    expect(gate.accept('right', { ...fixture.listResponse, requestToken: right })).toBe(true);
    gate.close('right');
    expect(gate.accept('right', { ...fixture.listResponse, requestToken: right })).toBe(false);
    expect(
      gate.accept('right', { ...fixture.listResponse, requestToken: right, entries: [null] }),
    ).toBe(false);
  });

  it('rejects malformed listing responses rather than applying partial results', () => {
    expect(() => asArchiveListResponse({ ...fixture.listResponse, requestToken: -1 })).toThrow();
    expect(() =>
      asArchiveListResponse({
        ...fixture.listResponse,
        entries: [{ ...fixture.entry, memberPath: '../escape' }],
      }),
    ).toThrow();
    expect(() =>
      asArchiveListResponse({
        ...fixture.listResponse,
        entries: [{ ...fixture.entry, memberPath: 'other/photo.jpg' }],
      }),
    ).toThrow();
  });
});
