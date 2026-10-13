/**
 * Directory Hotlist IPC (FER-458): thin typed wrappers over `src-tauri/src/hotlist`. Errors arrive
 * as safe kinds only (never paths). Outside the desktop app (plain browser dev, unit tests) an
 * in-memory backend keeps the UI usable without touching any file.
 */
import { invoke } from '@tauri-apps/api/core';
import {
  emptyDoc,
  parseDoc,
  type CommandItem,
  type HotItem,
  type HotlistDoc,
} from '../hotlist/model';
import { isInTauri } from './tauri';

export type HotlistStatus =
  | 'ok'
  | 'new'
  | 'missing'
  | 'cloud'
  | 'unreadable'
  | 'invalid'
  /** FER-467 R7: the custom location failed the safety checks (see `locationProblem`). */
  | 'unsafe';

export interface HotlistView {
  doc: HotlistDoc;
  status: HotlistStatus;
  readOnly: boolean;
  /** The user's own custom file path, or `null` for the default location. */
  customLocation: string | null;
  /** FER-467 R7: why the custom location is refused (`location_network`, `location_link`, ...). */
  locationProblem?: string;
}

export type LocationMode = 'move' | 'use';

export interface ImportWarning {
  index: number;
  kind: string;
}
export interface ImportResult {
  doc: HotlistDoc;
  warnings: ImportWarning[];
}

/** A failed hotlist call; `kind` is a stable code (`conflict`, `read_only`, `not_found`, …). */
export class HotlistError extends Error {
  constructor(public kind: string) {
    super(kind);
    this.name = 'HotlistError';
  }
}

export interface HotlistBackend {
  load(): Promise<HotlistView>;
  save(doc: HotlistDoc): Promise<HotlistView>;
  checkLocation(path: string | null): Promise<boolean>;
  setLocation(path: string | null, mode: LocationMode): Promise<HotlistView>;
  importPreview(path: string | null): Promise<ImportResult>;
  /**
   * Run the command item at `position` (index path in the tree). `item` is what the UI showed; the
   * backend runs it only if the stored hotlist has exactly that item there, and asks in a native
   * dialog when needed. `launched: false` = the user canceled that dialog.
   */
  runCommand(position: number[], item: CommandItem): Promise<{ launched: boolean }>;
}

const STATUSES: HotlistStatus[] = [
  'ok',
  'new',
  'missing',
  'cloud',
  'unreadable',
  'invalid',
  'unsafe',
];

/** Defensive: a malformed reply degrades to an empty, writable list rather than throwing. */
export function asView(raw: unknown): HotlistView {
  if (typeof raw === 'object' && raw !== null) {
    const o = raw as Record<string, unknown>;
    const doc = parseDoc(o.doc);
    if (doc && STATUSES.includes(o.status as HotlistStatus)) {
      const view: HotlistView = {
        doc,
        status: o.status as HotlistStatus,
        readOnly: o.readOnly === true,
        customLocation: typeof o.customLocation === 'string' ? o.customLocation : null,
      };
      if (typeof o.locationProblem === 'string') view.locationProblem = o.locationProblem;
      return view;
    }
  }
  return { doc: emptyDoc(), status: 'new', readOnly: false, customLocation: null };
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    const kind = typeof e === 'object' && e !== null && 'kind' in e ? String(e.kind) : 'unknown';
    throw new HotlistError(kind);
  }
}

const tauriBackend: HotlistBackend = {
  async load() {
    return asView(await call('hotlist_load'));
  },
  async save(doc) {
    return asView(await call('hotlist_save', { doc }));
  },
  async checkLocation(path) {
    return (await call<boolean>('hotlist_check_location', { path })) === true;
  },
  async setLocation(path, mode) {
    return asView(await call('hotlist_set_location', { path, mode }));
  },
  async importPreview(path) {
    const raw = await call<unknown>('hotlist_import_preview', { path });
    const doc = parseDoc((raw as { doc?: unknown } | null)?.doc);
    if (!doc) throw new HotlistError('invalid_doc');
    const warnings = (raw as { warnings?: ImportWarning[] }).warnings ?? [];
    return { doc, warnings };
  },
  async runCommand(position, item) {
    const r = await call<{ launched?: boolean } | null>('hotlist_run_command', {
      position,
      title: item.title,
      command: item.command,
      args: item.args,
    });
    return { launched: r?.launched === true };
  },
};

/** Browser-dev / test fallback: everything lives in memory, nothing is launched or imported. */
export function createMemoryBackend(initial: HotItem[] = []): HotlistBackend {
  let doc: HotlistDoc = { version: 2, items: initial };
  return {
    async load() {
      return { doc, status: 'new', readOnly: false, customLocation: null };
    },
    async save(next) {
      doc = next;
      return { doc, status: 'ok', readOnly: false, customLocation: null };
    },
    async checkLocation() {
      throw new HotlistError('unsupported');
    },
    async setLocation() {
      throw new HotlistError('unsupported');
    },
    async importPreview() {
      throw new HotlistError('unsupported');
    },
    async runCommand() {
      throw new HotlistError('unsupported');
    },
  };
}

let memory: HotlistBackend | null = null;

/** Resolved per call so tests can flip the environment. */
export function defaultBackend(): HotlistBackend {
  return {
    load: () => pick().load(),
    save: (d) => pick().save(d),
    checkLocation: (p) => pick().checkLocation(p),
    setLocation: (p, m) => pick().setLocation(p, m),
    importPreview: (p) => pick().importPreview(p),
    runCommand: (p, i) => pick().runCommand(p, i),
  };
}

function pick(): HotlistBackend {
  if (isInTauri()) return tauriBackend;
  memory ??= createMemoryBackend();
  return memory;
}

/** Test hook: forget the in-memory fallback state. */
export function resetMemoryBackend(): void {
  memory = null;
}
