/**
 * Pure helpers for the Activity page: day-bucket fill for the SVG bar chart
 * and coverage wording. The chart counts distinct ITEMS by latest-access UTC
 * day — it is not a click/visit histogram and must never be labelled so.
 */
import type { ActivityDailyBucketDto } from './ipc/activity';

const DAY_MS = 86_400_000;

export interface DayBar {
  dayUtcMs: number;
  items: number;
}

/** One bar per UTC day in the window (oldest first), zero-filled. */
export function fillDays(daily: ActivityDailyBucketDto[], days: number, nowMs: number): DayBar[] {
  const byDay = new Map(daily.map((b) => [b.dayUtcMs, b.items]));
  const today = Math.floor(nowMs / DAY_MS) * DAY_MS;
  const bars: DayBar[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const dayUtcMs = today - i * DAY_MS;
    bars.push({ dayUtcMs, items: byDay.get(dayUtcMs) ?? 0 });
  }
  return bars;
}

export function chartMax(bars: DayBar[]): number {
  return bars.reduce((m, b) => Math.max(m, b.items), 0);
}

/**
 * Honest coverage text: the retained interval, never "all time" or "all OS
 * activity". `oldestUtcMs` null means nothing is retained in the window.
 */
export function coverageLabel(
  oldestUtcMs: number | null,
  retentionDays: number,
  truncated: boolean,
  fmt: (ms: number) => string,
): string {
  const base = `Hanya aktivitas di dalam Ferrum, disimpan lokal maksimal ${retentionDays} hari. Bukan aktivitas OS atau aplikasi lain.`;
  if (oldestUtcMs === null) return `${base} Belum ada data di jendela ini.`;
  const since = fmt(oldestUtcMs).slice(0, 10);
  return truncated
    ? `${base} Tampilan dibatasi ke data terbaru; item tertua yang terlihat ${since}.`
    : `${base} Data terlama yang tersimpan di jendela ini: ${since}.`;
}
