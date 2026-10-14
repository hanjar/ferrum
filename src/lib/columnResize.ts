/**
 * Generic column resize helper and storage manager (FER-540, FER-541).
 *
 * Provides safe bounded column width calculations, sanitization,
 * and try/catch-guarded local storage persistence for table and grid headers.
 */

export interface ColumnResizeConfig<K extends string> {
  keys: readonly K[];
  defaults: Record<K, number | null>;
  mins?: Partial<Record<K, number>>;
  defaultMin?: number;
  max?: number | Partial<Record<K, number>>;
  defaultMax?: number;
  step?: number;
  storageKey?: string;
}

export interface ColumnResizeHelper<K extends string> {
  readonly keys: readonly K[];
  readonly defaults: Record<K, number | null>;
  readonly step: number;
  readonly storageKey: string | undefined;
  getMin(key: K): number;
  getMax(key: K): number;
  clamp(key: K, width: number): number;
  sanitize(raw: unknown): Record<K, number | null>;
  load(storage?: Storage | null): Record<K, number | null>;
  persist(widths: Record<K, number | null>, storage?: Storage | null): void;
  reset(): Record<K, number | null>;
  resetKey(key: K): number | null;
  resetCol(current: Record<K, number | null>, key: K): Record<K, number | null>;
}

const DEFAULT_GLOBAL_MIN = 56;
const DEFAULT_GLOBAL_MAX = 2000;
const DEFAULT_RESIZE_STEP = 16;

function resolveStorage(override?: Storage | null): Storage | null {
  if (override !== undefined) return override;
  try {
    return typeof globalThis !== 'undefined' ? (globalThis.localStorage ?? null) : null;
  } catch {
    return null;
  }
}

export function createColumnResize<K extends string>(
  config: ColumnResizeConfig<K>,
): ColumnResizeHelper<K> {
  const {
    keys,
    defaults,
    defaultMin = DEFAULT_GLOBAL_MIN,
    defaultMax = DEFAULT_GLOBAL_MAX,
    step = DEFAULT_RESIZE_STEP,
    storageKey,
  } = config;

  function getMin(key: K): number {
    const m = config.mins?.[key];
    return typeof m === 'number' && Number.isFinite(m) ? Math.round(m) : defaultMin;
  }

  function getMax(key: K): number {
    if (typeof config.max === 'number' && Number.isFinite(config.max))
      return Math.round(config.max);
    if (typeof config.max === 'object' && config.max !== null) {
      const m = config.max[key];
      if (typeof m === 'number' && Number.isFinite(m)) return Math.round(m);
    }
    return defaultMax;
  }

  function clamp(key: K, width: number): number {
    const min = getMin(key);
    const mx = getMax(key);
    if (!Number.isFinite(width)) return min;
    return Math.max(min, Math.min(mx, Math.round(width)));
  }

  function sanitize(raw: unknown): Record<K, number | null> {
    if (typeof raw !== 'object' || raw === null) {
      return { ...defaults };
    }
    const r = raw as Record<string, unknown>;
    const out = {} as Record<K, number | null>;
    for (const key of keys) {
      const val = r[key];
      const def = defaults[key];
      const min = getMin(key);
      const mx = getMax(key);

      if (val === null) {
        out[key] = def === null ? null : def;
      } else if (typeof val === 'number' && Number.isFinite(val) && val >= min && val <= mx) {
        out[key] = Math.round(val);
      } else {
        out[key] = def;
      }
    }
    return out;
  }

  function load(storageOverride?: Storage | null): Record<K, number | null> {
    try {
      const storage = resolveStorage(storageOverride);
      if (!storage || !storageKey) return { ...defaults };
      const raw = storage.getItem(storageKey);
      if (raw === null || raw === undefined) return { ...defaults };
      return sanitize(JSON.parse(raw));
    } catch {
      return { ...defaults };
    }
  }

  function persist(widths: Record<K, number | null>, storageOverride?: Storage | null): void {
    try {
      const storage = resolveStorage(storageOverride);
      if (!storage || !storageKey) return;
      storage.setItem(storageKey, JSON.stringify(widths));
    } catch {
      /* storage full or blocked — saving is best effort */
    }
  }

  function reset(): Record<K, number | null> {
    return { ...defaults };
  }

  function resetKey(key: K): number | null {
    return defaults[key];
  }

  function resetCol(current: Record<K, number | null>, key: K): Record<K, number | null> {
    return { ...current, [key]: defaults[key] };
  }

  return {
    keys,
    defaults,
    step,
    storageKey,
    getMin,
    getMax,
    clamp,
    sanitize,
    load,
    persist,
    reset,
    resetKey,
    resetCol,
  };
}
