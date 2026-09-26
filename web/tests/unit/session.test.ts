import { describe, expect, it } from "vitest";

const SECRET = "ask_" + "sessiontest".padEnd(43, "0");
const ENVIRONMENT = { environment_id: "9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c", name: "checkout-e2e", state: "ready" as const };

describe("test environment world", () => {
  it("re-reading an unchanged environment keeps the same world, so nothing that follows it re-runs", async () => {
    // The world is read from sessionStorage when the session is first created.
    sessionStorage.setItem("extend.testing", JSON.stringify({ secret: SECRET, environment: ENVIRONMENT }));
    const { session } = await import("../../src/lib/session");
    const s = session();
    const before = s.world();
    expect(before.kind).toBe("testing");

    s.updateEnvironment({ ...ENVIRONMENT });
    expect(s.world()).toBe(before);

    s.updateEnvironment({ ...ENVIRONMENT, state: "cleaning" });
    const after = s.world();
    expect(after).not.toBe(before);
    expect(after.kind === "testing" && after.environment.state).toBe("cleaning");
    expect(JSON.parse(sessionStorage.getItem("extend.testing")!).environment.state).toBe("cleaning");
  });
});
