import { expect, test as base, type Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { SLT_SAKET } from "../mock/fixtures";

export const SHOTS = "test-results/screenshots";
mkdirSync(SHOTS, { recursive: true });

export const test = base.extend<{ mock: { reset(): Promise<void>; enroll(os?: string): Promise<string>; config(c: Record<string, number>): Promise<void> } }>({
  mock: async ({ request }, use) => {
    const api = {
      async reset() {
        await request.post("/__mock/reset");
      },
      async enroll(os = "android") {
        const res = await request.post("/__mock/enroll", { data: { os } });
        return (await res.json()).data.pairing_code as string;
      },
      async config(c: Record<string, number>) {
        await request.post("/__mock/config", { data: c });
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
