/**
 * The 1.0.0 website against a 1.1 service (the mock): it keeps working, as the design promises for
 * the time between the service release and the website release. What changes for it: every device
 * the Carbon paired is listed whatever Team is selected, the "Team devices" tab is empty, the
 * visibility radio changes nothing, grants go into the selected Team, and a routed request shows
 * the hidden recipient.
 */
import { expect, test } from "../e2e/fixtures";
import { DEVICE_FAMILY_TV, DEVICE_PIXEL, DEVICE_STUDIO_MAC, SLT_SAKET } from "../mock/fixtures";

test("the 1.0.0 website works against the 1.1 service", async ({ page, mock }) => {
  void mock;
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  // Only the local site and mock: a bundle built for production must never reach the real service.
  const outside: string[] = [];
  await page.route(/^https?:\/\/(?!localhost|127\.0\.0\.1)/, (route) => (outside.push(route.request().url()), route.abort()));
  await page.goto("/");
  await page.getByTestId("slt-input").fill(SLT_SAKET);
  await page.getByTestId("slt-submit").click();
  await expect(page.getByTestId("devices-page")).toBeVisible();
  // Every device the Carbon paired, whatever Team is selected.
  await expect(page.getByTestId("device-list").getByTestId("device-row")).toHaveCount(7);
  await page.getByTestId("team-picker").selectOption("labs");
  await expect(page.getByTestId("device-list").getByTestId("device-row")).toHaveCount(7);
  // No device is visible to Team colleagues any more.
  await page.getByTestId("tab-team").click();
  await expect(page.getByText("No other Carbon in this team has made a device visible.")).toBeVisible();
  await page.getByTestId("team-picker").selectOption("acme");
  await page.getByTestId("tab-mine").click();

  // A device page renders; the visibility radio is accepted and ignored.
  await page.goto(`/devices/${DEVICE_PIXEL}`);
  await expect(page.getByTestId("device-name")).toHaveText("Saket's Pixel");
  await expect(page.getByTestId("in-use-silicon")).toHaveText("si:chef");
  await page.getByTestId("visibility-team").check();
  await expect(page.getByTestId("visibility-personal")).toBeChecked();
  // Grants go into the selected Team (X-Org-ID).
  await page.getByText("Give another Silicon access").click();
  await page.getByTestId("grant-input").fill("si:atlas");
  await page.getByTestId("grant-submit").click();
  await expect(page.locator('[data-testid="grant"][data-silicon="si:atlas"]')).toBeVisible();

  // A request routed to the Carbon: the requester's side sees only the hidden recipient.
  await page.goto(`/devices/${DEVICE_STUDIO_MAC}`);
  await expect(page.getByTestId("requests")).toContainText("asked the Carbon who gave access to the Silicon using it");
  // Another side's session: 1.0 doesn't show it as in use (it reads the device as free).
  await expect(page.getByTestId("in-use-card")).toContainText("No Silicon is using");
  // The Carbon's own side still shows, with Stop.
  await page.goto(`/devices/${DEVICE_FAMILY_TV}`);
  await expect(page.getByTestId("in-use-silicon")).toHaveText("si:scout");
  await page.getByTestId("stop-session").click();
  await expect(page.getByTestId("toast").last()).toContainText("Stopped si:scout");
  expect(errors).toEqual([]);
  expect(outside).toEqual([]);
});
