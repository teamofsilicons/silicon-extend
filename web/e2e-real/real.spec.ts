import { expect, test, type Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { createTestEnvironment, FakeDevice, login, pairViaApi, startSession, takeover } from "./service";

const SHOTS = "test-results/screenshots-real";
mkdirSync(SHOTS, { recursive: true });

async function signIn(page: Page, slt = "c:alice") {
  await page.goto("/");
  await page.getByTestId("slt-input").fill(slt);
  await page.getByTestId("slt-submit").click();
  await expect(page.getByTestId("devices-page")).toBeVisible();
  await expect(page.getByTestId("member-id")).toHaveText(slt);
}

test("sign in, pair a device with live setup, manage it, stop a Silicon, remove it", async ({ page }) => {
  const device = await FakeDevice.enroll("android");
  await signIn(page);
  await page.getByTestId("add-device").click();
  await page.getByTestId("kind-android").click();
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("pairing-code-input").fill(`${device.code.slice(0, 3).toLowerCase()}-${device.code.slice(3).toLowerCase()}`);
  await page.getByTestId("wizard-next").click();
  const name = `Real Pixel ${Date.now().toString(36)}`;
  await page.getByTestId("device-name-input").fill(name);
  await page.getByTestId("pair-submit").click();
  await expect(page.getByTestId("setup-waiting")).toBeVisible();

  // The app learns it was paired and reports its setup over the device socket.
  await device.waitPaired();
  await device.connect(false);
  await expect(page.locator('[data-testid="setup-step"][data-status="needs_carbon"]')).toHaveCount(1, { timeout: 15_000 });
  await page.screenshot({ path: `${SHOTS}/setup-needs-carbon.png`, fullPage: true });
  device.hello(true);
  await expect(page.getByTestId("setup-complete")).toBeVisible({ timeout: 15_000 });
  await page.getByTestId("setup-next").click();
  const roster = page.locator('[data-roster="team"]');
  await expect(roster).toContainText("si:chef");
  await expect(roster).toContainText("si:sous");
  await roster.getByTestId("grant-suggestion").filter({ hasText: "si:chef" }).click();
  await page.getByTestId("grant-submit").click();
  await expect(page.getByTestId("wizard-done")).toContainText("si:chef can use it now");
  await page.getByTestId("open-device").click();
  await expect(page.getByTestId("device-name")).toHaveText(name);

  // Rename, pair lifetime and visibility (PATCH with If-Match).
  await page.getByTestId("rename").click();
  await page.getByTestId("rename-input").fill(`${name} renamed`);
  await page.getByTestId("rename-save").click();
  await expect(page.getByTestId("device-name")).toHaveText(`${name} renamed`);
  await page.getByTestId("ttl-slider").fill("9");
  await page.getByTestId("ttl-save").click();
  await expect(page.getByTestId("pair-expires")).toContainText("in 9 days");
  await page.getByTestId("visibility-personal").check();
  await expect(page.getByTestId("toast").last()).toContainText("Visible only to you");
  const summaries = page.getByTestId("activity-summary");
  await expect(summaries.filter({ hasText: `Renamed from \u201c${name}\u201d to \u201c${name} renamed\u201d` })).toHaveCount(1);
  await expect(summaries.filter({ hasText: "Stays paired 9 days without activity" })).toHaveCount(1);
  await expect(summaries.filter({ hasText: "Visible only to its owner" })).toHaveCount(1);

  // A Silicon starts using it; the Carbon stops it from the website.
  const chef = await login("si:chef");
  const session = await startSession(chef, device.deviceId);
  await expect(page.getByTestId("in-use-silicon")).toHaveText("si:chef", { timeout: 12_000 });
  await expect(page.getByTestId("in-use-card")).toContainText(session.session_id);
  await page.screenshot({ path: `${SHOTS}/device-in-use.png`, fullPage: true });

  // The Silicon hands the device to its Carbon; the website shows why and gives it back with Done.
  await takeover(chef, session.session_id, "Please approve the Face ID prompt");
  await expect(page.getByTestId("takeover-reason")).toHaveText("Please approve the Face ID prompt", { timeout: 12_000 });
  await page.screenshot({ path: `${SHOTS}/takeover.png`, fullPage: true });
  await page.getByTestId("takeover-done").click();
  await expect(page.getByTestId("takeover")).toHaveCount(0, { timeout: 12_000 });
  await expect(page.getByTestId("in-use-card")).toContainText("is using it now");

  await page.getByTestId("stop-session").click();
  await expect(page.getByTestId("in-use-card")).toContainText("No Silicon is using", { timeout: 12_000 });

  // Take access away.
  await page.locator('[data-testid="grant"][data-silicon="si:chef"]').getByTestId("revoke").click();
  await expect(page.locator('[data-testid="grant"][data-silicon="si:chef"]')).toHaveCount(0);
  await expect(page.getByTestId("activity-item").first()).toBeVisible();
  await page.screenshot({ path: `${SHOTS}/device-page.png`, fullPage: true });

  // Remove with typed confirmation.
  await page.getByTestId("remove-device").click();
  await page.getByTestId("remove-confirm-input").fill(`${name} renamed`);
  await page.getByTestId("remove-confirm").click();
  await expect(page).toHaveURL(/\/devices$/);
  await expect(page.getByTestId("devices-page")).not.toContainText(`${name} renamed`);
  device.close();
});

test("a wrong pairing code shows the service's message", async ({ page }) => {
  await signIn(page);
  await page.goto("/devices/new?kind=android");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("pairing-code-input").fill("0A0A0A");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("device-name-input").fill("Nope");
  await page.getByTestId("pair-submit").click();
  await expect(page.getByTestId("code-step")).toBeVisible();
  const text = await page.getByTestId("code-error").innerText();
  expect(text.length).toBeGreaterThan(20);
  console.log(`service said: ${text.replace(/\s+/g, " ")}`);
});

test("IAM consent round trip through the local stand-in", async ({ page }) => {
  // iam_login_url from GET /api/v1/iam, with app_id and redirect_uri, exactly as real IAM takes them.
  await page.goto("/");
  await page.getByTestId("sign-in-iam").click();
  await expect(page).toHaveURL(/\/dev\/iam\/login\?app_id=bridge&redirect_uri=/);
  const redirect = new URL(new URL(page.url()).searchParams.get("redirect_uri")!);
  expect(redirect.pathname).toBe("/auth/callback");
  expect(redirect.searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{43}$/);
  await page.screenshot({ path: `${SHOTS}/iam-stand-in.png` });
  const input = page.locator("input:not([type=hidden])").first();
  await input.fill("c:bob");
  await page.getByRole("button").first().click();
  await expect(page.getByTestId("devices-page")).toBeVisible();
  await expect(page.getByTestId("member-id")).toHaveText("c:bob");
});

test("test environment: banner, test member login, device limit, exit back to production", async ({ page }) => {
  const { secret } = await createTestEnvironment(`web-e2e-${Date.now().toString(36)}`);
  await signIn(page, "c:alice");
  await page.goto("/settings");
  await page.getByTestId("testing-secret-input").fill("ask_" + "z".repeat(43));
  await page.getByTestId("testing-secret-submit").click();
  const bad = page.getByTestId("testing-form").getByTestId("error");
  await expect(bad).toBeVisible();
  console.log(`unknown secret: ${(await bad.innerText()).replace(/\s+/g, " ")}`);

  await page.getByTestId("testing-secret-input").fill(secret);
  await page.getByTestId("testing-secret-submit").click();
  const banner = page.getByTestId("testing-banner");
  await expect(banner).toContainText("Test environment: web-e2e-");
  await expect(banner).toContainText("not signed in");
  await page.getByTestId("slt-input").fill("c:alice");
  await page.getByTestId("slt-submit").click();
  await expect(banner).toContainText("signed in as c:alice");
  await expect(page.getByTestId("devices-page")).toContainText("No devices paired yet");

  const token = await login("c:alice", secret);
  for (let i = 0; i < 5; i++) {
    const d = await FakeDevice.enroll("android");
    await pairViaApi(token, d.code, `Test phone ${i + 1}`, secret);
  }
  await page.goto("/devices");
  await expect(page.getByTestId("device-row")).toHaveCount(5);
  const sixth = await FakeDevice.enroll("android");
  await page.goto("/devices/new?kind=android");
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("pairing-code-input").fill(sixth.code);
  await page.getByTestId("wizard-next").click();
  await page.getByTestId("device-name-input").fill("One too many");
  await page.getByTestId("pair-submit").click();
  await expect(page.getByTestId("pairing-error")).toContainText("In test environment you are limited to 5 paired devices per environment.");
  await page.screenshot({ path: `${SHOTS}/test-device-limit.png`, fullPage: true });

  await page.getByTestId("exit-testing").click();
  await expect(banner).toHaveCount(0);
  await expect(page.getByTestId("member-id")).toHaveText("c:alice");
  await expect(page.getByTestId("devices-page")).not.toContainText("Test phone 1");
});
