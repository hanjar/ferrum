/**
 * Build-time feature flags. A flag is OFF unless its Vite env variable is exactly `'1'`, so a release
 * build (which sets none of them) never shows an unfinished feature.
 */

/** True only for the exact string `'1'`; anything else (unset, `'true'`, `'0'`, ...) is off. */
export function flagOn(value: unknown): boolean {
  return value === '1';
}

/**
 * Automation UI (Automation P1.5): the sidebar entry and the Automation page. Default OFF in
 * release builds. To try it in development run `VITE_FERRUM_AUTOMATION_UI=1 npm run dev`;
 * `playwright.config.ts` starts its own dev server with the flag on. Run now / Retry are not wired
 * in this UI either way.
 */
export const automationUiEnabled: boolean = flagOn(import.meta.env.VITE_FERRUM_AUTOMATION_UI);
