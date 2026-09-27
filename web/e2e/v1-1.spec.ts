/**
 * Silicon Extend 1.1 on the website, against the mock: devices belong to the Carbon (not a Team),
 * access per Team, several Carbons on one device with separate sides, requests routed to the Carbon
 * who gave the holder access, waking, Ting app types and recipient Teams, setup retry (contract A), signing out,
 * and a device in the 1.0 shape.
 */
import { DEVICE_FAMILY_TV, DEVICE_IPHONE, DEVICE_MAC, DEVICE_PIXEL, DEVICE_STUDIO_MAC, SLT_CHEF } from "../mock/fixtures";
import { expect, shoot, signInWithSlt, test } from "./fixtures";

test.describe("several Carbons on one device", () => {
  test("a family TV: my own side in use, the other Carbon's Silicon's request shows who asked and why", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    const row = page.locator(`[data-device-id="${DEVICE_FAMILY_TV}"]`);
    await expect(row.getByTestId("row-shared")).toHaveText("Shared");
    await expect(row.getByTestId("in-use")).toContainText("si:scout");
    await expect(row.getByTestId("in-use")).toContainText("acme");
    await row.click();
    await expect(page.getByTestId("device-eyebrow")).toHaveText("TV · Also paired by another Carbon");
    await expect(page.getByTestId("shared-note")).toContainText("Your pair is separate");
    await expect(page.getByTestId("shared-device-warning")).toHaveText("Silicons any Carbon gives access to can use this whole device, including what others leave on it.");
    await expect(page.getByTestId("in-use-silicon")).toHaveText("si:scout");
    await expect(page.getByTestId("in-use-team")).toHaveText("acme");
    // Carbon decision 2: the Carbon a request is routed to sees which Silicon asked, and why.
    const received = page.getByTestId("requests-received");
    await expect(received.getByTestId("request-from")).toHaveText("si:sous");
    await expect(received).toContainText("asked you for Family TV");
    await expect(received).toContainText("The match starts in 5 minutes; can I have the TV?");
    await expect(page.getByTestId("request-from-hidden")).toHaveCount(0);
    await expect(page.getByTestId("activity-summary").filter({ hasText: "(another Carbon had already paired it)" })).toHaveCount(1);
    await shoot(page, "30-shared-tv");
  });

  test("a computer another Carbon installed Extend on: no terminal for my Silicons, Stop for the other side, my request hidden", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_STUDIO_MAC}`);
    // Carbon decision 3.
    await expect(page.getByTestId("shared-computer-terminal")).toContainText("Only Silicons given access by the Carbon who installed Silicon Extend on this computer can use its terminal.");
    await expect(page.getByTestId("shared-computer-terminal")).toContainText("That isn't you, so your Silicons use the screen, the keyboard and the apps.");
    await expect(page.getByTestId("shared-computer-warning")).toContainText("Share a computer only with Carbons you trust");
    await page.getByTestId("capabilities").locator("summary").click();
    await expect(page.getByTestId("capabilities")).toContainText("Only Silicons given access by the Carbon who installed Silicon Extend on it can use its terminal.");
    // The other side is in use: no Silicon named, and Stop works because the device is Saket's too.
    await expect(page.getByTestId("in-use-card")).toHaveAttribute("data-side", "other");
    await expect(page.getByTestId("in-use-card").getByTestId("in-use-other")).toContainText("A Silicon another Carbon gave access to is using it.");
    await expect(page.getByTestId("in-use-card")).not.toContainText("si:pilot");
    // chef's request went to the Carbon who gave the holder access, and chef's side never learns who.
    const sent = page.getByTestId("requests-sent");
    await expect(sent.getByTestId("request-to-hidden")).toHaveText("the Carbon who gave access to the Silicon using it");
    await expect(sent).toContainText("Not delivered yet; it is retried.");
    await shoot(page, "31-shared-computer");
    await page.getByTestId("stop-session").click();
    await expect(page.getByTestId("toast").last()).toContainText("Stopped the Silicon using Studio Mac (another Carbon gave it access)");
    await expect(page.getByTestId("in-use-card")).toHaveAttribute("data-side", "free");
  });

  test("a device the computer carries for another Carbon is in use: no Stop here, and the service's 409 says where", async ({ page, mock, request }) => {
    await mock.post("scenario", { name: "carried_busy" });
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_STUDIO_MAC}`);
    await expect(page.getByTestId("in-use-card").getByTestId("in-use-carried")).toContainText("A device this computer carries is in use.");
    await expect(page.getByTestId("stop-session")).toHaveCount(0);
    await expect(page.locator(`[data-device-id="${DEVICE_STUDIO_MAC}"]`).getByTestId("in-use-carried")).toBeVisible();
    const token = await page.evaluate(() => JSON.parse(localStorage.getItem("extend.auth.production")!).access_token);
    const res = await request.post(`/api/v1/devices/${DEVICE_STUDIO_MAC}/stop`, { headers: { Authorization: `Bearer ${token}` } });
    expect(res.status()).toBe(409);
    expect((await res.json()).data.message).toBe("A device carried by Studio Mac is in use. It can be stopped by the Carbon who paired it, or from Studio Mac's Extend app.");
    await shoot(page, "32-carried-in-use");
  });
});

