/**
 * Properties / occupied space / volume info IPC (F2-21). Mirrors
 * `src-tauri/src/properties.rs`; every payload is runtime-guarded.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { FsError, type ErrorCode } from './types';
import { isInTauri } from './tauri';

export interface EntryProperties {
  path: string;
  name: string;
  /** `file` | `dir` | `symlink` | `other` */
  kind: string;
  size: number;
  allocatedBytes: number;
  allocatedSupported: boolean;
  createdMs: number;
  modifiedMs: number;
  accessedMs: number;
  readOnly: boolean;
  hidden: boolean;
  system: boolean;
  /** POSIX permission bits; absent when the platform does not report them. */
  mode?: number;
  /** True when Change Attributes can really write read-only/hidden here. */
  attributesEditable: boolean;
  symlinkTarget?: string;
}

export interface FailedPath {
  path: string;
  code: ErrorCode;
  message: string;
}

export interface OccupiedReport {
  files: number;
  dirs: number;
  symlinks: number;
  logicalBytes: number;
  allocatedBytes: number;
  allocatedSupported: boolean;
  failed: FailedPath[];
  /** True when the walk was cancelled; totals are then partial. */
  cancelled: boolean;
}

/** Throttled running totals of one occupied-space walk. */
export interface OccupiedProgress {
  scanId: string;
  files: number;
  dirs: number;
  symlinks: number;
  logicalBytes: number;
}

/** Host facts from Rust std (OS version is intentionally not reported). */
export interface HostInfo {
  os: string;
  arch: string;
  family: string;
  hostName?: string;
}

export interface VolumeInfo {
  root: string;
  label: string;
  fsName: string;
  totalBytes: number;
  freeBytes: number;
}

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Filesystem access requires the desktop app (not browser dev)',
  });
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined;
}

function num(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

function bool(v: unknown): boolean | undefined {
  return typeof v === 'boolean' ? v : undefined;
}

function bad(shape: string): FsError {
  return new FsError({ code: 'io', message: `Malformed ${shape} from IPC` });
}

export function asEntryProperties(raw: unknown): EntryProperties {
  if (typeof raw !== 'object' || raw === null) throw bad('EntryProperties');
  const r = raw as Record<string, unknown>;
  const required = {
    path: str(r.path),
    name: str(r.name),
    kind: str(r.kind),
    size: num(r.size),
    allocatedBytes: num(r.allocatedBytes),
    allocatedSupported: bool(r.allocatedSupported),
    createdMs: num(r.createdMs),
    modifiedMs: num(r.modifiedMs),
    accessedMs: num(r.accessedMs),
    readOnly: bool(r.readOnly),
    hidden: bool(r.hidden),
    system: bool(r.system),
  };
  for (const v of Object.values(required)) {
    if (v === undefined) throw bad('EntryProperties');
  }
  // Absent means "unknown" → treated as not editable, never as editable.
  const out = {
    ...required,
    attributesEditable: bool(r.attributesEditable) ?? false,
  } as EntryProperties;
  const mode = num(r.mode);
  if (mode !== undefined) out.mode = mode;
  const target = str(r.symlinkTarget);
  if (target !== undefined) out.symlinkTarget = target;
  return out;
}

export function asOccupiedReport(raw: unknown): OccupiedReport {
  if (typeof raw !== 'object' || raw === null) throw bad('OccupiedReport');
  const r = raw as Record<string, unknown>;
  const files = num(r.files);
  const dirs = num(r.dirs);
  const symlinks = num(r.symlinks);
  const logicalBytes = num(r.logicalBytes);
  const allocatedBytes = num(r.allocatedBytes);
  const allocatedSupported = bool(r.allocatedSupported);
  if (
    files === undefined ||
    dirs === undefined ||
    symlinks === undefined ||
    logicalBytes === undefined ||
    allocatedBytes === undefined ||
    allocatedSupported === undefined
  ) {
    throw bad('OccupiedReport');
  }
  const failed: FailedPath[] = Array.isArray(r.failed)
    ? r.failed.flatMap((item) => {
        if (typeof item !== 'object' || item === null) throw bad('OccupiedReport');
        const f = item as Record<string, unknown>;
        const path = str(f.path);
        const code = str(f.code) as ErrorCode | undefined;
        const message = str(f.message);
        if (!path || !code || !message) throw bad('OccupiedReport');
        return [{ path, code, message }];
      })
    : [];
  return {
    files,
    dirs,
    symlinks,
    logicalBytes,
    allocatedBytes,
    allocatedSupported,
    failed,
    cancelled: bool(r.cancelled) ?? false,
  };
}

export function asHostInfo(raw: unknown): HostInfo {
  if (typeof raw !== 'object' || raw === null) throw bad('HostInfo');
  const r = raw as Record<string, unknown>;
  const os = str(r.os);
  const arch = str(r.arch);
  const family = str(r.family);
  if (os === undefined || arch === undefined || family === undefined) throw bad('HostInfo');
  const out: HostInfo = { os, arch, family };
  const hostName = str(r.hostName);
  if (hostName !== undefined) out.hostName = hostName;
  return out;
}

export function asVolumeInfo(raw: unknown): VolumeInfo {
  if (typeof raw !== 'object' || raw === null) throw bad('VolumeInfo');
  const r = raw as Record<string, unknown>;
  const root = str(r.root);
  const label = str(r.label);
  const fsName = str(r.fsName);
  const totalBytes = num(r.totalBytes);
  const freeBytes = num(r.freeBytes);
  if (
    root === undefined ||
    label === undefined ||
    fsName === undefined ||
    totalBytes === undefined ||
    freeBytes === undefined
  ) {
    throw bad('VolumeInfo');
  }
  return { root, label, fsName, totalBytes, freeBytes };
}

async function call<T>(
  command: string,
  args: Record<string, unknown>,
  guard: (raw: unknown) => T,
): Promise<T> {
  if (!isInTauri()) throw unsupported();
  let raw: unknown;
  try {
    raw = await invoke(command, args);
  } catch (e) {
    throw FsError.fromIPC(e);
  }
  return guard(raw);
}

export function entryProperties(path: string): Promise<EntryProperties> {
  return call('entry_properties', { path }, asEntryProperties);
}

/** Starts a cancellable walk; resolves (also when cancelled) with the report. */
export function occupiedSpace(scanId: string, paths: string[]): Promise<OccupiedReport> {
  return call('occupied_space', { scanId, paths }, asOccupiedReport);
}

/** Cancels a running walk; false when it already finished. */
export async function cancelOccupiedSpace(scanId: string): Promise<boolean> {
  if (!isInTauri()) return false;
  try {
    return (await invoke('occupied_space_cancel', { scanId })) === true;
  } catch {
    return false;
  }
}

export async function onOccupiedProgress(
  handler: (p: OccupiedProgress) => void,
): Promise<UnlistenFn> {
  if (!isInTauri()) return () => {};
  return listen<OccupiedProgress>('properties://occupied-progress', (e) => handler(e.payload));
}

export function hostInfo(): Promise<HostInfo> {
  return call('system_info', {}, asHostInfo);
}

export function volumeInfo(path: string): Promise<VolumeInfo> {
  return call('volume_info', { path }, asVolumeInfo);
}

export function setAttributes(
  path: string,
  readOnly: boolean | null,
  hidden: boolean | null,
): Promise<void> {
  return call('set_attributes', { path, readOnly, hidden }, () => undefined);
}
