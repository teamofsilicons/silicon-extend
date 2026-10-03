import { describe, expect, it } from "vitest";
import { client, json } from "./helpers";

const ID = "723594d6-4b08-4bc6-b079-fb4d7a62a6ca";
const KEY = "853fc3ca-ff62-4912-b4bd-0fa0d3029a85";
const REQUEST = { id: ID, consent_url: "https://iam.test/consent", expires_at: "2026-10-03T12:00:00Z" };
const ROOTS = [{ audience: "briefcase", endpoint_id: "briefcase.uploads.reserve" }];

describe("separate feature approval", () => {
  it.each([null, "ask_test_world"])("preserves scope and the explicit retry key in %s", async (secret) => {
    const { client: c, calls } = client((_call, index) => index === 0 ? new TypeError("connection reset") : json(200, { type: "permission", data: REQUEST }), { testingSecret: () => secret });
    expect(await c.requestPermissions(ROOTS, KEY)).toEqual(REQUEST);
    expect(calls).toHaveLength(2);
    for (const call of calls) {
      expect(call.url).toBe("https://api.test/api/v1/permissions");
      expect(call.headers.Authorization).toBe("Bearer oat_old");
      expect(call.headers["X-Org-ID"]).toBe("acme");
      expect(call.headers["X-Testing-Application-Secret"]).toBe(secret ?? undefined);
      expect(call.headers["Idempotency-Key"]).toBe(KEY);
      expect(call.body).toEqual({ type: "permission", data: { endpoints: ROOTS } });
    }
  });

  it("completes with the code without executing a feature or receiving credentials", async () => {
    const item = { ...ROOTS[0], grant_id: ID, org_id: "selected-team", actor: { public_id: "c:other" }, expires_at: "2026-10-03T12:00:00Z" };
    const { client: c, calls } = client(() => json(200, { type: "permissions", data: { items: [item] } }));
    expect(await c.completePermissions(ID, "obc_single_use", KEY)).toEqual([item]);
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(`https://api.test/api/v1/permissions/${ID}/complete`);
    expect(calls[0].body).toEqual({ type: "permission", data: { code: "obc_single_use" } });
    expect(calls[0].headers["Idempotency-Key"]).toBe(KEY);
    expect(await c.permissions()).toEqual([item]);
    expect(calls[1].method).toBe("GET");
    expect(calls[1].headers["Idempotency-Key"]).toBeUndefined();
  });

  it("keeps login on rejected consent and leaves the original action to the user", async () => {
    const { client: c, calls, tokens } = client(() => json(400, { type: "error", data: { code: "permission_code_invalid", message: "Use the code for this request." } }));
    await expect(c.completePermissions(ID, "obc_wrong", KEY)).rejects.toMatchObject({ code: "permission_code_invalid" });
    expect(calls).toHaveLength(1);
    expect(tokens.value?.access_token).toBe("oat_old");
  });
});


it("retains popup correlation with the same pinned organization for completion retry", async () => {
  const callback={redirect_uri:"https://extend.example/auth/obo/callback",state:"a".repeat(43)};
  const {client:c,calls}=client(call => json(200,call.url.endsWith("/complete") ? {type:"permissions",data:{items:[]}} : {type:"permission",data:REQUEST}));
  await c.requestPermissions(ROOTS,KEY,callback);
  await c.completePermissions(ID,"obc_same_code",KEY,callback.state);
  await c.completePermissions(ID,"obc_same_code",KEY,callback.state);
  expect(calls[0].body).toEqual({type:"permission",data:{endpoints:ROOTS,callback}});
  expect(calls[1].body).toEqual({type:"permission",data:{code:"obc_same_code",state:callback.state}});
  expect(calls[2].body).toEqual(calls[1].body);
  expect(calls[1].headers["Idempotency-Key"]).toBe(calls[2].headers["Idempotency-Key"]);
  expect(calls.every(c=>c.headers["X-Org-ID"]==="acme")).toBe(true);
});
