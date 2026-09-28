import { describe, expect, it } from "vitest";
import { registerCommand, TING_TYPES, tingType } from "../../src/lib/ting";

describe("Extend's Ting types", () => {
  it("are the four the protocol names", () => {
    expect(TING_TYPES.map((t) => t.event)).toEqual(["device.requested", "device.wake_requested", "device.woken", "device.wake_declined"]);
    expect(tingType("extend.device.woken")?.description).toBe("A device a Silicon asked to wake is awake");
    expect(tingType("device.requested")?.event).toBe("device.requested");
    expect(tingType("extend.device.unknown")).toBeUndefined();
  });

  it("builds the exact register command the CLI prints (crates/extend-protocol ting.rs)", () => {
    expect(registerCommand("extend.device.wake_requested")).toBe(
      "ting --org '<owning-team>' types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'",
    );
    expect(registerCommand("device.woken")).toBe("ting --org '<owning-team>' types register --type extend.device.woken --description 'A device a Silicon asked to wake is awake'");
  });
});
