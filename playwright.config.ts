import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.e2e.ts',
  timeout: 30_000,
  retries: 0,
  use: {
    baseURL: 'http://localhost:5173',
    browserName: 'chromium',
  },
  webServer: {
    command: 'npx vite --port 5173',
    port: 5173,
    // The Automation UI is behind a default-off flag (src/lib/featureFlags.ts); e2e turns it on.
    env: { VITE_FERRUM_AUTOMATION_UI: '1' },
    reuseExistingServer: true,
    timeout: 15_000,
  },
});
