import { describe, expect, it } from 'vitest';
import {
  driveIcon,
  driveLabel,
  driveLetter,
  driveStatusText,
  driveTooltip,
  usedPercent,
} from './driveDisplay';
import type { DriveInfo } from './ipc/drives';

const base: DriveInfo = {
  label: 'System',
  path: 'C:\\',
  freeBytes: 25 * 1024 ** 3,
  totalBytes: 100 * 1024 ** 3,
  driveType: 'fixed',
  fsName: 'NTFS',
  serial: '1A2B-3C4D',
  status: 'ok',
};

describe('driveDisplay', () => {
  it('labels with volume name or type fallback plus letter', () => {
    expect(driveLabel(base)).toBe('System (C:)');
    expect(driveLabel({ ...base, label: '' })).toBe('Local Disk (C:)');
    expect(driveLabel({ ...base, label: ' ', driveType: 'optical', path: 'E:\\' })).toBe(
      'CD/DVD Drive (E:)',
    );
    expect(driveLetter('d:\\')).toBe('D:');
  });

  it('chooses an icon per drive type', () => {
    expect(driveIcon(base)).toBe('drive');
    expect(driveIcon({ ...base, driveType: 'removable' })).toBe('usb');
    expect(driveIcon({ ...base, driveType: 'network' })).toBe('net');
    expect(driveIcon({ ...base, driveType: 'optical' })).toBe('disk');
  });

  it('formats usage like the status bar and handles missing media', () => {
    expect(driveStatusText(base)).toBe('25.0 GB free of 100 GB');
    expect(usedPercent(base)).toBe(75);
    const none = { ...base, status: 'noMedia' as const, freeBytes: 0, totalBytes: 0 };
    expect(driveStatusText(none)).toBe('tidak ada media');
    expect(usedPercent(none)).toBe(0);
    expect(driveStatusText({ ...base, status: 'unavailable' })).toBe('tidak tersedia');
  });

  it('puts file system, serial and label in the tooltip', () => {
    const t = driveTooltip(base);
    expect(t).toContain('Label: System');
    expect(t).toContain('File system: NTFS');
    expect(t).toContain('Serial: 1A2B-3C4D');
  });
});
