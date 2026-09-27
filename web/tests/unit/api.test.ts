import { describe, expect, it, vi } from "vitest";
import { ApiError, ifMatchValue } from "../../src/lib/api";
import { client, json, memoryStore, NOW, pair, session } from "./helpers";

const DEVICES = { type: "devices", data: { items: [], next_cursor: null } };

describe("envelopes", () => {
  it("returns data from a {type, data} envelope", async () => {
    const { client: c } = client(() => json(200, { type: "devices", data: { items: [{ device_id: "7c1e09ab" }], next_cursor: null } }));
    const page = await c.listDevices({ scope: "mine" });
    expect(page.items[0].device_id).toBe("7c1e09ab");
  });

  it("rejects a body without an envelope", async () => {
    const { client: c } = client(() => json(200, { items: [] }));
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "unexpected_response" });
  });

  it("rejects an envelope of the wrong type", async () => {
    const { client: c } = client(() => json(200, { type: "device", data: {} }));
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "unexpected_response", message: expect.stringContaining('"devices"') });
  });

  it("treats 204 as no content", async () => {
    const { client: c } = client(() => new Response(null, { status: 204 }));
    await expect(c.revokeAccess("7c1e09ab", "si:chef")).resolves.toBeUndefined();
  });
});

describe("errors", () => {
  it("carries code, message, hint, docs link, request id and details from the service", async () => {
    const body = {
      type: "error",
      data: {
        code: "device_in_use",
        message: "Device 7c1e09ab is being used by si:chef.",
        hint: "Ask for it with: extend request send 7c1e09ab",
        docs_url: "https://docs.extend.teamofsilicons.com/errors#device_in_use",
        request_id: "01926f41",
        details: { in_use: { silicon_id: "si:chef" } },
      },
    };
    const { client: c } = client(() => json(409, body));
    const error = await c.stopDevice("7c1e09ab").catch((e) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 409,
      code: "device_in_use",
      message: "Device 7c1e09ab is being used by si:chef.",
      hint: "Ask for it with: extend request send 7c1e09ab",
      docsUrl: "https://docs.extend.teamofsilicons.com/errors#device_in_use",
      requestId: "01926f41",
      details: { in_use: { silicon_id: "si:chef" } },
    });
  });

  it("falls back to the X-Request-ID header when the body has no request id", async () => {
    const { client: c } = client(() => json(404, { type: "error", data: { code: "device_not_found", message: "No device." } }, { "X-Request-ID": "hdr-9" }));
    await expect(c.getDevice("7c1e09ab")).rejects.toMatchObject({ requestId: "hdr-9", hint: null });
  });

  it("explains a non-JSON failure instead of saying something went wrong", async () => {
    const { client: c } = client(() => new Response("<html>Bad gateway</html>", { status: 502, statusText: "Bad Gateway" }));
    const error: ApiError = await c.listDevices({ scope: "mine" }).catch((e) => e);
    expect(error.code).toBe("http_502");
    expect(error.message).toContain("HTTP 502");
    expect(error.hint).toBeTruthy();
    expect(error.message.toLowerCase()).not.toContain("something went wrong");
  });

  it("names the service it could not reach, and retries a GET once", async () => {
    const { client: c, calls } = client(() => new TypeError("Failed to fetch"));
    const error: ApiError = await c.listDevices({ scope: "mine" }).catch((e) => e);
    expect(error.code).toBe("network_error");
    expect(error.message).toContain("https://api.test");
    expect(error.message).toContain("Failed to fetch");
    expect(calls).toHaveLength(2);
  });

  it("does not retry a non-idempotent change after a network failure", async () => {
    const { client: c, calls } = client(() => new TypeError("Failed to fetch"));
    await expect(c.stopDevice("7c1e09ab")).rejects.toMatchObject({ code: "network_error" });
    expect(calls).toHaveLength(1);
  });

  it("retries a POST with an Idempotency-Key once, with the same key and body", async () => {
    const { client: c, calls } = client((_call, i) => (i === 0 ? new TypeError("reset") : json(201, { type: "device", data: { device_id: "aa11bb22" } }, { ETag: '"1"' })));
    const { device, etag } = await c.claimPairing({ pairing_code: "4F9C2A", name: "Pixel" });
    expect(device.device_id).toBe("aa11bb22");
    expect(etag).toBe('"1"');
    expect(calls).toHaveLength(2);
    expect(calls[0].headers["Idempotency-Key"]).toBe(calls[1].headers["Idempotency-Key"]);
    expect(calls[0].body).toEqual(calls[1].body);
  });
});

