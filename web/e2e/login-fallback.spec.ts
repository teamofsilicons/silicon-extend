import { expect, test } from "./fixtures";

for (const kind of ["carbon", "silicon"] as const) {
  test(`blocked ${kind} popup can complete a typed full-page sign-in`, async ({page, mock}) => {
    void mock;
    await page.addInitScript(() => { window.open = () => null; });
    const logins: string[] = [];
    let callback = "";
    page.on("request", request => {
      if (new URL(request.url()).pathname === "/api/v1/auth/login") logins.push(request.url());
      if (new URL(request.url()).pathname === "/auth/callback") callback = request.url();
    });
    await page.goto("/");
    await page.getByTestId(`sign-in-${kind}`).click();
    await expect(page.getByTestId("error")).toContainText("popup was blocked");
    await page.getByTestId(`sign-in-${kind}-page`).click();
    await expect(page).toHaveURL(/\/__mock\/iam\/login/);
    const url = new URL(page.url());
    expect(url.searchParams.get("identity_kind")).toBe(kind);
    expect(url.searchParams.has("display")).toBe(false);
    expect(new URL(url.searchParams.get("redirect_uri")!).searchParams.has("display")).toBe(false);
    await page.getByRole("button", {name: /Continue as/}).first().click();
    await expect(page.getByTestId("devices-page")).toBeVisible();
    await expect(page.getByTestId("member-id")).toHaveText(kind === "carbon" ? /^c:/ : /^si:/);
    expect(logins).toHaveLength(1);
    expect(new URL(page.url()).search).toBe("");
    await page.goto(callback);
    await expect(page.getByTestId("error")).toHaveAttribute("data-code", "invalid_login_state");
    expect(logins).toHaveLength(1);
  });
}

test("full-page choice closes the pending popup and replaces its state", async ({page,mock}) => {
  void mock;
  await page.goto("/");
  const opened = page.waitForEvent("popup");
  await page.getByTestId("sign-in-carbon").click();
  const popup = await opened;
  await expect(popup).toHaveURL(/\/__mock\/iam\/login/);
  const oldState = new URL(new URL(popup.url()).searchParams.get("redirect_uri")!).searchParams.get("state");
  await page.getByTestId("sign-in-silicon-page").click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect(page).toHaveURL(/\/__mock\/iam\/login/);
  const url = new URL(page.url());
  expect(url.searchParams.get("identity_kind")).toBe("silicon");
  expect(new URL(url.searchParams.get("redirect_uri")!).searchParams.get("state")).not.toBe(oldState);
  await page.getByRole("button", {name: /Continue as/}).first().click();
  await expect(page.getByTestId("member-id")).toHaveText(/^si:/);
});

test("full-page callback refuses an account selection changed during the redirect", async ({page,mock}) => {
  void mock;
  let exchanges = 0;
  page.on("request", request => { if (new URL(request.url()).pathname === "/api/v1/auth/login") exchanges++; });
  await page.goto("/");
  await page.getByTestId("sign-in-carbon-page").click();
  await expect(page).toHaveURL(/\/__mock\/iam\/login/);
  await page.evaluate(() => localStorage.setItem("extend.login.context-epoch", "different-selection"));
  await page.getByRole("button", {name: /Continue as/}).first().click();
  await expect(page.getByTestId("error")).toHaveAttribute("data-code", "invalid_login_state");
  expect(exchanges).toBe(0);
});

test("full-page callback does not save a login when backend verification returns the wrong kind", async ({page,mock}) => {
  void mock;
  await page.route("**/api/v1/auth/me", async route => {
    const response = await route.fetch();
    const body = await response.json();
    body.data.member.type = "silicon";
    await route.fulfill({response, json:body});
  });
  await page.goto("/");
  await page.getByTestId("sign-in-carbon-page").click();
  await expect(page).toHaveURL(/\/__mock\/iam\/login/);
  await page.getByRole("button", {name: /Continue as/}).first().click();
  await expect(page.getByTestId("error")).toHaveAttribute("data-code", "identity_kind_mismatch");
  expect(await page.evaluate(() => localStorage.getItem("extend.auth.production.selected"))).toBeNull();
});
