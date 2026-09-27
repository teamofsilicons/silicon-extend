import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { parse } from "yaml";
import { commandErrors, commandText, errorTable, normalizeCommand } from "../../scripts/docs-model.mjs";

describe("commandErrors (a command's errors in cli.yaml)", () => {
  it("keeps a list of codes", () => {
    expect(commandErrors(["device_in_use", "no_access"], "extend session new")).toEqual({ codes: ["device_in_use", "no_access"], note: null });
  });

  it("keeps a sentence as a sentence, instead of treating it as a list", () => {
    // `extend file get` describes its errors in words; the page used to call .join on it and crash.
    const note = "A refused download says which file failed and why, and the hint gives its Briefcase link.";
    expect(commandErrors(note, "extend file get <file_id>")).toEqual({ codes: [], note });
  });

  it("reads `- code: when` items and a code map as codes", () => {
    expect(commandErrors([{ device_in_use: "another Silicon has it" }, "no_access"], "x")).toEqual({ codes: ["device_in_use", "no_access"], note: null });
    expect(commandErrors({ device_offline: "…", command_timeout: "…" }, "x")).toEqual({ codes: ["device_offline", "command_timeout"], note: null });
  });

  it("shows nothing for an absent or empty value", () => {
    expect(commandErrors(undefined, "x")).toBeNull();
    expect(commandErrors(null, "x")).toBeNull();
    expect(commandErrors("", "x")).toBeNull();
    expect(commandErrors([], "x")).toBeNull();
  });

  it("stops the build, naming the command, for a shape it can't show", () => {
    expect(() => commandErrors([["a", "b"]], "extend device ls")).toThrow(/`extend device ls` has `errors` as a list holding a list/);
  });
});

describe("commandText", () => {
  it("joins a list and stringifies scalars", () => {
    expect(commandText(["GET /a", "GET /b"], "x", "api")).toBe("GET /a, GET /b");
    expect(commandText(3, "x", "who")).toBe("3");
    expect(commandText(undefined, "x", "who")).toBeNull();
  });

  it("refuses a map where the page shows a line of text", () => {
    expect(() => commandText({ a: 1 }, "extend file get", "summary")).toThrow(/`extend file get` has `summary` as a map/);
  });
});

describe("errorTable", () => {
  it("turns `code: [exit, meaning]` into rows", () => {
    expect(errorTable({ no_access: [4, "No access."] })).toEqual([{ code: "no_access", exit: 4, meaning: "No access." }]);
  });
  it("names the entry it can't read", () => {
    expect(() => errorTable({ no_access: 4 })).toThrow(/errors\.no_access/);
  });
});

// Vitest runs from web/; understanding/ sits beside it in the repository (absent in a web-only checkout).
const cliYaml = resolve(process.cwd(), "..", "understanding", "cli.yaml");

describe.runIf(existsSync(cliYaml))("the real understanding/cli.yaml", () => {
  const cli = parse(readFileSync(cliYaml, "utf8")) as {
    commands: Record<string, unknown>[];
    device_commands: Record<string, unknown>[];
    errors: unknown;
  };

  it("normalises every command into the shape the CLI reference page draws", () => {
    const all = [...cli.commands.map((c) => normalizeCommand(c, false)), ...cli.device_commands.map((c) => normalizeCommand(c, true))];
    expect(all.length).toBeGreaterThan(70);
    for (const c of all) {
      for (const field of ["who", "capability", "summary", "api", "notes"] as const) expect(c[field] === null || typeof c[field] === "string").toBe(true);
      if (c.errors) {
        expect(Array.isArray(c.errors.codes)).toBe(true);
        expect(c.errors.codes.length > 0 || typeof c.errors.note === "string").toBe(true);
      }
    }
    expect(errorTable(cli.errors).length).toBeGreaterThan(20);
  });
});
