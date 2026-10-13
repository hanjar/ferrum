/** Pure display helpers for the sidebar Drives section (FER-496). */
import { formatSize } from './format';
import type { DriveInfo } from './ipc/drives';

const TYPE_NAMES: Record<string, string> = {
  fixed: 'Local Disk',
  removable: 'Removable Disk',
  network: 'Network Drive',
  optical: 'CD/DVD Drive',
  ramdisk: 'RAM Disk',
  unknown: 'Drive',
};

/** `C:` from `C:\`; UNC-style roots fall back to the trimmed path. */
export function driveLetter(path: string): string {
  return /^[A-Za-z]:/.test(path) ? path.slice(0, 2).toUpperCase() : path.replace(/[\\/]+$/, '');
}

/** "System (C:)", or "Local Disk (C:)" when the volume has no label. */
export function driveLabel(d: DriveInfo): string {
  const name = d.label.trim() || TYPE_NAMES[d.driveType] || TYPE_NAMES.unknown;
  return `${name} (${driveLetter(d.path)})`;
}

export type DriveIconName = 'drive' | 'usb' | 'net' | 'disk';

export function driveIcon(d: DriveInfo): DriveIconName {
  switch (d.driveType) {
    case 'removable':
      return 'usb';
    case 'network':
      return 'net';
    case 'optical':
      return 'disk';
    default:
      return 'drive';
  }
}

/** Used share 0..100 for the usage bar; 0 when the size is unknown. */
export function usedPercent(d: DriveInfo): number {
  if (d.status !== 'ok' || d.totalBytes <= 0) return 0;
  return Math.max(0, Math.min(100, (1 - d.freeBytes / d.totalBytes) * 100));
}

/** Second line under the label (same size format as the pane status bar). */
export function driveStatusText(d: DriveInfo): string {
  if (d.status === 'noMedia') return 'tidak ada media';
  if (d.status === 'unavailable') return 'tidak tersedia';
  return `${formatSize(d.freeBytes)} free of ${formatSize(d.totalBytes)}`;
}

/** Tooltip: path, label, file system, serial, sizes. */
export function driveTooltip(d: DriveInfo): string {
  const lines = [d.path];
  if (d.label.trim()) lines.push(`Label: ${d.label.trim()}`);
  lines.push(`Tipe: ${TYPE_NAMES[d.driveType] ?? TYPE_NAMES.unknown}`);
  if (d.fsName) lines.push(`File system: ${d.fsName}`);
  if (d.serial) lines.push(`Serial: ${d.serial}`);
  lines.push(driveStatusText(d));
  return lines.join('\n');
}
