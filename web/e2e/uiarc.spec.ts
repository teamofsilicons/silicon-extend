import { expect, signInWithSlt, test } from "./fixtures";

test("device scope tabs support roving keyboard focus without changing organization", async ({ page, mock }) => {
  void mock;
  await signInWithSlt(page);
  const organization = await page.getByTestId("team-picker").inputValue();
  const mine = page.getByTestId("tab-mine"), team = page.getByTestId("tab-team"), removed = page.getByTestId("tab-removed");
  await mine.focus();
  await page.keyboard.press("ArrowRight");
  await expect(team).toBeFocused();
  await expect(team).toHaveAttribute("aria-selected", "true");
  await expect(mine).toHaveAttribute("tabindex", "-1");
  await page.keyboard.press("End");
  await expect(removed).toBeFocused();
  await expect(page.getByTestId("removed-device-list")).toBeVisible();
  await page.keyboard.press("ArrowRight");
  await expect(mine).toBeFocused();
  await expect(page.getByTestId("device-list")).toBeVisible();
  await page.keyboard.press("ArrowLeft");
  await expect(removed).toBeFocused();
  await page.keyboard.press("Home");
  await expect(mine).toBeFocused();
  await expect(page.getByTestId("team-picker")).toHaveValue(organization);
});

test("mobile import keeps its visible label and keyboard dismissal", async ({ page, mock }) => {
  void mock;
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await signInWithSlt(page);
  const button = page.getByRole("button", { name: "Import configured devices", exact: true });
  await expect(button).toContainText("Import");
  await button.click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await expect(button).toBeFocused();
  expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(0);
});