test.describe("access per Team", () => {
  test("grants grouped by Team, a Team the login doesn't reach marked, giving access in another Team and taking it away in one", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    const deletes: string[] = [];
    const puts: string[] = [];
    page.on("request", (r) => {
      if (r.url().includes("/access/")) (r.method() === "DELETE" ? deletes : r.method() === "PUT" ? puts : []).push(new URL(r.url()).search);
    });
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    const teams = page.getByTestId("grant-team");
    await expect(teams).toHaveCount(3);
    await expect(teams.nth(0)).toHaveAttribute("data-team", "acme");
    await expect(teams.nth(1)).toHaveAttribute("data-team", "labs");
    await expect(teams.nth(2)).toHaveAttribute("data-team", "studio");
    await expect(teams.nth(2).getByTestId("sign-in-marker")).toHaveText(
      "Sign in to Extend for studio to see names, add Silicons from it, open their files and get Tings there. You can still take access away.",
    );
    await expect(teams.nth(0).getByTestId("sign-in-marker")).toHaveCount(0);

    // Give si:atlas access in labs, from the Team select (the menu's Team, acme, is only the default).
    await page.getByText("Give another Silicon access").click();
    await expect(page.getByTestId("grant-team-select")).toHaveValue("acme");
    await page.getByTestId("grant-team-select").selectOption("labs");
    const roster = page.locator('[data-roster="team"]');
    await expect(roster).toContainText("Silicons in labs");
    // si:juniper already has access in labs, so only si:atlas is offered there.
    await expect(roster.getByTestId("grant-suggestion")).toHaveCount(1);
    await roster.getByTestId("grant-suggestion").click();
    await page.getByTestId("grant-submit").click();
    await expect(page.getByTestId("toast").last()).toContainText("si:atlas can now use Saket's Pixel (in labs)");
    await expect(page.locator('[data-testid="grant"][data-team="labs"][data-silicon="si:atlas"]')).toBeVisible();
    expect(puts).toContain("?team=labs");

    // A grant in a Team the login doesn't reach can still be taken away, in that Team only.
    await page.locator('[data-testid="grant"][data-team="studio"]').getByTestId("revoke").click();
    await expect(page.locator('[data-testid="grant"][data-team="studio"]')).toHaveCount(0);
    expect(deletes).toContain("?team=studio");
    await shoot(page, "33-access-by-team");
  });

  test("a grant in a Team the login doesn't reach is refused with the service's words", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    const token = await page.evaluate(() => JSON.parse(localStorage.getItem("extend.auth.production")!).access_token);
    const res = await request.put(`/api/v1/devices/${DEVICE_PIXEL}/access/si:orbit?team=studio`, { headers: { Authorization: `Bearer ${token}`, "X-Org-ID": "acme" } });
    expect(res.status()).toBe(403);
    const body = (await res.json()).data;
    expect(body.code).toBe("not_a_team_member");
    expect(body.message).toBe("c:saket's Extend login doesn't reach studio.");
    expect(body.hint).toContain("Sign in to Extend again and select studio");
  });
});

