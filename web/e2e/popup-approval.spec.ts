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

test("pending approval can cancel immediately and resume its original request", async ({page,mock}) => {
  void mock; await signInWithSlt(page); await page.goto("/settings");
  const starts: {key:string,body:unknown}[]=[];
  let completions=0;
  page.on("request",req=>{
    const path=new URL(req.url()).pathname;
    if(path==="/api/v1/permissions" && req.method()==="POST") starts.push({key:req.headers()["idempotency-key"],body:req.postDataJSON()});
    if(path.endsWith("/complete")) completions++;
  });
  const opened=page.waitForEvent("popup"); await page.getByRole("button",{name:"Request access"}).click(); const popup=await opened;
  await expect(popup.getByRole("heading",{name:"Demo feature approval"})).toBeVisible();
  const firstUrl=popup.url();
  const panel=page.getByTestId("settings-permissions");
  await expect(panel.getByRole("button",{name:"Cancel",exact:true})).toBeEnabled();
  await panel.getByRole("button",{name:"Cancel",exact:true}).click();
  await expect.poll(()=>popup.isClosed()).toBe(true);
  await expect(panel).toContainText("Approval paused");
  await expect(page.getByTestId("member-id")).toHaveText("c:saket");
  const resumed=page.waitForEvent("popup");await page.getByRole("button",{name:"Request access"}).click();const next=await resumed;
  await expect(next).toHaveURL(firstUrl);
  expect(starts).toHaveLength(1);expect(completions).toBe(0);
  await next.getByRole("button",{name:"Approve demo request"}).click();
  await expect(panel).toContainText("Access approved");expect(completions).toBe(1);
});

for (const blocked of ["null", "throws"] as const) test(`${blocked} blocked popup completes through a no-opener review tab using the same bound request`, async ({page,mock,context}) => {
  void mock; await page.addInitScript(mode=>{window.open=()=>{if(mode==="throws") throw new Error("Popup blocked");return null;};},blocked);
  await signInWithSlt(page); await page.goto("/settings");
  const starts:{body:any,key:string}[]=[],completed:{url:string,body:any,key:string}[]=[];
  page.on("request",req=>{
    const path=new URL(req.url()).pathname;
    if(path==="/api/v1/permissions" && req.method()==="POST") starts.push({body:req.postDataJSON(),key:req.headers()["idempotency-key"]});
    if(path.endsWith("/complete")) completed.push({url:req.url(),body:req.postDataJSON(),key:req.headers()["idempotency-key"]});
  });
  const panel=page.getByTestId("settings-permissions");
  await page.getByRole("button",{name:"Request access"}).click();
  await expect(panel).toContainText("popup was blocked");
  const opened=context.waitForEvent("page");await panel.getByRole("link",{name:"Review access in IAM"}).click();const review=await opened;
  expect(await review.evaluate(()=>window.opener===null)).toBe(true);
  await review.getByRole("button",{name:"Approve demo request"}).click();
  const code=await review.getByLabel("Single-use approval code").inputValue();
  expect(new URL(review.url()).search).toBe("");
  await panel.getByLabel("IAM approval code").fill(code);
  await panel.getByRole("button",{name:"Save approval"}).click();
  await expect(panel).toContainText("Access approved");
  expect(starts).toHaveLength(1);expect(completed).toHaveLength(1);
  expect(completed[0].body.data.state).toBe(starts[0].body.data.callback.state);
  expect(completed[0].url).toContain(code.replace("obc_mock_",""));
  await expect(page.getByTestId("member-id")).toHaveText("c:saket");
  await review.close();
});

test("cancel ignores a delayed completion while an explicit manual retry keeps its code and key", async ({page,mock}) => {
  void mock;await signInWithSlt(page);await page.goto("/settings");
  let release!:()=>void;const delayed=new Promise<void>(resolve=>release=resolve);
  const calls:{key:string,body:any}[]=[];
  await page.route("**/permissions/*/complete",async route=>{
    calls.push({key:route.request().headers()["idempotency-key"],body:route.request().postDataJSON()});
    const response=await route.fetch();
    if(calls.length===1) await delayed;
    await route.fulfill({response});
  });
  const panel=page.getByTestId("settings-permissions");
  const opened=page.waitForEvent("popup");await page.getByRole("button",{name:"Request access"}).click();const popup=await opened;
  await popup.getByRole("button",{name:"Approve demo request"}).click();
  await expect.poll(()=>calls.length).toBe(1);
  await panel.getByRole("button",{name:"Cancel",exact:true}).click();release();
  await expect(panel).toContainText("Approval paused");await expect(panel).not.toContainText("Access approved");
  await expect(page.getByTestId("member-id")).toHaveText("c:saket");
  // No revocation request is sent: the explicitly approved server grant may already exist.
  await page.evaluate(()=>{window.open=()=>null;});
  await panel.getByRole("button",{name:"Request access"}).click();
  await expect(panel.getByLabel("IAM approval code")).toHaveValue(calls[0].body.data.code);
  await panel.getByRole("button",{name:"Save approval"}).click();
  await expect(panel).toContainText("Access approved");
  expect(calls).toHaveLength(2);expect(calls[1]).toEqual(calls[0]);
});


test("a declined request stays bound until the user explicitly starts a fresh same-feature review", async ({page,mock}) => {
  void mock;await signInWithSlt(page);await page.goto("/settings");
  const starts:{body:any,key:string}[]=[];
  page.on("request",req=>{if(new URL(req.url()).pathname==="/api/v1/permissions"&&req.method()==="POST")starts.push({body:req.postDataJSON(),key:req.headers()["idempotency-key"]});});
  const opened=page.waitForEvent("popup");await page.getByRole("button",{name:"Request access"}).click();const popup=await opened;
  const original=popup.url();await popup.getByRole("button",{name:"Decline demo request"}).click();
  const panel=page.getByTestId("settings-permissions");await expect(panel).toContainText("Access was not approved");
  const nextOpened=page.waitForEvent("popup");await panel.getByRole("button",{name:"Start a new request"}).click();const next=await nextOpened;
  await expect(next.getByRole("heading",{name:"Demo feature approval"})).toBeVisible();
  expect(next.url()).not.toBe(original);expect(starts).toHaveLength(2);
  expect(starts[1].body.data.endpoints).toEqual(starts[0].body.data.endpoints);
  expect(starts[1].body.data.callback.state).not.toBe(starts[0].body.data.callback.state);
  expect(starts[1].key).not.toBe(starts[0].key);
  await next.getByRole("button",{name:"Approve demo request"}).click();await expect(panel).toContainText("Access approved");
});
