/**
 * A visual tour of the restyled website against the mock, at 1440×900 and 390×844, in light and
 * dark. It writes test-results/restyle/*.png and checks what a screenshot can't show on its own:
 * no sideways scroll at phone width, the dithered print really drew (WebGL), and dark mode is dark.
 */
import { mkdirSync } from "node:fs";
import type { Page } from "@playwright/test";
import { DEVICE_PIXEL, TEST_SECRET } from "../mock/fixtures";
import { expect, signInWithSlt, test } from "./fixtures";

const OUT = "test-results/restyle";
mkdirSync(OUT, { recursive: true });

async function settle(page: Page) {
  await page.evaluate(() => document.fonts.ready);
  // The dithered print resolves over 0.8 s the first time it is on screen.
  await page.waitForTimeout(1100);
}

/** Desktop (full page when `full`), then phone: the first screen, and optionally a second one further down. */
async function capture(page: Page, name: string, opts: { full?: boolean; phoneAlso?: string } = {}) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await settle(page);
  await page.screenshot({ path: `${OUT}/${name}-desktop.png`, fullPage: !!opts.full });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await settle(page);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${name} scrolls sideways at phone width`).toBeLessThanOrEqual(0);
  await page.screenshot({ path: `${OUT}/${name}-phone.png` });
  if (opts.phoneAlso) {
    await page.locator(opts.phoneAlso).first().scrollIntoViewIfNeeded();
    await page.evaluate(() => window.scrollBy(0, -70));
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${OUT}/${name}-phone-2.png` });
  }
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.setViewportSize({ width: 1440, height: 900 });
}

