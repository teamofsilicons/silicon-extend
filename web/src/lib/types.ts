/** Wire shapes from understanding/api.yaml (components.schemas). Optional fields may be absent. */

export type DeviceOs =
  | "android"
  | "android_tv"
  | "macos"
  | "windows"
  | "linux"
  | "ios"
  | "ipados"
  | "tvos"
  | "samsung_tv"
  | "lg_tv";

export type AttachOs = "ios" | "ipados" | "tvos" | "samsung_tv" | "lg_tv";
/**
 * On the wire only. Since 1.1 a device is visible only to the Carbons who paired it, so the service
 * always says "personal" and ignores changes; the website neither shows nor sends it.
 */
export type Visibility = "team" | "personal";

/**
 * Why an online device isn't awake. Open on the wire: a value the website doesn't know yet is shown
 * as it comes, never dropped.
 */
export type SleepState = "screen_off" | "locked" | "asleep" | "standby" | "other_session" | (string & {});
/** Whether Extend's Tings reach a member in a Team. */
export type TingStatus = "on" | "off" | "pending" | (string & {});
/** Where the Ting about a request or a wake request is. A Silicon sees `deferred` as `pending`. */
export type TingDelivery = "pending" | "deferred" | "delivered" | "failed" | "covered" | (string & {});
export type WakeState = "open" | "woken" | "expired" | "withdrawn" | "declined" | (string & {});
export type WakeEndReason =
  | "woken_on_device"
  | "confirmed_by_carbon"
  | "expired"
  | "cancelled"
  | "declined"
  | "session_started"
  | "access_removed"
  | "left_team"
  | "device_removed"
  | "muted"
  | "rollback"
  | (string & {});
/** Whether the device itself showed a wake request. */
export type DeviceNotice = "sent" | "shown" | "not_shown" | "offline" | "unsupported" | (string & {});
/** Where a request for a device in use went: to the Silicon using it, or to the Carbon who gave that Silicon access. */
export type RequestRoute = "holder" | "carbon" | (string & {});
/**
 * Whether the device itself shows that a Silicon is using it (a badge, banner, notification or icon
 * change, for 10 seconds when a session starts). One setting per physical device, shared by every
 * Carbon who paired it. Absent means shown.
 */
export type InUseIndicator = "shown" | "hidden" | (string & {});

export interface Envelope<T> {
  type: string;
  data: T;
}

export interface ErrorBody {
  code: string;
  message: string;
  hint?: string | null;
  docs_url?: string | null;
  request_id?: string;
  details?: Record<string, unknown>;
}

export interface Member {
  type: "carbon" | "silicon";
  id: string;
  display_name?: string | null;
}

export interface TestingEnvironment {
  environment_id: string;
  name: string;
  state: "preparing" | "ready" | "cleaning" | "disabled" | "removed";
  paired_devices?: number;
  device_limit?: number;
}

export interface AuthSession {
  access_token: string;
  refresh_token: string;
  token_type: "Bearer";
  expires_in: number;
  member: Member;
  teams: string[];
  testing_environment?: TestingEnvironment | null;
}

export interface Me {
  authenticated: true;
  member: Member;
  teams: string[];
  team?: string | null;
  team_role?: string | null;
  testing_environment?: TestingEnvironment | null;
}

export interface IamInfo {
  app_id: string;
  iam_base_url: string;
  api_base_url: string;
  website_url: string;
  docs_url: string;
  repository_url?: string;
  /** IAM's consent screen, e.g. https://auth.iam.teamofsilicons.com/login. */
  iam_login_url?: string | null;
  /**
   * IAM's sign-up page, if Extend names one. api.yaml doesn't define it yet; the website uses it when
   * present and otherwise derives the page from `iam_login_url` (see `iamSignupUrl` in lib/auth.ts).
   */
  iam_signup_url?: string | null;
  testing_environment?: TestingEnvironment | null;
}

export interface InUse {
  silicon_id: string;
  session_id: string;
  since: string;
  paused?: boolean;
  /** The Silicon's Team (owner views, 1.1). */
  team?: string | null;
}

export type Capability = string;

export interface Device {
  device_id: string;
  name: string;
  os: DeviceOs;
  os_version?: string | null;
  model?: string | null;
  kind: "phone" | "tablet" | "tv" | "computer";
  owner: Member;
  /** 1.0: the device's Team. 1.1: absent for the Carbon who paired it; the Silicon's Team for a Silicon. */
  team?: string | null;
  visibility: Visibility;
  host_device_id?: string | null;
  state: "setup" | "ready";
  online: boolean;
  last_seen_at?: string | null;
  in_use?: InUse | null;
  last_used_at?: string | null;
  paired_at?: string;
  pair_ttl_days?: number;
  pair_expires_at?: string;
  days_left?: number;
  access_count?: number;
  app_version?: string | null;
  version?: number;
  /**
   * Set only on a removed device, which only the Carbon who paired it can still read (with
   * `include_removed=true`, or by id). It reads as offline, with no in_use, pair_expires_at or days_left.
   */
  removed_at?: string | null;
  /** Why it was removed: an EndReason such as device_removed, pair_revoked, pair_expired, left_team. */
  removed_reason?: string | null;

  // ───── 1.1.0. Every field is optional: a device in the 1.0 shape still renders. ─────