describe("headers", () => {
  it("sends the bearer token, team, API version and nothing test-related in production", async () => {
    const { client: c, calls } = client(() => json(200, DEVICES));
    await c.listDevices({ scope: "team", limit: 20 });
    const [call] = calls;
    expect(call.url).toBe("https://api.test/api/v1/devices?scope=team&limit=20");
    expect(call.headers.Authorization).toBe("Bearer oat_old");
    expect(call.headers["X-Org-ID"]).toBe("acme");
    expect(call.headers["Silicon-Extend-API-Version"]).toBe("1");
    expect(call.headers["X-Testing-Application-Secret"]).toBeUndefined();
    expect(call.headers["X-Extend-Telemetry"]).toBeUndefined();
  });

  it("sends the test app secret on every request in a test environment, including login and refresh", async () => {
    const secret = "ask_" + "a".repeat(43);
    const tokens = memoryStore(pair({ expires_at: NOW + 1000 }));
    const { client: c, calls } = client(
      (call) => (call.url.endsWith("/auth/refresh") ? json(200, { type: "refresh", data: session("oat_new", "ort_new") }) : call.url.endsWith("/auth/login") ? json(200, { type: "login", data: session("oat_x", "ort_x") }) : json(200, DEVICES)),
      { tokens, testingSecret: () => secret },
    );
    await c.listDevices({ scope: "mine" });
    await c.login("c:alice");
    expect(calls.map((x) => x.url.replace("https://api.test", ""))).toEqual(["/api/v1/auth/refresh", "/api/v1/devices?scope=mine", "/api/v1/auth/login"]);
    for (const call of calls) expect(call.headers["X-Testing-Application-Secret"]).toBe(secret);
  });

  it("sends X-Extend-Telemetry: off when telemetry is off, and sends no telemetry events", async () => {
    const { client: c, calls } = client(() => json(200, DEVICES), { telemetryOff: () => true });
    await c.listDevices({ scope: "mine" });
    await c.telemetry({ event: "pairing", step: "web.pairing.claim", success: true, duration_ms: 5 });
    expect(calls).toHaveLength(1);
    expect(calls[0].headers["X-Extend-Telemetry"]).toBe("off");
  });

  it("sends Idempotency-Key on login and pairing, and If-Match on changes", async () => {
    const { client: c, calls } = client((call) =>
      call.url.endsWith("/auth/login")
        ? json(200, { type: "login", data: session("oat_a", "ort_a") })
        : call.method === "PATCH"
          ? json(200, { type: "device", data: { device_id: "7c1e09ab", version: 4 } }, { ETag: '"4"' })
          : new Response(null, { status: 204 }),
    );
    await c.login("oac_x");
    await c.updateDevice("7c1e09ab", { pair_ttl_days: 7 }, '"3"');
    await c.removeDevice("7c1e09ab", '"4"');
    expect(calls[0].headers["Idempotency-Key"]).toBe("key-1");
    expect(calls[0].headers.Authorization).toBeUndefined();
    expect(calls[0].headers["X-Org-ID"]).toBeUndefined();
    expect(calls[0].body).toEqual({ type: "login", data: { slt: "oac_x" } });
    expect(calls[1].headers["If-Match"]).toBe('"3"');
    expect(calls[1].body).toEqual({ type: "device", data: { pair_ttl_days: 7 } });
    expect(calls[2].method).toBe("DELETE");
    expect(calls[2].headers["If-Match"]).toBe('"4"');
  });

  it("refuses to send a team-scoped request without a team, without calling the service", async () => {
    const { client: c, calls } = client(() => json(200, DEVICES), { team: () => null });
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "no_team_selected" });
    expect(calls).toHaveLength(0);
  });

  it("encodes path parameters", async () => {
    const { client: c, calls } = client(() => json(200, { type: "access_grant", data: {} }));
    await c.grantAccess("7c1e09ab", "si:chef");
    expect(calls[0].url).toBe("https://api.test/api/v1/devices/7c1e09ab/access/si%3Achef");
    expect(calls[0].method).toBe("PUT");
  });
});

