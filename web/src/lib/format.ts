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
  // A Stop by another Carbon who paired the device is logged with details {"stopped_by":"another_carbon"}
  // (activitySummary says so); without it, the Carbon who reads the log stopped it.
  stopped_by_carbon: "stopped by you",
  access_removed: "access was removed",
  device_removed: "the device was removed",
  pair_revoked: "the pair was revoked",
  pair_expired: "the pair expired",
  silicon_logged_out: "the Silicon signed out",
  // Since 1.1 a Carbon signing out also ends the sessions of the Silicons they gave access to.
  carbon_logged_out: "you signed out",
  // The Silicon left its Team, or the Carbon who gave it access left that Team (the device stays paired).
  left_team: "the Silicon, or the Carbon who gave it access, left the Silicon's Team",
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
  request_received: "Asked you for the device",
  pair_revoked: "Revoked the pair",
  another_carbon_paired: "Another Carbon paired this device",
  connection_replaced: "Connection replaced",
  duplicate_device: "Duplicate device",
  device_linked: "Recognised as the same device",
  wake_requested: "Asked you to wake it",
  wake_refreshed: "Asked again to wake it",
  woken: "Woke up",
  woke_without_input: "Reported awake without anyone at it",
  wake_confirmed: "Said it's awake",
  wake_declined: "Declined a request to wake it",
  wake_cancelled: "Withdrew a request to wake it",
  wake_expired: "A request to wake it expired",
  wake_withdrawn: "A request to wake it was withdrawn",
  wake_muted: "Turned wake requests off",
  wake_unmuted: "Turned wake requests on",
  removed: "Removed the device",
  pair_expired: "Pair expired",
  settings_changed: "Changed settings",
};
export function actionLabel(action: string): string {
  return ACTION[action] ?? action;
}

/**
 * Why a "removed" entry was logged, where it reads differently from a session's end. Extend removes a
 * device for `left_team` only when the Carbon who paired it left the team (revocation.rs), together
 * with the devices paired through it.
 */
const REMOVED_REASON: Record<string, string> = {
  left_team: "its Carbon left the Team",
};

const WAKE_END: Record<string, string> = {
  woken_on_device: "the device woke up",
  confirmed_by_carbon: "a Carbon said it's awake",
  expired: "it expired",
  cancelled: "the Silicon withdrew it",
  declined: "declined",
  session_started: "the Silicon started a session",
  access_removed: "its access was taken away",
  left_team: "the Silicon, or its Carbon, left the Team",
  device_removed: "the device was removed",
  muted: "wake requests were turned off",
  rollback: "Extend was rolled back",
};
/** Why a wake request ended, in words. */
export function wakeEnd(reason: string | null | undefined): string {
  return reason ? (WAKE_END[reason] ?? reason.replace(/_/g, " ")) : "";
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
  // What the device shows while a Silicon uses it: "hidden" / "shown", or {"from", "to"}.
  const indicator = d.in_use_indicator as { to?: unknown } | string | undefined;
  const shown = indicator && typeof indicator === "object" ? str(indicator.to) : str(indicator);
  if (shown === "hidden") parts.push("turned off the banner while a Silicon uses it");
  if (shown === "shown") parts.push("turned on the banner while a Silicon uses it");
  // Only 1.0 wrote these: since 1.1 a device is visible only to the Carbons who paired it.
  if (d.visibility === "team") parts.push("visible to the Team");
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
      const shared = d.with_existing_pairs ? " (another Carbon had already paired it)" : "";
      return `Paired${str(d.name) ? ` as “${d.name}”` : ""}${through ? ` through ${through}` : ""}${access}${shared}`;
    }
    case "renamed":
    case "settings_changed": {
      const parts = settingsSummary(d);
      if (!parts.length) return action === "renamed" ? "Renamed" : "Changed settings";
      const text = parts.join(", ");
      return text.charAt(0).toUpperCase() + text.slice(1);
    }
    case "access_granted":
      return `Gave ${silicon ?? "a Silicon"} access${d.restored_after_rollback ? " again (restored after a rollback)" : ""}`;
    case "access_revoked":
      return `Took access away from ${silicon ?? "a Silicon"}${d.reason === "left_team" ? " (it, or you, left its Team)" : ""}`;
    case "session_started":
      return "Started a session";
    case "session_ended": {
      if (d.stopped_by === "another_carbon") return "Session ended: stopped by another Carbon who paired this device";
      const reason = str(d.reason) ?? str(d.end_reason);
      return `Session ended${reason ? `: ${endReason(reason)}` : ""}`;
    }
    case "stopped":
      // A Silicon another Carbon gave access to is never named, so the entry has no silicon_id.
      return `Stopped ${silicon ?? "the Silicon using it"}`;
    case "request_sent":
      return `Asked ${str(d.to) ?? "the Silicon using it"} for the device${str(d.reason) ? `: “${d.reason}”` : ""}`;
    case "request_received":
      // Carbon decision 2: the Carbon a request is routed to sees which Silicon asked.
      return `${str(d.from) ?? "A Silicon"} asked you for the device${str(d.reason) ? `: “${d.reason}”` : ""}`;
    case "another_carbon_paired":
      return "Another Carbon paired this device. Their pair is separate: you don't see their Silicons, and they don't see yours";
    case "connection_replaced":
      return d.while_in_use_by_other
        ? "Connection replaced: another connection took over this pair while another Carbon's Silicon was using the device"
        : "Connection replaced: another connection took over this pair. If you didn't reconnect the app, check the device";
    case "duplicate_device":
      return d.kind === "same_carbon"
        ? "Duplicate device: you already added it through another pair, so this one isn't used"
        : "Duplicate device: it was already added through another computer";
    case "device_linked":
      return "Recognised as the same device another Carbon added through this computer";
    case "wake_requested":
    case "wake_refreshed":
      return `${action === "wake_refreshed" ? "Asked again to wake it" : "Asked you to wake it"}${str(d.reason) ? `: “${d.reason}”` : ""}`;
    case "woken":
      return "Woke up; every Silicon that asked was told";
    case "woke_without_input":
      return "Reported awake, but nobody used it, so the requests to wake it stay open";
    case "wake_confirmed":
      return "Said it's awake; every Silicon that asked was told";
    case "wake_declined":
      return `Declined ${silicon ? `${silicon}'s` : "a"} request to wake it`;
    case "wake_cancelled":
      return "Withdrew its request to wake it";
    case "wake_expired":
      return "A request to wake it expired";
    case "wake_withdrawn":
      return `A request to wake it ended${str(d.reason) ? `: ${wakeEnd(str(d.reason))}` : ""}`;
    case "wake_muted":
    case "wake_unmuted":
      return `Turned wake requests ${action === "wake_muted" ? "off" : "on"}${silicon ? ` for ${silicon}` : ""}`;
    case "takeover_started":
      return `Handed the device to you${str(d.reason) ? `: “${d.reason}”` : ""}`;
    case "takeover_released":
      return "Took the device back after the handover";
    case "removed": {
      const reason = str(d.reason);
      if (!reason || reason === "device_removed") return "Removed the device";
      return `Removed: ${REMOVED_REASON[reason] ?? endReason(reason)}`;
    }
    case "pair_revoked":
      return "Revoked the pair";
    case "pair_expired":
      return "The pair ended: unused for longer than its pairing lasts";
    default: {
      const extra = Object.entries(d)
        .filter(([, v]) => v !== null && v !== undefined && typeof v !== "object")
        .map(([k, v]) => `${k.replace(/_/g, " ")}: ${v}`)
        .join(", ");
      return `${actionLabel(action)}${extra ? ` (${extra})` : ""}`;
    }
  }
}