test.describe("waking", () => {
  test("the wake banner: who asked, why, whether the device showed it; It's awake answers every side", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    const row = page.locator(`[data-device-id="${DEVICE_MAC}"]`);
    await expect(row.getByTestId("row-awake")).toHaveText("Locked");
    await expect(row.getByTestId("row-wake-requested")).toBeVisible();
    await row.click();
    const banner = page.getByTestId("wake-banner");
    await expect(banner).toContainText("si:atlas asks you to wake MacBook Pro.");
    await expect(banner.getByTestId("wake-request")).toHaveCount(1);
    await expect(banner.getByTestId("wake-request")).toContainText("acme");
    await expect(banner.getByTestId("wake-request")).toContainText("Need the screen to check tonight's deploy dashboard");
    await expect(banner.getByTestId("wake-delivery")).toHaveText("The device showed it. You were told through Ting.");
    await expect(page.getByTestId("wake-awake-note")).toContainText("including ones that came through other Carbons who paired it");
    await expect(page.getByTestId("device-awake")).toContainText("Locked since");
    await shoot(page, "34-wake-banner");
    await page.getByTestId("wake-awake").click();
    await expect(page.getByTestId("toast").last()).toContainText("Told every Silicon that asked: MacBook Pro is awake");
    await expect(page.getByTestId("wake-banner")).toHaveCount(0);
    await expect(page.getByTestId("device-awake")).toContainText("Awake");
    await expect(page.locator('[data-testid="wake-history"][data-state="woken"]')).toHaveCount(2);
    await expect(page.getByTestId("requests-wake")).toContainText("a Carbon said it's awake");
  });

  test("Decline, and turning wake requests off for one Silicon and for the device", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_MAC}`);
    await page.getByTestId("wake-mute-silicon").click();
    await expect(page.getByTestId("toast").last()).toContainText("si:atlas can't ask you to wake MacBook Pro any more (in acme)");
    await expect(page.getByTestId("wake-banner")).toHaveCount(0);
    const grant = page.locator('[data-testid="grant"][data-silicon="si:atlas"]');
    await expect(grant.getByTestId("grant-wake-muted")).toHaveText("Wake requests off");
    await grant.getByTestId("grant-unmute").click();
    await expect(grant.getByTestId("grant-wake-muted")).toHaveCount(0);

    // si:atlas asks again; Saket declines it.
    const login = await request.post("/api/v1/auth/login", { headers: { "Idempotency-Key": "atlas-login-1" }, data: { type: "login", data: { slt: "oac_si_atlas" } } });
    const atlas = (await login.json()).data.access_token;
    const ask = await request.post(`/api/v1/devices/${DEVICE_MAC}/wake-requests`, {
      headers: { Authorization: `Bearer ${atlas}`, "X-Org-ID": "acme", "Idempotency-Key": "atlas-wake-1" },
      data: { type: "wake_request", data: { reason: "The export needs the screen" } },
    });
    expect(ask.status()).toBe(201);
    await expect(page.getByTestId("wake-banner")).toContainText("The export needs the screen", { timeout: 12_000 });
    await page.getByTestId("wake-decline").click();
    await expect(page.getByTestId("toast").last()).toContainText("Declined: si:atlas was told");
    await expect(page.getByTestId("wake-banner")).toHaveCount(0);

    // The whole device, from the Pairing card.
    await expect(page.getByTestId("wake-toggle")).toBeChecked();
    await page.getByTestId("wake-toggle").uncheck();
    await expect(page.getByTestId("toast").last()).toContainText("Wake requests for MacBook Pro are off");
    const refused = await request.post(`/api/v1/devices/${DEVICE_MAC}/wake-requests`, {
      headers: { Authorization: `Bearer ${atlas}`, "X-Org-ID": "acme", "Idempotency-Key": "atlas-wake-2" },
      data: { type: "wake_request", data: { reason: "Once more" } },
    });
    expect(refused.status()).toBe(409);
  });

  test("a device a computer carries: wake the computer too, and say It's awake where Extend can't tell", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    const token = await page.evaluate(() => JSON.parse(localStorage.getItem("extend.auth.production")!).access_token);
    await request.put(`/api/v1/devices/${DEVICE_IPHONE}/access/si:atlas?team=acme`, { headers: { Authorization: `Bearer ${token}` } });
    const login = await request.post("/api/v1/auth/login", { headers: { "Idempotency-Key": "atlas-login-2" }, data: { type: "login", data: { slt: "oac_si_atlas" } } });
    const atlas = (await login.json()).data.access_token;
    const ask = await request.post(`/api/v1/devices/${DEVICE_IPHONE}/wake-requests`, {
      headers: { Authorization: `Bearer ${atlas}`, "X-Org-ID": "acme", "Idempotency-Key": "atlas-wake-iphone" },
      data: { type: "wake_request", data: { reason: "Need to confirm the order in the app" } },
    });
    expect(ask.status()).toBe(201);
    await page.goto(`/devices/${DEVICE_IPHONE}`);
    const banner = page.getByTestId("wake-banner");
    await expect(banner).toContainText("Extend can't tell when this phone wakes, so choose It's awake once it is.");
    await expect(banner.getByTestId("wake-host")).toHaveText("Saket's iPhone pairs through MacBook Pro: wake that computer too.");
    await expect(banner.getByTestId("wake-delivery")).toHaveText("This device can't show it itself. You were told through Ting.");
    await shoot(page, "34b-wake-carried");
  });

  test("an iPhone: Extend can't tell whether it's awake", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await expect(page.locator(`[data-device-id="${DEVICE_IPHONE}"]`).getByTestId("row-awake")).toHaveText("Awake: unknown");
    await expect(page.locator('[data-device-id="0d44e1f2"]').getByTestId("row-awake")).toHaveText("Offline, last seen in standby");
  });
});

test.describe("Ting app types and recipient Teams", () => {
  test("the device page names the Team missing Extend's types, with the exact command", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    const banner = page.getByTestId("ting-banner");
    await expect(banner.getByTestId("ting-missing")).toHaveCount(1);
    await expect(banner.getByTestId("ting-missing")).toHaveAttribute("data-team", "labs");
    await expect(banner).toContainText("ting --org '<owning-team>' types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'");
    await expect(banner).toContainText("ting --org '<owning-team>' types register --type extend.device.woken --description 'A device a Silicon asked to wake is awake'");
    // A device with grants only in acme, where nothing is missing, shows no banner.
    await page.goto(`/devices/${DEVICE_MAC}`);
    await expect(page.getByTestId("device-name")).toHaveText("MacBook Pro");
    await expect(page.getByTestId("ting-banner")).toHaveCount(0);
  });

  test("Settings: Turn on registers the recipient while missing app types keep their owner guidance", async ({ page, mock }) => {
    await signInWithSlt(page);
    await page.goto("/settings");
    const rows = page.getByTestId("ting-row");
    await expect(rows).toHaveCount(3);
    await expect(page.locator('[data-testid="ting-row"][data-team="acme"]').getByTestId("ting-status")).toHaveText("On");
    const labs = page.locator('[data-testid="ting-row"][data-team="labs"]');
    await expect(labs.getByTestId("ting-status")).toHaveText("Not set up yet");
    await expect(labs.getByTestId("ting-missing")).toContainText("extend.device.wake_requested");
    const studio = page.locator('[data-testid="ting-row"][data-team="studio"]');
    await expect(studio).toContainText("Sign in to Extend for studio to get Tings there.");
    await expect(studio.getByTestId("ting-turn-on")).toHaveCount(0);
    await shoot(page, "35-settings-ting");

    await labs.getByTestId("ting-turn-on").click();
    await expect(page.getByTestId("toast").last()).toContainText("a Ting manager in Extend's owning Team still has to register 2 app types");
    await expect(labs.getByTestId("ting-status")).toHaveText("On");
    await expect(labs.getByTestId("ting-missing")).toBeVisible();

    await expect(labs.getByTestId("ting-turn-on")).toHaveCount(0);
    await expect(labs.getByTestId("ting-missing")).toContainText("Turning your notifications on does not register app types.");
    await page.reload();
    await expect(labs.getByTestId("ting-missing")).toBeVisible();
    // The owning-Team manager registers the types outside Extend, and a later send succeeds.
    await mock.post("ting-types-known", { team: "labs" });
    await page.reload();
    await expect(labs.getByTestId("ting-missing")).toHaveCount(0);
  });
});

test.describe("setup retry (contract A)", () => {
  test("a failed step shows its plain error and a Retry button; the device runs it again", async ({ page, mock }) => {
    await mock.post("fail-step", { device_id: DEVICE_PIXEL, step: "notifications", error: "Notifications are off for Silicon Extend. Turn them on in Settings, then tap Retry." });
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    const step = page.locator('[data-testid="setup-step"][data-status="failed"]');
    await expect(step).toHaveCount(1);
    await expect(step.getByTestId("setup-step-error")).toHaveText("Notifications are off for Silicon Extend. Turn them on in Settings, then tap Retry.");
    await shoot(page, "36-setup-failed");
    await step.getByTestId("setup-retry").click();
    // The device ran it again and finished: the page's setup card goes once the device is ready.
    await expect(page.getByTestId("setup-steps")).toHaveCount(0, { timeout: 10_000 });
    await expect(page.getByTestId("setup-retry-error")).toHaveCount(0);
    expect(await mock.get("retries")).toEqual({ items: [{ device_id: DEVICE_PIXEL, step: "notifications" }] });
  });

  test("failing again brings Retry back, and a second retry within 5 s shows the service's 429", async ({ page, mock }) => {
    // Slow enough that the website sees the step running again before it fails again.
    await mock.config({ step_ms: 2000 });
    await mock.post("fail-step", { device_id: DEVICE_PIXEL, step: "notifications", error: "Notifications are off for Silicon Extend. Turn them on in Settings, then tap Retry.", again: true });
    await signInWithSlt(page);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await page.getByTestId("setup-retry").click();
    await expect(page.getByTestId("setup-retrying")).toBeVisible();
    // The device picked it up, then reported it failed again.
    await expect(page.getByTestId("setup-retry")).toBeVisible({ timeout: 10_000 });
    await page.getByTestId("setup-retry").click();
    const error = page.getByTestId("setup-retry-error");
    await expect(error).toHaveAttribute("data-code", "rate_limited");
    await expect(error).toContainText("A retry was just sent to Saket's Pixel.");
  });

  test("an app older than 1.1 can't retry from here: the 426 says to update it", async ({ page, mock }) => {
    const code = await mock.enroll("android", { app_version: "1.0.2" });
    await mock.post("fail-step", { os: "android", step: "developer_options", error: "Developer options are off. Turn them on, then tap Retry on the phone." });
    await signInWithSlt(page);
    await page.goto("/devices/new?kind=android");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill(code);
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Old phone");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("banner-step")).toBeVisible();
    await page.getByTestId("banner-next").click();
    await expect(page.getByTestId("setup-step-error")).toHaveText("Developer options are off. Turn them on, then tap Retry on the phone.", { timeout: 10_000 });
    await page.getByTestId("setup-retry").click();
    const error = page.getByTestId("setup-retry-error");
    await expect(error).toHaveAttribute("data-code", "upgrade_required");
    await expect(error).toContainText("Old phone runs Silicon Extend 1.0.2, which can't retry from here. Update it to 1.1, or tap Retry on the device.");
  });
});

test.describe("adding a device (1.1)", () => {
  test("no visibility step; the code step explains Pair with another Carbon", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.goto("/devices/new?kind=android");
    await page.getByTestId("wizard-next").click();
    await expect(page.getByTestId("already-paired-note")).toContainText("choose Pair with another Carbon (Extend 1.1 or later)");
    await page.getByTestId("pairing-code-input").fill("4F9C2A");
    await page.getByTestId("wizard-next").click();
    await expect(page.getByTestId("name-step")).not.toContainText("Who can see it exists");
    await expect(page.locator('input[name="visibility"]')).toHaveCount(0);
  });

  test("pairing a computer another Carbon paired: the warning, then access in any of my Teams", async ({ page, mock }) => {
    // Alice's Windows PC shows "Pair with another Carbon".
    const code = await mock.enroll("windows", { instance_of: "b3f81c20" });
    await signInWithSlt(page);
    const claims: unknown[] = [];
    page.on("request", (r) => r.url().endsWith("/api/v1/pairings") && claims.push(r.postDataJSON()));
    await page.goto("/devices/new?kind=windows");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill(code);
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Shared PC");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("banner-step")).toBeVisible();
    await page.getByTestId("banner-next").click();
    await expect(page.getByTestId("wizard-shared-note")).toContainText("Another Carbon paired this computer too.");
    await expect(page.getByTestId("wizard-shared-computer-warning")).toContainText(
      "Only Silicons given access by the Carbon who installed Silicon Extend on this computer can use its terminal",
    );
    // A pair of a device already set up is ready at once.
    await expect(page.getByTestId("setup-complete")).toBeVisible();
    expect(claims).toEqual([{ type: "pairing", data: { pairing_code: code, name: "Shared PC", pair_ttl_days: 14 } }]);
    await shoot(page, "37-wizard-shared-computer");
    await page.getByTestId("setup-next").click();
    await page.getByTestId("grant-team-select").selectOption("labs");
    await page.getByTestId("grant-input").fill("si:juniper");
    await page.getByTestId("grant-submit").click();
    await expect(page.getByTestId("wizard-done")).toContainText("si:juniper can use it now");
    await expect(page.getByTestId("wizard-done")).toContainText("extend --team labs device show");
  });

  test("a device I already paired: the claim's 409 names my own pair", async ({ page, mock }) => {
    const code = await mock.enroll("macos", { instance_of: "8e4f3a21" });
    await signInWithSlt(page);
    await page.goto("/devices/new?kind=mac");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("pairing-code-input").fill(code);
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Studio again");
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("pairing-error")).toHaveAttribute("data-code", "conflict");
    await expect(page.getByTestId("pairing-error")).toContainText(`You already paired this device: it's Studio Mac (${DEVICE_STUDIO_MAC}) in your devices.`);
  });

  test("after an attach: the duplicate and recognising steps, with no Retry", async ({ page, mock }) => {
    await signInWithSlt(page);
    await page.goto("/devices/new?kind=samsung_tv");
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("host-option").filter({ hasText: "MacBook Pro" }).click();
    await page.getByTestId("wizard-next").click();
    await page.getByTestId("device-name-input").fill("Bedroom TV");
    const created = page.waitForResponse((r) => r.url().includes("/attachments") && r.status() === 201);
    await page.getByTestId("pair-submit").click();
    await expect(page.getByTestId("banner-step")).toBeVisible();
    await page.getByTestId("banner-next").click();
    const deviceId = ((await (await created).json()) as { data: { device_id: string } }).data.device_id;

    await mock.post("carried", { device_id: deviceId, state: "duplicate_other" });
    const step = page.locator('[data-testid="setup-step"][data-status="failed"]');
    await expect(step).toContainText("Added through another computer", { timeout: 10_000 });
    await expect(step.getByTestId("setup-step-error")).toHaveText(
      "This TV is already added to Extend through another computer. Add it through that computer instead: on its Extend app choose Pair with another Carbon, then add the TV there.",
    );
    await expect(step.getByTestId("setup-retry")).toHaveCount(0);
    await shoot(page, "38-wizard-duplicate");

    await mock.post("carried", { device_id: deviceId, state: "recognising" });
    await expect(page.locator('[data-testid="setup-step"][data-status="in_progress"]').first()).toContainText("Waiting for MacBook Pro to recognise this device", { timeout: 10_000 });
  });
});

