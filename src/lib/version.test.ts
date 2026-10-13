import { describe, expect, it } from 'vitest';
import { APP_NAME, APP_VERSION } from './version';

describe('app metadata', () => {
  it('exposes the product name', () => {
    expect(APP_NAME).toBe('Ferrum');
  });

  it('exposes a semver-looking version', () => {
    expect(APP_VERSION).toMatch(/^\d+\.\d+\.\d+$/);
  });
});
