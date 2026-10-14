/** App metadata shared by UI and tests. */

export interface BuildInfo {
  version: string;
  commit: string;
  dirty: boolean;
  date: string;
}

const DEFAULT_BUILD_INFO: BuildInfo = {
  version: '0.2.0',
  commit: 'unknown',
  dirty: false,
  date: '',
};

export const BUILD_INFO: BuildInfo =
  typeof __FERRUM_BUILD__ !== 'undefined' ? __FERRUM_BUILD__ : DEFAULT_BUILD_INFO;

export const APP_NAME = 'Ferrum';
export const APP_VERSION = BUILD_INFO.version;

function formatBuildDate(iso: string, locale: string = 'id'): string {
  try {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return '';
    return new Intl.DateTimeFormat(locale, {
      day: 'numeric',
      month: 'short',
      timeZone: 'UTC',
    }).format(d);
  } catch {
    return '';
  }
}

/**
 * Format pure build label:
 * - normal: `0.2.0 · 8c616c0 · 14 Okt` (or `Oct 14` with locale 'en')
 * - dirty: `0.2.0 · 8c616c0* · 14 Okt`
 * - unknown commit: `0.2.0 · dev`
 */
export function formatBuildLabel(info: BuildInfo, locale: string = 'id'): string {
  if (!info.commit || info.commit === 'unknown') {
    return `${info.version} · dev`;
  }
  const commitPart = `${info.commit}${info.dirty ? '*' : ''}`;
  const datePart = formatBuildDate(info.date, locale);
  return datePart
    ? `${info.version} · ${commitPart} · ${datePart}`
    : `${info.version} · ${commitPart}`;
}

/**
 * Concise build label suitable for tight spaces like Titlebar:
 * - normal: `0.2.0 · 8c616c0`
 * - dirty: `0.2.0 · 8c616c0*`
 * - unknown commit: `0.2.0 · dev`
 */
export function formatCompactBuildLabel(info: BuildInfo): string {
  if (!info.commit || info.commit === 'unknown') {
    return `${info.version} · dev`;
  }
  const commitPart = `${info.commit}${info.dirty ? '*' : ''}`;
  return `${info.version} · ${commitPart}`;
}

/**
 * Detailed multi-line build info for tooltips and copy-to-clipboard:
 * contains version, complete commit hash, build date & time, and dirty status.
 */
export function formatBuildDetail(info: BuildInfo): string {
  const commit = info.commit || 'unknown';
  const dirty = info.dirty ? 'dirty' : 'clean';
  let dateFormatted = info.date;
  try {
    const d = new Date(info.date);
    if (!isNaN(d.getTime())) {
      dateFormatted = d
        .toISOString()
        .replace('T', ' ')
        .replace(/\.\d+Z$/, ' UTC');
    }
  } catch {
    // keep raw info.date
  }
  return [
    `Ferrum ${info.version}`,
    `Commit: ${commit}`,
    `Status: ${dirty}`,
    `Built: ${dateFormatted}`,
  ].join('\n');
}

export const BUILD_LABEL = formatBuildLabel(BUILD_INFO);
export const BUILD_LABEL_COMPACT = formatCompactBuildLabel(BUILD_INFO);
export const BUILD_DETAIL = formatBuildDetail(BUILD_INFO);
