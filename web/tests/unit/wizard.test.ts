import { describe, expect, it } from "vitest";
import { canAdvance, initialState, nameProblem, reduce, stepsFor, type WizardEvent, type WizardState } from "../../src/lib/wizard";
import { deviceKind } from "../../src/config";
import type { Device } from "../../src/lib/types";

const device = (state: "setup" | "ready" = "setup"): Device => ({
  device_id: "aa11bb22",
  name: "Saket's Pixel",
  os: "android",
  kind: "phone",
  owner: { type: "carbon", id: "c:saket" },
  visibility: "team",
  online: true,
  state,
});

const run = (state: WizardState, ...events: WizardEvent[]) => events.reduce(reduce, state);

describe("wizard steps", () => {
  it("app devices go kind → guide → code → name → setup → access → done", () => {
    expect(stepsFor(deviceKind("android"))).toEqual(["kind", "guide", "code", "name", "setup", "access", "done"]);
  });
  it("devices through a computer pick a host instead of a code", () => {
    for (const k of ["iphone", "ipad", "apple_tv", "samsung_tv", "lg_tv"]) expect(stepsFor(deviceKind(k))).toEqual(["kind", "guide", "host", "name", "setup", "access", "done"]);
  });
});

describe("wizard reducer", () => {
  it("walks an Android phone from kind to done", () => {
    let s = initialState();
    expect(s.step).toBe("kind");
    expect(canAdvance(s)).toBe(false);
    s = run(s, { type: "choose_kind", kind: "android" });
    expect(s.step).toBe("guide");
    s = run(s, { type: "next" });
    expect(s.step).toBe("code");
    s = run(s, { type: "set_code", code: "4f9-c2" }, { type: "next" });
    expect(s.step).toBe("code");
    s = run(s, { type: "set_code", code: "4F9-C2A" }, { type: "next" });
    expect(s.step).toBe("name");
    s = run(s, { type: "set_name", name: "  " }, { type: "submit" });
    expect(s.submitting).toBe(false);
    s = run(s, { type: "set_name", name: "Saket's Pixel" }, { type: "set_ttl", days: 40 });
    expect(s.ttlDays).toBe(30);
    s = run(s, { type: "submit" });
    expect(s.submitting).toBe(true);
    expect(canAdvance(s)).toBe(false);
    s = run(s, { type: "created", device: device() });
    expect(s).toMatchObject({ step: "setup", submitting: false, setupComplete: false });
    s = run(s, { type: "back" });
    expect(s.step).toBe("setup");
    s = run(s, { type: "setup_complete" }, { type: "next" });
    expect(s.step).toBe("access");
    s = run(s, { type: "access_done", granted: ["si:chef"] });
    expect(s).toMatchObject({ step: "done", granted: ["si:chef"] });
  });

  it("goes back to the code step when the code was wrong, keeping the name", () => {
    const s = run(
      initialState("android"),
      { type: "next" },
      { type: "set_code", code: "4F9C2A" },
      { type: "next" },
      { type: "set_name", name: "Pixel" },
      { type: "submit" },
      { type: "failed", error: { code: "pairing_code_invalid", message: "That pairing code is wrong, expired or already used.", hint: "Codes rotate every 5 minutes." } },
    );
    expect(s).toMatchObject({ step: "code", name: "Pixel", submitting: false, error: { code: "pairing_code_invalid" } });
    expect(run(s, { type: "set_code", code: "AAAAAA" }).error).toBeNull();
  });

  it("stays on the name step for the test device limit, showing the service's message", () => {
    const message = "In test environment you are limited to 5 paired devices per environment.";
    const s = run(initialState("mac"), { type: "next" }, { type: "set_code", code: "B00C1E" }, { type: "next" }, { type: "set_name", name: "Mac" }, { type: "submit" }, { type: "failed", error: { code: "test_device_limit", message, hint: null } });
    expect(s).toMatchObject({ step: "name", error: { code: "test_device_limit", message } });
  });

  it("an iPhone picks a host, and goes back to it if the host went offline", () => {
    let s = run(initialState(), { type: "choose_kind", kind: "iphone" }, { type: "next" });
    expect(s.step).toBe("host");
    expect(canAdvance(s)).toBe(false);
    s = run(s, { type: "set_host", hostId: "2e7f00d1" }, { type: "next" }, { type: "set_name", name: "iPhone" }, { type: "submit" });
    s = run(s, { type: "failed", error: { code: "device_offline", message: "MacBook Pro is offline", hint: null } });
    expect(s.step).toBe("host");
  });

  it("skips choosing Silicons", () => {
    const s = run(initialState("android"), { type: "next" }, { type: "set_code", code: "4F9C2A" }, { type: "next" }, { type: "set_name", name: "P" }, { type: "submit" }, { type: "created", device: device("ready") }, { type: "next" }, { type: "skip_access" });
    expect(s).toMatchObject({ step: "done", granted: [], setupComplete: true });
  });

  it("can't change the kind once the device exists", () => {
    const s = run(initialState("android"), { type: "next" }, { type: "set_code", code: "4F9C2A" }, { type: "next" }, { type: "set_name", name: "P" }, { type: "submit" }, { type: "created", device: device() });
    expect(run(s, { type: "choose_kind", kind: "mac" })).toBe(s);
  });

  it("back walks one step at a time before pairing", () => {
    const s = run(initialState("android"), { type: "next" }, { type: "set_code", code: "4F9C2A" }, { type: "next" });
    expect(run(s, { type: "back" }).step).toBe("code");
    expect(run(s, { type: "back" }, { type: "back" }).step).toBe("guide");
    expect(run(s, { type: "back" }, { type: "back" }, { type: "back" }).step).toBe("kind");
  });

  it("checks names like the service does", () => {
    expect(nameProblem("")).toBeTruthy();
    expect(nameProblem("x".repeat(65))).toContain("64");
    expect(nameProblem("ok\u0007")).toContain("control");
    expect(nameProblem("Saket's Pixel")).toBeNull();
  });
});
