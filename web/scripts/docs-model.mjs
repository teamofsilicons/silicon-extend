// Turns entries of understanding/cli.yaml into the fixed shapes the docs page renders
// (src/pages/Docs.tsx). cli.yaml is written by hand, so the same field is sometimes a string, a
// list or a map: `errors` is usually a list of codes, but a command may describe its errors in a
// sentence instead. Everything is normalised here, so the page never meets a shape it can't draw.
// A shape that can't be shown faithfully stops the build with the command and field named, rather
// than shipping a reference page that fails in the browser.

/** @param {unknown} value */
const scalar = (value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean";

/**
 * A command's `errors`: `[code, …]`, a sentence, or `{code: when}`. Returns the codes (in order)
 * and any sentence, or null when there is nothing to show.
 * @param {unknown} value
 * @param {string} usage the command, for the error message
 * @returns {{ codes: string[]; note: string | null } | null}
 */
export function commandErrors(value, usage) {
  if (value === null || value === undefined || value === "") return null;
  if (typeof value === "string") return { codes: [], note: value };
  if (Array.isArray(value)) {
    const codes = [];
    for (const item of value) {
      if (scalar(item)) codes.push(String(item));
      // `- device_in_use: when another Silicon has it` parses as a one-key map.
      else if (item && typeof item === "object" && !Array.isArray(item)) codes.push(...Object.keys(item));
      else throw shapeError(usage, "errors", "a list holding a list");
    }
    return codes.length ? { codes, note: null } : null;
  }
  if (typeof value === "object") {
    const codes = Object.keys(value);
    return codes.length ? { codes, note: null } : null;
  }
  return { codes: [String(value)], note: null };
}

/**
 * A field the page shows as one line of text (who, summary, api, notes, capability): a string, a
 * number, or a list of those joined with `separator`.
 * @param {unknown} value
 * @param {string} usage
 * @param {string} field
 * @param {string} [separator]
 * @returns {string | null}
 */
export function commandText(value, usage, field, separator = ", ") {
  if (value === null || value === undefined) return null;
  if (scalar(value)) return String(value);
  if (Array.isArray(value) && value.every(scalar)) return value.map(String).join(separator);
  throw shapeError(usage, field, Array.isArray(value) ? "a list of lists or maps" : "a map");
}

/** @param {unknown} value */
const json = (value) => (value === null || value === undefined ? null : JSON.parse(JSON.stringify(value)));

/**
 * One command from cli.yaml (`commands` or `device_commands`), as docs.json stores it.
 * @param {Record<string, unknown>} c
 * @param {boolean} device
 */
export function normalizeCommand(c, device) {
  const usage = String(c.usage);
  return {
    usage,
    who: commandText(c.who, usage, "who"),
    needs: json(c.needs),
    capability: commandText(c.capability, usage, "capability"),
    summary: commandText(c.summary, usage, "summary", " "),
    takes: json(c.takes),
    gives: json(c.gives),
    errors: commandErrors(c.errors, usage),
    api: commandText(c.api, usage, "api"),
    notes: commandText(c.notes, usage, "notes", " "),
    device,
  };
}

/**
 * The top-level `errors` map (`code: [exit, meaning]`) as table rows.
 * @param {unknown} errors
 * @returns {{ code: string; exit: number | null; meaning: string }[]}
 */
export function errorTable(errors) {
  if (errors === null || errors === undefined) return [];
  if (typeof errors !== "object" || Array.isArray(errors))
    throw new Error("gen-docs: cli.yaml `errors` must map each code to [exit code, meaning].");
  return Object.entries(errors).map(([code, entry]) => {
    if (Array.isArray(entry)) return { code, exit: typeof entry[0] === "number" ? entry[0] : null, meaning: String(entry[1] ?? "") };
    if (typeof entry === "string") return { code, exit: null, meaning: entry };
    throw new Error(`gen-docs: cli.yaml \`errors.${code}\` must be [exit code, meaning]; got ${JSON.stringify(entry)}.`);
  });
}

/**
 * @param {string} usage
 * @param {string} field
 * @param {string} got
 */
function shapeError(usage, field, got) {
  return new Error(
    `gen-docs: in understanding/cli.yaml, \`${usage}\` has \`${field}\` as ${got}, which the CLI reference page can't show. ` +
      `Write it as a string or a list of strings (for errors: a list of error codes, or one sentence).`,
  );
}
