import { describe, expect, it } from 'vitest';
import {
  APP_NAME,
  APP_VERSION,
  BUILD_DETAIL,
  BUILD_INFO,
  BUILD_LABEL,
  BUILD_LABEL_COMPACT,
  formatBuildDetail,
  formatBuildLabel,
  formatCompactBuildLabel,
} from './version';

describe('app metadata', () => {
  it('exposes the product name', () => {
    expect(APP_NAME).toBe('Ferrum');
  });

  it('exposes version matching package.json', () => {
    expect(APP_VERSION).toBe('0.2.3');
    expect(APP_VERSION).toMatch(/^\d+\.\d+\.\d+$/);
  });
});

describe('formatBuildLabel', () => {
  it('formats normal build with id locale by default', () => {
    const label = formatBuildLabel({
      version: '0.2.0',
      commit: '8c616c0',
      dirty: false,
      date: '2026-10-14T10:00:00.000Z',
    });
    expect(label).toBe('0.2.0 · 8c616c0 · 14 Okt');
  });

  it('appends asterisk when working tree is dirty', () => {
    const label = formatBuildLabel({
      version: '0.2.0',
      commit: '8c616c0',
      dirty: true,
      date: '2026-10-14T10:00:00.000Z',
    });
    expect(label).toBe('0.2.0 · 8c616c0* · 14 Okt');
  });

  it('shows dev when commit is unknown or empty', () => {
    expect(
      formatBuildLabel({
        version: '0.2.0',
        commit: 'unknown',
        dirty: false,
        date: '2026-10-14T10:00:00.000Z',
      }),
    ).toBe('0.2.0 · dev');

    expect(
      formatBuildLabel({
        version: '0.2.0',
        commit: '',
        dirty: true,
        date: '2026-10-14T10:00:00.000Z',
      }),
    ).toBe('0.2.0 · dev');
  });

  it('supports id and en locales for date formatting', () => {
    const info = {
      version: '0.2.0',
      commit: '8c616c0',
      dirty: false,
      date: '2026-10-14T00:00:00.000Z',
    };
    expect(formatBuildLabel(info, 'id')).toBe('0.2.0 · 8c616c0 · 14 Okt');
    expect(formatBuildLabel(info, 'en')).toBe('0.2.0 · 8c616c0 · Oct 14');
  });

  it('omits date gracefully if date is invalid', () => {
    const label = formatBuildLabel({
      version: '0.2.0',
      commit: '8c616c0',
      dirty: false,
      date: 'not-a-date',
    });
    expect(label).toBe('0.2.0 · 8c616c0');
  });
});

describe('formatCompactBuildLabel', () => {
  it('formats concise label for titlebar', () => {
    expect(
      formatCompactBuildLabel({
        version: '0.2.0',
        commit: '8c616c0',
        dirty: false,
        date: '2026-10-14T10:00:00.000Z',
      }),
    ).toBe('0.2.0 · 8c616c0');

    expect(
      formatCompactBuildLabel({
        version: '0.2.0',
        commit: '8c616c0',
        dirty: true,
        date: '2026-10-14T10:00:00.000Z',
      }),
    ).toBe('0.2.0 · 8c616c0*');

    expect(
      formatCompactBuildLabel({
        version: '0.2.0',
        commit: 'unknown',
        dirty: false,
        date: '2026-10-14T10:00:00.000Z',
      }),
    ).toBe('0.2.0 · dev');
  });
});

describe('formatBuildDetail', () => {
  it('includes version, commit, dirty status, and UTC date/time', () => {
    const detail = formatBuildDetail({
      version: '0.2.0',
      commit: '8c616c0',
      dirty: false,
      date: '2026-10-14T10:20:30.000Z',
    });
    expect(detail).toContain('Ferrum 0.2.0');
    expect(detail).toContain('Commit: 8c616c0');
    expect(detail).toContain('Status: clean');
    expect(detail).toContain('Built: 2026-10-14 10:20:30 UTC');

    const dirtyDetail = formatBuildDetail({
      version: '0.2.0',
      commit: '8c616c0',
      dirty: true,
      date: '2026-10-14T10:20:30.000Z',
    });
    expect(dirtyDetail).toContain('Status: dirty');
  });
});

describe('deterministic test environment values', () => {
  it('injects test000 commit and fixed date in Vitest', () => {
    expect(BUILD_INFO.version).toBe('0.2.3');
    expect(BUILD_INFO.commit).toBe('test000');
    expect(BUILD_INFO.dirty).toBe(false);
    expect(BUILD_INFO.date).toBe('2026-10-14T00:00:00.000Z');

    expect(BUILD_LABEL).toBe('0.2.3 · test000 · 14 Okt');
    expect(BUILD_LABEL_COMPACT).toBe('0.2.3 · test000');
    expect(BUILD_DETAIL).toContain('test000');
  });
});
