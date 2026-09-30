import { DEVICE_IPHONE, DEVICE_MAC, DEVICE_PIXEL, DEVICE_TV, TEST_ENVIRONMENT_NAME, TEST_SECRET, UNKNOWN_SECRET } from "../mock/fixtures";
import { expect, shoot, signInWithSlt, test } from "./fixtures";
import { readFileSync } from "node:fs";

test.describe("signing in", () => {
  test("with a short-lived token, then lists devices", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await expect(page.getByTestId("sign-in")).toBeVisible();
    await shoot(page, "01-sign-in");
    await page.getByTestId("slt-input").fill("oac_saket");
    await page.getByTestId("slt-submit").click();
    const rows = page.getByTestId("device-list").getByTestId("device-row");
    await expect(rows).toHaveCount(7);
    await expect(page.getByTestId("member-id")).toHaveText("c:saket");
    const pixel = page.locator(`[data-device-id="${DEVICE_PIXEL}"]`);
    await expect(pixel).toContainText("Saket's Pixel");
    // In use: the label says what the cobalt dot says.
    await expect(pixel).toContainText("In use");
    await expect(pixel.getByTestId("in-use")).toContainText("si:chef");
    await expect(pixel).toContainText("in 14 days");
    await expect(page.locator('[data-device-id="0d44e1f2"]')).toContainText("Offline");
    await expect(page.locator('[data-device-id="51ab93c0"]')).toContainText("through MacBook Pro");
    await shoot(page, "02-devices");

    // 1.1: devices belong to the Carbon, not to a Team. No "Team devices" tab, and no other Carbon's device.
    await expect(page.getByTestId("tab-team")).toHaveCount(0);
    await expect(page.getByTestId("device-list")).not.toContainText("Alice's");
    await expect(page.getByTestId("list-label")).toContainText("Every Team · 7");
    // The Team menu doesn't change the list: the labs device shows in acme, and acme's in labs.
    await expect(page.getByTestId("device-list")).toContainText("Lab Linux box");
    await page.getByTestId("team-picker").selectOption("labs");
    await expect(rows).toHaveCount(7);
    await expect(page.getByTestId("device-list")).toContainText("Saket's Pixel");
    await expect(page.getByTestId("device-list")).toContainText("Lab Linux box");
  });

  test("shows the service's message and hint for a bad token", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await page.getByTestId("slt-input").fill("oac_nobody");
    await page.getByTestId("slt-submit").click();
    const error = page.getByTestId("error");
    await expect(error).toHaveAttribute("data-code", "slt_invalid");
    await expect(error).toContainText("IAM rejected the short-lived token");
    await expect(error).toContainText("Get a new SLT and sign in again.");
  });

  test("a member id is refused in production", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await page.getByTestId("slt-input").fill("c:alice");
    await page.getByTestId("slt-submit").click();
    await expect(page.getByTestId("error")).toContainText("works only in a test environment");
  });

  test("through the Silicon IAM consent screen", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await page.getByTestId("sign-in-iam").click();
    await expect(page).toHaveURL(/\/__mock\/iam\/login\?app_id=extend&redirect_uri=/);
    const redirect = new URL(page.url()).searchParams.get("redirect_uri")!;
    expect(new URL(redirect).pathname).toBe("/auth/callback");
    expect(new URL(redirect).searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{43}$/);
    await page.getByRole("button", { name: /Continue as Saket/ }).click();
    await expect(page.getByTestId("devices-page")).toBeVisible();
    await expect(page).toHaveURL(/\/devices$/);
    await expect(page.getByTestId("member-id")).toHaveText("c:saket");
  });

  test("New to Silicon IAM: sign-up goes to IAM's /signup beside its /login, with the same app_id and callback", async ({ page, mock }) => {
    void mock;
    // Extend names IAM's real layout (<auth origin>/login); the sign-up page is /signup beside it.
    await page.route("**/api/v1/iam", async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.data.iam_login_url = "https://auth.iam.example.test/login";
      await route.fulfill({ response: res, json: body });
    });
    let landed: URL | null = null;
    await page.route("https://auth.iam.example.test/**", async (route) => {
      landed = new URL(route.request().url());
      await route.fulfill({ status: 200, contentType: "text/html", body: "<h1>Silicon IAM sign-up</h1>" });
    });
    await page.goto("/");
    await expect(page.getByTestId("signup")).toContainText("New to Silicon IAM?");
    await expect(page.getByTestId("signup-note")).toContainText("checks your email and phone, creates your Carbon account and signs you in with a code");
    await page.getByTestId("sign-up-iam").click();
    await expect(page.getByRole("heading", { name: "Silicon IAM sign-up" })).toBeVisible();
    const url = landed as unknown as URL;
    expect(url.pathname).toBe("/signup");
    expect(url.searchParams.get("app_id")).toBe("extend");
    expect(url.searchParams.has("org_id")).toBe(false);
    const callback = new URL(url.searchParams.get("redirect_uri")!);
    expect(callback.pathname).toBe("/auth/callback");
    expect(callback.searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{43}$/);
  });

  test("New to Silicon IAM, with a login page Extend can't place: it says so and opens IAM's sign-in page", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    // The mock's consent screen lives at /__mock/iam/login, which is not IAM's layout.
    await expect(page.getByTestId("signup-note")).toContainText("gives Extend no sign-up page, so this opens its sign-in page");
    await page.getByTestId("sign-up-iam").click();
    await expect(page).toHaveURL(/\/__mock\/iam\/login\?app_id=extend&redirect_uri=/);
    await page.getByRole("button", { name: /Continue as Saket/ }).click();
    await expect(page.getByTestId("devices-page")).toBeVisible();
  });

  test("a forged callback is refused", async ({ page, mock }) => {
    void mock;
    await page.goto("/auth/callback?state=forged&slt=oac_saket");
    await expect(page.getByTestId("error")).toHaveAttribute("data-code", "invalid_login_state");
    await expect(page).toHaveURL(/\/auth\/callback$/);
  });

  test("refreshes an expired access token without the Carbon noticing", async ({ page, mock }) => {
    await mock.config({ access_ttl_s: 1 });
    await signInWithSlt(page);
    const refreshes: string[] = [];
    page.on("request", (r) => r.url().includes("/api/v1/auth/refresh") && refreshes.push(r.url()));
    await page.waitForTimeout(1200);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await expect(page.getByTestId("device-name")).toHaveText("Saket's Pixel");
    expect(refreshes.length).toBeGreaterThanOrEqual(1);
    await expect(page.getByTestId("member-id")).toHaveText("c:saket");
  });
});

