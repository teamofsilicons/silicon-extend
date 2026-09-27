import { defineConfig, devices } from "@playwright/test";

// The released 1.0.0 website (built by scripts/build-web-1.0.mjs) against the 1.1 mock service:
// what a Carbon with a cached 1.0 bundle sees between the service and website releases.
const MOCK_PORT = Number(process.env.E2E_MOCK_PORT || 8492);
const WEB_PORT = Number(process.env.E2E_WEB_PORT || 5192);

export default defineConfig({
  testDir: "e2e-compat",
  outputDir: "test-results/artifacts-compat",
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  timeout: 45_000,
  expect: { timeout: 8_000 },
  reporter: [["list"]],
  use: { baseURL: `http://localhost:${WEB_PORT}`, trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 900 } } }],
  webServer: [
    {
      command: `MOCK_PORT=${MOCK_PORT} npx tsx mock/server.ts`,
      url: `http://127.0.0.1:${MOCK_PORT}/live`,
      reuseExistingServer: false,
      stdout: "ignore",
    },
    {
      command: `cd test-results/web-1.0/web && EXTEND_API_PROXY=http://127.0.0.1:${MOCK_PORT} npx vite preview --port ${WEB_PORT} --strictPort`,
      url: `http://localhost:${WEB_PORT}/`,
      reuseExistingServer: false,
      stdout: "ignore",
    },
  ],
});
