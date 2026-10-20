import { describe, expect, it, vi } from 'vitest';
import { parseIntent, type ResolvedPlan } from './contracts';
import { createArchiveAdapter, type ArchiveState } from './adapters/archive';
import { DEFAULT_PREFS, evaluatePolicy } from './policy';
import { runPack } from '../packService';
import type { CheckResult } from './context';
import type { AdapterPrepared, Availability, ResolvedContext } from './commands';
import type { ContextFlow } from './contextFlow';

vi.mock('../packService', () => ({
  measurePack: vi.fn(),
  packFailureCause: vi.fn(),
  runPack: vi.fn(async () => ({ outputPath: 'C:\\Target\\archive.zip' })),
}));

describe('FER-505 AI Guards: RAR format and rarExePath protection', () => {
  it('parseIntent rejects format: "rar"', () => {
    const invalidRarEnvelope = {
      schemaVersion: 1,
      requestId: 'req-1',
      intent: {
        capability: 'archive.create',
        sourcePane: 'left',
        selector: { kind: 'selected' },
        format: 'rar', // Forbidden: AI contracts only permit 'zip' and '7z'
        target: { kind: 'sourceDirectory' },
      },
    };

    expect(() => {
      parseIntent(invalidRarEnvelope, 'req-1');
    }).toThrow(/zip or 7z/i);
  });

  it('parseIntent accepts format: "zip"', () => {
    const validZipEnvelope = {
      schemaVersion: 1,
      requestId: 'req-1',
      intent: {
        capability: 'archive.create',
        sourcePane: 'left',
        selector: { kind: 'selected' },
        format: 'zip',
        target: { kind: 'sourceDirectory' },
      },
    };

    expect(() => {
      parseIntent(validZipEnvelope, 'req-1');
    }).not.toThrow();
  });

  it('createArchiveAdapter unconditionally forces format "zip" in runPack', async () => {
    const runPackMock = vi.mocked(runPack);
    runPackMock.mockClear();

    const mockPrepared = {
      operation: {
        kind: 'archive.create' as const,
        sourceManifest: 'h-1',
        targetDirectory: 'C:\\Target',
        outputName: 'archive.zip',
      },
      preview: {
        summary: 'Pack files',
        itemCount: 1,
        conflictCount: 0,
        totalBytes: 100,
        outputPath: 'C:\\Target\\archive.zip',
        warnings: [],
      },
      digestParts: ['h-1'],
      state: {
        scope: {
          handle: 'h-1',
          pane: 'left' as const,
          counts: { total: 1 },
          contextRevision: 1,
          selectionRevision: 1,
        },
        target: {
          targetPane: 'right' as const,
          targetDirectory: 'C:\\Target',
          finalName: 'archive.zip',
          outputPath: 'C:\\Target\\archive.zip',
          warnings: [],
        },
        selection: {
          handleId: 's-1',
          paths: () => ['C:\\Source\\file.txt'],
          digest: 'd-1',
        },
      },
    };

    const flowStub = {
      resolveArchive: vi.fn(),
      resolveArchiveEntity: vi.fn(),
    } as unknown as Pick<ContextFlow, 'resolveArchive' | 'resolveArchiveEntity'>;

    const adapter = createArchiveAdapter({
      flow: flowStub,
      resolver: {
        entries: vi.fn(),
        release: vi.fn(),
      },
      ctx: {
        check: vi.fn((): CheckResult => ({ ok: true })),
      },
      outputExists: vi.fn(() => false),
    });

    const ctx = {
      snapshot: {
        panes: {
          left: { path: 'C:\\Source', tabId: 1, filter: '', selection: [] },
          right: { path: 'C:\\Target', tabId: 2, filter: '', selection: [] },
        },
      },
    } as unknown as ResolvedContext;

    const res = await adapter.run(mockPrepared as unknown as AdapterPrepared<ArchiveState>, ctx);
    expect(res.ok).toBe(true);

    expect(runPackMock).toHaveBeenCalledTimes(1);
    expect(runPackMock).toHaveBeenCalledWith(
      expect.objectContaining({
        format: 'zip',
        paths: ['C:\\Source\\file.txt'],
        target: 'C:\\Target\\archive.zip',
      }),
    );
  });

  it('AI policy and prefs schema do not expose rarExePath or arbitrary prefs mutation', () => {
    // PolicyPrefs strictly only accepts autoCreate boolean
    expect(Object.keys(DEFAULT_PREFS)).toEqual(['autoCreate']);

    // Attempting to evaluate policy with rarExePath does not recognize or propagate it
    const decision = evaluatePolicy({
      plan: {
        operations: [],
        intent: {
          capability: 'archive.create',
          sourcePane: 'left',
          selector: { kind: 'selected' },
          format: 'zip',
          target: { kind: 'sourceDirectory' },
        },
        preview: { warnings: [] },
        explanation: 'test',
        digest: 'd1',
      } as unknown as ResolvedPlan,
      availability: {
        hasLeftPane: true,
        hasRightPane: true,
        activePane: 'left',
        leftPath: 'C:\\',
        rightPath: 'D:\\',
      } as unknown as Availability,
      prefs: {
        autoCreate: true,
        // @ts-expect-error rarExePath is not permitted in PolicyPrefs
        rarExePath: 'C:\\hacked\\Rar.exe',
      },
    });

    // Decision cannot execute or store rarExePath
    expect(decision).toBeDefined();
    expect((decision as Record<string, unknown>).rarExePath).toBeUndefined();
  });
});
