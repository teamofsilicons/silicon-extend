import { defineConfig, devices } from "@playwright/test";

// The e2e suite runs the website against the in-memory mock of the Extend service, on ports of its
// own so it doesn't collide with `pnpm dev:mock`. Tests share the mock's state, so they run one at a
// time and reset it first. E2E_MOCK_PORT and E2E_WEB_PORT move them if something else holds them.
const MOCK_PORT = Number(process.env.E2E_MOCK_PORT || 8491);
const WEB_PORT = Number(process.env.E2E_WEB_PORT || 5191);

export default defineConfig({
  testDir: "e2e",
  outputDir: "test-results/artifacts",
  fullyParallel: false,
  workers: 1,
  // One retry on CI only: a shared runner occasionally fails to capture a screenshot (a browser
  // protocol error, not a failed assertion). Locally a failure is always reported first time.
  retries: process.env.CI ? 1 : 0,
  timeout: 45_000,
  expect: { timeout: 8_000 },
  reporter: [["list"]],
  use: {
    baseURL: `http://localhost:${WEB_PORT}`,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 900 } } }],
  webServer: [
    {
      command: `MOCK_PORT=${MOCK_PORT} npx tsx mock/server.ts`,
      url: `http://127.0.0.1:${MOCK_PORT}/live`,
      reuseExistingServer: false,
      stdout: "ignore",
    },
    {
      command: `EXTEND_API_PROXY=http://127.0.0.1:${MOCK_PORT} PORT=${WEB_PORT} npx vite --strictPort`,
      url: `http://localhost:${WEB_PORT}/`,
      reuseExistingServer: false,
      stdout: "ignore",
    },
  ],
});
