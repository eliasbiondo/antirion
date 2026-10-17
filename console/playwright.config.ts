import { defineConfig, devices } from '@playwright/test';

/**
 * End-to-end runs against a real test-mode gateway (data 20480, admin 20481) built with this export
 * embedded, so the console is served by the admin listener itself. Setting
 * ANTIRION_E2E_CONSOLE_URL serves `out/` from scripts/serve.mjs instead, for a gateway built
 * without it. A gateway already listening on those ports is reused.
 */
const ADMIN = process.env.ANTIRION_E2E_ADMIN_URL ?? 'http://127.0.0.1:20481';
const CONSOLE = process.env.ANTIRION_E2E_CONSOLE_URL ?? ADMIN;

export default defineConfig({
  testDir: 'tests/e2e',
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  timeout: 30_000,
  expect: { timeout: 7_000 },
  use: {
    baseURL: `${CONSOLE}/ui/`,
    trace: 'retain-on-failure',
    ...devices['Desktop Chrome'],
    viewport: { width: 1440, height: 900 },
  },
  globalSetup: './tests/e2e/global-setup.ts',
  webServer: [
    {
      command: 'node tests/e2e/start-gateway.mjs',
      url: `${ADMIN}/-/readyz`,
      reuseExistingServer: true,
      timeout: 60_000,
      stdout: 'ignore',
      stderr: 'pipe',
    },
    ...(CONSOLE === ADMIN
      ? []
      : [
          {
            command: 'node scripts/serve.mjs',
            url: `${CONSOLE}/ui/login/`,
            reuseExistingServer: true,
            env: { ANTIRION_ADMIN_URL: ADMIN, PORT: new URL(CONSOLE).port },
          },
        ]),
  ],
});