test.describe("adding a device", () => {
  test("Android: code → name → setup → give a Silicon access", async ({ page, mock }) => {
    const code = await mock.enroll("android");
    await signInWithSlt(page);
    await page.getByTestId("add-device").click();
    await shoot(page, "04-wizard-kind");
    await page.getByTestId("kind-android").click();
    await expect(page.getByTestId("guide")).toBeVisible();
    await expect(page.getByTestId("download-link")).toHaveAttribute("href", "/download/android");
    await shoot(page, "05-wizard-guide");
    await page.getByTestId("wizard-next").click();

    // Lowercase with a dash, as people type it.
    const typed = `${code.slice(0, 3).toLowerCase()}-${code.slice(3).toLowerCase()}`;
    await page.getByTestId("pairing-code-input").fill(typed);
    await expect(page.getByTestId("pairing-code-input")).toHaveValue(typed.toUpperCase());
    await expect(page.getByTestId("code-help")).toContainText(`${code.slice(0, 3)} ${code.slice(3)}`);
    await shoot(page, "06-wizard-code");
    await page.getByTestId("wizard-next").click();

    await expect(page.getByTestId("name-step")).toBeVisible();
    await page.getByTestId("device-name-input").fill("Test Pixel");
    await shoot(page, "07-wizard-name");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("banner-step")).toBeVisible();
    await page.getByTestId("banner-next").click();

    await expect(page.getByTestId("setup-steps")).toBeVisible();
    await expect(page.getByTestId("setup-step").first()).toBeVisible();
    await expect(page.getByTestId("setup-complete")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("setup-step")).toHaveCount(4);
    await expect(page.locator('[data-testid="setup-step"][data-status="done"]')).toHaveCount(4);
    await shoot(page, "08-wizard-setup");
    await page.getByTestId("setup-next").click();

    await expect(page.getByTestId("access-step")).toBeVisible();
    // The team's Silicons come from GET /api/v1/team/silicons.
    const roster = page.locator('[data-roster="team"]');
    await expect(roster).toContainText("Silicons in acme");
    await expect(roster.getByTestId("grant-suggestion")).toHaveCount(5);
    await roster.getByTestId("grant-suggestion").filter({ hasText: "si:chef" }).click();
    await expect(page.getByTestId("grant-input")).toHaveValue("si:chef");
    await page.getByTestId("grant-submit").click();
    await expect(page.getByTestId("wizard-done")).toBeVisible();
    await expect(page.getByTestId("wizard-done")).toContainText("si:chef can use it now");
    await shoot(page, "09-wizard-done");
    await page.getByTestId("open-device").click();
    await expect(page.getByTestId("device-name")).toHaveText("Test Pixel");
    await expect(page.locator('[data-testid="grant"][data-silicon="si:chef"]')).toBeVisible();
  });

  test("a wrong code sends the Carbon back to the code step with the reason", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto("/devices/new?kind=android");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill("ABCDEF");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Nope");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("code-step")).toBeVisible();
    await expect(page.getByTestId("code-error")).toContainText("That pairing code is wrong, expired or already used.");
    await expect(page.getByTestId("code-error")).toContainText("Codes rotate every 5 minutes");
  });

  test("a Silicon id outside the team is refused with the reason, and the step can be skipped", async ({ page, mock }) => {
    const code = await mock.enroll("windows");
    await signInWithSlt(page);
    await page.goto("/devices/new?kind=windows");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill(code);
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Desk PC");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("banner-step")).toBeVisible();
    await page.getByTestId("banner-next").click();
    await page.getByTestId("setup-next").click();
    await page.getByTestId("grant-input").fill("si:juniper c:alice");
    await page.getByTestId("grant-submit").click();
    await expect(page.getByTestId("access-picker").getByTestId("error")).toContainText("c:alice is not a Silicon id");
    await page.getByTestId("grant-input").fill("si:juniper");
    await page.getByTestId("grant-submit").click();
    await expect(page.getByTestId("access-picker").getByTestId("error")).toContainText("si:juniper is not an active Silicon in team acme");
    await page.getByTestId("skip-access").click();
    await expect(page.getByTestId("wizard-done")).toContainText("No Silicon can use it yet");
  });

  test("Apple TV through the Mac, with the 4-digit code", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto("/devices/new");
    await page.getByTestId("kind-apple_tv").click();
    await page.getByTestId("wizard-next").click();
    await expect(page.getByTestId("host-step")).toBeVisible();
    // Every Mac Saket paired, whichever Team is selected: his MacBook and his pair of the Studio Mac.
    await expect(page.getByTestId("host-option")).toHaveCount(2);
    await shoot(page, "10-wizard-host");
    await page.getByTestId("host-option").filter({ hasText: "MacBook Pro" }).click();
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Bedroom Apple TV");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("banner-step")).toBeVisible();
    await page.getByTestId("banner-next").click();
    await expect(page.getByTestId("setup-code-input")).toBeVisible({ timeout: 10_000 });
    await shoot(page, "11-wizard-apple-tv-code");
    await page.getByTestId("setup-code-input").fill("4821");
    await page.getByTestId("setup-code-submit").click();
    await expect(page.getByTestId("setup-complete")).toBeVisible({ timeout: 10_000 });
    await page.getByTestId("setup-next").click();
    await page.getByTestId("skip-access").click();
    await page.getByTestId("open-device").click();
    await expect(page.getByTestId("device-page")).toContainText(`through ${DEVICE_MAC}`);
  });
});

