import { describe, expect, it } from "vitest";
import { displayPairingCode, formatCodeInput, normalizePairingCode, normalizeSetupCode, parseSiliconIds } from "../../src/lib/pairing";

describe("normalizePairingCode", () => {
  it.each([
    ["4F9C2A", "4F9C2A"],
    ["4f9c2a", "4F9C2A"],
    ["4F9-C2A", "4F9C2A"],
    ["4f9 c2a", "4F9C2A"],
    [" 4F9 - C2A ", "4F9C2A"],
    ["4f9.c2a", "4F9C2A"],
    ["4F9_C2A", "4F9C2A"],
    ["4F9–C2A", "4F9C2A"],
  ])("accepts %j as %s", (input, code) => {
    expect(normalizePairingCode(input)).toEqual({ code, valid: true, problem: null });
  });

  it("reads O as 0 and I or L as 1, which are never hexadecimal", () => {
    expect(normalizePairingCode("BOOC1E").code).toBe("B00C1E");
    expect(normalizePairingCode("i0l0ab")).toMatchObject({ code: "101 0AB".replace(" ", ""), valid: true });
  });

  it("says how many characters are missing", () => {
    expect(normalizePairingCode("4F9")).toMatchObject({ valid: false, problem: "3 more characters to go." });
    expect(normalizePairingCode("4F9C2")).toMatchObject({ valid: false, problem: "1 more character to go." });
    expect(normalizePairingCode("")).toMatchObject({ valid: false, problem: null });
  });

  it("names characters that can't be in a code", () => {
    const result = normalizePairingCode("4G9C2Z");
    expect(result.valid).toBe(false);
    expect(result.problem).toContain("“G”");
    expect(result.problem).toContain("“Z”");
  });

  it("refuses codes that are too long", () => {
    expect(normalizePairingCode("4F9C2AB")).toMatchObject({ valid: false, problem: "A pairing code is 6 characters; this has 7." });
  });
});

describe("display and input helpers", () => {
  it("groups the code in threes", () => {
    expect(displayPairingCode("4f9-c2a")).toBe("4F9 C2A");
  });
  it("uppercases what is typed and keeps separators", () => {
    expect(formatCodeInput("4f9-c2a")).toBe("4F9-C2A");
  });
  it("keeps only 4 digits of an Apple TV code", () => {
    expect(normalizeSetupCode("48 21")).toEqual({ code: "4821", valid: true });
    expect(normalizeSetupCode("48a")).toEqual({ code: "48", valid: false });
  });
});

describe("parseSiliconIds", () => {
  it("accepts si: ids and bare handles, split by commas or spaces, without duplicates", () => {
    expect(parseSiliconIds("si:chef, scout  si:chef;si:atlas")).toEqual({ ids: ["si:chef", "si:scout", "si:atlas"], invalid: [] });
  });
  it("refuses Carbon ids and malformed ids", () => {
    expect(parseSiliconIds("c:alice si:")).toEqual({ ids: [], invalid: ["c:alice", "si:"] });
  });
});
