import { expect, signInWithSlt, test } from "./fixtures";

test("feature popup binds callback and saves access without starting an action", async ({page,mock}) => {
  void mock; await signInWithSlt(page); await page.goto("/settings");
  const complete: {body:unknown,key?:string,org?:string,token?:string}[]=[];
  page.on("request", req => { if(req.url().includes("/permissions/") && req.url().endsWith("/complete")) complete.push({body:req.postDataJSON(),key:req.headers()["idempotency-key"],org:req.headers()["x-org-id"],token:req.headers().authorization}); });
  const opened=page.waitForEvent("popup"); await page.getByRole("button",{name:"Request access"}).click(); const popup=await opened;
  await expect(popup.getByRole("button",{name:"Approve demo request"})).toBeVisible();
  // A code from the wrong browsing context or state must never reach completion.
  await page.evaluate(()=>window.postMessage({type:"silicon:feature-approval",state:"a".repeat(43),code:"obc_forged"},location.origin));
  await popup.evaluate(()=>window.opener.postMessage({type:"silicon:feature-approval",state:"wrong-state",code:"obc_forged"},location.origin));
  expect(complete).toHaveLength(0);
  await popup.getByRole("button",{name:"Approve demo request"}).click();
  await expect(page.getByTestId("settings-permissions")).toContainText("Access approved");
  await expect.poll(()=>popup.isClosed()).toBe(true);
  expect(complete).toHaveLength(1); expect(complete[0].org).toBe("acme");expect(complete[0].key).toMatch(/^[0-9a-f-]{36}$/);
  expect((complete[0].body as {data:{state:string}}).data.state).toMatch(/^[A-Za-z0-9_-]{43}$/);
  await expect(page.getByTestId("member-id")).toHaveText("c:saket");
});

test("declined feature popup retains ordinary sign-in and grants no feature access", async ({page,mock}) => {
  void mock; await signInWithSlt(page); await page.goto("/settings");
  let completed=0;page.on("request",r=>{if(r.url().includes("/permissions/")&&r.url().endsWith("/complete"))completed++;});
  const opened=page.waitForEvent("popup");await page.getByRole("button",{name:"Request access"}).click();const popup=await opened;
  await popup.getByRole("button",{name:"Decline demo request"}).click();
  await expect(page.getByTestId("settings-permissions")).toContainText("Access was not approved");
  await expect(page.getByTestId("member-id")).toHaveText("c:saket"); expect(completed).toBe(0);
});

test("account selection cancels its old approval popup before code exchange", async ({page,mock}) => {
  void mock;await signInWithSlt(page);await page.goto("/settings");
  let completed=0;page.on("request",r=>{if(r.url().includes("/permissions/")&&r.url().endsWith("/complete"))completed++;});
  const opened=page.waitForEvent("popup");await page.getByRole("button",{name:"Request access"}).click();const popup=await opened;
  await expect(popup.getByRole("heading",{name:"Demo feature approval"})).toBeVisible();
  await page.evaluate(()=>{localStorage.removeItem("extend.auth.production.selected");window.dispatchEvent(new StorageEvent("storage",{key:"extend.auth.production.selected"}));});
  await expect.poll(()=>popup.isClosed()).toBe(true);expect(completed).toBe(0);
  await expect(page.getByText("Not signed in.", {exact:true})).toBeVisible();
});