test.describe("a device's page", () => {
  test("rename, pair lifetime, access, stop, activity and requests", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.locator(`[data-device-id="${DEVICE_PIXEL}"]`).click();
    await expect(page.getByTestId("device-name")).toHaveText("Saket's Pixel");
    await expect(page.getByTestId("in-use-silicon")).toHaveText("si:chef");
    await expect(page.getByTestId("activity-item")).toHaveCount(20);
    await expect(page.getByTestId("request")).toHaveCount(2);
    await expect(page.getByTestId("requests")).toContainText("I need to check the order confirmation in the Swiggy app, 2 minutes");
    await shoot(page, "12-device-page");

    // Rename (PATCH with If-Match).
    const ifMatch: string[] = [];
    page.on("request", (r) => r.method() === "PATCH" && ifMatch.push(r.headers()["if-match"]));
    await page.getByTestId("rename").click();
    await page.getByTestId("rename-input").fill("Pixel 9");
    await page.getByTestId("rename-save").click();
    await expect(page.getByTestId("device-name")).toHaveText("Pixel 9");

    // Pair lifetime.
    await page.getByTestId("ttl-slider").fill("7");
    await expect(page.getByTestId("ttl-value")).toHaveText("7 days");
    await page.getByTestId("ttl-save").click();
    await expect(page.getByTestId("toast").last()).toContainText("7 days without activity");
    await expect(page.getByTestId("pair-expires")).toContainText("in 7 days");
    expect(ifMatch).toEqual(['"3"', '"4"']);

    // No visibility: since 1.1 a device is only ever visible to the Carbons who paired it.
    await expect(page.getByTestId("visibility-personal")).toHaveCount(0);
    await expect(page.getByTestId("visibility-team")).toHaveCount(0);
    await expect(page.getByTestId("device-eyebrow")).toHaveText("Phone · Only you see it");

    // The activity log reads those changes back in plain words.
    const summaries = page.getByTestId("activity-summary");
    await expect(summaries.filter({ hasText: "Renamed from \u201cSaket's Pixel\u201d to \u201cPixel 9\u201d" })).toHaveCount(1);
    await expect(summaries.filter({ hasText: "Stays paired 7 days without activity" })).toHaveCount(1);

    // Take access away from a Silicon that isn't using it.
    await page.locator('[data-testid="grant"][data-silicon="si:scout"]').getByTestId("revoke").click();
    await expect(page.locator('[data-testid="grant"][data-silicon="si:scout"]')).toHaveCount(0);

    // Stop the Silicon using it.
    await page.getByTestId("stop-session").click();
    await expect(page.getByTestId("in-use-card")).toContainText("No Silicon is using");
    await expect(page.getByTestId("toast").last()).toContainText("Stopped si:chef");

    // Activity filters and paging.
    await page.getByTestId("filter-silicon").fill("si:scout");
    await page.getByTestId("filter-apply").click();
    await expect(page.getByTestId("activity-item").first()).toContainText("si:scout");
    await page.getByTestId("filter-silicon").fill("");
    await page.getByTestId("filter-apply").click();
    await expect(page.getByTestId("activity-item")).toHaveCount(20);
    await page.getByTestId("activity-more").click();
    await expect(page.getByTestId("activity-item")).toHaveCount(40);
    await expect(page.getByTestId("activity").getByText("Stopped", { exact: false }).first()).toBeVisible();
  });

  test("a Silicon hands the device over: the reason shows, and Done gives it back", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await expect(page.getByTestId("in-use-silicon")).toHaveText("si:chef");
    // si:chef, in session a3f, asks its Carbon to approve something on the phone.
    const login = await request.post("/api/v1/auth/login", { headers: { "Idempotency-Key": "chef-login-1" }, data: { type: "login", data: { slt: "oac_si_chef" } } });
    const chef = (await login.json()).data.access_token;
    const res = await request.post("/api/v1/sessions/a3f/takeover", {
      headers: { Authorization: `Bearer ${chef}`, "X-Org-ID": "acme" },
      data: { type: "takeover", data: { reason: "Please approve the Face ID prompt for the payment" } },
    });
    expect(res.status()).toBe(201);
    await expect(page.getByTestId("takeover-reason")).toHaveText("Please approve the Face ID prompt for the payment", { timeout: 12_000 });
    await expect(page.getByTestId("in-use-card")).toContainText("handed the device to you");
    // The header and the list row say the same as the card, at every width.
    await expect(page.locator(".device-header").getByTestId("device-status")).toHaveText("Paused for you");
    await expect(page.locator(`[data-device-id="${DEVICE_PIXEL}"]`).getByTestId("device-status")).toHaveText("Paused for you");
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.locator(".device-header").getByTestId("device-status")).toHaveText("Paused for you");
    await page.setViewportSize({ width: 1280, height: 900 });
    await shoot(page, "22-takeover");
    await page.getByTestId("takeover-done").click();
    await expect(page.getByTestId("takeover")).toHaveCount(0);
    await expect(page.getByTestId("in-use-card")).toContainText("is using it now");
    await expect(page.getByTestId("activity-summary").filter({ hasText: "Handed the device to you" })).toHaveCount(1);
  });

  test("taking access from the Silicon using it asks first and ends its session", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await page.locator('[data-testid="grant"][data-silicon="si:chef"]').getByTestId("revoke").click();
    await expect(page.getByTestId("revoke-confirm")).toBeVisible();
    await page.getByTestId("revoke-confirm-button").click();
    await expect(page.locator('[data-testid="grant"][data-silicon="si:chef"]')).toHaveCount(0);
    await expect(page.getByTestId("in-use-card")).toContainText("No Silicon is using");
  });

  test("grant access from the device page", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_MAC}`);
    await page.getByText("Give another Silicon access").click();
    // si:atlas already has access in acme, so chef, scout, sous and pilot are offered.
    await expect(page.locator('[data-roster="team"]').getByTestId("grant-suggestion")).toHaveCount(4);
    await page.getByTestId("grant-suggestion").filter({ hasText: "si:scout" }).click();
    await page.getByTestId("grant-submit").click();
    await expect(page.locator('[data-testid="grant"][data-silicon="si:scout"]')).toBeVisible();
  });

  test("a stale version shows the service's conflict and reloads", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_MAC}`);
    await expect(page.getByTestId("device-name")).toHaveText("MacBook Pro");
    // Someone else changes the device in another tab.
    const token = await page.evaluate(() => JSON.parse(localStorage.getItem("extend.auth.production")!).access_token);
    const res = await request.patch(`/api/v1/devices/${DEVICE_MAC}`, {
      headers: { Authorization: `Bearer ${token}`, "X-Org-ID": "acme", "If-Match": '"2"', "Content-Type": "application/json" },
      data: { type: "device", data: { name: "MacBook (renamed elsewhere)" } },
    });
    expect(res.status()).toBe(200);
    await page.getByTestId("ttl-slider").fill("3");
    await page.getByTestId("ttl-save").click();
    await expect(page.getByTestId("settings-error")).toHaveAttribute("data-code", "version_conflict");
    await expect(page.getByTestId("settings-error")).toContainText("changed since you read it");
    await expect(page.getByTestId("device-name")).toHaveText("MacBook (renamed elsewhere)");
  });

  test("remove a device only after typing its name", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto("/devices/0d44e1f2");
    await page.getByTestId("remove-device").click();
    const dialog = page.getByTestId("remove-dialog");
    await expect(dialog).toBeVisible();
    // What removal does to this offline TV with one Silicon, in words that agree with the count.
    await expect(page.getByTestId("remove-access")).toHaveText("1 Silicon loses access.");
    await expect(page.getByTestId("remove-unpair")).toHaveText("It's offline, so the Extend app on it unpairs the next time it connects, then shows a new pairing code.");
    await expect(page.getByTestId("remove-consequences")).toContainText("Its activity log stays readable: find it under Removed in your device list.");
    await expect(page.getByTestId("remove-confirm")).toBeDisabled();
    await page.getByTestId("remove-confirm-input").fill("Living room");
    await expect(page.getByTestId("remove-confirm")).toBeDisabled();
    await page.getByTestId("remove-confirm-input").fill("Living room TV");
    await shoot(page, "13-remove-dialog");
    await page.getByTestId("remove-confirm").click();
    await expect(page).toHaveURL(/\/devices$/);
    await expect(page.getByTestId("device-row")).toHaveCount(6);
    await expect(page.getByTestId("device-list")).not.toContainText("Living room TV");
    await expect(page.getByTestId("toast").last()).toContainText("Its activity log is under Removed");

    // Where the dialog said: under Removed, newest first, with the log readable.
    await page.getByTestId("tab-removed").click();
    const removed = page.getByTestId("removed-device-list").getByTestId("device-row");
    await expect(removed).toHaveCount(3);
    await expect(removed.first()).toContainText("Living room TV");
    await expect(removed.first()).toContainText("You removed it");
    await removed.first().click();
    await expect(page).toHaveURL(new RegExp(`/devices/${DEVICE_TV}$`));
    await expect(page.getByTestId("removed-card")).toContainText("You removed it.");
    await expect(page.getByTestId("activity-summary").filter({ hasText: "Removed the device" })).toHaveCount(1);
    await expect(page.getByTestId("activity-summary").filter({ hasText: "Session ended" })).toHaveCount(0);
  });

  test("the Removed tab: removed devices and their logs, read-only", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    await page.getByTestId("tab-removed").click();
    const rows = page.getByTestId("removed-device-list").getByTestId("device-row");
    await expect(rows).toHaveCount(2);
    await expect(page.getByTestId("removed-overview")).toContainText("A removed device can't be changed or used");
    // Newest removal first: the tablet (3 days ago), then the iMac whose pair ran out (9 days ago).
    await expect(rows.nth(0)).toContainText("Old Galaxy Tab");
    await expect(rows.nth(1)).toContainText("Office iMac");
    await expect(rows.nth(1)).toContainText("It went unused for longer than its pairing lasts (7 days)");
    await shoot(page, "23-removed-list");

    await rows.nth(0).click();
    await expect(page.getByTestId("device-name")).toHaveText("Old Galaxy Tab");
    await expect(page.getByTestId("removed-badge")).toBeVisible();
    await expect(page.getByTestId("removed-why")).toContainText("You removed it.");
    await expect(page.getByTestId("activity-item")).toHaveCount(6);
    await expect(page.getByTestId("activity-summary").first()).toHaveText("Removed the device");
    // Nothing that changes a device is offered on a removed one.
    for (const id of ["rename", "stop-session", "access-card", "settings-card", "danger-zone", "capabilities"]) await expect(page.getByTestId(id)).toHaveCount(0);
    await expect(page.getByTestId("pair-again")).toHaveAttribute("href", "/devices/new?kind=android");
    await shoot(page, "24-removed-device");

    // Extend's own entry (the pair ran out) is not shown as a Carbon's.
    await page.goto("/devices/f00d5eed");
    await expect(page.getByTestId("removed-why")).toContainText("It went unused for longer than its pairing lasts (7 days).");
    await expect(page.getByTestId("actor-extend")).toHaveText("Silicon Extend");

    // The mock refuses changes the way the service does: its Carbon hears when and why.
    const token = await page.evaluate(() => JSON.parse(localStorage.getItem("extend.auth.production")!).access_token);
    const res = await request.patch("/api/v1/devices/e1d0a7c3", {
      headers: { Authorization: `Bearer ${token}`, "X-Org-ID": "acme", "If-Match": '"4"', "Content-Type": "application/json" },
      data: { type: "device", data: { name: "Back again" } },
    });
    expect(res.status()).toBe(404);
    const body = await res.json();
    expect(body.data.code).toBe("device_not_found");
    expect(body.data.message).toMatch(/^Device e1d0a7c3 \(Old Galaxy Tab\) was removed at .+: its Carbon removed it\. A removed device can't be changed or used\.$/);
    expect(body.data.details.removed_reason).toBe("device_removed");
    // Only with scope=mine.
    const team = await request.get("/api/v1/devices?scope=team&include_removed=true", { headers: { Authorization: `Bearer ${token}`, "X-Org-ID": "acme" } });
    expect(team.status()).toBe(422);
  });

  test("removing a Mac also removes the devices paired through it", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_MAC}`);
    await page.getByTestId("remove-device").click();
    await expect(page.getByTestId("remove-children")).toHaveText("Also removed, because they pair through it: Saket's iPhone.");
    await expect(page.getByTestId("remove-unpair")).toHaveText("The Extend app on it unpairs now and shows a new pairing code.");
    await page.getByTestId("remove-confirm-input").fill("MacBook Pro");
    await page.getByTestId("remove-confirm").click();
    await expect(page.getByTestId("device-row")).toHaveCount(5);
    // The iPhone went with its Mac, for the same reason.
    await page.goto(`/devices/${DEVICE_IPHONE}`);
    await expect(page.getByTestId("removed-why")).toContainText("You removed it, or the computer it paired through.");
  });

  test("the Remove dialog for a device paired through a computer, with two Silicons", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_IPHONE}`);
    await page.getByTestId("remove-device").click();
    await expect(page.getByTestId("remove-unpair")).toHaveText("Extend stops reaching it through MacBook Pro.");
    await expect(page.getByTestId("remove-access")).toHaveCount(0);
    await page.getByRole("button", { name: "Cancel" }).click();
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await page.getByTestId("remove-device").click();
    await expect(page.getByTestId("remove-access")).toHaveText("4 Silicons lose access.");
    await expect(page.getByTestId("remove-consequences")).toContainText("si:chef is using it now; its session ends immediately.");
    await shoot(page, "13b-remove-dialog-pixel");
  });
});

