import { describe, expect, it } from "vitest";
import { activitySummary, awakeLabel, removedWhy, statusLabel, wakeEnd } from "../../src/lib/format";

describe("activitySummary (details as the service logs them)", () => {
  it.each([
    ["renamed", { name: { from: "Pixel", to: "Pixel 9" } }, "Renamed from “Pixel” to “Pixel 9”"],
    ["settings_changed", { pair_ttl_days: 9 }, "Stays paired 9 days without activity"],
    ["settings_changed", { visibility: "personal" }, "Visible only to its owner"],
    ["settings_changed", { in_use_indicator: "hidden" }, "Turned off the banner while a Silicon uses it"],
    ["settings_changed", { in_use_indicator: { from: "hidden", to: "shown" } }, "Turned on the banner while a Silicon uses it"],
    ["settings_changed", { name: { from: "A", to: "B" }, pair_ttl_days: 1, visibility: "team" }, "Renamed from “A” to “B”, stays paired 1 day without activity, visible to the Team"],
    ["access_granted", { silicon_id: "si:chef" }, "Gave si:chef access"],
    ["access_revoked", { silicon_id: "si:chef", reason: "left_team" }, "Took access away from si:chef (it, or you, left its Team)"],
    ["session_started", {}, "Started a session"],
    ["session_ended", { reason: "stopped_by_carbon", explain: "…" }, "Session ended: stopped by you"],
    ["request_sent", { to: "si:chef", reason: "Need the OTP" }, "Asked si:chef for the device: “Need the OTP”"],
    ["takeover_started", { reason: "Approve Face ID" }, "Handed the device to you: “Approve Face ID”"],
    ["paired", { name: "Pixel", access: ["si:chef"] }, "Paired as “Pixel”, gave access to si:chef"],
    ["paired", { name: "Apple TV", through: "2e7f00d1" }, "Paired as “Apple TV” through 2e7f00d1"],
    // The last entries of a removed device's log, as unpair writes them.
    ["removed", { reason: "device_removed" }, "Removed the device"],
    // 1.0 removed a device for left_team when the Carbon who paired it left the Team; 1.1 keeps the device.
    ["removed", { reason: "left_team" }, "Removed: its Carbon left the Team"],
    ["removed", { reason: "environment_cleaned" }, "Removed: the test environment was cleaned"],
    // A session ends for left_team when its Silicon, or the Carbon who gave it access, left the Silicon's Team.
    ["session_ended", { reason: "left_team", explain: "a member left the team" }, "Session ended: the Silicon, or the Carbon who gave it access, left the Silicon's Team"],
    // 1.1: several Carbons, waking.
    ["session_ended", { reason: "stopped_by_carbon", stopped_by: "another_carbon" }, "Session ended: stopped by another Carbon who paired this device"],
    ["another_carbon_paired", {}, "Another Carbon paired this device. Their pair is separate: you don't see their Silicons, and they don't see yours"],
    ["paired", { name: "Family TV", with_existing_pairs: true }, "Paired as “Family TV” (another Carbon had already paired it)"],
    ["connection_replaced", { while_in_use_by_other: false }, "Connection replaced: another connection took over this pair. If you didn't reconnect the app, check the device"],
    ["duplicate_device", { kind: "other_computer" }, "Duplicate device: it was already added through another computer"],
    ["duplicate_device", { kind: "same_carbon" }, "Duplicate device: you already added it through another pair, so this one isn't used"],
    ["request_received", { from: "si:sous", reason: "Need the TV for the match" }, "si:sous asked you for the device: “Need the TV for the match”"],
    ["request_sent", { to: "the Carbon who gave access to the Silicon using it", routed_to: "carbon", reason: "2 minutes" }, "Asked the Carbon who gave access to the Silicon using it for the device: “2 minutes”"],
    ["wake_requested", { reason: "Need the screen" }, "Asked you to wake it: “Need the screen”"],
    ["wake_refreshed", { reason: "Still need it" }, "Asked again to wake it: “Still need it”"],
    ["woken", {}, "Woke up; every Silicon that asked was told"],
    ["wake_confirmed", {}, "Said it's awake; every Silicon that asked was told"],
    ["wake_declined", { silicon_id: "si:atlas" }, "Declined si:atlas's request to wake it"],
    ["wake_withdrawn", { reason: "access_removed" }, "A request to wake it ended: its access was taken away"],
    ["wake_muted", { silicon_id: "si:atlas" }, "Turned wake requests off for si:atlas"],
    ["wake_unmuted", {}, "Turned wake requests on"],
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
    [{ removed_reason: "left_team" }, "Its Carbon left the Team"],
    [{ removed_reason: "environment_cleaned" }, "The test environment was cleaned"],
    [{ removed_reason: "something_new" }, "Something new"],
    [{}, "You removed it"],
  ])("%j", (device, text) => {
    expect(removedWhy(device)).toBe(text);
  });
});

describe("awakeLabel", () => {
  it.each([
    [{ online: true, awake: true }, "Awake", "awake"],
    [{ online: true, awake: false, sleep_state: "screen_off" }, "Asleep: screen off", "asleep"],
    [{ online: true, awake: false, sleep_state: "locked" }, "Locked", "asleep"],
    [{ online: true, awake: false, sleep_state: "standby" }, "In standby", "asleep"],
    [{ online: true, awake: false, sleep_state: "other_session" }, "Another account is in use", "asleep"],
    [{ online: true, awake: false, sleep_state: "dozing" }, "Not awake: dozing", "asleep"],
    [{ online: false, last_sleep_state: "asleep" }, "Offline, last seen asleep", "asleep"],
    // iPhones, iPads and apps older than 1.1: Extend can't tell (a 1.1 service leaves `awake` out).
    [{ online: true, wake_detectable: false }, "Awake: unknown", "unknown"],
  ])("%j", (device, text, state) => {
    expect(awakeLabel(device)).toEqual({ text, state });
  });

  it("says nothing for an offline device with no last state, a removed one, or a 1.0 service's device", () => {
    expect(awakeLabel({ online: false })).toBeNull();
    expect(awakeLabel({ online: true })).toBeNull();
    expect(awakeLabel({ online: false, removed_at: "2026-09-20T10:00:00Z", last_sleep_state: "asleep" })).toBeNull();
  });
});

describe("wakeEnd", () => {
  it("says why a wake request ended, and keeps a reason it doesn't know", () => {
    expect(wakeEnd("confirmed_by_carbon")).toBe("a Carbon said it's awake");
    expect(wakeEnd("woken_on_device")).toBe("the device woke up");
    expect(wakeEnd("new_reason")).toBe("new reason");
    expect(wakeEnd(null)).toBe("");
  });
});
