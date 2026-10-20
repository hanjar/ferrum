import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.e2e.ts',
  timeout: 30_000,
  retries: 0,
  use: {
    baseURL: 'http://localhost:5183',
    browserName: 'chromium',
    // FER-569: keep evidence for the intermittent waitForSelector timeouts seen only in CI.
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  webServer: {
    // FER-582: run against a static production build, not the dev server. The dev server's
    // dependency re-optimisation / restart made /node_modules/.vite/deps/chunk-*.js vanish
    // (ERR_CONNECTION_REFUSED) mid-suite -> white page -> waitForSelector('[data-path]') timeouts.
    // Own outDir (dist-e2e) so it never clashes with the Tauri build in dist/.
    command:
      'npx vite build --outDir dist-e2e --emptyOutDir && npx vite preview --outDir dist-e2e --port 5183 --strictPort',
    port: 5183,
    // The Automation UI is behind a default-off flag (src/lib/featureFlags.ts); e2e turns it on.
    // Own port and a fresh server: a server already running on 5173 has no flag, and reusing
    // it would make the Automation specs fail. The flag is baked in at build time.
    env: { VITE_FERRUM_AUTOMATION_UI: '1' },
    reuseExistingServer: false,
    // Includes the production build (~tens of seconds on CI).
    timeout: 180_000,
  },
});
