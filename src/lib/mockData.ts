/**
 * Mock data mirroring the DEFAULT STATE of prototype v2 as rendered at
 * 1440×900 (screenshots in docs/screenshots/): left pane C:\Projects, right
 * pane D:\Backup. Shapes are provisional until the domain contract lands in
 * task 03/04 — components must treat these as display-only inputs, not as
 * the IPC contract.
 */

export type MockKind = 'dir' | 'code' | 'image' | 'media' | 'archive' | 'data' | 'docs' | 'text';

export interface MockEntry {
  name: string;
  isDir: boolean;
  kind: MockKind;
  sizeText: string;
  modText: string;
}

export const projectsEntries: MockEntry[] = [
  { name: 'archive-2025', isDir: true, kind: 'dir', sizeText: '', modText: '2026-01-04 10:00' },
  {
    name: 'atlas-design-system',
    isDir: true,
    kind: 'dir',
    sizeText: '',
    modText: '2026-09-23 15:40',
  },
  { name: 'datasets', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-24 08:15' },
  { name: 'helix-engine', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-26 18:42' },
  { name: 'orbit-web', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-25 11:08' },
  { name: 'renders', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-26 14:02' },
  {
    name: 'ambient-loop.flac',
    isDir: false,
    kind: 'media',
    sizeText: '43 MB',
    modText: '2026-09-08 22:30',
  },
  {
    name: 'budget-q3-2026.xlsx',
    isDir: false,
    kind: 'data',
    sizeText: '188 KB',
    modText: '2026-09-21 17:12',
  },
  {
    name: 'kickoff-recording.mp4',
    isDir: false,
    kind: 'media',
    sizeText: '1.3 GB',
    modText: '2026-09-02 11:05',
  },
  {
    name: 'meeting-notes.md',
    isDir: false,
    kind: 'docs',
    sizeText: '5.6 KB',
    modText: '2026-09-26 10:24',
  },
  {
    name: 'README.md',
    isDir: false,
    kind: 'docs',
    sizeText: '2.9 KB',
    modText: '2026-09-14 09:40',
  },
  {
    name: 'roadmap-2026.pdf',
    isDir: false,
    kind: 'docs',
    sizeText: '2.2 MB',
    modText: '2026-09-19 16:51',
  },
  {
    name: 'workspace.code-workspace',
    isDir: false,
    kind: 'text',
    sizeText: '612 B',
    modText: '2026-09-01 09:00',
  },
];

export const backupEntries: MockEntry[] = [
  { name: 'db-dumps', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-20 02:10' },
  { name: 'helix-engine', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-20 02:00' },
  { name: 'orbit-web', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-20 02:02' },
  { name: 'photos-2026', isDir: true, kind: 'dir', sizeText: '', modText: '2026-09-14 20:00' },
  {
    name: 'backup-manifest.json',
    isDir: false,
    kind: 'data',
    sizeText: '48 KB',
    modText: '2026-09-20 02:14',
  },
  {
    name: 'projects-2026-09-20.zip',
    isDir: false,
    kind: 'archive',
    sizeText: '1.6 GB',
    modText: '2026-09-20 02:12',
  },
  {
    name: 'restore.log',
    isDir: false,
    kind: 'text',
    sizeText: '13 KB',
    modText: '2026-09-20 02:14',
  },
];

export interface SideItem {
  label: string;
  icon: 'home' | 'folder';
}

/** Browser-dev only: shown when the activity backend is absent. */
export const mockFavorites: SideItem[] = [
  { label: 'Home', icon: 'home' },
  { label: 'Projects', icon: 'folder' },
  { label: 'Downloads', icon: 'folder' },
  { label: 'Documents', icon: 'folder' },
  { label: 'Backup', icon: 'folder' },
];

/**
 * Browser-dev only (no Tauri backend): fixture for the sidebar Drives section.
 * Never used in the desktop app, which reads `list_drives` (FER-496).
 */
export const mockDrives: DriveInfo[] = [
  {
    label: 'System',
    path: 'C:\\',
    freeBytes: 82 * 1024 ** 3,
    totalBytes: 512 * 1024 ** 3,
    driveType: 'fixed',
    fsName: 'NTFS',
    serial: '0000-0000',
    status: 'ok',
  },
  {
    label: 'Archive',
    path: 'D:\\',
    freeBytes: 1.8 * 1024 ** 4,
    totalBytes: 4 * 1024 ** 4,
    driveType: 'fixed',
    fsName: 'NTFS',
    serial: '0000-0001',
    status: 'ok',
  },
];

// ── Browser-dev fallback ────────────────────────────────────────────────
// Serves prototype fixture listings for the two default panes; anything
// else 404s like a real missing directory.

import type { FileEntry } from './ipc/types';
import type { DriveInfo } from './ipc/drives';

export const mockHome: [string, string] = ['C:\\Projects', 'D:\\Backup'];

/** Inverse of formatSize for fixture sizes ("612 B", "1.3 GB"). */
function parseSizeText(text: string): number {
  const m = /^(?:([\d.]+)\s*(B|KB|MB|GB|TB))?$/.exec(text.trim());
  if (!m || !m[1]) return 0;
  const units: Record<string, number> = {
    B: 1,
    KB: 1024,
    MB: 1024 ** 2,
    GB: 1024 ** 3,
    TB: 1024 ** 4,
  };
  return Math.round(parseFloat(m[1]) * units[m[2]]);
}

export async function mockFetcher(path: string): Promise<FileEntry[]> {
  const which =
    path === 'C:\\Projects' ? projectsEntries : path === 'D:\\Backup' ? backupEntries : null;
  if (!which) {
    throw Object.assign(new Error('Not found (mock)'), {
      name: 'FsError',
      code: 'notFound',
      path,
    });
  }
  return which.map((e) => ({
    name: e.name,
    path: `${path}\\${e.name}`,
    isDir: e.isDir,
    isSymlink: false,
    size: e.isDir ? 0 : parseSizeText(e.sizeText),
    modifiedMs: Date.parse(e.modText) || 0,
    hidden: e.name.startsWith('.'),
  }));
}