test.describe("signing out and Silicons", () => {
  test("signing out says the Silicons you gave access to lose their running sessions (Carbon decision 1)", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page);
    await page.getByTestId("sign-out").click();
    await expect(page.getByTestId("sign-out-effect")).toHaveText(
      "Signing out ends the running sessions of the Silicons you gave access to, on every device you paired. Silicons other Carbons gave access to carry on. Your Silicons keep their access.",
    );
    await shoot(page, "39-sign-out");
    await page.getByRole("button", { name: "Stay signed in" }).click();
    await expect(page.getByTestId("member-id")).toHaveText("c:saket");
    await page.goto("/settings");
    await expect(page.getByTestId("settings-sign-out-note")).toContainText("Signing out ends the running sessions of the Silicons you gave access to");
    await page.getByTestId("settings-sign-out").click();
    await page.getByTestId("sign-out-confirm").click();
    await expect(page.getByTestId("sign-in")).toBeVisible();
  });

  test("a Silicon sees the devices it may use in its Team, and another side's session only as in use", async ({ page, mock }) => {
    void mock;
    await signInWithSlt(page, SLT_CHEF);
    await expect(page.getByTestId("list-label")).toContainText("Yours to use in acme · 3");
    await expect(page.locator(`[data-device-id="${DEVICE_STUDIO_MAC}"]`).getByTestId("in-use-other")).toHaveText("Another Silicon is using it");
    await expect(page.locator(`[data-device-id="${DEVICE_PIXEL}"]`).getByTestId("in-use")).toContainText("si:chef");
    await expect(page.getByTestId("device-list")).not.toContainText("si:pilot");
  });
});

