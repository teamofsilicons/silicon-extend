import { expect, test as base, type Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { SLT_SAKET } from "../mock/fixtures";

export const SHOTS = "test-results/screenshots";
mkdirSync(SHOTS, { recursive: true });

interface MockControls {
  reset(): Promise<void>;
  /** A pairing code from an Extend app; `instance_of` makes it "Pair with another Carbon" on that pair's device. */
  enroll(os?: string, options?: { instance_of?: string; app_version?: string }): Promise<string>;
  config(c: Record<string, number>): Promise<void>;
  /** Any other control under /__mock (fail-step, scenario, carried, awake, ting-manager, …). */
  post(path: string, data?: Record<string, unknown>): Promise<unknown>;
  get(path: string): Promise<unknown>;
}

export const test = base.extend<{ mock: MockControls }>({
  mock: async ({ request }, use) => {
    const api: MockControls = {
      async reset() {
        await request.post("/__mock/reset");
      },
      async enroll(os = "android", options = {}) {
        const res = await request.post("/__mock/enroll", { data: { os, ...options } });
        return (await res.json()).data.pairing_code as string;
      },
      async config(c: Record<string, number>) {
        await request.post("/__mock/config", { data: c });
      },
      async post(path, data = {}) {
        const res = await request.post(`/__mock/${path}`, { data });
        return (await res.json()).data;
      },
      async get(path) {
        const res = await request.get(`/__mock/${path}`);
        return (await res.json()).data;
      },
    };
    await api.reset();
    await api.config({ step_ms: 250, access_ttl_s: 1800 });
    await use(api);
  },
});

export { expect };

export async function signInWithSlt(page: Page, slt = SLT_SAKET) {
  await page.goto("/");
  await page.getByTestId("slt-input").fill(slt);
  await page.getByTestId("slt-submit").click();
  await expect(page.getByTestId("devices-page")).toBeVisible();
}

/** Full-page screenshots at desktop and phone width. */
export async function shoot(page: Page, name: string) {
  const size = page.viewportSize();
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.waitForTimeout(150);
  await page.screenshot({ path: `${SHOTS}/${name}-desktop.png`, fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(150);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  await page.screenshot({ path: `${SHOTS}/${name}-phone.png`, fullPage: true });
  if (size) await page.setViewportSize(size);
  expect(overflow, `${name} scrolls sideways at phone width`).toBeLessThanOrEqual(0);
}
