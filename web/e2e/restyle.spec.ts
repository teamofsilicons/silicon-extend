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
    // Production's layout: IAM's consent screen at <auth origin>/login, so sign-up is /signup beside it.
    await page.route("**/api/v1/iam", async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.data.iam_login_url = "https://auth.iam.teamofsilicons.com/login";
      await route.fulfill({ response: res, json: body });
    });
    await page.goto("/");
    await expect(page.getByTestId("signup-note")).toContainText("checks your email and phone");
    await expect(page.getByTestId("sign-in")).toBeVisible();
    await capture(page, "01-sign-in");
    // The print is drawn by WebGL; without it the CSS fallback gradient would show.
    await expect(page.locator(".welcome-art canvas")).toHaveAttribute("data-ready", "true");

    await signInWithSlt(page);
    await expect(page.getByTestId("device-list").getByTestId("device-row")).toHaveCount(7);
    await expect(page.locator(`[data-device-id="${DEVICE_PIXEL}"] .pixel-dot.in-use`)).toHaveCount(1);
    await expect(page.locator('[data-device-id="0d44e1f2"] .pixel-dot.offline')).toHaveCount(1);
    // The tally is a quiet stat, not a second poster beside the pairing code.
    const tally = await page.locator(".tally-numbers dd").first().evaluate((el) => ({ size: parseFloat(getComputedStyle(el).fontSize), weight: getComputedStyle(el).fontWeight }));
    expect(tally.size).toBeLessThanOrEqual(38);
    expect(tally.weight).toBe("400");
    await capture(page, "02-devices");

    // 1.1: a device another Carbon paired too, with that Carbon's Silicon's request to Saket.
    await page.goto("/devices/5a1e7f00");
    await expect(page.getByTestId("shared-note")).toBeVisible();
    await capture(page, "03-shared-device", { full: true });
    // A locked Mac a Silicon asks Saket to wake.
    await page.goto("/devices/2e7f00d1");
    await expect(page.getByTestId("wake-banner")).toBeVisible();
    await capture(page, "03a-wake-banner", { full: true });
    await page.goto("/devices");
    await page.getByTestId("tab-removed").click();
    await expect(page.getByTestId("removed-device-list").getByTestId("device-row")).toHaveCount(2);
    await capture(page, "03b-removed-devices");
    await page.getByTestId("removed-device-list").getByTestId("device-row").first().click();
    await expect(page.getByTestId("removed-card")).toBeVisible();
    await expect(page.getByTestId("activity-item")).toHaveCount(6);
    await capture(page, "03c-removed-device", { full: true });
    await page.goto("/devices");
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
    // "Get the app" has one primary action: going on to the code. Download is secondary.
    await expect(page.getByTestId("download-link")).toHaveClass(/\bsecondary\b/);
    await expect(page.getByTestId("guide").locator(".button.primary")).toHaveCount(1);
    await expect(page.getByTestId("guide").locator(".button.primary")).toContainText("I have the code");
    await capture(page, "05b-wizard-get-the-app");
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
    await expect(page.locator(".device-header").getByTestId("device-status")).toHaveText("Paused for you");
    await capture(page, "09-device-takeover", { full: true, phoneAlso: '[data-testid="activity"]' });
    // At phone width "Take away" stays on its grant's first line, at the right, not wrapped and indented.
    await page.setViewportSize({ width: 390, height: 844 });
    const grant = page.locator('[data-testid="grant"][data-silicon="si:scout"]');
    await grant.scrollIntoViewIfNeeded();
    const place = await grant.evaluate((el) => {
      const who = el.querySelector(".grant-who")!.getBoundingClientRect();
      const text = el.querySelector(":scope > div")!.getBoundingClientRect();
      const button = el.querySelector('[data-testid="revoke"]')!.getBoundingClientRect();
      const row = el.getBoundingClientRect();
      return { buttonTop: button.top, whoBottom: who.bottom, buttonLeft: button.left, textRight: text.right, buttonRight: button.right, rowRight: row.right };
    });
    expect(place.buttonTop, "Take away sits on the grant's first line").toBeLessThan(place.whoBottom);
    expect(place.buttonLeft, "Take away is beside the text, not under it").toBeGreaterThanOrEqual(place.textRight);
    expect(Math.abs(place.buttonRight - place.rowRight), "Take away is flush right").toBeLessThanOrEqual(1);
    await page.screenshot({ path: `${OUT}/09-device-access-phone.png` });
    await page.setViewportSize({ width: 1440, height: 900 });

    // The Remove dialog, with its count in agreement.
    await page.getByTestId("remove-device").click();
    await expect(page.getByTestId("remove-access")).toHaveText("4 Silicons lose access.");
    await capture(page, "09b-remove-dialog");
    await page.keyboard.press("Escape");

    await page.goto("/settings");
    await capture(page, "10-settings", { full: true });

    await page.goto("/docs");
    await capture(page, "11-docs");
    await page.goto("/docs/cli");
    await expect(page.getByTestId("cli-command").first()).toBeVisible();
    await capture(page, "12-docs-cli");
    // A command whose errors are a sentence, and whose "who" is long, at phone width.
    const fileGet = page.getByTestId("cli-command").filter({ has: page.locator("pre.usage", { hasText: /^extend file get / }) });
    if (await fileGet.count()) {
      await page.setViewportSize({ width: 390, height: 844 });
      await fileGet.first().scrollIntoViewIfNeeded();
      await page.evaluate(() => window.scrollBy(0, -70));
      await settle(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0);
      await page.screenshot({ path: `${OUT}/12b-docs-cli-file-get-phone.png` });
      await page.setViewportSize({ width: 1440, height: 900 });
    }
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
    // The empty-state orb is printed at the sign-in print's finer 3 px cell.
    await expect(page.locator(".empty-orb")).toHaveAttribute("data-cell", "3");
    await capture(page, "14-testing-banner-empty");
  });

  test("wide desktop: the column sits in the middle of its pane", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.setViewportSize({ width: 1920, height: 1080 });
    const centred = async (content: string, pane: string) =>
      page.evaluate(
        ([c, p]) => {
          const a = document.querySelector(c)!.getBoundingClientRect();
          const b = document.querySelector(p)!.getBoundingClientRect();
          return Math.abs(a.left - b.left - (b.right - a.right));
        },
        [content, pane],
      );
    await expect(page.locator(".overview")).toBeVisible();
    await settle(page);
    // The pane's own padding is symmetric, so equal gaps on both sides mean the column is centred.
    expect(await centred(".overview", ".main-pane"), "overview centred").toBeLessThanOrEqual(2);
    await page.screenshot({ path: `${OUT}/18-wide-overview-1920.png` });
    await page.goto("/devices/new?kind=android");
    await expect(page.getByTestId("guide")).toBeVisible();
    await settle(page);
    expect(await centred(".wizard-body", ".page-main"), "wizard centred").toBeLessThanOrEqual(2);
    const edges = await page.evaluate(() => [document.querySelector(".wizard .page-heading")!.getBoundingClientRect().left, document.querySelector(".wizard-body")!.getBoundingClientRect().left]);
    expect(Math.abs(edges[0] - edges[1]), "the heading and the wizard share a left edge").toBeLessThanOrEqual(1);
    await page.screenshot({ path: `${OUT}/19-wide-wizard-1920.png` });
    await page.goto("/settings");
    await expect(page.getByTestId("settings-page")).toBeVisible();
    await settle(page);
    expect(await centred(".page-main > .card", ".page-main"), "settings centred").toBeLessThanOrEqual(2);
    await page.screenshot({ path: `${OUT}/20-wide-settings-1920.png` });
    // At 1440 the column keeps its place at the left of the pane.
    await page.setViewportSize({ width: 1440, height: 900 });
    expect(await centred(".page-main > .card", ".page-main")).toBeGreaterThan(100);
  });

  test("dark mode", async ({ page, mock }) => {
    void mock;
    test.setTimeout(90_000);
    await page.emulateMedia({ colorScheme: "dark" });
    await signInWithSlt(page);
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe("rgb(20, 22, 21)");
    await expect(page.getByTestId("device-list").getByTestId("device-row")).toHaveCount(7);
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
    // The print's paper is the card's, so no darker band shows above the tear line.
    const ticket = await page.evaluate(() => [getComputedStyle(document.querySelector(".ticket-print")!).backgroundColor, getComputedStyle(document.documentElement).getPropertyValue("--card").trim()]);
    expect(ticket[0]).toBe("rgb(26, 29, 28)");
    expect(ticket[1]).toBe("#1a1d1c");
    await capture(page, "17-dark-pairing-code");
    await page.goto("/devices");
    await expect(page.locator(".tally")).toBeVisible();
    const tally = await page.evaluate(() => [getComputedStyle(document.querySelector(".tally-print")!).backgroundColor, getComputedStyle(document.querySelector(".tally")!).backgroundColor]);
    expect(tally[0]).toBe(tally[1]);
    expect(tally[0]).not.toBe("rgba(0, 0, 0, 0)");
  });

  test("reduced motion: no animation runs", async ({ page, mock }) => {
    void mock;
    await page.emulateMedia({ reducedMotion: "reduce" });
    await signInWithSlt(page);
    const running = await page.evaluate(() => document.getAnimations().filter((a) => a.playState === "running" && (a.effect?.getTiming().duration as number) > 1).length);
    expect(running).toBe(0);
  });
});

test.describe("touch screens", () => {
  test.use({ viewport: { width: 1024, height: 768 }, hasTouch: true, isMobile: true });

  test("search shows Cancel instead of keyboard hints", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    expect(await page.evaluate(() => matchMedia("(pointer: coarse)").matches), "the context emulates a touch screen").toBe(true);
    await expect(page.locator(".topbar-command kbd")).toBeHidden();
    await page.getByTestId("open-search").click();
    await expect(page.getByTestId("command-menu")).toBeVisible();
    await expect(page.getByTestId("command-esc")).toBeHidden();
    await expect(page.locator(".command-hint")).toBeHidden();
    await expect(page.getByTestId("command-cancel")).toBeVisible();
    await settle(page);
    await page.screenshot({ path: `${OUT}/04b-search-touch.png` });
    await page.getByTestId("command-cancel").click();
    await expect(page.getByTestId("command-menu")).toBeHidden();
  });
});

test("a keyboard keeps its esc hint", async ({ page, mock }) => {
  void mock;
  await signInWithSlt(page);
  await page.getByTestId("open-search").click();
  await expect(page.getByTestId("command-esc")).toBeVisible();
  await expect(page.getByTestId("command-cancel")).toBeHidden();
});