test.describe("compatibility", () => {
  test("a device in the 1.0 shape still renders", async ({ page, mock }) => {
    void mock;
    // What a 1.0 service answers: visibility, the device's Team, and none of the 1.1 fields.
    const strip = (d: Record<string, unknown>) => {
      for (const k of ["engine_version", "awake", "sleep_state", "last_sleep_state", "awake_changed_at", "wake_detectable", "in_use_by_other", "in_use_by_other_carried", "open_wake_requests", "wake_requests", "wake_muted", "paired_by_others", "same_device"]) delete d[k];
      if (d.in_use) delete (d.in_use as Record<string, unknown>).team;
      return { ...d, visibility: "team", team: "acme" };
    };
    await page.route(/\/api\/v1\/devices(\/[0-9a-f]{8})?(\?.*)?$/, async (route) => {
      if (route.request().method() !== "GET") return route.fallback();
      const res = await route.fetch();
      const body = await res.json();
      body.data = body.type === "devices" ? { ...body.data, items: body.data.items.map(strip) } : strip(body.data);
      await route.fulfill({ response: res, json: body });
    });
    await page.route(/\/api\/v1\/devices\/[0-9a-f]{8}\/access$/, async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.data.items = body.data.items.map((g: Record<string, unknown>) => ({ device_id: g.device_id, silicon_id: g.silicon_id, granted_by: g.granted_by, granted_at: g.granted_at, last_used_at: g.last_used_at }));
      await route.fulfill({ response: res, json: body });
    });
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await signInWithSlt(page);
    await expect(page.getByTestId("device-row")).toHaveCount(7);
    await expect(page.getByTestId("row-awake")).toHaveCount(0);
    await page.goto(`/devices/${DEVICE_PIXEL}`);
    await expect(page.getByTestId("in-use-silicon")).toHaveText("si:chef");
    await expect(page.getByTestId("device-awake")).toHaveCount(0);
    await expect(page.getByTestId("wake-toggle")).toHaveCount(0);
    await expect(page.getByTestId("shared-note")).toHaveCount(0);
    // Grants without a Team read as the device's Team.
    await expect(page.getByTestId("grant-team")).toHaveCount(1);
    await expect(page.getByTestId("grant-team")).toHaveAttribute("data-team", "acme");
    expect(errors).toEqual([]);
  });

  test("the 1.0 website's calls get answers 1.0 can read from a 1.1 service", async ({ page, mock, request }) => {
    void mock;
    await signInWithSlt(page);
    const token = await page.evaluate(() => JSON.parse(localStorage.getItem("extend.auth.production")!).access_token);
    const headers = { Authorization: `Bearer ${token}`, "X-Org-ID": "labs" };
    // Every device the Carbon paired, whatever X-Org-ID; always "personal"; the Team tab empty.
    const mine = (await (await request.get("/api/v1/devices?scope=mine", { headers })).json()).data.items as { visibility: string }[];
    expect(mine).toHaveLength(7);
    expect(mine.every((d) => d.visibility === "personal")).toBe(true);
    expect((await (await request.get("/api/v1/devices?scope=team", { headers })).json()).data.items).toEqual([]);
    // A grant without ?team goes into X-Org-ID's Team; a revoke without ?team removes every Team's grant.
    const put = await request.put(`/api/v1/devices/${DEVICE_PIXEL}/access/si:atlas`, { headers });
    expect((await put.json()).data.team).toBe("labs");
    await request.put(`/api/v1/devices/${DEVICE_PIXEL}/access/si:atlas`, { headers: { ...headers, "X-Org-ID": "acme" } });
    await request.delete(`/api/v1/devices/${DEVICE_PIXEL}/access/si:atlas`, { headers });
    const grants = (await (await request.get(`/api/v1/devices/${DEVICE_PIXEL}/access`, { headers })).json()).data.items as { silicon_id: string }[];
    expect(grants.some((g) => g.silicon_id === "si:atlas")).toBe(false);
    // Visibility is accepted and ignored.
    const patched = await request.patch(`/api/v1/devices/${DEVICE_MAC}`, { headers: { ...headers, "If-Match": '"2"' }, data: { type: "device", data: { visibility: "team" } } });
    expect((await patched.json()).data.visibility).toBe("personal");
  });
});

