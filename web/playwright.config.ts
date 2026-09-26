import { defineConfig, devices } from "@playwright/test";

// The e2e suite runs the website against the in-memory mock of the Extend service, on ports of its
// own so it doesn't collide with `pnpm dev:mock`. Tests share the mock's state, so they run one at a
// time and reset it first.
const MOCK_PORT = 8491;
const WEB_PORT = 5191;

export default defineConfig({
  testDir: "e2e",
  outputDir: "test-results/artifacts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
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