test.describe("test environments", () => {
  test("enter from sign-in, sign in as a test member, hit the device limit, exit to production", async ({ page, mock }) => {
    // Production login first, so exiting has somewhere to return to.
    await signInWithSlt(page);
    await page.getByTestId("sign-out").click();
    await page.getByTestId("sign-out-confirm").click();
    await expect(page.getByTestId("sign-in")).toBeVisible();
    await page.getByTestId("slt-input").fill("oac_saket");
    await page.getByTestId("slt-submit").click();
    await expect(page.getByTestId("devices-page")).toBeVisible();

    await page.goto("/settings");
    await page.getByTestId("testing-secret-input").fill(UNKNOWN_SECRET);
    await page.getByTestId("testing-secret-submit").click();
    await expect(page.getByTestId("testing-form").getByTestId("error")).toHaveAttribute("data-code", "testing_secret_invalid");
    await expect(page.getByTestId("testing-banner")).toHaveCount(0);

    await page.getByTestId("testing-secret-input").fill(TEST_SECRET);
    await page.getByTestId("testing-secret-submit").click();
    const banner = page.getByTestId("testing-banner");
    await expect(banner).toBeVisible();
    await expect(banner).toContainText(`Test environment: ${TEST_ENVIRONMENT_NAME}`);
    await expect(banner).toContainText("not signed in");
    await expect(page.getByTestId("testing-sign-in-note")).toBeVisible();
    await shoot(page, "14-testing-sign-in");

    await page.getByTestId("slt-input").fill("c:alice");
    await page.getByTestId("slt-submit").click();
    await expect(banner).toContainText("signed in as c:alice");
    await expect(page.getByTestId("devices-page")).toContainText("No devices paired yet");

    // Every API call now carries the test secret.
    const headers: (string | undefined)[] = [];
    page.on("request", (r) => r.url().includes("/api/v1/") && headers.push(r.headers()["x-testing-application-secret"]));

    // Pair 5 devices through the API, then the website shows the exact limit message for the 6th.
    const codes = [];
    for (let i = 0; i < 6; i++) codes.push(await mock.enroll("android"));
    await page.evaluate(
      async ({ codes, secret }) => {
        const envKey = Object.keys(sessionStorage).find((k) => k.startsWith("extend.auth.testing."))!;
        const token = JSON.parse(sessionStorage.getItem(envKey)!).access_token;
        for (const [i, code] of codes.entries()) {
          const res = await fetch("/api/v1/pairings", {
            method: "POST",
            headers: { Authorization: `Bearer ${token}`, "X-Org-ID": "acme", "X-Testing-Application-Secret": secret, "Idempotency-Key": `seed-device-${i}`, "Content-Type": "application/json" },
            body: JSON.stringify({ type: "pairing", data: { pairing_code: code, name: `Test phone ${i + 1}` } }),
          });
          if (res.status !== 201) throw new Error(`pairing ${i} answered ${res.status}`);
        }
      },
      { codes: codes.slice(0, 5), secret: TEST_SECRET },
    );
    await page.goto("/devices");
    await expect(page.getByTestId("device-row")).toHaveCount(5);
    await shoot(page, "15-testing-devices");

    await page.goto("/devices/new?kind=android");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill(codes[5]);
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("One too many");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("pairing-error")).toHaveAttribute("data-code", "test_device_limit");
    await expect(page.getByTestId("pairing-error")).toContainText("In test environment you are limited to 5 paired devices per environment.");
    await shoot(page, "16-testing-device-limit");
    expect(headers.length).toBeGreaterThan(0);
    expect(headers.every((h) => h === TEST_SECRET)).toBe(true);

    // Exit: back to the production session, untouched.
    await page.getByTestId("exit-testing").click();
    await expect(page.getByTestId("testing-banner")).toHaveCount(0);
    await expect(page.getByTestId("devices-page")).toBeVisible();
    await expect(page.getByTestId("member-id")).toHaveText("c:saket");
    await expect(page.getByTestId("device-row")).toHaveCount(7);
  });

  test("exiting with no production login asks to sign in", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await page.getByTestId("use-test-environment").click();
    await page.getByTestId("testing-secret-input").fill(TEST_SECRET);
    await page.getByTestId("testing-secret-submit").click();
    await expect(page.getByTestId("testing-banner")).toBeVisible();
    await page.getByTestId("slt-input").fill("si:chef");
    await page.getByTestId("slt-submit").click();
    await expect(page.getByTestId("testing-banner")).toContainText("signed in as si:chef");
    await expect(page.getByTestId("devices-page")).toContainText("You are signed in as a Silicon");
    await page.getByTestId("exit-testing").click();
    await expect(page.getByTestId("sign-in")).toBeVisible();
    await expect(page.getByTestId("testing-banner")).toHaveCount(0);
  });

  test("an unknown test member is refused", async ({ page, mock }) => {
    void mock;
    await page.goto("/");
    await page.getByTestId("use-test-environment").click();
    await page.getByTestId("testing-secret-input").fill(TEST_SECRET);
    await page.getByTestId("testing-secret-submit").click();
    await page.getByTestId("slt-input").fill("c:nobody");
    await page.getByTestId("slt-submit").click();
    await expect(page.getByTestId("error")).toContainText("c:nobody is not an active member of test environment");
  });
});