test.describe("restyle tour", () => {
  test.use({ viewport: { width: 1440, height: 900 } });

  test("sign-in, devices, wizard, device page, settings, docs, search", async ({ page, mock, request }) => {
    test.setTimeout(180_000);
    await page.goto("/");
    await expect(page.getByTestId("sign-in")).toBeVisible();
    await capture(page, "01-sign-in");
    // The print is drawn by WebGL; without it the CSS fallback gradient would show.
    await expect(page.locator(".welcome-art canvas")).toHaveAttribute("data-ready", "true");

    await signInWithSlt(page);
    await expect(page.getByTestId("device-list").getByTestId("device-row")).toHaveCount(4);
    await expect(page.locator(`[data-device-id="${DEVICE_PIXEL}"] .pixel-dot.in-use`)).toHaveCount(1);
    await expect(page.locator('[data-device-id="0d44e1f2"] .pixel-dot.offline')).toHaveCount(1);
    await capture(page, "02-devices");

    await page.getByTestId("tab-team").click();
    await expect(page.getByTestId("team-device-list").getByTestId("device-row")).toHaveCount(1);
    await capture(page, "03-team-devices");
    await page.getByTestId("tab-mine").click();

    // ⌘K
    await page.getByTestId("open-search").click();
    await expect(page.getByTestId("command-menu")).toBeVisible();
    await page.getByTestId("command-input").fill("pixel");
    await expect(page.getByTestId("command-option").first()).toContainText("Saket's Pixel");
    await capture(page, "04-search");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(new RegExp(`/devices/${DEVICE_PIXEL}$`));

    // The wizard: kind, then the pairing code as a ticket, then live setup.
    const code = await mock.enroll("android");
    await mock.config({ step_ms: 2500 });
    await page.goto("/devices/new");
    await capture(page, "05-wizard-kind");
    await page.getByTestId("kind-android").click();
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill(code);
    await expect(page.getByTestId("code-step")).toHaveAttribute("data-valid", "true");
    await capture(page, "06-wizard-pairing-code");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Kitchen tablet");
    await page.getByTestId("pair-submit").click();
    await expect(page.locator('[data-testid="setup-step"][data-status="done"]')).toHaveCount(1, { timeout: 10_000 });
    await capture(page, "07-wizard-setup");
    // A narrow desktop: the strip is wider than the page, so it scrolls itself to the current step.
    await page.setViewportSize({ width: 740, height: 900 });
    await page.waitForTimeout(700);
    const rail = page.locator(".step-rail");
    expect(await rail.evaluate((el) => el.scrollWidth > el.clientWidth), "the step strip overflows at 740 px").toBe(true);
    // The edge with more steps fades, so the strip reads as scrollable.
    await expect(rail).toHaveClass(/more-(left|right)/);
    const currentInView = await rail.evaluate((el) => {
      const box = el.getBoundingClientRect();
      const current = el.querySelector("li.current")!.getBoundingClientRect();
      return current.left >= box.left - 1 && current.right <= box.right + 1;
    });
    expect(currentInView, "the current step is scrolled into view").toBe(true);
    await page.screenshot({ path: `${OUT}/07-wizard-setup-narrow.png` });
    // A phone: one mono line and a pixel bar say where the Carbon is.
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByTestId("step-progress")).toBeVisible();
    await expect(page.getByTestId("step-progress")).toContainText("Step 4 of 6 · Device setup.");
    await expect(rail).toBeHidden();
    await page.setViewportSize({ width: 1440, height: 900 });
    await expect(page.getByTestId("setup-complete")).toBeVisible({ timeout: 20_000 });
    await page.getByTestId("setup-next").click();
    await capture(page, "08-wizard-silicons");

    // A device in use, paused for its Carbon, with the activity log.
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await expect(page.getByTestId("in-use-silicon")).toHaveText("si:chef");
    const login = await request.post("/api/v1/auth/login", { headers: { "Idempotency-Key": "tour-chef-login" }, data: { type: "login", data: { slt: "oac_si_chef" } } });
    const chef = (await login.json()).data.access_token;
    const res = await request.post("/api/v1/sessions/a3f/takeover", {
      headers: { Authorization: `Bearer ${chef}`, "X-Org-ID": "acme" },
      data: { type: "takeover", data: { reason: "Please approve the Face ID prompt for the payment" } },
    });
    expect(res.status()).toBe(201);
    await expect(page.getByTestId("takeover-reason")).toHaveText("Please approve the Face ID prompt for the payment", { timeout: 12_000 });
    await expect(page.getByTestId("activity-item").first()).toBeVisible();
    await capture(page, "09-device-takeover", { full: true, phoneAlso: '[data-testid="activity"]' });

    await page.goto("/settings");
    await capture(page, "10-settings", { full: true });

    await page.goto("/docs");
    await capture(page, "11-docs");
    await page.goto("/docs/cli");
    await expect(page.getByTestId("cli-command").first()).toBeVisible();
    await capture(page, "12-docs-cli");
  });

  test("a test environment: banner, empty state", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await page.getByTestId("use-test-environment").click();
    await page.getByTestId("testing-secret-input").fill(TEST_SECRET);
    await page.getByTestId("testing-secret-submit").click();
    await expect(page.getByTestId("testing-banner")).toBeVisible();
    await capture(page, "13-testing-sign-in");
    await page.getByTestId("slt-input").fill("c:alice");
    await page.getByTestId("slt-submit").click();
    await expect(page.getByTestId("devices-page")).toContainText("No devices paired yet");
    await capture(page, "14-testing-banner-empty");
  });

  test("dark mode", async ({ page, mock }) => {
    void mock;
    test.setTimeout(90_000);
    await page.emulateMedia({ colorScheme: "dark" });
    await signInWithSlt(page);
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe("rgb(20, 22, 21)");
    await expect(page.getByTestId("device-list").getByTestId("device-row")).toHaveCount(4);
    // Focus and selection use dark tokens, not light values that vanish on the dark surface.
    await page.getByTestId("device-filter").focus();
    expect(await page.locator(".list-search").evaluate((el) => getComputedStyle(el).outlineColor)).toBe("rgb(143, 163, 255)");
    expect(await page.evaluate(() => getComputedStyle(document.body, "::selection").backgroundColor)).toBe("rgb(52, 64, 122)");
    await page.getByTestId("device-filter").blur();
    await capture(page, "15-dark-devices");
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await expect(page.getByTestId("in-use-silicon")).toBeVisible();
    await capture(page, "16-dark-device-page", { full: true });
    await page.goto("/devices/new?kind=android");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill("4F9C2A");
    await capture(page, "17-dark-pairing-code");
  });

  test("reduced motion: no animation runs", async ({ page, mock }) => {
    void mock;
    await page.emulateMedia({ reducedMotion: "reduce" });
    await signInWithSlt(page);
    const running = await page.evaluate(() => document.getAnimations().filter((a) => a.playState === "running" && (a.effect?.getTiming().duration as number) > 1).length);
    expect(running).toBe(0);
  });
});
