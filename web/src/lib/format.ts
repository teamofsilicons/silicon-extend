/** Time and label formatting shared by the pages. */

export function relativeTime(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "never";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diff = Math.round((now - t) / 1000);
  const future = diff < 0;
  const s = Math.abs(diff);
  let text: string;
  if (s < 45) return future ? "in a moment" : "just now";
  if (s < 3600) text = `${Math.round(s / 60)} min`;
  else if (s < 86_400) text = plural(Math.round(s / 3600), "hour");
  else text = plural(Math.round(s / 86_400), "day");
  return future ? `in ${text}` : `${text} ago`;
}

/** "4m", "1h 12m": how long something has been going. */
export function duration(since: string | null | undefined, now = Date.now()): string {
  if (!since) return "";
  const s = Math.max(0, Math.round((now - Date.parse(since)) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

export function dateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function clock(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function day(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

const END_REASON: Record<string, string> = {
  ended_by_silicon: "ended by the Silicon",
  idle_timeout: "ended after 5 minutes without a command",
  stopped_by_carbon: "stopped by you",
  access_removed: "access was removed",
  device_removed: "the device was removed",
  pair_revoked: "the pair was revoked",
  pair_expired: "the pair expired",
  silicon_logged_out: "the Silicon signed out",
  left_team: "the Silicon left the team",
  device_offline: "the device went offline",
  environment_disabled: "the test environment was disabled",
  environment_cleaned: "the test environment was cleaned",
};
export function endReason(reason: string | null | undefined): string {
  return reason ? (END_REASON[reason] ?? reason.replace(/_/g, " ")) : "";
}

const ACTION: Record<string, string> = {
  paired: "Paired",
  renamed: "Renamed",
  access_granted: "Gave access",
  access_revoked: "Took access away",
  session_started: "Started a session",
  session_ended: "Ended the session",
  command: "Command",
  takeover_started: "Handed the device to you",
  takeover_released: "Took the device back",
  stopped: "Stopped",
  request_sent: "Asked for the device",
  pair_revoked: "Revoked the pair",
  removed: "Removed the device",
  pair_expired: "Pair expired",
  settings_changed: "Changed settings",
};
export function actionLabel(action: string): string {
  return ACTION[action] ?? action;
}

type Details = Record<string, unknown> | undefined | null;
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

/** Describes a settings change from `details`, e.g. {"name":{"from","to"}}, {"pair_ttl_days":9}, {"visibility":"personal"}. */
function settingsSummary(d: Details): string[] {
  const parts: string[] = [];
  if (!d) return parts;
  const name = d.name as { from?: unknown; to?: unknown } | string | undefined;
  if (name && typeof name === "object") parts.push(`renamed from “${str(name.from) ?? "?"}” to “${str(name.to) ?? "?"}”`);
  else if (str(d.from) && str(d.to)) parts.push(`renamed from “${d.from}” to “${d.to}”`);
  if (typeof d.pair_ttl_days === "number") parts.push(`stays paired ${plural(d.pair_ttl_days, "day")} without activity`);
  if (d.visibility === "team") parts.push("visible to the team");
  if (d.visibility === "personal") parts.push("visible only to its owner");
  return parts;
}

/**
 * One readable line for an activity entry that isn't a command, built from its action and details.
 * Unknown actions and details still show, so nothing the service logs is hidden.
 */
export function activitySummary(action: string, details: Details): string {
  const d = details ?? {};
  const silicon = str(d.silicon_id);
  switch (action) {
    case "paired": {
      const through = str(d.through) ?? str(d.host_device_id);
      const access = Array.isArray(d.access) && d.access.length ? `, gave access to ${d.access.join(", ")}` : "";
      return `Paired${str(d.name) ? ` as “${d.name}”` : ""}${through ? ` through ${through}` : ""}${access}`;
    }
    case "renamed":
    case "settings_changed": {
      const parts = settingsSummary(d);
      if (!parts.length) return action === "renamed" ? "Renamed" : "Changed settings";
      const text = parts.join(", ");
      return text.charAt(0).toUpperCase() + text.slice(1);
    }
    case "access_granted":
      return `Gave ${silicon ?? "a Silicon"} access`;
    case "access_revoked":
      return `Took access away from ${silicon ?? "a Silicon"}${d.reason === "left_team" ? " (it left the team)" : ""}`;
    case "session_started":
      return "Started a session";
    case "session_ended": {
      const reason = str(d.reason) ?? str(d.end_reason);
      return `Session ended${reason ? `: ${endReason(reason)}` : ""}`;
    }
    case "stopped":
      return `Stopped ${silicon ?? "the Silicon using it"}`;
    case "request_sent":
      return `Asked ${str(d.to) ?? "the Silicon using it"} for the device${str(d.reason) ? `: “${d.reason}”` : ""}`;
    case "takeover_started":
      return `Handed the device to you${str(d.reason) ? `: “${d.reason}”` : ""}`;
    case "takeover_released":
      return "Took the device back after the handover";
    default: {
      const extra = Object.entries(d)
        .filter(([, v]) => v !== null && v !== undefined && typeof v !== "object")
        .map(([k, v]) => `${k.replace(/_/g, " ")}: ${v}`)
        .join(", ");
      return `${actionLabel(action)}${extra ? ` (${extra})` : ""}`;
    }
  }
}