  /** The device engine's version. */
  engine_version?: string | null;
  /** Deprecated duplicate of `engine_version`, kept for API v1. */
  agent_device_version?: string | null;
  /** Whether the device is awake; absent while offline, or when Extend can't tell. */
  awake?: boolean | null;
  sleep_state?: SleepState | null;
  /** While offline: what it was when last seen, when that wasn't awake. */
  last_sleep_state?: SleepState | null;
  /** Owner only: when `awake` last changed. */
  awake_changed_at?: string | null;
  /** Whether Extend can tell when it wakes (false for iPhones, iPads and apps older than 1.1). */
  wake_detectable?: boolean | null;
  /** A Silicon you can't see is using it (or, for a computer, a device it carries). */
  in_use_by_other?: boolean;
  /** Owner views of a computer: what is busy is a device it carries that you didn't pair; only the computer's own Stop ends it. */
  in_use_by_other_carried?: boolean;
  open_wake_requests?: number | null;
  /** Single-device reads: the open wake requests you may see. */
  wake_requests?: WakeRequest[] | null;
  /** Owner only: wake requests are turned off for this pair. */
  wake_muted?: boolean | null;
  /** Owner only: another Carbon paired this device too (never who). */
  paired_by_others?: boolean | null;
  /** Silicon only: its other pairs of this same device. */
  same_device?: string[] | null;
  /** What the device itself shows while a Silicon uses it; absent means "shown". */
  in_use_indicator?: InUseIndicator | null;
}

export interface DeviceDetail extends Device {
  capabilities: Capability[];
  missing?: { capability: Capability; reason: string }[];
  commands: string[];
}

export interface SetupStep {
  key: string;
  title: string;
  status: "todo" | "in_progress" | "needs_carbon" | "done" | "failed";
  help?: string | null;
  error?: string | null;
  /** What the Carbon must enter on the website for this step, e.g. "code" for an Apple TV PIN. */
  input?: string | null;
}

export interface Setup {
  state: "in_progress" | "needs_carbon" | "complete";
  steps: SetupStep[];
}

export interface AccessGrant {
  device_id: string;
  silicon_id: string;
  granted_by: string;
  granted_at: string;
  last_used_at?: string | null;
  /** The Silicon's Team (1.1). A 1.0 service leaves it out: the grant is in the device's Team. */
  team?: string | null;
  /** This Silicon's wake requests are turned off. */
  wake_muted?: boolean | null;
}

/** Contract A: the 202 answer to a setup retry, the keys of the steps the device was asked to run again. */
export interface RetryResult {
  retrying: string[];
}

export interface Session {
  session_id: string;
  device_id: string;
  silicon_id: string;
  state: "active" | "paused" | "ended";
  started_at: string;
  last_command_at: string | null;
  idle_ends_at: string | null;
  ended_at: string | null;
  end_reason: string | null;
  command_count?: number;
  team?: string | null;
}

export interface ExtendRequest {
  request_id: string;
  device_id: string;
  from: string;
  to: string;
  session_id?: string | null;
  reason: string;
  created_at: string;
  delivery: "pending" | "delivered" | "failed" | (string & {});
  last_error?: string | null;
  /** The asking Silicon's Team, when you may see it. */
  team?: string | null;
  routed_to?: RequestRoute | null;
  /** `to` is the stand-in text: the asker doesn't see who got it. */
  to_hidden?: boolean;
  /** `from` is the stand-in text: a service that hides the asking Silicon from you. */
  from_hidden?: boolean;
}

/** A Silicon's request that its Carbon wake a device. */
export interface WakeRequest {
  wake_id: string;
  /** The pair the Silicon asked through. */
  device_id: string;
  team: string;
  from: string;
  to: string;
  reason: string;
  created_at: string;
  last_asked_at: string;
  asks: number;
  expires_at: string;
  state: WakeState;
  ended_at?: string | null;
  end_reason?: WakeEndReason | null;
  wake_detectable: boolean;
  device_notice: DeviceNotice;
  device_notice_note?: string | null;
  ting?: TingDelivery | null;
  ting_covered_by?: string | null;
  ting_last_error?: string | null;
  answer_ting?: TingDelivery | null;
  answer_ting_last_error?: string | null;
  /** For a carried device: the computer it pairs through, which must be awake too. */
  host?: { device_id: string; name: string; online: boolean } | null;
}

export interface WakeAnswered {
  answer: "woken" | "declined" | (string & {});
  /** The requests on your own pair that it ended. */
  ended: WakeRequest[];
}

export interface WakeSettingsView {
  device_id: string;
  muted: boolean;
  silicons_muted: { silicon_id: string; team: string }[];
}

/** Whether Extend's Tings reach you in one Team, and which of Extend's Ting types that Team is missing. */
export interface TingRegistration {
  team: string;
  member: string;
  status: TingStatus;
  registered_at?: string | null;
  refused_at?: string | null;
  last_error?: string | null;
  /** Full type names, like extend.device.wake_requested. */
  missing_types: string[];
}

/** The answer to Stop when the session ran through another Carbon's pair of the device. */
export interface DeviceStopped {
  device_id: string;
  stopped_at: string;
  in_use_by_other: true;
}

/** Whether one Team's directory could be read, in a list across Teams. */
export interface TeamReach {
  team: string;
  ok: boolean;
  error?: ErrorBody | null;
}

export interface ActivityEntry {
  id: string;
  at: string;
  actor: Member;
  action: string;
  session_id?: string | null;
  command?: string | null;
  args?: string[] | null;
  outcome?: "ok" | "failed" | "timeout" | "unknown" | null;
  files?: string[];
  details?: Record<string, unknown>;
  /** The acting Silicon's Team; absent for the Carbon's own and the device's entries. */
  team?: string | null;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

export interface TeamSilicon {
  id: string;
  display_name?: string | null;
  /** 1.1, with `team=any`: the Team it was listed from. */
  team?: string | null;
}

/** GET /team/silicons?team=any: every reachable Team's Silicons, and how each Team's read went. */
export interface TeamSilicons {
  items: TeamSilicon[];
  teams?: TeamReach[];
}

export interface Takeover {
  takeover_id: string;
  session_id: string;
  reason: string;
  started_at: string;
  expires_at: string;
}
