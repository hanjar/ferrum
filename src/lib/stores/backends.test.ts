import { beforeEach, describe, expect, it, vi } from 'vitest';
import { handleExternalDrop } from './externalDrop';
import { driveForPath } from './driveStatus.svelte';
import { ancestorPaths } from './treeStore.svelte';
import { view } from './viewStore.svelte';

const beginTransferTo = vi.fn();
vi.mock('./dialogStore.svelte', () => ({
  dialogs: {
    beginTransferTo: (...a: unknown[]) => beginTransferTo(...(a as [string, string[], string])),
  },
}));

beforeEach(() => {
  beginTransferTo.mockClear();
  view.set('details');
});

describe('externalDrop.handleExternalDrop', () => {
  it('no-ops on empty paths or missing target', () => {
    expect(handleExternalDrop([], 'C:\\t')).toBe(false);
    expect(handleExternalDrop(['C:\\a.txt'], null)).toBe(false);
    expect(beginTransferTo).not.toHaveBeenCalled();
  });

  it('copies dropped files into the target via the transfer dialog', () => {
    expect(handleExternalDrop(['C:\\a.txt', 'C:\\b.png'], 'D:\\dest')).toBe(true);
    expect(beginTransferTo).toHaveBeenCalledWith('copy', ['C:\\a.txt', 'C:\\b.png'], 'D:\\dest');
  });
});

describe('driveForPath', () => {
  const meta = { driveType: 'fixed', fsName: '', serial: '', status: 'ok' as const };
  const drives = [
    { label: 'System', path: 'C:\\', freeBytes: 1, totalBytes: 10, ...meta },
    { label: 'Data', path: 'D:\\', freeBytes: 2, totalBytes: 20, ...meta },
    { label: 'Share', path: '\\\\server\\share', freeBytes: 3, totalBytes: 30, ...meta },
  ];

  it('matches by longest prefix, normalizing case and separators', () => {
    expect(driveForPath('C:\\Users\\alice', drives)?.label).toBe('System');
    expect(driveForPath('c:/users/alice', drives)?.label).toBe('System');
    expect(driveForPath('D:\\Backup\\x', drives)?.label).toBe('Data');
  });

  it('matches UNC share roots', () => {
    expect(driveForPath('\\\\SERVER\\share\\sub\\f.txt', drives)?.label).toBe('Share');
  });

  it('returns null when nothing matches', () => {
    expect(driveForPath('Q:\\nowhere', drives)).toBeNull();
  });
});

describe('view store', () => {
  it('defaults to details, set/toggle work', () => {
    expect(view.mode).toBe('details');
    view.set('grid');
    expect(view.mode).toBe('grid');
    view.toggle();
    expect(view.mode).toBe('details');
  });
});

describe('treeStore.ancestorPaths', () => {
  it('windows ancestors match listed rows (no trailing separator below the drive)', () => {
    expect(ancestorPaths('C:\\Users\\you')).toEqual(['C:\\', 'C:\\Users']);
  });

  it('unix ancestors match listed rows (no trailing separator)', () => {
    expect(ancestorPaths('/home/arif/docs')).toEqual(['/home', '/home/arif']);
  });

  it('root itself has no ancestors', () => {
    expect(ancestorPaths('C:\\')).toEqual([]);
  });
});
