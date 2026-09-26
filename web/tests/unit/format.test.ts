import { describe, expect, it } from "vitest";
import { activitySummary } from "../../src/lib/format";

describe("activitySummary (details as the service logs them)", () => {
  it.each([
    ["renamed", { name: { from: "Pixel", to: "Pixel 9" } }, "Renamed from “Pixel” to “Pixel 9”"],
    ["settings_changed", { pair_ttl_days: 9 }, "Stays paired 9 days without activity"],
    ["settings_changed", { visibility: "personal" }, "Visible only to its owner"],
    ["settings_changed", { name: { from: "A", to: "B" }, pair_ttl_days: 1, visibility: "team" }, "Renamed from “A” to “B”, stays paired 1 day without activity, visible to the team"],
    ["access_granted", { silicon_id: "si:chef" }, "Gave si:chef access"],
    ["access_revoked", { silicon_id: "si:chef", reason: "left_team" }, "Took access away from si:chef (it left the team)"],
    ["session_started", {}, "Started a session"],
    ["session_ended", { reason: "stopped_by_carbon", explain: "…" }, "Session ended: stopped by you"],
    ["request_sent", { to: "si:chef", reason: "Need the OTP" }, "Asked si:chef for the device: “Need the OTP”"],
    ["takeover_started", { reason: "Approve Face ID" }, "Handed the device to you: “Approve Face ID”"],
    ["paired", { name: "Pixel", access: ["si:chef"] }, "Paired as “Pixel”, gave access to si:chef"],
    ["paired", { name: "Apple TV", through: "2e7f00d1" }, "Paired as “Apple TV” through 2e7f00d1"],
  ])("%s %j", (action, details, text) => {
    expect(activitySummary(action, details)).toBe(text);
  });

  it("still shows actions and details it doesn't know", () => {
    expect(activitySummary("pair_expired", { days: 14 })).toBe("Pair expired (days: 14)");
    expect(activitySummary("something_new", null)).toBe("something_new");
  });
});
