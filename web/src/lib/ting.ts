/**
 * Extend's Ting notification types, mirrored from crates/extend-protocol/src/ting.rs.
 *
 * Ting resolves app types across delivery Teams. A type manager registers them in the app's
 * owning Team. The notification's delivery Team does not identify that owner.
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
 * The command a manager in the app's owning Team runs. The owner is not supplied by the current
 * API, so the UI explains this shell-quoted placeholder rather than guessing the delivery Team.
 */
export function registerCommand(fullName: string, appId = "extend"): string {
  const type = tingType(fullName);
  const name = fullName.startsWith("device.") ? `${appId}.${fullName}` : fullName;
  return `ting --org '<owning-team>' types register --type ${name} --description ${shellQuote(type?.description ?? name)}`;
}
