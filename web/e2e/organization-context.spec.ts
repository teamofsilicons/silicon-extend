import { expect, test } from "@playwright/test";

const account = { type: "carbon", id: "c:alice", display_name: "Alice" };
const context = (org: string) => encodeURIComponent(JSON.stringify([account.type, account.id, org]));
const pair = (org: string) => ({
  member: account,
  teams: [org],
  access_token: `access-${org}`,
  refresh_token: `refresh-${org}`,
  expires_at: Date.now() + 3600_000,
  testing_environment: null,
});
const device = (org: string, id = "1234abcd", name = `${org} laptop`, owner = account) => ({
  device_id: id,
  name,
  os: "macos",
  kind: "computer",
  owner,
  team: org,
  visibility: "team",
  state: "ready",
  online: true,
  version: 1,
  capabilities: [],
  commands: [],
  pair_ttl_days: 14,
});

test("organization switching, private imports and shared discovery remain isolated", async ({ page }, testInfo) => {
  const imported = new Set<string>();
  let holdSearch = false;
  let releaseSearch: (() => void) | undefined;
  const requests: { path: string; org?: string; body: unknown; token?: string; key?: string }[] = [];
  await page.addInitScript(
    ({ acme, labs, first, second }) => {
      localStorage.setItem("extend.auth.production.contexts", JSON.stringify([first, second]));
      localStorage.setItem("extend.auth.production.selected", first);
      localStorage.setItem(`extend.auth.production.context.${first}`, JSON.stringify(acme));
      localStorage.setItem(`extend.auth.production.context.${second}`, JSON.stringify(labs));
    },
    { acme: pair("acme"), labs: pair("labs"), first: context("acme"), second: context("labs") },
  );
  await page.route("**/api/**", async (route) => {
    const req = route.request(),
      url = new URL(req.url()),
      path = url.pathname,
      org = req.headers()["x-org-id"];
    if (holdSearch && path === "/api/v1/devices" && url.searchParams.get("limit") === "100") {
      holdSearch = false;
      await new Promise<void>(resolve => { releaseSearch = resolve; });
    }
    const body = req.postDataJSON();
    requests.push({ path, org, body, token: req.headers().authorization, key: req.headers()["idempotency-key"] });
    let type = "devices",
      data: unknown = { items: [], next_cursor: null };
    if (path === "/api/version") {
      type = "version";
      data = { api_version: 1, supported: [1], service_version: "iam5-fixture" };
    } else if (path.endsWith("/auth/me")) {
      type = "me";
      data = { authenticated: true, member: account, teams: [org], team: org };
    } else if (path.endsWith("/importable"))
      data = {
        items: imported.has(org!)
          ? []
          : [{ device_id: "abcd1234", name: "Configured Mac", os: "macos", model: null, host_device_id: null }],
        next_cursor: null,
      };
    else if (path.endsWith("/import")) {
      imported.add(org!);
      type = "device";
      data = { ...device(org!, "abcd1234", "Configured Mac"), visibility: body.data.visibility };
    } else if (path === "/api/v1/devices")
      data = {
        items:
          url.searchParams.get("scope") === "team"
            ? [device(org!, "5678abcd", "Shared studio", { type: "carbon", id: "c:bob", display_name: "Bob" })]
            : [device(org!), ...(imported.has(org!) ? [{ ...device(org!, "abcd1234", "Configured Mac"), visibility: "personal" }] : [])],
        next_cursor: null,
      };
    else if (path === "/api/v1/devices/5678abcd") {
      type = "device";
      data = device(org!, "5678abcd", "Shared studio", { type: "carbon", id: "c:bob", display_name: "Bob" });
    } else if (path.endsWith("/ting-registration")) {
      type = "ting_registrations";
    } else if (path.endsWith("/telemetry")) {
      await route.fulfill({ status: 204 });
      return;
    }
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ type, data }) });
  });
  await page.goto("/devices");
  await expect(page.getByTestId("device-row")).toContainText("acme laptop");
  await page.getByTestId("team-picker").selectOption(context("labs"));
  await expect(page.getByTestId("device-row")).toContainText("labs laptop");
  await page.getByRole("button", { name: "Import configured devices" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Configured Mac");
  await expect(dialog.getByRole("checkbox")).not.toBeChecked();
  await dialog.getByRole("button", { name: "Import", exact: true }).click();
  await expect(dialog).not.toContainText("Configured Mac");
  const mutation = requests.find((r) => r.path.endsWith("/import"))!;
  expect(mutation.org).toBe("labs");
  expect(mutation.token).toBe("Bearer access-labs");
  expect(mutation.key).toMatch(/^[0-9a-f-]{36}$/);
  expect(mutation.body).toEqual({ type: "device_import", data: { visibility: "personal" } });
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("device-row")).toHaveCount(2);
  await page.getByTestId("team-picker").selectOption(context("acme"));
  await expect(page.getByTestId("device-row")).toHaveCount(1);
  await expect(page.getByTestId("device-row")).toContainText("acme laptop");
  await page.getByTestId("tab-team").click();
  await page.getByTestId("device-row").click();
  await expect(page.getByTestId("device-page")).toContainText("Shared by Bob in acme");
  await expect(page.getByTestId("settings-card")).toHaveCount(0);
  await expect(page.getByTestId("rename")).toHaveCount(0);
  await expect(page.getByTestId("remove-device")).toHaveCount(0);
  // A pending global search belongs to its original context even if another tab switches it.
  holdSearch = true;
  await page.getByTestId("open-search").click();
  await expect.poll(() => !!releaseSearch).toBe(true);
  await page.evaluate(id => {
    localStorage.setItem("extend.auth.production.selected", id);
    window.dispatchEvent(new StorageEvent("storage", { key: "extend.auth.production.selected" }));
  }, context("labs"));
  await expect(page.getByRole("dialog")).not.toBeVisible();
  releaseSearch!();
  await page.getByTestId("open-search").click();
  await expect(page.getByRole("dialog")).toContainText("labs laptop");
  await expect(page.getByRole("dialog")).not.toContainText("acme laptop");
  await page.keyboard.press("Escape");
  for (const request of requests.filter((r) => r.token)) expect(request.token).toBe(`Bearer access-${request.org}`);
  await page.screenshot({ path: testInfo.outputPath("organization-devices-desktop.png"), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: testInfo.outputPath("organization-devices-mobile.png"), fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(0);
});
