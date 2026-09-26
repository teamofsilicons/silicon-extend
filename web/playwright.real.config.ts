import { defineConfig, devices } from "@playwright/test";

// Final pass against the real Extend service (crates/extend-service) running locally with
// EXTEND_IAM_MODE=local on EXTEND_REAL_URL (default http://127.0.0.1:8480). This config does not
// start or stop the service; it only starts the website, proxied to it.
const REAL = process.env.EXTEND_REAL_URL || "http://127.0.0.1:8480";
const WEB_PORT = 5192;

export default defineConfig({
  testDir: "e2e-real",
  outputDir: "test-results/artifacts-real",
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: [["list"]],
  use: { baseURL: `http://localhost:${WEB_PORT}`, trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 900 } } }],
  webServer: {
    command: `EXTEND_API_PROXY=${REAL} PORT=${WEB_PORT} npx vite --strictPort`,
    url: `http://localhost:${WEB_PORT}/`,
    reuseExistingServer: false,
    stdout: "ignore",
  },
});
