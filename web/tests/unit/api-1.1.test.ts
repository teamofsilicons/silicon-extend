import { describe, expect, it } from "vitest";
import { client, json } from "./helpers";

const path = (url: string) => url.replace("https://api.test", "");

describe("access per Team (1.1)", () => {
  it("gives access in the Team the Carbon picked, as ?team=, keeping X-Org-ID for the default Team", async () => {
    const { client: c, calls } = client(() => json(200, { type: "access_grant", data: { device_id: "7c1e09ab", silicon_id: "si:juniper", team: "labs" } }));
    const grant = await c.grantAccess("7c1e09ab", "si:juniper", "labs");
    expect(grant.team).toBe("labs");
    expect(calls[0].method).toBe("PUT");
    expect(path(calls[0].url)).toBe("/api/v1/devices/7c1e09ab/access/si%3Ajuniper?team=labs");
    expect(calls[0].headers["X-Org-ID"]).toBe("acme");
  });

  it("takes access away in one Team, or in every Team without one", async () => {
    const { client: c, calls } = client(() => new Response(null, { status: 204 }));
    await c.revokeAccess("7c1e09ab", "si:chef", "labs");
    await c.revokeAccess("7c1e09ab", "si:chef");
    expect(calls.map((x) => `${x.method} ${path(x.url)}`)).toEqual(["DELETE /api/v1/devices/7c1e09ab/access/si%3Achef?team=labs", "DELETE /api/v1/devices/7c1e09ab/access/si%3Achef"]);
  });

  it("lists every Team's Silicons with team=any, and says which Teams couldn't be read", async () => {
    const { client: c, calls } = client(() =>
      json(200, {
        type: "team_silicons",
        data: {
          items: [
            { id: "si:chef", team: "acme" },
            { id: "si:juniper", team: "labs" },
          ],
          teams: [
            { team: "acme", ok: true },
            { team: "labs", ok: true },
            { team: "globex", ok: false, error: { code: "not_a_team_member", message: "c:saket's Extend login doesn't reach globex." } },
          ],
        },
      }),
    );
    const all = await c.listAllTeamSilicons();
    expect(path(calls[0].url)).toBe("/api/v1/team/silicons?team=any");
    expect(all.across).toBe(true);
    expect(all.items.map((m) => `${m.team}/${m.id}`)).toEqual(["acme/si:chef", "labs/si:juniper"]);
    expect(all.teams.find((t) => !t.ok)?.error?.code).toBe("not_a_team_member");
  });

  it("knows a 1.0 service answered only the selected Team (no `teams`), and can read one Team with X-Org-ID set to it", async () => {
    const { client: c, calls } = client(() => json(200, { type: "team_silicons", data: { items: [{ id: "si:chef" }] } }));
    expect((await c.listAllTeamSilicons()).across).toBe(false);
    await c.listTeamSilicons("labs");
    expect(calls[1].headers["X-Org-ID"]).toBe("labs");
  });
});

describe("stop across Carbons (1.1)", () => {
  it("returns the stopped session when it ran through the Carbon's own pair", async () => {
    const { client: c } = client(() => json(200, { type: "session", data: { session_id: "a3f", silicon_id: "si:chef" } }));
    const stopped = await c.stopDevice("7c1e09ab");
    expect(stopped.kind).toBe("session");
    if (stopped.kind === "session") expect(stopped.session.silicon_id).toBe("si:chef");
  });

  it("returns device_stopped, naming no Silicon, when it ran through another Carbon's pair", async () => {
    const { client: c } = client(() => json(200, { type: "device_stopped", data: { device_id: "5a1e7f00", stopped_at: "2026-09-27T10:00:00Z", in_use_by_other: true } }));
    const stopped = await c.stopDevice("5a1e7f00");
    expect(stopped).toEqual({ kind: "other", stopped: { device_id: "5a1e7f00", stopped_at: "2026-09-27T10:00:00Z", in_use_by_other: true } });
  });

  it("carries the service's words when a carried device can't be stopped from here", async () => {
    const message = "A device carried by Studio Mac is in use. It can be stopped by the Carbon who paired it, or from Studio Mac's Extend app.";
    const { client: c } = client(() => json(409, { type: "error", data: { code: "conflict", message } }));
    await expect(c.stopDevice("7d3e2f10")).rejects.toMatchObject({ status: 409, code: "conflict", message });
  });
});

