import { describe, expect, it } from 'vitest';
import {
  boundedCopyBufferBytes,
  defaultPrefs,
  normalizePrefs,
  requiresDeleteConfirmation,
  supportsPermissionPreservation,
} from './prefs';

describe('file-operation preferences', () => {
  it('provides safe defaults for old preference objects', () => {
    const prefs = normalizePrefs({
      theme: 'light',
      palette: 'aqua',
      showHidden: true,
      terminal: 'wt',
    });
    expect(prefs.theme).toBe('light');
    expect(prefs.showHidden).toBe(true);
    expect(prefs.confirmBeforeDeleting).toBe(true);
    expect(prefs.verifyAfterCopy).toBe(false);
    expect(prefs.copyBufferKiB).toBe(1024);
    expect(prefs.copyThreads).toBe(0);
    expect(prefs.copySafeWrite).toBe(false);
    expect(prefs.copyPriority).toBe('normal');
    expect(prefs.conflictPolicy).toBe('ask');
    expect(prefs.preserveTimestamps).toBe(true);
    expect(prefs.preservePermissions).toBe(false);
  });

  it('bounds copy buffer and replaces invalid policy values', () => {
    expect(
      normalizePrefs({ ...defaultPrefs(), copyBufferKiB: -10, conflictPolicy: 'unsafe' as never })
        .copyBufferKiB,
    ).toBe(64);
    expect(normalizePrefs({ ...defaultPrefs(), copyBufferKiB: 90000 }).copyBufferKiB).toBe(8192);
    expect(normalizePrefs({ ...defaultPrefs(), copyBufferKiB: Number.NaN }).copyBufferKiB).toBe(
      1024,
    );
    expect(
      normalizePrefs({ ...defaultPrefs(), conflictPolicy: 'unsafe' as never }).conflictPolicy,
    ).toBe('ask');
  });

  it('FER-471: bounds the default copy threads and priority', () => {
    const norm = (over: object) => normalizePrefs({ ...defaultPrefs(), ...over } as never);
    expect(norm({ copyThreads: 99 }).copyThreads).toBe(16);
    expect(norm({ copyThreads: -3 }).copyThreads).toBe(0);
    expect(norm({ copyThreads: 4.6 }).copyThreads).toBe(5);
    expect(norm({ copyThreads: Number.NaN }).copyThreads).toBe(0);
    expect(norm({ copyThreads: '8' }).copyThreads).toBe(0);
    expect(norm({ copySafeWrite: true }).copySafeWrite).toBe(true);
    expect(norm({ copySafeWrite: 'yes' }).copySafeWrite).toBe(false);
    expect(norm({ copyPriority: 'background' }).copyPriority).toBe('background');
    expect(norm({ copyPriority: 'realtime' }).copyPriority).toBe('normal');
  });

  it('clamps allocation bytes and preserves permanent-delete confirmation', () => {
    expect(boundedCopyBufferBytes(8192)).toBe(8 * 1024 * 1024);
    expect(requiresDeleteConfirmation(true, false)).toBe(true);
    expect(requiresDeleteConfirmation(false, false)).toBe(false);
  });

  it('only advertises permission preservation on supported desktop platforms', () => {
    expect(supportsPermissionPreservation('Android 14')).toBe(false);
    expect(supportsPermissionPreservation('iPhone')).toBe(false);
    expect(supportsPermissionPreservation('win32')).toBe(false);
    expect(supportsPermissionPreservation('Linux x86_64')).toBe(true);
    expect(supportsPermissionPreservation('MacIntel')).toBe(true);
  });
});
