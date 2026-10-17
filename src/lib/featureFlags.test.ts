import { describe, expect, it } from 'vitest';
import { automationUiEnabled, flagOn } from './featureFlags';

describe('featureFlags', () => {
  it('the automation UI is OFF by default (no env variable in a release or unit-test build)', () => {
    expect(import.meta.env.VITE_FERRUM_AUTOMATION_UI).toBeUndefined();
    expect(automationUiEnabled).toBe(false);
  });

  it('a flag is on only for the exact string 1', () => {
    expect(flagOn('1')).toBe(true);
    for (const v of [undefined, '', '0', 'true', 'on', ' 1', 1, true, null]) {
      expect(flagOn(v)).toBe(false);
    }
  });
});