test("docs and downloads say 1.1", async ({ page, mock }) => {
  void mock;
  await page.goto("/docs");
  for (const heading of ["Several Carbons, one device", "Waking a device", "Ting notifications"]) await expect(page.getByRole("heading", { name: heading })).toBeVisible();
  await expect(page.getByTestId("docs-page")).toContainText("only Silicons given access by the Carbon who installed Silicon Extend on it can use the terminal");
  await expect(page.getByTestId("docs-page")).toContainText("extend --team acme device wake 7c1e09ab");
  await expect(page.getByTestId("docs-page")).not.toContainText("agent-device");
  await page.goto("/download/mac");
  await expect(page.getByTestId("download-version")).toHaveText("Download · version 1.1.0");
  await expect(page.getByTestId("download-file")).toHaveAttribute("href", "https://github.com/teamofsilicons/silicon-extend/releases/latest/download/Silicon-Extend-macos-arm64.zip");
});

test("a Carbon can hide and show the in-use banner without ending the session", async ({ page, mock }) => {
  void mock;
  await signInWithSlt(page);
  await page.goto(`/devices/${DEVICE_PIXEL}`);
  const setting = page.getByTestId("banner-setting");
  await expect(setting).toHaveAttribute("data-indicator", "shown");
  const stopBefore = await page.getByTestId("stop-session").count();
  await setting.getByTestId("banner-toggle").uncheck();
  await expect(setting).toHaveAttribute("data-indicator", "hidden");
  await page.reload();
  await expect(setting).toHaveAttribute("data-indicator", "hidden");
  await expect(page.getByTestId("stop-session")).toHaveCount(stopBefore);
  await setting.getByTestId("banner-toggle").check();
  await expect(setting).toHaveAttribute("data-indicator", "shown");
});