const SLEEP: Record<string, string> = {
  screen_off: "Asleep: screen off",
  locked: "Locked",
  asleep: "Asleep",
  standby: "In standby",
  other_session: "Another account is in use",
};
const LAST_SEEN: Record<string, string> = {
  screen_off: "with its screen off",
  locked: "locked",
  asleep: "asleep",
  standby: "in standby",
  other_session: "on another account",
};

/**
 * Whether a device is awake, in words, or null when there is nothing to say (removed, or a 1.0
 * service, which never says). Awake is information and the wake flow, never a gate: the terminal
 * and Android debugging work either way. A 1.1 service leaves `awake` out when it can't tell, and
 * always sends `wake_detectable`, which is how "unknown" is told apart from a 1.0 answer.
 */
export function awakeLabel(d: {
  online: boolean;
  removed_at?: string | null;
  awake?: boolean | null;
  sleep_state?: string | null;
  last_sleep_state?: string | null;
  wake_detectable?: boolean | null;
}): { text: string; state: "awake" | "asleep" | "unknown" } | null {
  if (d.removed_at) return null;
  if (!d.online) {
    const last = d.last_sleep_state;
    return last ? { text: `Offline, last seen ${LAST_SEEN[last] ?? last.replace(/_/g, " ")}`, state: "asleep" } : null;
  }
  if (d.awake === true) return { text: "Awake", state: "awake" };
  if (d.awake === false) {
    const why = d.sleep_state;
    return { text: why ? (SLEEP[why] ?? `Not awake: ${why.replace(/_/g, " ")}`) : "Not awake", state: "asleep" };
  }
  if (d.wake_detectable === undefined || d.wake_detectable === null) return null;
  return { text: "Awake: unknown", state: "unknown" };
}

/** What a device's status label says: the words always match the pixel dot's colour. */
export function statusLabel(status: { online: boolean; inUse?: boolean; paused?: boolean }): string {
  if (status.inUse) return status.paused ? "Paused for you" : "In use";
  return status.online ? "Online" : "Offline";
}

/**
 * Why a removed device was removed, told to the Carbon who paired it (the only member who can still
 * read it). A device paired through a computer ends with that computer, for the same reason.
 */
export function removedWhy(device: { removed_reason?: string | null; host_device_id?: string | null; pair_ttl_days?: number | null }): string {
  const hosted = !!device.host_device_id;
  switch (device.removed_reason ?? "device_removed") {
    case "device_removed":
      return hosted ? "You removed it, or the computer it paired through" : "You removed it";
    case "pair_revoked":
      return hosted ? "The computer it paired through had its pair revoked" : "The pair was revoked on the device itself";
    case "pair_expired": {
      const days = device.pair_ttl_days ? ` (${plural(device.pair_ttl_days, "day")})` : "";
      return hosted
        ? `It, or the computer it paired through, went unused for longer than its pairing lasts${days}`
        : `It went unused for longer than its pairing lasts${days}`;
    }
    case "left_team":
      return "Its Carbon left the Team";
    default: {
      const text = endReason(device.removed_reason);
      return text.charAt(0).toUpperCase() + text.slice(1);
    }
  }
}