describe("token refresh", () => {
  it("refreshes after a 401 and retries with the new token, storing the pair in one save", async () => {
    const { client: c, calls, tokens } = client((call) => {
      if (call.url.endsWith("/auth/refresh")) return json(200, { type: "refresh", data: session("oat_new", "ort_new") });
      return call.headers.Authorization === "Bearer oat_new" ? json(200, DEVICES) : json(401, { type: "error", data: { code: "token_expired", message: "expired" } });
    });
    await c.listDevices({ scope: "mine" });
    expect(calls.map((x) => x.url.split("/api/v1")[1])).toEqual(["/devices?scope=mine", "/auth/refresh", "/devices?scope=mine"]);
    expect(calls[1].body).toEqual({ type: "refresh", data: { refresh_token: "ort_old" } });
    expect(calls[1].headers["Idempotency-Key"]).toBeTruthy();
    expect(tokens.value).toMatchObject({ access_token: "oat_new", refresh_token: "ort_new", expires_at: NOW + 1800_000, teams: ["acme", "labs"] });
    expect(tokens.saves).toBe(1);
  });

  it("refreshes before sending when the access token is about to expire", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW + 10_000 }));
    const { client: c, calls } = client((call) => (call.url.endsWith("/auth/refresh") ? json(200, { type: "refresh", data: session("oat_new", "ort_new") }) : json(200, DEVICES)), { tokens });
    await c.listDevices({ scope: "mine" });
    expect(calls[0].url).toContain("/auth/refresh");
    expect(calls[1].headers.Authorization).toBe("Bearer oat_new");
  });

  it("runs one refresh for many concurrent requests (a reused refresh token would revoke the session)", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW - 1 }));
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const { client: c, calls } = client(
      async (call) => {
        if (call.url.endsWith("/auth/refresh")) {
          await gate;
          return json(200, { type: "refresh", data: session("oat_new", "ort_new") });
        }
        return json(200, DEVICES);
      },
      { tokens },
    );
    const all = Promise.all([c.listDevices({ scope: "mine" }), c.listDevices({ scope: "team" }), c.getSetup("7c1e09ab").catch(() => null), c.listAccess("7c1e09ab").catch(() => null)]);
    await new Promise((r) => setTimeout(r, 5));
    release();
    await all;
    expect(calls.filter((x) => x.url.endsWith("/auth/refresh"))).toHaveLength(1);
    for (const call of calls.filter((x) => !x.url.endsWith("/auth/refresh"))) expect(call.headers.Authorization).toBe("Bearer oat_new");
  });

  it("holds a cross-tab lock and skips refreshing when another tab already rotated the pair", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW - 1 }));
    const names: string[] = [];
    const lock = async <T,>(name: string, fn: () => Promise<T>): Promise<T> => {
      names.push(name);
      tokens.value = pair({ access_token: "oat_other_tab", refresh_token: "ort_other_tab" });
      return fn();
    };
    const { client: c, calls } = client(() => json(200, DEVICES), { tokens, lock });
    await c.listDevices({ scope: "mine" });
    expect(names).toEqual(["extend-refresh:production"]);
    expect(calls).toHaveLength(1);
    expect(calls[0].headers.Authorization).toBe("Bearer oat_other_tab");
  });

  it("signs out when the refresh is refused, and reports why", async () => {
    const onSignedOut = vi.fn();
    const tokens = memoryStore(pair());
    const { client: c } = client(
      (call) =>
        call.url.endsWith("/auth/refresh")
          ? json(401, { type: "error", data: { code: "token_expired", message: "The refresh token was already used.", hint: "Sign in again." } })
          : json(401, { type: "error", data: { code: "token_expired", message: "expired" } }),
      { tokens, onSignedOut },
    );
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "token_expired", message: "The refresh token was already used." });
    expect(tokens.value).toBeNull();
    expect(onSignedOut).toHaveBeenCalledWith(expect.objectContaining({ code: "token_expired" }));
  });

  it("keeps the tokens when the refresh fails for a network reason", async () => {
    const tokens = memoryStore(pair({ expires_at: NOW - 1 }));
    const { client: c } = client(() => new TypeError("offline"), { tokens });
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "network_error" });
    expect(tokens.value?.refresh_token).toBe("ort_old");
  });

  it("does not refresh when the test secret is what was refused", async () => {
    const { client: c, calls } = client(() => json(401, { type: "error", data: { code: "testing_secret_invalid", message: "The test secret is invalid." } }));
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "testing_secret_invalid" });
    expect(calls).toHaveLength(1);
  });

  it("needs a login for protected calls", async () => {
    const { client: c, calls } = client(() => json(200, DEVICES), { tokens: memoryStore(null) });
    await expect(c.listDevices({ scope: "mine" })).rejects.toMatchObject({ code: "not_signed_in", status: 401 });
    expect(calls).toHaveLength(0);
  });

  it("logout revokes the refresh token and forgets the pair even if the service is down", async () => {
    const tokens = memoryStore(pair());
    const { client: c, calls } = client(() => new TypeError("down"), { tokens });
    await expect(c.logout()).rejects.toMatchObject({ code: "network_error" });
    expect(tokens.value).toBeNull();
    expect(calls[0].body).toEqual({ type: "logout", data: { token: "ort_old" } });
  });
});

