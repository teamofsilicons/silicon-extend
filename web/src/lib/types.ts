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
export type Visibility = "team" | "personal";

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
  team?: string;
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
}

export interface ExtendRequest {
  request_id: string;
  device_id: string;
  from: string;
  to: string;
  session_id?: string;
  reason: string;
  created_at: string;
  delivery: "pending" | "delivered" | "failed";
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
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

export interface TeamSilicon {
  id: string;
  display_name?: string | null;
}

export interface Takeover {
  takeover_id: string;
  session_id: string;
  reason: string;
  started_at: string;
  expires_at: string;
}