describe("setup retry (contract A)", () => {
  it("posts the step to retry, and returns the keys the device was asked to run again", async () => {
    const { client: c, calls } = client(() => json(202, { type: "setup_retry", data: { retrying: ["wireless_debugging"] } }));
    expect(await c.retrySetup("7c1e09ab", "wireless_debugging")).toEqual({ retrying: ["wireless_debugging"] });
    expect(calls[0].method).toBe("POST");
    expect(path(calls[0].url)).toBe("/api/v1/devices/7c1e09ab/setup/retry");
    expect(calls[0].body).toEqual({ type: "setup_retry", data: { step: "wireless_debugging" } });
  });

  it("sends an empty body for every failed step", async () => {
    const { client: c, calls } = client(() => json(202, { type: "setup_retry", data: { retrying: ["a", "b"] } }));
    expect((await c.retrySetup("7c1e09ab")).retrying).toEqual(["a", "b"]);
    expect(calls[0].body).toEqual({ type: "setup_retry", data: {} });
  });

  it.each([
    [409, "device_offline", "Saket's Pixel is offline. Setup carries on when it reconnects."],
    [409, "conflict", "Nothing to retry: no setup step has failed."],
    [426, "upgrade_required", "Saket's Pixel runs Silicon Extend 1.0.2, which can't retry from here. Update it to 1.1, or tap Retry on the device."],
    [429, "rate_limited", "Wait a few seconds before retrying again."],
  ])("keeps the service's message for HTTP %i %s", async (status, code, message) => {
    const { client: c } = client(() => json(status, { type: "error", data: { code, message, details: status === 429 ? { retry_after_s: 4 } : {} } }));
    await expect(c.retrySetup("7c1e09ab", "wireless_debugging")).rejects.toMatchObject({ status, code, message });
  });
});

describe("waking (1.1)", () => {
  it("lists a pair's wake requests", async () => {
    const { client: c, calls } = client(() => json(200, { type: "wake_requests", data: { items: [{ wake_id: "w1", from: "si:atlas", team: "acme", state: "open" }], next_cursor: null } }));
    const page = await c.listWakeRequests("2e7f00d1", { state: "open" });
    expect(page.items[0].from).toBe("si:atlas");
    expect(path(calls[0].url)).toBe("/api/v1/devices/2e7f00d1/wake-requests?state=open");
  });

  it("answers It's awake for the whole device, and Decline for some requests, with an Idempotency-Key", async () => {
    const { client: c, calls } = client((call) => json(200, { type: "wake_answer", data: { answer: (call.body as { data: { answer: string } }).data.answer, ended: [] } }));
    await c.answerWake("2e7f00d1", "woken");
    await c.answerWake("2e7f00d1", "declined", ["w1"]);
    expect(calls[0].body).toEqual({ type: "wake_answer", data: { answer: "woken" } });
    expect(calls[1].body).toEqual({ type: "wake_answer", data: { answer: "declined", wake_ids: ["w1"] } });
    expect(calls.every((x) => x.headers["Idempotency-Key"])).toBe(true);
    expect(path(calls[0].url)).toBe("/api/v1/devices/2e7f00d1/wake-requests/answer");
  });

  it("turns wake requests off for the pair, or for one Silicon in one Team", async () => {
    const { client: c, calls } = client(() => json(200, { type: "wake_settings", data: { device_id: "2e7f00d1", muted: false, silicons_muted: [] } }));
    await c.setWakeSettings("2e7f00d1", { muted: true });
    await c.setWakeSettings("2e7f00d1", { muted: true, silicon_id: "si:atlas", team: "acme" });
    expect(calls[0].method).toBe("PUT");
    expect(calls[0].body).toEqual({ type: "wake_settings", data: { muted: true } });
    expect(calls[1].body).toEqual({ type: "wake_settings", data: { muted: true, silicon_id: "si:atlas", team: "acme" } });
  });
});

describe("Ting registration (1.1)", () => {
  const row = (team: string) => ({ team, member: "c:saket", status: "on", missing_types: [] });
  it("reads every Team, whether the answer is a page, a list or one registration", async () => {
    for (const data of [{ items: [row("acme"), row("labs")], next_cursor: null }, [row("acme"), row("labs")]]) {
      const { client: c, calls } = client(() => json(200, { type: "ting_registrations", data }));
      expect((await c.getTingRegistrations("any")).map((r) => r.team)).toEqual(["acme", "labs"]);
      expect(path(calls[0].url)).toBe("/api/v1/ting-registration?team=any");
    }
    const { client: c } = client(() => json(200, { type: "ting_registration", data: row("labs") }));
    expect((await c.getTingRegistrations("labs")).map((r) => r.team)).toEqual(["labs"]);
  });

  it("turns Tings on in one Team", async () => {
    const { client: c, calls } = client(() => json(200, { type: "ting_registration", data: row("labs") }));
    expect((await c.turnOnTing("labs")).status).toBe("on");
    expect(`${calls[0].method} ${path(calls[0].url)}`).toBe("PUT /api/v1/ting-registration?team=labs");
  });
});

describe("pairing (1.1)", () => {
  it("claims without visibility or silicon_ids (access is given afterwards, per Team)", async () => {
    const { client: c, calls } = client(() => json(201, { type: "device", data: { device_id: "aa11bb22", paired_by_others: true } }, { ETag: '"1"' }));
    const { device } = await c.claimPairing({ pairing_code: "4F9C2A", name: "Family TV", pair_ttl_days: 14 });
    expect(device.paired_by_others).toBe(true);
    expect(calls[0].body).toEqual({ type: "pairing", data: { pairing_code: "4F9C2A", name: "Family TV", pair_ttl_days: 14 } });
  });

  it("refuses an answer of a type the route never gives", async () => {
    const { client: c } = client(() => json(200, { type: "device", data: {} }));
    await expect(c.stopDevice("7c1e09ab")).rejects.toMatchObject({ code: "unexpected_response", message: 'Expected a "session" or "device_stopped" response from Extend, got "device".' });
  });
});
