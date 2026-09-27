/**
 * Extend's Ting notification types, mirrored from crates/extend-protocol/src/ting.rs.
 *
 * Ting keeps notification types per environment, Team and app, and only a Team's Ting manager can
 * register them. Where Ting says one is missing in a Team (`TingRegistration.missing_types`), the
 * website shows the exact command that Team's Ting manager runs, instead of retrying silently.
 */

export interface TingType {
  /** The name after the app id, e.g. "device.woken". */
  event: string;
  /** What it tells the recipient, as registered. */
  description: string;
}

export const TING_TYPES: TingType[] = [
  { event: "device.requested", description: "A Silicon asks to use a device another Silicon is using" },
  { event: "device.wake_requested", description: "A Silicon asks its Carbon to wake a device" },
  { event: "device.woken", description: "A device a Silicon asked to wake is awake" },
  { event: "device.wake_declined", description: "A Carbon turned down a request to wake a device" },
];

/** Finds a type by its full name ("extend.device.woken") or its event ("device.woken"). */
export function tingType(name: string): TingType | undefined {
  const dot = name.indexOf(".");
  const event = dot > 0 && name.slice(0, dot) !== "device" ? name.slice(dot + 1) : name;
  return TING_TYPES.find((t) => t.event === event);
}

/** Single quotes inside a shell argument that is itself single-quoted. */
const shellQuote = (text: string) => `'${text.replace(/'/g, `'\\''`)}'`;

/**
 * The command a Team's Ting manager runs to register one of Extend's types there, exactly as the
 * CLI prints it: `ting --org <team> types register --type extend.device.X --description '...'`.
 */
export function registerCommand(team: string, fullName: string, appId = "extend"): string {
  const type = tingType(fullName);
  const name = fullName.startsWith("device.") ? `${appId}.${fullName}` : fullName;
  return `ting --org ${team} types register --type ${name} --description ${shellQuote(type?.description ?? name)}`;
}