test.describe("settings and docs", () => {
  test("telemetry off is sent on every request", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto("/settings");
    await page.getByTestId("telemetry-toggle").uncheck();
    const seen: (string | undefined)[] = [];
    page.on("request", (r) => r.url().includes("/api/v1/") && seen.push(r.headers()["x-extend-telemetry"]));
    await page.goto("/devices");
    await expect(page.getByTestId("device-row")).toHaveCount(7);
    expect(seen.length).toBeGreaterThan(0);
    expect(seen.every((h) => h === "off")).toBe(true);
    await page.goto("/settings");
    await shoot(page, "17-settings");
  });

  test("docs: instructive first, then the generated reference", async ({ page, mock }) => {
    void mock;
    await page.goto("/docs");
    await expect(page.getByTestId("docs-page")).toContainText("honeycomb install 'extend'");
    await expect(page.getByTestId("docs-page")).toContainText("extend login <slt>");
    await expect(page.getByTestId("docs-page")).toContainText("Ask your Silicon to use it");
    await shoot(page, "18-docs");
    // The reference is generated from cli.yaml, whose fields come as strings, lists or maps: every
    // command must render, and the page must not throw on any of them.
    const pageErrors: string[] = [];
    page.on("pageerror", (e) => pageErrors.push(e.message));
    await page.getByRole("link", { name: "CLI reference" }).first().click();
    await expect(page.getByTestId("cli-command").first()).toBeVisible();
    // Playwright runs from web/, where `pnpm test:e2e` has just regenerated this file.
    const docs = JSON.parse(readFileSync("src/generated/docs.json", "utf8")) as {
      cli: { groups: { commands: { usage: string; errors: { codes: string[]; note: string | null } | null }[] }[] };
    };
    const generated = docs.cli.groups.flatMap((g) => g.commands);
    await expect(page.getByTestId("cli-command")).toHaveCount(generated.length);
    expect(generated.length).toBeGreaterThan(70);
    for (const c of generated.filter((x) => x.errors?.note).slice(0, 3))
      await expect(page.getByTestId("cli-command").filter({ hasText: c.usage }).getByTestId("cli-command-errors")).toContainText(c.errors!.note!.split("`")[0]);
    await expect(page.getByTestId("docs-page")).toContainText("test_device_limit");
    expect(pageErrors).toEqual([]);
    await shoot(page, "19-docs-cli");
    await page.getByRole("link", { name: "How Extend works" }).first().click();
    await expect(page.getByTestId("docs-page")).toContainText("Identifiers and values");
    await page.goto("/docs/devices");
    await expect(page.getByTestId("docs-page")).toContainText("Apple TV");
    for (const platform of ["android", "android-tv"]) {
      await page.goto(`/download/${platform}`);
      await expect(page.getByTestId("download-version")).toHaveText("Download · version 1.1.2");
      await expect(page.getByTestId("download-file")).toHaveAttribute(
        "href",
        "https://github.com/teamofsilicons/silicon-extend/releases/download/android-v1.1.2/Silicon-Extend-Android-1.1.2.apk",
      );
      await expect(page.getByRole("link", { name: "release notes" })).toHaveAttribute(
        "href", "https://github.com/teamofsilicons/silicon-extend/releases/tag/android-v1.1.2",
      );
      await expect(page.getByRole("link", { name: "SHA-256 checksum" })).toHaveAttribute(
        "href", "https://github.com/teamofsilicons/silicon-extend/releases/download/android-v1.1.2/SHA256SUMS",
      );
    }
    await page.goto("/download/linux");
    await expect(page.getByTestId("download-version")).toHaveText("Download · version 1.1.0");
    await expect(page.getByTestId("download-file")).toHaveCount(4);
    await expect(page.getByTestId("download-file").first()).toHaveAttribute(
      "href", "https://github.com/teamofsilicons/silicon-extend/releases/latest/download/silicon-extend_amd64.deb",
    );
    await expect(page.getByRole("link", { name: "SHA-256 checksum" })).toHaveAttribute(
      "href", "https://github.com/teamofsilicons/silicon-extend/releases/latest/download/SHA256SUMS",
    );
  });
});

test("dark theme follows the system", async ({ page, mock }) => {
  void mock;
  await page.emulateMedia({ colorScheme: "dark" });
  await signInWithSlt(page);
  const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(background).toBe("rgb(20, 22, 21)");
  await shoot(page, "20-dark-devices");
  await page.goto(`/devices/${DEVICE_PIXEL}`);
  await expect(page.getByTestId("in-use-silicon")).toBeVisible();
  await shoot(page, "21-dark-device-page");
});
