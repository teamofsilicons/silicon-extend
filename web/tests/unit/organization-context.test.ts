import { beforeEach, describe, expect, it } from "vitest";
import { createRoot } from "solid-js";
import { contextId, createSession } from "../../src/lib/session";
import { client, json, memoryStore, NOW, pair, session as authSession } from "./helpers";

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

describe("account and organization sessions", () => {
  it("keeps each login and an in-flight client's credentials bound to its original account and organization", () =>
    createRoot((dispose) => {
      const s = createSession(),
        first = pair(),
        second = pair({ teams: ["labs"], access_token: "oat_labs", refresh_token: "ort_labs" });
      s.client().ctx.onLogin!(first);
      const original = s.client();
      s.client().ctx.onLogin!(second);
      expect(s.contexts()).toHaveLength(2);
      expect(s.team()).toBe("labs");
      expect(original.ctx.team()).toBe("acme");
      original.ctx.tokens.save({ ...first, access_token: "oat_rotated", refresh_token: "ort_rotated" });
      expect(s.pair()?.access_token).toBe("oat_labs");
      s.selectContext(contextId(first)!);
      expect(s.pair()?.access_token).toBe("oat_rotated");
      expect(s.team()).toBe("acme");
      dispose();
    }));
  it("never accepts a multi-organization legacy token as a saved context", () => {
    expect(contextId(pair({ teams: ["acme", "labs"] }))).toBeNull();
    expect(contextId(pair({ teams: [] }))).toBeNull();
  });
  it("does not clear another selected login when the original request is signed out", () =>
    createRoot((dispose) => {
      const s = createSession(),
        first = pair(),
        other = pair({ member: { type: "carbon", id: "c:other" }, access_token: "other", refresh_token: "other-r" });
      s.client().ctx.onLogin!(first);
      const original = s.client();
      s.client().ctx.onLogin!(other);
      original.ctx.tokens.clear();
      expect(s.pair()?.member.id).toBe("c:other");
      expect(s.contexts()).toHaveLength(1);
      dispose();
    }));
});

describe("immutable refresh and device imports", () => {
  it("rejects a changed account while waiting for refresh, instead of sending its token into the original org", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW }));
    const { client: c, calls } = client(() => json(200, {}), {
      tokens,
      lock: async (_name, fn) => {
        tokens.value = pair({ teams: ["labs"] });
        return fn();
      },
    });
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "context_changed" });
    expect(calls).toHaveLength(0);
  });
  it("rejects a refresh response that changes the selected organization", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW }));
    const { client: c } = client(() => json(200, { type: "refresh", data: { ...authSession("new", "new-r"), teams: ["labs"] } }), {
      tokens,
    });
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "context_changed" });
    expect(tokens.value?.access_token).toBe("oat_old");
  });
  it("persists the refresh retry key through an uncertain response", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW }));
    let ready = false;
    const { client: c, calls } = client(
      (call) =>
        !ready
          ? new TypeError("connection closed")
          : json(
              200,
              call.url.endsWith("refresh")
                ? { type: "refresh", data: authSession("new", "new-r") }
                : { type: "devices", data: { items: [], next_cursor: null } },
            ),
      { tokens },
    );
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "network_error" });
    ready = true;
    await c.listDevices({ scope: "mine" });
    const keys = calls.filter((c) => c.url.endsWith("refresh")).map((c) => c.headers["Idempotency-Key"]);
    expect(new Set(keys).size).toBe(1);
    expect(keys.length).toBe(3);
  });
  it("imports with explicit private visibility, selected org and one stable retry key", async () => {
    const { client: c, calls } = client((_call, index) =>
      index === 0 ? new TypeError("connection closed") : json(200, { type: "device", data: { device_id: "1234abcd" } }),
    );
    await c.importDevice("1234abcd", "personal", "import-1");
    expect(calls).toHaveLength(2);
    for (const call of calls) {
      expect(call.headers["X-Org-ID"]).toBe("acme");
      expect(call.headers["Idempotency-Key"]).toBe("import-1");
      expect(call.body).toEqual({ type: "device_import", data: { visibility: "personal" } });
    }
  });
});

it("a delayed refresh cannot overwrite a newer login in the same account and organization", () => createRoot(dispose => {
  const s = createSession(), original = pair();
  s.client().ctx.onLogin!(original);
  const pending = s.client();
  const newer = pair({access_token:"new-login-access",refresh_token:"new-login-refresh"});
  s.client().ctx.onLogin!(newer);
  expect(() => pending.ctx.tokens.save(pair({access_token:"old-family-refresh"}), original.refresh_token)).toThrow("saved login changed");
  expect(s.pair()?.access_token).toBe("new-login-access");
  dispose();
}));


it("a refused old refresh cannot clear a newer login in the same organization", async () => {
  const tokens = memoryStore(pair({expires_at:NOW}));
  const newer = pair({access_token:"new-login",refresh_token:"new-refresh"});
  const {client:c} = client(() => {
    tokens.value = newer;
    return json(401,{type:"error",data:{code:"token_expired",message:"Old family ended"}});
  },{tokens});
  await expect(c.listDevices({scope:"mine"})).rejects.toMatchObject({code:"token_expired"});
  expect(tokens.value).toEqual(newer);
});


it("late typed-popup verification cannot switch back after an account round trip", async () => createRoot(async dispose => {
  const s = createSession(), original = pair(), other = pair({teams:["labs"],access_token:"labs",refresh_token:"labs-r"});
  s.client().ctx.onLogin!(original);
  const c = s.client();
  let finish!: () => void, began!: () => void;
  const started = new Promise<void>(resolve => began = resolve);
  const hold = new Promise<void>(resolve => finish = resolve);
  c.ctx.fetch = async input => {
    if (String(input).endsWith("/auth/login")) return json(200,{type:"login",data:authSession("late-login","late-refresh")});
    began(); await hold;
    return json(200,{type:"me",data:{authenticated:true,member:original.member,teams:original.teams}});
  };
  const pending = c.login("oac_popup", "carbon");
  await started;
  s.client().ctx.onLogin!(other);
  s.selectContext(contextId(original)!);
  finish();
  await expect(pending).rejects.toMatchObject({code:"context_changed"});
  expect(s.pair()?.access_token).toBe(original.access_token);
  expect(s.contexts()).toHaveLength(2);
  dispose();
}));

it("refreshing an existing family does not invalidate the selected popup context", () => createRoot(dispose => {
  const s = createSession(), original = pair();
  s.client().ctx.onLogin!(original);
  const c = s.client(), revision = s.contextRevision();
  c.ctx.tokens.save({...original,access_token:"rotated",refresh_token:"rotated-r"},original.refresh_token);
  expect(s.contextRevision()).toBe(revision);
  c.ctx.onLogin!(pair({access_token:"popup-login",refresh_token:"popup-r"}));
  expect(s.pair()?.access_token).toBe("popup-login");
  dispose();
}));
