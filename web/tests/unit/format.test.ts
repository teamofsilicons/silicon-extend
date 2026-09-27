import { describe, expect, it } from "vitest";
import { activitySummary, removedWhy, statusLabel } from "../../src/lib/format";

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
    // The last entries of a removed device's log, as unpair writes them.
    ["removed", { reason: "device_removed" }, "Removed the device"],
    // Extend removes a device for left_team only when the Carbon who paired it left the team.
    ["removed", { reason: "left_team" }, "Removed: its Carbon left the team"],
    ["removed", { reason: "environment_cleaned" }, "Removed: the test environment was cleaned"],
    // A session ends for left_team when either its Silicon or the device's Carbon left.
    ["session_ended", { reason: "left_team", explain: "a member left the team" }, "Session ended: the Silicon or the device's Carbon left the team"],
    ["pair_revoked", { reason: "pair_revoked" }, "Revoked the pair"],
    ["pair_expired", { reason: "pair_expired" }, "The pair ended: unused for longer than its pairing lasts"],
  ])("%s %j", (action, details, text) => {
    expect(activitySummary(action, details)).toBe(text);
  });

  it("still shows actions and details it doesn't know", () => {
    expect(activitySummary("firmware_updated", { version: "2.1" })).toBe("firmware_updated (version: 2.1)");
    expect(activitySummary("something_new", null)).toBe("something_new");
  });
});

describe("statusLabel", () => {
  it("says what the dot says, and a paused session is paused for the Carbon, not in use", () => {
    expect(statusLabel({ online: true })).toBe("Online");
    expect(statusLabel({ online: false })).toBe("Offline");
    expect(statusLabel({ online: true, inUse: true })).toBe("In use");
    expect(statusLabel({ online: true, inUse: true, paused: true })).toBe("Paused for you");
  });
});

describe("removedWhy", () => {
  it.each([
    [{ removed_reason: "device_removed" }, "You removed it"],
    [{ removed_reason: "device_removed", host_device_id: "2e7f00d1" }, "You removed it, or the computer it paired through"],
    [{ removed_reason: "pair_revoked" }, "The pair was revoked on the device itself"],
    [{ removed_reason: "pair_revoked", host_device_id: "2e7f00d1" }, "The computer it paired through had its pair revoked"],
    [{ removed_reason: "pair_expired", pair_ttl_days: 14 }, "It went unused for longer than its pairing lasts (14 days)"],
    [{ removed_reason: "left_team" }, "Its Carbon left the team"],
    [{ removed_reason: "environment_cleaned" }, "The test environment was cleaned"],
    [{ removed_reason: "something_new" }, "Something new"],
    [{}, "You removed it"],
  ])("%j", (device, text) => {
    expect(removedWhy(device)).toBe(text);
  });
});
