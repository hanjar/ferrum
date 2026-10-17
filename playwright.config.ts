import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.e2e.ts',
  timeout: 30_000,
  retries: 0,
  use: {
    baseURL: 'http://localhost:5183',
    browserName: 'chromium',
  },
  webServer: {
    command: 'npx vite --port 5183',
    port: 5183,
    // The Automation UI is behind a default-off flag (src/lib/featureFlags.ts); e2e turns it on.
    // Own port and a fresh server: a dev server already running on 5173 has no flag, and reusing
    // it would make the Automation specs fail.
    env: { VITE_FERRUM_AUTOMATION_UI: '1' },
    reuseExistingServer: false,
    timeout: 15_000,
  },
});