describe("testing environment validation", () => {
  it("sends the secret being validated, not the current world's", async () => {
    const { client: c, calls } = client(() => json(200, { type: "testing_environment", data: { environment_id: "e1", name: "checkout-e2e", state: "ready" } }), {
      testingSecret: () => "ask_" + "o".repeat(43),
    });
    const env = await c.testingEnvironment("ask_" + "n".repeat(43));
    expect(env.name).toBe("checkout-e2e");
    expect(calls[0].headers["X-Testing-Application-Secret"]).toBe("ask_" + "n".repeat(43));
    expect(calls[0].headers.Authorization).toBeUndefined();
  });
});

describe("ifMatchValue", () => {
  it("prefers a readable ETag, falls back to the version", () => {
    expect(ifMatchValue('"7"', 3)).toBe('"7"');
    expect(ifMatchValue('W/"7"', 3)).toBe('"7"');
    expect(ifMatchValue(null, 3)).toBe('"3"');
    expect(() => ifMatchValue(null, undefined)).toThrow(ApiError);
  });
});

describe("team Silicons and takeovers", () => {
  it("lists the team's Silicons with the team header", async () => {
    const { client: c, calls } = client(() => json(200, { type: "team_silicons", data: { items: [{ id: "si:chef", display_name: null }] } }));
    expect(await c.listTeamSilicons()).toEqual([{ id: "si:chef", display_name: null }]);
    expect(calls[0].url).toBe("https://api.test/api/v1/team/silicons");
    expect(calls[0].headers["X-Org-ID"]).toBe("acme");
  });

  it("reads a takeover (or null) and releases it", async () => {
    const { client: c, calls } = client((call, i) =>
      call.method === "DELETE" ? new Response(null, { status: 204 }) : json(200, { type: "takeover", data: i === 0 ? { takeover_id: "t", session_id: "a3f", reason: "Approve Face ID", started_at: "x", expires_at: "y" } : null }),
    );
    expect((await c.getTakeover("a3f"))?.reason).toBe("Approve Face ID");
    expect(await c.getTakeover("a3f")).toBeNull();
    await c.releaseTakeover("a3f");
    expect(calls.map((x) => `${x.method} ${x.url.replace("https://api.test", "")}`)).toEqual([
      "GET /api/v1/sessions/a3f/takeover",
      "GET /api/v1/sessions/a3f/takeover",
      "DELETE /api/v1/sessions/a3f/takeover",
    ]);
  });
});

describe("removed devices", () => {
  const device = (id: string, removed_at?: string) => ({ device_id: id, name: `Device ${id}`, os: "android", online: false, ...(removed_at ? { removed_at, removed_reason: "device_removed" } : {}) });

  it("sends include_removed=true only when asked", async () => {
    const { client: c, calls } = client(() => json(200, DEVICES));
    await c.listDevices({ scope: "mine" });
    await c.listDevices({ scope: "mine", include_removed: true });
    expect(new URL(calls[0].url).searchParams.has("include_removed")).toBe(false);
    expect(new URL(calls[1].url).searchParams.get("include_removed")).toBe("true");
    expect(new URL(calls[1].url).searchParams.get("scope")).toBe("mine");
  });

  it("reads every page and keeps only the removed ones, newest removal first", async () => {
    const pages = [
      { items: [device("00000001"), device("00000002", "2026-09-20T10:00:00Z")], next_cursor: "00000002" },
      { items: [device("00000003", "2026-09-25T10:00:00Z"), device("00000004")], next_cursor: null },
    ];
    const { client: c, calls } = client((_call, i) => json(200, { type: "devices", data: pages[i] }));
    const removed = await c.listRemovedDevices();
    expect(removed.map((d) => d.device_id)).toEqual(["00000003", "00000002"]);
    expect(calls).toHaveLength(2);
    expect(new URL(calls[1].url).searchParams.get("cursor")).toBe("00000002");
    expect(calls.every((call) => new URL(call.url).searchParams.get("include_removed") === "true")).toBe(true);
  });
});
