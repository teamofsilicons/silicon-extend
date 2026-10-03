/**
 * An in-memory mock of the Extend service, following understanding/api.yaml, so the website runs
 * and is tested without the Rust service. It checks what the real service checks where that
 * matters to the website: envelopes, X-Org-ID, X-Testing-Application-Secret, Idempotency-Key,
 * If-Match versions, owner-only actions, the test-environment device limit, and token rotation.
 *
 * 1.1: devices belong to the Carbons who paired them (X-Org-ID doesn't filter them), grants are per
 * Team, several Carbons can pair one physical device (an "instance") with separate pairs and sides,
 * one Silicon at a time holds the instance, requests are routed to the Carbon who gave the holder
 * access, wake requests, Ting registration per Team, setup retry (contract A), and whether the device
 * shows that a Silicon is using it (in_use_indicator, one setting per physical device).
 *
 * It also serves a stand-in for the IAM consent screen under /__mock/iam/login, and control
 * endpoints under /__mock/* for tests. Run: `pnpm mock` (port 8490) or `pnpm dev:mock`.
 */
import http from "node:http";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import {
  DEVICE_FAMILY_TV,
  DEVICE_STUDIO_MAC,
  FAR_TEAM,
  MOCK_PORT,
  OTHER_TEAM,
  SEEDED_CODE,
  TEAM,
  TEST_ENVIRONMENT_ID,
  TEST_ENVIRONMENT_NAME,
  TEST_SECRET,
} from "./fixtures.ts";

// ───────────── Types ─────────────

type Os = "android" | "android_tv" | "macos" | "windows" | "linux" | "ios" | "ipados" | "tvos" | "samsung_tv" | "lg_tv";
type StepStatus = "todo" | "in_progress" | "needs_carbon" | "done" | "failed";

interface Member {
  type: "carbon" | "silicon";
  id: string;
  display_name: string;
  teams: string[];
}

interface StepDef {
  key: string;
  title: string;
  help: string;
  /** Needs the Carbon to do something on the device while it is the current step. */
  carbon?: boolean;
  /** Only finishes after POST /setup/code. */
  code?: boolean;
}

/** One physical device; every pair of it (one per Carbon) shares it. */
interface InstanceRec {
  instance_id: string;
  /** null: not reported (an app older than 1.1, an iPhone, or offline since connecting). */
  awake: boolean | null;
  sleep_state: string | null;
  awake_changed_at: string | null;
  /**
   * Whether the device itself shows that a Silicon is using it: one setting for the physical
   * device, shared by every Carbon who paired it. Unset means shown.
   */
  in_use_indicator?: "shown" | "hidden";
}

interface DeviceRec {
  device_id: string;
  /** The physical device; pairs of the same device by several Carbons share it. */
  instance_id: string;
  name: string;
  os: Os;
  os_version: string | null;
  model: string | null;
  kind: "phone" | "tablet" | "tv" | "computer";
  owner: string;
  team: string;
  visibility: "team" | "personal";
  organizations?: Record<string, { visibility: "team" | "personal"; removed: boolean; removed_at: string | null; removed_reason: string | null }>;
  host_device_id: string | null;
  online: boolean;
  last_seen_at: string | null;
  last_used_at: string | null;
  last_activity_at: number;
  paired_at: string;
  pair_ttl_days: number;
  app_version: string | null;
  version: number;
  removed: boolean;
  /** Set with `removed`: when and why (an EndReason), as the service keeps them. */
  removed_at: string | null;
  removed_reason: string | null;
  setup: SetupRec | null;
  wake_muted: boolean;
  /** A carried pair the service holds back: already added by the same Carbon, or through another computer. */
  duplicate: { kind: "own"; of: string } | { kind: "other_computer" } | null;
  /** A carried pair in a test environment at its limit, waiting for its computer to recognise it. */
  recognising: boolean;
}

interface SetupRec {
  steps: StepDef[];
  started: number;
  codeEnteredAt: number | null;
  /** Steps that failed, with the plain-language error the device reported (contract A). */
  failures: Record<string, string>;
  lastRetryAt: number | null;
}

interface SessionRec {
  session_id: string;
  /** The pair the session runs through. */
  device_id: string;
  silicon_id: string;
  /** The Silicon's Team. A session's side is (team, the owner of its pair). */
  team: string;
  state: "active" | "paused" | "ended";
  started_at: string;
  last_command_at: string | null;
  idle_ends_at: string | null;
  ended_at: string | null;
  end_reason: string | null;
  command_count: number;
  takeover?: { takeover_id: string; session_id: string; reason: string; started_at: string; expires_at: string } | null;
}

interface Grant {
  device_id: string;
  silicon_id: string;
  granted_by: string;
  granted_at: string;
  last_used_at: string | null;
  /** The Silicon's Team. */
  team: string;
  wake_muted: boolean;
}

interface Activity {
  id: string;
  at: string;
  actor: { type: "carbon" | "silicon"; id: string };
  action: string;
  session_id: string | null;
  command: string | null;
  args: string[] | null;
  outcome: "ok" | "failed" | "timeout" | "unknown" | null;
  files: string[];
  details: Record<string, unknown>;
  team: string | null;
}

/** As stored; `requestView` shows each viewer their own side of it. */
interface RequestRec {
  request_id: string;
  /** The requester's pair. */
  device_id: string;
  from: string;
  to: string;
  session_id?: string;
  reason: string;
  created_at: string;
  delivery: "pending" | "delivered" | "failed";
  last_error?: string;
  /** The requester's Team. */
  team: string;
  routed_to: "holder" | "carbon";
  /** Carbon rows: the Carbon who gave the holder access, and the holder's pair. */
  routed_to_id?: string;
  holder_device_id?: string;
}

interface WakeRec {
  wake_id: string;
  device_id: string;
  instance_id: string;
  team: string;
  from: string;
  to: string;
  reason: string;
  created_at: string;
  last_asked_at: string;
  asks: number;
  expires_at: string;
  state: "open" | "woken" | "expired" | "withdrawn" | "declined";
  ended_at: string | null;
  end_reason: string | null;
  device_notice: "sent" | "shown" | "not_shown" | "offline" | "unsupported";
  device_notice_note: string | null;
  ting: "pending" | "deferred" | "delivered" | "failed" | "covered" | null;
  ting_last_error: string | null;
  answer_ting: "pending" | "delivered" | "failed" | null;
}

interface TingRec {
  team: string;
  member: string;
  status: "on" | "off" | "pending";
  last_error: string | null;
}

interface Environment {
  environment_id: string;
  name: string;
  state: "preparing" | "ready" | "cleaning" | "disabled" | "removed";
}

interface World {
  featureRequests: Map<string, { member: string; team: string; endpoints: { audience: string; endpoint_id: string }[]; expires: number; approved: boolean }>;
  featureGrants: Map<string, unknown[]>;
  key: string;
  environment: Environment | null;
  members: Map<string, Member>;
  instances: Map<string, InstanceRec>;
  devices: Map<string, DeviceRec>;
  /** Grants per pair, keyed `team\nsilicon_id`. */
  access: Map<string, Map<string, Grant>>;
  wakes: WakeRec[];
  /** Keyed `member\nteam`. */
  ting: Map<string, TingRec>;
  /** Extend's Ting types Ting doesn't know, per Team. */
  tingMissing: Map<string, string[]>;
  sessions: Map<string, SessionRec>;
  activity: Map<string, Activity[]>;
  requests: RequestRec[];
  accessTokens: Map<string, { member: string; expires: number; family: string; org: string }>;
  refreshTokens: Map<string, { member: string; family: string; used: boolean; org: string }>;
  revokedFamilies: Set<string>;
  failedClaims: Map<string, number[]>;
}

interface Enrollment {
  enrollment_id: string;
  secret: string;
  os: Os;
  model: string | null;
  os_version: string | null;
  app_version: string;
  code: string;
  code_expires: number;
  paired?: { device_id: string; world: string };
  /** "Pair with another Carbon": started with the credential of this live pair, so it adds a pair to its device. */
  from_device_id?: string;
}

// ───────────── State ─────────────

const config = { stepMs: 1200, accessTtlS: 1800 };
const now = () => Date.now();
const iso = (t: number) => new Date(t).toISOString();
const MIN = 60_000;
const DAY = 86_400_000;

let worlds = new Map<string, World>();
let enrollments = new Map<string, Enrollment>();
let mintedSlts = new Map<string, { member: string; world: string; expires: number }>();
let idempotency = new Map<string, { hash: string; status: number; body: unknown; headers: Record<string, string> }>();
/** Setup retries the website sent, for tests: {device_id, step}. */
let retryLog: { device_id: string; step: string | null }[] = [];
/** Failures a newly paired device of an OS reports for a step (`POST /__mock/fail-step` with `os`). */
const pendingFailures = new Map<string, Record<string, string>>();
/** A step that fails again after the next retry of a device. */
const retryFailures = new Map<string, { step: string; error: string }>();
/** Teams whose directory read fails in team=any. */
const failingDirectories = new Set<string>();
/** member\nteam pairs that are the Team's Ting manager. */

const gkey = (team: string, silicon: string) => `${team}\n${silicon}`;

const hex = (n: number) => randomBytes(Math.ceil(n / 2)).toString("hex").slice(0, n);
const b64 = (n: number) => randomBytes(n).toString("base64url");

function newWorld(key: string, environment: Environment | null): World {
  return {
    featureRequests: new Map(),
    featureGrants: new Map(),
    key,
    environment,
    members: new Map(),
    instances: new Map(),
    devices: new Map(),
    access: new Map(),
    wakes: [],
    ting: new Map(),
    tingMissing: new Map(),
    sessions: new Map(),
    activity: new Map(),
    requests: [],
    accessTokens: new Map(),
    refreshTokens: new Map(),
    revokedFamilies: new Set(),
    failedClaims: new Map(),
  };
}

function addMember(world: World, m: Member) {
  world.members.set(m.id, m);
}

const SETUP_STEPS: Record<Os, StepDef[]> = {
  android: [
    { key: "app_installed", title: "Install and open the app", help: "Done when the app showed the pairing code." },
    { key: "developer_options", title: "Turn on Developer options", help: "Settings → About phone → tap Build number seven times.", carbon: true },
    { key: "wireless_debugging", title: "Turn on wireless debugging", help: "Settings → System → Developer options → Wireless debugging. The app pairs with it.", carbon: true },
    { key: "notifications", title: "Allow notifications and running in the background", help: "Tap Allow when the app asks." },
  ],
  android_tv: [
    { key: "app_installed", title: "Install and open the app", help: "Done when the TV showed the pairing code." },
    { key: "network_debugging", title: "Turn on Developer options and network debugging", help: "Settings → Device Preferences → About → Build (press select 7 times), then Developer options → Network debugging.", carbon: true },
    { key: "allow_debugging", title: "Approve “Allow debugging” on the TV", help: "Tick “Always allow from this computer” and choose OK.", carbon: true },
  ],
  macos: [
    { key: "app_installed", title: "Install and open the app", help: "Done when the menu bar showed the pairing code." },
    { key: "accessibility", title: "Allow Accessibility", help: "System Settings → Privacy & Security → Accessibility → Silicon Extend.", carbon: true },
    { key: "screen_recording", title: "Allow Screen Recording", help: "System Settings → Privacy & Security → Screen & System Audio Recording → Silicon Extend.", carbon: true },
  ],
  windows: [
    { key: "app_installed", title: "Install and open the app", help: "Done when the tray showed the pairing code." },
    { key: "allow", title: "Allow it when Windows asks", help: "Choose Yes on the User Account Control prompt.", carbon: true },
  ],
  linux: [
    { key: "app_installed", title: "Install and open the app", help: "Done when the app showed the pairing code." },
    { key: "remote_desktop", title: "Approve screen sharing and remote control", help: "Your desktop asks once. Choose Share.", carbon: true },
  ],
  ios: [
    { key: "trust", title: "Plug the iPhone into the Mac and tap Trust", help: "Use a cable once. Tap Trust on the iPhone and enter its passcode.", carbon: true },
    { key: "developer_mode", title: "Turn on Developer Mode", help: "Settings → Privacy & Security → Developer Mode. The iPhone restarts.", carbon: true },
    { key: "helper", title: "Extend puts its helper on the iPhone", help: "Keep the iPhone unlocked and near the Mac." },
  ],
  ipados: [
    { key: "trust", title: "Plug the iPad into the Mac and tap Trust", help: "Use a cable once. Tap Trust on the iPad and enter its passcode.", carbon: true },
    { key: "developer_mode", title: "Turn on Developer Mode", help: "Settings → Privacy & Security → Developer Mode. The iPad restarts.", carbon: true },
    { key: "helper", title: "Extend puts its helper on the iPad", help: "Keep the iPad unlocked and near the Mac." },
  ],
  tvos: [
    { key: "discover", title: "Find the Apple TV on the network", help: "The Mac looks for it on the same network." },
    { key: "setup_code", title: "Enter the code the Apple TV shows", help: "A 4-digit code appears on the TV.", carbon: true, code: true },
    { key: "connect", title: "Connect the Mac to the Apple TV", help: "Takes a few seconds." },
  ],
  samsung_tv: [
    { key: "discover", title: "Find the TV on the network", help: "The computer looks for it on the same network." },
    { key: "approve", title: "Allow the connection on the TV", help: "Choose Allow on the TV with the remote.", carbon: true },
  ],
  lg_tv: [
    { key: "discover", title: "Find the TV on the network", help: "The computer looks for it on the same network." },
    { key: "approve", title: "Accept the connection on the TV", help: "Choose Accept on the TV with the remote.", carbon: true },
  ],
};

const CAPABILITIES: Record<Os, string[]> = {
  android: ["screen.read", "screen.capture", "screen.record", "input.touch", "input.text", "input.keyboard", "nav.system", "apps.launch", "apps.list", "apps.install", "alerts", "clipboard", "logs", "replay", "takeover", "notifications", "adb", "links"],
  android_tv: ["screen.read", "screen.capture", "input.text", "input.remote", "nav.system", "apps.launch", "apps.list", "apps.install", "alerts", "logs", "replay", "takeover", "adb", "display", "links"],
  macos: ["screen.read", "screen.capture", "screen.record", "input.pointer", "input.text", "apps.launch", "apps.list", "alerts", "clipboard", "logs", "replay", "takeover", "terminal", "links"],
  windows: ["screen.read", "screen.capture", "screen.record", "input.pointer", "input.text", "apps.launch", "apps.list", "alerts", "clipboard", "logs", "replay", "takeover", "terminal", "links"],
  linux: ["screen.read", "screen.capture", "screen.record", "input.pointer", "input.text", "apps.launch", "apps.list", "clipboard", "logs", "replay", "takeover", "terminal", "links"],
  ios: ["screen.read", "screen.capture", "screen.record", "input.touch", "input.text", "input.keyboard", "nav.system", "apps.launch", "apps.list", "alerts", "replay", "takeover", "links"],
  ipados: ["screen.read", "screen.capture", "screen.record", "input.touch", "input.text", "input.keyboard", "nav.system", "apps.launch", "apps.list", "alerts", "replay", "takeover", "links"],
  tvos: ["input.remote", "nav.system", "apps.launch", "apps.list", "replay", "takeover", "display"],
  samsung_tv: ["input.remote", "nav.system", "apps.launch", "apps.list", "replay", "takeover", "links"],
  lg_tv: ["input.remote", "nav.system", "apps.launch", "apps.list", "replay", "takeover", "links"],
};

const COMMANDS: Record<string, string[]> = {
  "screen.read": ["snapshot", "get", "find", "is", "wait"],
  "screen.capture": ["screenshot", "diff"],
  "screen.record": ["record"],
  "input.touch": ["click", "press", "longpress", "swipe", "scroll", "gesture"],
  "input.pointer": ["click", "press", "hover", "scroll"],
  "input.text": ["fill", "type", "focus"],
  "input.keyboard": ["keyboard"],
  "input.remote": ["tv-remote"],
  "nav.system": ["back", "home", "app-switcher"],
  "apps.launch": ["open", "close", "appstate"],
  "apps.list": ["apps"],
  "apps.install": ["install", "reinstall"],
  alerts: ["alert"],
  clipboard: ["clipboard"],
  logs: ["logs"],
  replay: ["replay", "test", "batch"],
  takeover: ["takeover"],
  notifications: ["notifications"],
  adb: ["adb"],
  terminal: ["terminal"],
  display: ["display"],
  links: ["open"],
};

function kindFor(os: Os, model: string | null): DeviceRec["kind"] {
  if (os === "android") return model && /tab|pad/i.test(model) ? "tablet" : "phone";
  if (os === "ios") return "phone";
  if (os === "ipados") return "tablet";
  if (os === "android_tv" || os === "tvos" || os === "samsung_tv" || os === "lg_tv") return "tv";
  return "computer";
}

function newEnrollment(os: Os, model: string | null, code?: string, options: { app_version?: string; from_device_id?: string } = {}): Enrollment {
  const e: Enrollment = {
    enrollment_id: randomUUID(),
    secret: `ees_${b64(32)}`,
    os,
    model,
    os_version: null,
    app_version: options.app_version ?? "1.1.0",
    code: code ?? hex(6).toUpperCase(),
    code_expires: now() + 300_000,
    from_device_id: options.from_device_id,
  };
  enrollments.set(e.enrollment_id, e);
  return e;
}

/** The plain-language texts the service uses (crates/extend-protocol lib.rs). */
const REQUEST_TO_HIDDEN = "the Carbon who gave access to the Silicon using it";
const TERMINAL_NOT_SHARED_REASON =
  "Several Carbons paired this computer. Only Silicons given access by the Carbon who installed Silicon Extend on it can use its terminal. The screen, keyboard and apps work as usual.";
const TING_TYPES = ["extend.device.requested", "extend.device.wake_requested", "extend.device.woken", "extend.device.wake_declined"];

function seed() {
  worlds = new Map();
  enrollments = new Map();
  mintedSlts = new Map();
  idempotency = new Map();
  retryLog = [];
  const t = now();

  // Production
  const prod = newWorld("production", null);
  addMember(prod, { type: "carbon", id: "c:saket", display_name: "Saket", teams: [TEAM, OTHER_TEAM] });
  addMember(prod, { type: "carbon", id: "c:alice", display_name: "Alice", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:chef", display_name: "Chef", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:scout", display_name: "Scout", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:atlas", display_name: "Atlas", teams: [TEAM, OTHER_TEAM] });
  addMember(prod, { type: "silicon", id: "si:juniper", display_name: "Juniper", teams: [OTHER_TEAM] });
  // Alice's Silicons, on her side of the devices she and Saket both paired.
  addMember(prod, { type: "silicon", id: "si:sous", display_name: "Sous", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:pilot", display_name: "Pilot", teams: [TEAM] });
  // A Silicon in a Team Saket's Extend login doesn't reach (he was given access there before, or signed in without it).
  addMember(prod, { type: "silicon", id: "si:orbit", display_name: "Orbit", teams: [FAR_TEAM] });
  worlds.set(prod.key, prod);

  const device = (d: Partial<DeviceRec> & Pick<DeviceRec, "device_id" | "name" | "os" | "owner" | "team">): DeviceRec => {
    const rec: DeviceRec = {
      instance_id: randomUUID(),
      os_version: null,
      model: null,
      kind: kindFor(d.os, d.model ?? null),
      visibility: "team",
      host_device_id: null,
      online: true,
      last_seen_at: iso(t - 5000),
      last_used_at: null,
      last_activity_at: t - DAY,
      paired_at: iso(t - 20 * DAY),
      pair_ttl_days: 14,
      app_version: "1.1.0",
      version: 1,
      removed: false,
      removed_at: null,
      removed_reason: null,
      setup: null,
      wake_muted: false,
      duplicate: null,
      recognising: false,
      ...d,
    };
    return rec;
  };
  /** Adds a pair; its instance is created with it (or shared when `instance_id` names another pair's). */
  const put = (w: World, d: DeviceRec, instance: Partial<InstanceRec> = {}) => {
    w.devices.set(d.device_id, d);
    if (!w.instances.has(d.instance_id)) w.instances.set(d.instance_id, { instance_id: d.instance_id, awake: d.online ? true : null, sleep_state: null, awake_changed_at: iso(t - 3600_000), ...instance });
  };

  put(prod, device({ device_id: "7c1e09ab", name: "Saket's Pixel", os: "android", os_version: "15", model: "Pixel 9", owner: "c:saket", team: TEAM, last_used_at: iso(t - 10_000), last_activity_at: t - 10_000, version: 3 }));
  // Locked, with a Silicon asking Saket to wake it.
  put(prod, device({ device_id: "2e7f00d1", name: "MacBook Pro", os: "macos", os_version: "15.4", model: "MacBookPro18,3", owner: "c:saket", team: TEAM, last_used_at: iso(t - 2 * 3600_000), last_activity_at: t - 2 * 3600_000, pair_ttl_days: 30, version: 2 }), {
    awake: false,
    sleep_state: "locked",
    awake_changed_at: iso(t - 12 * MIN),
  });
  put(prod, device({ device_id: "0d44e1f2", name: "Living room TV", os: "android_tv", os_version: "12", model: "Chromecast with Google TV", owner: "c:saket", team: TEAM, online: false, last_seen_at: iso(t - 2 * DAY), last_used_at: iso(t - 5 * DAY), last_activity_at: t - 5 * DAY }), {
    awake: false,
    sleep_state: "standby",
  });
  // Extend can't tell when an iPhone wakes.
  put(prod, device({ device_id: "51ab93c0", name: "Saket's iPhone", os: "ios", os_version: "18.1", model: "iPhone 16", owner: "c:saket", team: TEAM, host_device_id: "2e7f00d1", app_version: null, last_used_at: iso(t - 3 * DAY), last_activity_at: t - 3 * DAY }), { awake: null });
  put(prod, device({ device_id: "b3f81c20", name: "Alice's Windows PC", os: "windows", owner: "c:alice", team: TEAM }));
  put(prod, device({ device_id: "c0ffee42", name: "Lab Linux box", os: "linux", os_version: "Ubuntu 24.04", owner: "c:saket", team: OTHER_TEAM, online: false, last_seen_at: iso(t - 6 * 3600_000) }));

  // A family TV both Carbons paired: Saket's pair "Family TV", Alice's pair "Our TV". Saket's si:scout is using it.
  const tv = randomUUID();
  put(prod, device({ device_id: DEVICE_FAMILY_TV, instance_id: tv, name: "Family TV", os: "android_tv", os_version: "13", model: "Google TV Streamer", owner: "c:saket", team: TEAM, paired_at: iso(t - 10 * DAY), last_used_at: iso(t - 6 * MIN), last_activity_at: t - 6 * MIN }));
  put(prod, device({ device_id: "6b2f8e11", instance_id: tv, name: "Our TV", os: "android_tv", os_version: "13", model: "Google TV Streamer", owner: "c:alice", team: TEAM, paired_at: iso(t - 30 * DAY), last_activity_at: t - 6 * MIN }));
  // A computer both paired. Alice installed Extend on it (the first pair), so only her Silicons get its terminal.
  // Her si:pilot is using it now; she also carries her iPad through it.
  const studio = randomUUID();
  put(prod, device({ device_id: "8e4f3a21", instance_id: studio, name: "Alice's Studio", os: "macos", os_version: "26.0", model: "Mac16,10", owner: "c:alice", team: TEAM, paired_at: iso(t - 40 * DAY) }));
  put(prod, device({ device_id: DEVICE_STUDIO_MAC, instance_id: studio, name: "Studio Mac", os: "macos", os_version: "26.0", model: "Mac16,10", owner: "c:saket", team: TEAM, paired_at: iso(t - 4 * DAY), last_activity_at: t - 20 * MIN }));
  put(prod, device({ device_id: "a9d2c4e7", name: "Alice's iPad", os: "ipados", os_version: "18.0", owner: "c:alice", team: TEAM, host_device_id: "8e4f3a21", app_version: null }), { awake: null });

  const grant = (w: World, device_id: string, silicon_id: string, team: string, ago: number, lastUsed: number | null = null) => {
    if (!w.access.has(device_id)) w.access.set(device_id, new Map());
    w.access.get(device_id)!.set(gkey(team, silicon_id), {
      device_id,
      silicon_id,
      team,
      granted_by: w.devices.get(device_id)!.owner,
      granted_at: iso(t - ago),
      last_used_at: lastUsed === null ? null : iso(t - lastUsed),
      wake_muted: false,
    });
  };
  grant(prod, "7c1e09ab", "si:chef", TEAM, 12 * DAY, 10_000);
  grant(prod, "7c1e09ab", "si:scout", TEAM, 6 * DAY, 2 * DAY);
  grant(prod, "7c1e09ab", "si:juniper", OTHER_TEAM, 5 * DAY);
  grant(prod, "7c1e09ab", "si:orbit", FAR_TEAM, 8 * DAY, 4 * DAY);
  grant(prod, "2e7f00d1", "si:atlas", TEAM, 3 * DAY, 2 * 3600_000);
  grant(prod, "0d44e1f2", "si:chef", TEAM, 9 * DAY, 5 * DAY);
  grant(prod, DEVICE_FAMILY_TV, "si:scout", TEAM, 9 * DAY, 6 * MIN);
  grant(prod, "6b2f8e11", "si:sous", TEAM, 20 * DAY, DAY);
  grant(prod, DEVICE_STUDIO_MAC, "si:chef", TEAM, 3 * DAY);
  grant(prod, "8e4f3a21", "si:pilot", TEAM, 30 * DAY, 20 * MIN);
  grant(prod, "a9d2c4e7", "si:pilot", TEAM, 30 * DAY);

  const session = (w: World, id: string, device_id: string, silicon_id: string, team: string, startedAgo: number, extra: Partial<SessionRec> = {}) =>
    w.sessions.set(id, {
      session_id: id,
      device_id,
      silicon_id,
      team,
      state: "active",
      started_at: iso(t - startedAgo),
      last_command_at: iso(t - 10_000),
      idle_ends_at: iso(t - 10_000 + 300_000),
      ended_at: null,
      end_reason: null,
      command_count: 23,
      ...extra,
    });
  // si:chef is using the Pixel right now, in session a3f.
  session(prod, "a3f", "7c1e09ab", "si:chef", TEAM, 4 * MIN);
  session(prod, "7d2", "7c1e09ab", "si:scout", TEAM, 2 * DAY + 20 * MIN, {
    state: "ended",
    last_command_at: iso(t - 2 * DAY - 6 * MIN),
    idle_ends_at: null,
    ended_at: iso(t - 2 * DAY - MIN),
    end_reason: "idle_timeout",
    command_count: 9,
  });
  session(prod, "e51", DEVICE_FAMILY_TV, "si:scout", TEAM, 6 * MIN);
  session(prod, "f0c", "8e4f3a21", "si:pilot", TEAM, 20 * MIN);

  // Activity: enough entries on the Pixel to page.
  const pixelLog: Activity[] = [];
  const entry = (at: number, actor: string, action: string, extra: Partial<Activity> = {}): Activity => ({
    id: randomUUID(),
    at: iso(at),
    actor: { type: actor.startsWith("si:") ? "silicon" : "carbon", id: actor },
    action,
    session_id: null,
    command: null,
    args: null,
    outcome: null,
    files: [],
    details: {},
    team: actor.startsWith("si:") ? TEAM : null,
    ...extra,
  });
  pixelLog.push(entry(t - 20 * DAY, "c:saket", "paired", { details: { name: "Saket's Pixel", access: [] } }));
  pixelLog.push(entry(t - 19 * DAY, "c:saket", "settings_changed", { details: { pair_ttl_days: 14, visibility: "team" } }));
  pixelLog.push(entry(t - 12 * DAY, "c:saket", "access_granted", { team: TEAM, details: { silicon_id: "si:chef" } }));
  pixelLog.push(entry(t - 8 * DAY, "c:saket", "access_granted", { team: FAR_TEAM, details: { silicon_id: "si:orbit" } }));
  pixelLog.push(entry(t - 6 * DAY, "c:saket", "access_granted", { team: TEAM, details: { silicon_id: "si:scout" } }));
  pixelLog.push(entry(t - 5 * DAY, "c:saket", "access_granted", { team: OTHER_TEAM, details: { silicon_id: "si:juniper" } }));
  pixelLog.push(entry(t - 2 * DAY - 20 * MIN, "si:scout", "session_started", { session_id: "7d2" }));
  const scoutCommands: [string, string[], Activity["outcome"]][] = [
    ["open", ["com.whatsapp"], "ok"],
    ["snapshot", ["-i"], "ok"],
    ["click", ["@e4"], "ok"],
    ["fill", ["@e7", "[redacted 9 chars]"], "ok"],
    ["is", ["visible", 'text="Sent"'], "failed"],
    ["screenshot", ["--scale", "0.5"], "ok"],
  ];
  scoutCommands.forEach(([command, args, outcome], i) =>
    pixelLog.push(entry(t - 2 * DAY - (18 - i) * MIN, "si:scout", "command", { session_id: "7d2", command, args, outcome, files: command === "screenshot" ? [randomUUID()] : [] })),
  );
  pixelLog.push(entry(t - 2 * DAY - MIN, "si:scout", "session_ended", { session_id: "7d2", details: { reason: "idle_timeout" } }));
  pixelLog.push(entry(t - 4 * MIN, "si:chef", "session_started", { session_id: "a3f" }));
  const chefCommands = ["snapshot", "click", "scroll", "snapshot", "click", "fill", "back", "open", "snapshot", "screenshot"];
  for (let i = 0; i < 48; i++) {
    const command = chefCommands[i % chefCommands.length];
    const args = command === "click" ? [`@e${(i % 9) + 1}`] : command === "fill" ? ["@e3", "[redacted 12 chars]"] : command === "scroll" ? ["down"] : command === "open" ? ["com.swiggy.android"] : command === "snapshot" ? ["-i"] : [];
    pixelLog.push(entry(t - 4 * MIN + (i + 1) * 4000, "si:chef", "command", { session_id: "a3f", command, args, outcome: i === 17 ? "timeout" : "ok", files: command === "screenshot" ? [randomUUID()] : [] }));
  }
  pixelLog.push(entry(t - 3 * MIN, "si:scout", "request_sent", { details: { to: "si:chef", reason: "I need to check the order confirmation in the Swiggy app, 2 minutes" } }));
  prod.activity.set("7c1e09ab", pixelLog.sort((a, b) => Date.parse(b.at) - Date.parse(a.at)));
  prod.activity.set(
    "2e7f00d1",
    [
      entry(t - 30 * DAY, "c:saket", "paired"),
      entry(t - 3 * DAY, "c:saket", "access_granted", { team: TEAM, details: { silicon_id: "si:atlas" } }),
      entry(t - 26 * 3600_000, "si:atlas", "wake_requested", { details: { reason: "Need the screen to export last night's report" } }),
      entry(t - 25 * 3600_000, "extend", "woken", { actor: { type: "carbon", id: "extend" }, team: null }),
      entry(t - 3 * MIN, "si:atlas", "wake_requested", { details: { reason: "Need the screen to check tonight's deploy dashboard" } }),
    ].reverse(),
  );
  prod.activity.set(
    DEVICE_FAMILY_TV,
    [
      entry(t - 10 * DAY, "c:saket", "paired", { details: { name: "Family TV", with_existing_pairs: true } }),
      entry(t - 9 * DAY, "c:saket", "access_granted", { team: TEAM, details: { silicon_id: "si:scout" } }),
      entry(t - 6 * MIN, "si:scout", "session_started", { session_id: "e51" }),
      entry(t - 2 * MIN, "si:scout", "request_received", { actor: { type: "silicon", id: "si:sous" }, team: null, session_id: "e51", details: { from: "si:sous", reason: "The match starts in 5 minutes; can I have the TV?" } }),
    ].reverse(),
  );
  prod.activity.set(
    DEVICE_STUDIO_MAC,
    [
      entry(t - 4 * DAY, "c:saket", "paired", { details: { name: "Studio Mac", with_existing_pairs: true } }),
      entry(t - 3 * DAY, "c:saket", "access_granted", { team: TEAM, details: { silicon_id: "si:chef" } }),
      entry(t - 5 * MIN, "si:chef", "request_sent", { details: { to: REQUEST_TO_HIDDEN, routed_to: "carbon", reason: "Need the terminal for 5 minutes to run the backup" } }),
    ].reverse(),
  );
  prod.activity.set("8e4f3a21", [entry(t - 40 * DAY, "c:alice", "paired"), entry(t - 4 * DAY, "extend", "another_carbon_paired", { actor: { type: "carbon", id: "extend" }, team: null })].reverse());

  // Two of Saket's devices were removed: their logs stay readable to him (include_removed=true).
  put(prod, device({ device_id: "e1d0a7c3", name: "Old Galaxy Tab", os: "android", os_version: "13", model: "Galaxy Tab S7", owner: "c:saket", team: TEAM, online: false, last_seen_at: iso(t - 3 * DAY), last_used_at: iso(t - 6 * DAY), last_activity_at: t - 3 * DAY, paired_at: iso(t - 40 * DAY), version: 4, removed: true, removed_at: iso(t - 3 * DAY), removed_reason: "device_removed" }));
  prod.activity.set(
    "e1d0a7c3",
    [
      entry(t - 40 * DAY, "c:saket", "paired", { details: { name: "Old Galaxy Tab", access: [] } }),
      entry(t - 39 * DAY, "c:saket", "access_granted", { details: { silicon_id: "si:scout" } }),
      entry(t - 6 * DAY - 30 * MIN, "si:scout", "session_started", { session_id: "b41" }),
      entry(t - 6 * DAY - 28 * MIN, "si:scout", "command", { session_id: "b41", command: "open", args: ["com.android.chrome"], outcome: "ok" }),
      entry(t - 6 * DAY - 20 * MIN, "si:scout", "session_ended", { session_id: "b41", details: { reason: "ended_by_silicon" } }),
      entry(t - 3 * DAY, "c:saket", "removed", { details: { reason: "device_removed" } }),
    ].reverse(),
  );
  put(prod, device({ device_id: "f00d5eed", name: "Office iMac", os: "macos", os_version: "14.6", model: "iMac21,1", owner: "c:saket", team: TEAM, online: false, last_seen_at: iso(t - 16 * DAY), last_activity_at: t - 16 * DAY, paired_at: iso(t - 30 * DAY), pair_ttl_days: 7, version: 2, removed: true, removed_at: iso(t - 9 * DAY), removed_reason: "pair_expired" }));
  prod.activity.set(
    "f00d5eed",
    [
      entry(t - 30 * DAY, "c:saket", "paired", { details: { name: "Office iMac", access: [] } }),
      entry(t - 9 * DAY, "extend", "pair_expired", { actor: { type: "carbon", id: "extend" }, details: { reason: "pair_expired" } }),
    ].reverse(),
  );

  const request = (r: Omit<RequestRec, "request_id" | "routed_to" | "team"> & Partial<Pick<RequestRec, "routed_to" | "team">>): RequestRec => ({ request_id: randomUUID(), routed_to: "holder", team: TEAM, ...r });
  prod.requests.push(
    request({ device_id: "7c1e09ab", from: "si:scout", to: "si:chef", session_id: "a3f", reason: "I need to check the order confirmation in the Swiggy app, 2 minutes", created_at: iso(t - 3 * MIN), delivery: "delivered" }),
    request({ device_id: "7c1e09ab", from: "si:chef", to: "si:scout", session_id: "7d2", reason: "Need 2 minutes to check an OTP for the vendor portal", created_at: iso(t - 2 * DAY - 10 * MIN), delivery: "delivered" }),
    // Alice's si:sous asked for the family TV Saket's si:scout is using: it went to Saket, who sees who asked.
    request({ device_id: "6b2f8e11", from: "si:sous", to: REQUEST_TO_HIDDEN, reason: "The match starts in 5 minutes; can I have the TV?", created_at: iso(t - 2 * MIN), delivery: "delivered", routed_to: "carbon", routed_to_id: "c:saket", holder_device_id: DEVICE_FAMILY_TV, session_id: "e51" }),
    // Saket's si:chef asked for the Studio Mac Alice's si:pilot is using: it went to Alice, and chef sees only that.
    request({ device_id: DEVICE_STUDIO_MAC, from: "si:chef", to: REQUEST_TO_HIDDEN, reason: "Need the terminal for 5 minutes to run the backup", created_at: iso(t - 5 * MIN), delivery: "pending", last_error: "Not delivered yet; it is retried.", routed_to: "carbon", routed_to_id: "c:alice", holder_device_id: "8e4f3a21", session_id: "f0c" }),
  );

  // si:atlas asks Saket to wake the locked MacBook; an older request was answered when it woke.
  const mac = prod.devices.get("2e7f00d1")!;
  prod.wakes.push(
    {
      wake_id: randomUUID(),
      device_id: mac.device_id,
      instance_id: mac.instance_id,
      team: TEAM,
      from: "si:atlas",
      to: "c:saket",
      reason: "Need the screen to check tonight's deploy dashboard",
      created_at: iso(t - 3 * MIN),
      last_asked_at: iso(t - 3 * MIN),
      asks: 1,
      expires_at: iso(t - 3 * MIN + 30 * MIN),
      state: "open",
      ended_at: null,
      end_reason: null,
      device_notice: "shown",
      device_notice_note: null,
      ting: "delivered",
      ting_last_error: null,
      answer_ting: null,
    },
    {
      wake_id: randomUUID(),
      device_id: mac.device_id,
      instance_id: mac.instance_id,
      team: TEAM,
      from: "si:atlas",
      to: "c:saket",
      reason: "Need the screen to export last night's report",
      created_at: iso(t - 26 * 3600_000),
      last_asked_at: iso(t - 26 * 3600_000),
      asks: 1,
      expires_at: iso(t - 26 * 3600_000 + 30 * MIN),
      state: "woken",
      ended_at: iso(t - 25 * 3600_000),
      end_reason: "woken_on_device",
      device_notice: "shown",
      device_notice_note: null,
      ting: "delivered",
      ting_last_error: null,
      answer_ting: "delivered",
    },
  );

  // Ting: on in acme; labs is missing two of Extend's types and Saket isn't registered there yet.
  const ting = (w: World, member: string, team: string, status: TingRec["status"], last_error: string | null = null) => w.ting.set(`${member}\n${team}`, { member, team, status, last_error });
  ting(prod, "c:saket", TEAM, "on");
  ting(prod, "c:saket", OTHER_TEAM, "pending");
  prod.tingMissing.set(OTHER_TEAM, ["extend.device.wake_requested", "extend.device.woken"]);

  // Test environment: starts empty, with its own test members.
  const test = newWorld(TEST_ENVIRONMENT_ID, { environment_id: TEST_ENVIRONMENT_ID, name: TEST_ENVIRONMENT_NAME, state: "ready" });
  addMember(test, { type: "carbon", id: "c:alice", display_name: "Alice (test)", teams: [TEAM] });
  addMember(test, { type: "carbon", id: "c:saket", display_name: "Saket (test)", teams: [TEAM] });
  addMember(test, { type: "silicon", id: "si:chef", display_name: "Chef (test)", teams: [TEAM] });
  addMember(test, { type: "silicon", id: "si:scout", display_name: "Scout (test)", teams: [TEAM] });
  worlds.set(test.key, test);

  newEnrollment("android", "Pixel 8a", SEEDED_CODE);
}

const SECRETS = new Map<string, string>([[TEST_SECRET, TEST_ENVIRONMENT_ID]]);

// ───────────── HTTP plumbing ─────────────

class MockError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly hint: string | null = null,
    readonly details: Record<string, unknown> = {},
  ) {
    super(message);
  }
}

const fail = (status: number, code: string, message: string, hint: string | null = null, details: Record<string, unknown> = {}): never => {
  throw new MockError(status, code, message, hint, details);
};

interface Ctx {
  req: http.IncomingMessage;
  url: URL;
  params: Record<string, string>;
  body: { type?: unknown; data?: unknown } | null;
  rawBody: string;
  world: World;
  requestId: string;
  origin: string;
}

interface Reply {
  status: number;
  type?: string;
  data?: unknown;
  headers?: Record<string, string>;
  html?: string;
  redirect?: string;
}

const ok = (status: number, type: string, data: unknown, headers: Record<string, string> = {}): Reply => ({ status, type, data, headers });
const none = (): Reply => ({ status: 204 });

function header(ctx: Ctx, name: string): string | null {
  const v = ctx.req.headers[name.toLowerCase()];
  return Array.isArray(v) ? v[0] : (v ?? null);
}

function worldFor(req: http.IncomingMessage): World {
  const secret = req.headers["x-testing-application-secret"];
  if (secret === undefined) return worlds.get("production")!;
  const s = Array.isArray(secret) ? secret[0] : secret;
  if (!/^ask_[A-Za-z0-9_-]{43}$/.test(s))
    fail(401, "testing_secret_invalid", "X-Testing-Application-Secret is not a test app secret (it must look like ask_ followed by 43 characters). Nothing ran in production.", "Copy the test application's app_secret from Honeycomb again.");
  const id = SECRETS.get(s);
  const world = id ? worlds.get(id) : undefined;
  if (!world || !world.environment)
    fail(401, "testing_secret_invalid", "The test secret is invalid, revoked, or its environment is not active. Nothing ran in production.", "Check the secret in Honeycomb, or exit testing to use production.");
  if (world!.environment!.state !== "ready")
    fail(503, "testing_environment_not_ready", `Test environment ${world!.environment!.name} is ${world!.environment!.state}; Honeycomb has not confirmed every service is ready.`, "Wait for Honeycomb to finish preparing it, then try again.");
  return world!;
}

function environmentView(world: World) {
  if (!world.environment) return null;
  // 1.1: the limit counts physical devices; another Carbon's pair of the same device doesn't count.
  const paired = new Set([...world.devices.values()].filter((d) => !d.removed).map((d) => d.instance_id)).size;
  return { ...world.environment, paired_devices: paired, device_limit: 5 };
}

function caller(ctx: Ctx): Member {
  const auth = header(ctx, "authorization");
  if (!auth?.startsWith("Bearer "))
    fail(401, "not_signed_in", "This request needs an Extend access token (Authorization: Bearer oat_…).", "Sign in with `extend login <slt>` or on the website.");
  const token = auth!.slice(7);
  const record = ctx.world.accessTokens.get(token);
  if (!record) {
    const elsewhere = [...worlds.values()].some((w) => w !== ctx.world && w.accessTokens.has(token));
    if (elsewhere)
      fail(401, "not_signed_in", `This access token belongs to a different environment than ${ctx.world.environment ? `test environment ${ctx.world.environment.name}` : "production"}. Production and test logins never mix.`, "Sign in again in this environment.");
    fail(401, "token_expired", "The access token is unknown or was revoked.", "Refresh the token, or sign in again.");
  }
  if (ctx.world.revokedFamilies.has(record!.family)) fail(401, "token_expired", "This session was signed out.", "Sign in again.");
  if (record!.expires < now()) fail(401, "token_expired", "The access token expired.", "Refresh it with POST /api/v1/auth/refresh.");
  const member = ctx.world.members.get(record!.member);
  if (!member) fail(401, "token_expired", "The member behind this token no longer exists.", "Sign in again.");
  const org = header(ctx, "x-org-id");
  if (org && org !== record!.org) fail(403, "not_a_team_member", "The requested organization does not match this login.");
  return { ...member!, teams: [record!.org] };
}

function teamOf(ctx: Ctx, member: Member, required = true): string | null {
  const team = header(ctx, "x-org-id");
  if (!team) {
    if (required) fail(400, "invalid_input", "X-Org-ID is required: it names the team (IAM handle) this request is for.", "Send the team handle in X-Org-ID, or pass --team.");
    return member.teams[0] ?? null;
  }
  if (!member.teams.includes(team))
    fail(403, "not_a_team_member", `${member.id} is not an active member of team ${team}.`, `Pick one of your teams: ${member.teams.join(", ")}.`);
  return team;
}

function carbonOnly(member: Member) {
  if (member.type !== "carbon") fail(403, "carbon_only", `${member.id} is a Silicon. Only Carbons pair and manage devices.`, "Ask the Carbon who owns the device.");
}

function requireKey(ctx: Ctx) {
  const key = header(ctx, "idempotency-key");
  if (!key || !/^[!-~]{8,255}$/.test(key))
    fail(400, "invalid_input", "Idempotency-Key is required on this request (8–255 visible ASCII characters).", "Send a fresh UUID, and reuse it only to retry the identical request.");
  return key!;
}

function envelope(ctx: Ctx, type: string): Record<string, unknown> {
  if (!ctx.body || ctx.body.type !== type || typeof ctx.body.data !== "object" || ctx.body.data === null || Array.isArray(ctx.body.data))
    fail(400, "invalid_input", `The body must be the envelope {"type": "${type}", "data": {…}}.`, "See api.yaml for the request shape.");
  return ctx.body!.data as Record<string, unknown>;
}

function onlyKeys(data: Record<string, unknown>, allowed: string[]) {
  const extra = Object.keys(data).filter((k) => !allowed.includes(k));
  if (extra.length) fail(422, "invalid_input", `Unknown field${extra.length > 1 ? "s" : ""}: ${extra.join(", ")}. Accepted: ${allowed.join(", ")}.`, null, { fields: extra });
}

function checkName(name: unknown): string {
  if (typeof name !== "string" || !name.trim() || [...name.trim()].length > 64)
    fail(422, "invalid_input", "name must be 1–64 characters after trimming.", "Choose a shorter name.", { field: "name" });
  return (name as string).trim();
}
function checkVisibility(v: unknown): "team" | "personal" {
  if (v !== "team" && v !== "personal") fail(422, "invalid_input", 'visibility must be "team" or "personal".', null, { field: "visibility" });
  return v as "team" | "personal";
}
function checkIndicator(v: unknown): "shown" | "hidden" {
  if (v !== "shown" && v !== "hidden") fail(422, "invalid_input", 'in_use_indicator must be "shown" or "hidden".', null, { field: "in_use_indicator" });
  return v as "shown" | "hidden";
}
function checkTtl(v: unknown): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 1 || v > 30)
    fail(422, "invalid_input", `pair_ttl_days must be a whole number from 1 to 30; got ${JSON.stringify(v)}.`, "Pick between 1 and 30 days.", { field: "pair_ttl_days" });
  return v as number;
}

function checkSilicon(world: World, team: string, id: string) {
  if (!/^si:/.test(id)) fail(422, "invalid_input", `${id} is not a Silicon id (they start with si:).`, null, { silicon_id: id });
  const m = world.members.get(id);
  if (!m || m.type !== "silicon" || !m.teams.includes(team))
    fail(422, "not_a_team_member", `${id} is not an active Silicon in team ${team}, so it can't be given access.`, "Check the id with the Silicon, or add it to the team in Silicon IAM first.", { silicon_id: id });
}

// ───────────── Views ─────────────

type StepView = { key: string; title: string; status: StepStatus; help: string | null; error: string | null; input: string | null };
function setupView(world: World, d: DeviceRec): { state: "in_progress" | "needs_carbon" | "complete"; steps: StepView[] } {
  const out: StepView[] = [];
  // Held back by the service (1.1): the same TV added twice, or waiting for its computer to recognise it.
  const what = `This ${d.kind === "tv" ? "TV" : d.kind}`;
  if (d.duplicate?.kind === "own") {
    const other = world.devices.get(d.duplicate.of);
    const host = other?.host_device_id ? world.devices.get(other.host_device_id) : null;
    out.push({ key: "duplicate_device", title: "Added twice", status: "failed", help: null, input: null, error: `${what} is already added as ${other?.name ?? "another device"} (${d.duplicate.of}) through ${host?.name ?? "a computer"}; remove one of them.` });
  } else if (d.duplicate?.kind === "other_computer") {
    out.push({
      key: "duplicate_device",
      title: "Added through another computer",
      status: "failed",
      help: null,
      input: null,
      error: `${what} is already added to Extend through another computer. Add it through that computer instead: on its Extend app choose Pair with another Carbon, then add the ${d.kind === "tv" ? "TV" : d.kind} there.`,
    });
  } else if (d.recognising) {
    const host = d.host_device_id ? world.devices.get(d.host_device_id) : null;
    out.push({ key: "recognising", title: `Waiting for ${host?.name ?? "the computer"} to recognise this device`, status: "in_progress", help: null, error: null, input: null });
  }
  if (!d.setup) {
    if (!out.length) return { state: "complete", steps: [] };
    return { state: "in_progress", steps: out };
  }
  const { steps, started, codeEnteredAt, failures } = d.setup;
  const push = (step: StepDef, status: StepStatus) =>
    out.push({ key: step.key, title: step.title, status, help: step.help, error: status === "failed" ? (failures[step.key] ?? null) : null, input: step.code ? "code" : null });
  let clock = started;
  let blocked = false;
  for (const step of steps) {
    if (blocked) {
      push(step, "todo");
      continue;
    }
    if (step.code) {
      if (codeEnteredAt === null) {
        push(step, "needs_carbon");
        blocked = true;
        continue;
      }
      clock = Math.max(clock, codeEnteredAt);
      push(step, "done");
      continue;
    }
    const doneAt = clock + config.stepMs;
    if (now() >= doneAt && failures[step.key]) {
      // The device reported this step failed: it stays failed until a retry.
      push(step, "failed");
      blocked = true;
    } else if (now() >= doneAt) {
      push(step, "done");
      clock = doneAt;
    } else {
      push(step, step.carbon ? "needs_carbon" : "in_progress");
      blocked = true;
    }
  }
  const complete = out.every((s) => s.status === "done");
  const state = complete ? "complete" : out.some((s) => s.status === "needs_carbon") ? "needs_carbon" : "in_progress";
  return { state, steps: out };
}

/** Folds setup progress and pair expiry into the stored record. */
function settle(world: World, d: DeviceRec) {
  if (d.setup && setupView(world, d).state === "complete") {
    d.setup = null;
    d.version += 1;
    const host = d.host_device_id ? world.devices.get(d.host_device_id) : null;
    d.online = host ? host.online : true;
    d.last_seen_at = iso(now());
  }
}

/** Live pairs of one physical device. */
function pairsOf(world: World, instanceId: string): DeviceRec[] {
  return [...world.devices.values()].filter((x) => x.instance_id === instanceId && !x.removed);
}

/** The session holding a physical device, through whichever pair: one Silicon at a time. */
function holderOf(world: World, d: DeviceRec): SessionRec | null {
  const pairs = new Set(pairsOf(world, d.instance_id).map((x) => x.device_id));
  return [...world.sessions.values()].find((x) => pairs.has(x.device_id) && x.state !== "ended") ?? null;
}

/** Sessions on devices this computer's instance carries (through any Carbon's pair of it). */
function carriedSessions(world: World, d: DeviceRec): SessionRec[] {
  if (d.kind !== "computer" || d.host_device_id) return [];
  const hosts = new Set(pairsOf(world, d.instance_id).map((x) => x.device_id));
  return [...world.sessions.values()].filter((x) => {
    const carried = world.devices.get(x.device_id);
    return x.state !== "ended" && !!carried?.host_device_id && hosts.has(carried.host_device_id);
  });
}

const sideOf = (world: World, s: SessionRec) => ({ team: s.team, carbon: world.devices.get(s.device_id)?.owner ?? "" });

function inUseView(s: SessionRec, withTeam: boolean) {
  return { silicon_id: s.silicon_id, session_id: s.session_id, since: s.started_at, paused: s.state === "paused", ...(withTeam ? { team: s.team } : {}) };
}

/** Who is looking: the Carbon who paired the pair (owner), or a Silicon with a grant in its X-Org-ID Team. */
type Viewer = { member: Member; team: string | null };

function deviceView(world: World, d: DeviceRec, viewer: Viewer, detail = false) {
  settle(world, d);
  const owner = world.members.get(d.owner);
  const ownerView = viewer.member.type === "carbon" && viewer.member.id === d.owner;
  const base = {
    device_id: d.device_id,
    name: d.name,
    os: d.os,
    kind: d.kind,
    owner: { type: "carbon", id: d.owner, display_name: owner?.display_name ?? null },
    visibility: d.visibility,
    online: d.removed ? false : d.online,
    team: viewer.team ?? d.team,
  };
  if (d.removed) {
    // Like the service: a removed device reads offline, with no in_use, pair_expires_at or days_left.
    return {
      ...base,
      os_version: d.os_version,
      model: d.model,
      host_device_id: d.host_device_id,
      state: "ready",
      last_seen_at: d.last_seen_at,
      last_used_at: d.last_used_at,
      paired_at: d.paired_at,
      pair_ttl_days: d.pair_ttl_days,
      access_count: 0,
      app_version: d.app_version,
      version: d.version,
      removed_at: d.removed_at,
      removed_reason: d.removed_reason,
      paired_by_others: false,
      wake_muted: d.wake_muted,
    };
  }
  const instance = world.instances.get(d.instance_id);
  const holder = holderOf(world, d);
  const carried = carriedSessions(world, d);
  let inUse = null as ReturnType<typeof inUseView> | null;
  let byOther = false;
  let byOtherCarried = false;
  if (ownerView) {
    if (holder && holder.device_id === d.device_id && holder.team === (viewer.team ?? d.team)) inUse = inUseView(holder, true);
    else if (holder) byOther = true;
    else {
      const others = carried.filter((x) => sideOf(world, x).carbon !== viewer.member.id);
      if (others.length) {
        byOther = true;
        // Busy only with carried devices the viewer didn't pair: only the computer's own Stop ends them.
        byOtherCarried = others.every((x) => !pairsOf(world, world.devices.get(x.device_id)!.instance_id).some((p) => p.owner === viewer.member.id));
      }
    }
  } else if (holder) {
    const sameSide = holder.device_id === d.device_id && holder.team === viewer.team;
    if (sameSide || holder.silicon_id === viewer.member.id) inUse = inUseView(holder, false);
    else byOther = true;
  } else if (carried.some((x) => !(x.team === viewer.team && sideOf(world, x).carbon === d.owner))) byOther = true;
  const grants = [...(world.access.get(d.device_id)?.values() ?? [])].filter(g => g.team === (viewer.team ?? d.team));
  const wakes = world.wakes.filter((w) => w.instance_id === d.instance_id && w.state === "open" && w.team === (viewer.team ?? d.team));
  const visibleWakes = ownerView ? wakes.filter((w) => w.device_id === d.device_id) : wakes.filter((w) => w.from === viewer.member.id && w.team === viewer.team);
  const hostApp = d.host_device_id ? world.devices.get(d.host_device_id)?.app_version : d.app_version;
  const wakeDetectable = !!hostApp && hostApp >= "1.1" && d.os !== "ios" && d.os !== "ipados";
  const expires = d.last_activity_at + d.pair_ttl_days * DAY;
  const online = d.online;
  const awake = online ? (instance?.awake ?? null) : null;
  return {
    ...base,
    os_version: d.os_version,
    model: d.model,
    host_device_id: d.host_device_id,
    state: d.setup || d.duplicate || d.recognising ? "setup" : "ready",
    last_seen_at: d.last_seen_at,
    in_use: inUse,
    ...(ownerView ? { last_used_at: d.last_used_at } : {}),
    paired_at: d.paired_at,
    pair_ttl_days: d.pair_ttl_days,
    pair_expires_at: iso(expires),
    days_left: Math.max(0, Math.ceil((expires - now()) / DAY)),
    access_count: ownerView ? grants.length : grants.filter((g) => g.team === viewer.team).length,
    app_version: d.app_version,
    engine_version: d.app_version ? "0.19.0" : null,
    agent_device_version: d.app_version ? "0.19.0" : null,
    version: d.version,
    awake,
    ...(awake === false && instance?.sleep_state ? { sleep_state: instance.sleep_state } : {}),
    ...(!online && instance?.awake === false && instance.sleep_state ? { last_sleep_state: instance.sleep_state } : {}),
    ...(ownerView && online && instance?.awake_changed_at ? { awake_changed_at: instance.awake_changed_at } : {}),
    wake_detectable: wakeDetectable,
    ...(byOther ? { in_use_by_other: true } : {}),
    ...(byOtherCarried ? { in_use_by_other_carried: true } : {}),
    open_wake_requests: visibleWakes.length,
    ...(detail ? { wake_requests: visibleWakes.map((w) => wakeView(world, w, viewer)) } : {}),
    ...(ownerView ? { wake_muted: d.wake_muted, paired_by_others: pairsOf(world, d.instance_id).some((x) => x.owner !== d.owner) } : {}),
    ...(ownerView ? { in_use_indicator: instance?.in_use_indicator ?? "shown" } : {}),
  };
}

function wakeView(world: World, w: WakeRec, viewer: Viewer) {
  const d = world.devices.get(w.device_id);
  const host = d?.host_device_id ? world.devices.get(d.host_device_id) : null;
  const ownerView = viewer.member.type === "carbon";
  const hostApp = host ? host.app_version : d?.app_version;
  return {
    wake_id: w.wake_id,
    device_id: w.device_id,
    team: w.team,
    from: w.from,
    to: w.to,
    reason: w.reason,
    created_at: w.created_at,
    last_asked_at: w.last_asked_at,
    asks: w.asks,
    expires_at: w.expires_at,
    state: w.state,
    ...(w.ended_at ? { ended_at: w.ended_at, end_reason: w.end_reason } : {}),
    wake_detectable: !!hostApp && hostApp >= "1.1" && d?.os !== "ios" && d?.os !== "ipados",
    device_notice: w.device_notice,
    ...(w.device_notice_note ? { device_notice_note: w.device_notice_note } : {}),
    // A Silicon sees a deferred Ting as pending.
    ...(w.ting ? { ting: !ownerView && w.ting === "deferred" ? "pending" : w.ting } : {}),
    ...(w.ting_last_error ? { ting_last_error: w.ting_last_error } : {}),
    ...(w.answer_ting ? { answer_ting: w.answer_ting } : {}),
    ...(host ? { host: { device_id: host.device_id, name: host.name, online: host.online } } : {}),
  };
}

/** A request as `viewer` may see it: the requester's side never sees who got a routed request. */
function requestView(world: World, r: RequestRec, viewer: Member, viewerPair: string | null) {
  const base = { request_id: r.request_id, reason: r.reason, created_at: r.created_at, delivery: r.delivery, ...(r.last_error ? { last_error: r.last_error } : {}), routed_to: r.routed_to };
  if (r.routed_to === "carbon" && r.routed_to_id === viewer.id && (viewerPair === null || r.holder_device_id === viewerPair)) {
    // The Carbon it was routed to. Carbon decision 2: they see which Silicon asked, and why; its
    // Team only when it is their own Silicon.
    const ownSilicon = world.devices.get(r.device_id)?.owner === viewer.id;
    return { ...base, device_id: r.holder_device_id!, from: r.from, to: viewer.id, session_id: r.session_id, ...(ownSilicon ? { team: r.team } : {}) };
  }
  if (r.routed_to === "carbon") return { ...base, device_id: r.device_id, from: r.from, to: REQUEST_TO_HIDDEN, to_hidden: true, team: r.team };
  return { ...base, device_id: r.device_id, from: r.from, to: r.to, session_id: r.session_id, team: r.team };
}

const REMOVED_WHY: Record<string, string> = {
  device_removed: "its Carbon removed it",
  pair_revoked: "the pair was revoked on the device",
  pair_expired: "it went unused for longer than its pairing lasts",
  left_team: "its Carbon left the team",
};

function notFound(ctx: Ctx, team: string | null): never {
  return fail(
    404,
    "device_not_found",
    `No device ${ctx.params.device_id} is visible to you${team ? ` in team ${team}` : ""} and ${ctx.world.environment ? `test environment ${ctx.world.environment.name}` : "production"}.`,
    "List your devices with `extend device ls`.",
  );
}

/**
 * A device the caller paired, for changing it, whatever X-Org-ID says (1.1). A removed one is refused
 * like the service refuses it: its Carbon hears when and why it was removed; anyone else gets the
 * plain device_not_found.
 */
/** Select an independent organization binding while sharing physical configuration. */
function scopedDevice(d: DeviceRec | undefined, org: string | null): DeviceRec | undefined {
  if (!d || !org) return undefined;
  if (d.team === org) return d;
  const binding = d.organizations?.[org];
  if (!binding) return undefined;
  return new Proxy(d, {
    get(target, key) { return key === "team" ? org : key in binding ? Reflect.get(binding, key) : Reflect.get(target, key); },
    set(target, key, value) { return key in binding ? Reflect.set(binding, key, value) : Reflect.set(target, key, value); },
  });
}

function bindDevice(d: DeviceRec, org: string, visibility: "personal" | "team") {
  if (org === d.team) { d.removed = false; d.removed_at = null; d.removed_reason = null; d.visibility = visibility; }
  else { d.organizations ??= {}; d.organizations[org] = { visibility, removed: false, removed_at: null, removed_reason: null }; }
}

function ownedDevice(ctx: Ctx, member: Member): DeviceRec {
  const d = scopedDevice(ctx.world.devices.get(ctx.params.device_id), teamOf(ctx, member, false));
  if (!/^[0-9a-f]{8}$/.test(ctx.params.device_id))
    fail(400, "invalid_input", `${ctx.params.device_id} is not a device id (8 lowercase hexadecimal characters).`);
  if (d && d.removed && d.owner === member.id)
    fail(
      404,
      "device_not_found",
      `Device ${d.device_id} (${d.name}) was removed at ${d.removed_at}: ${REMOVED_WHY[d.removed_reason ?? "device_removed"] ?? d.removed_reason}. A removed device can't be changed or used.`,
      `Its activity log stays readable: \`extend device activity ${d.device_id}\`, or the device's page on the website. To use the device again, pair it again.`,
      { removed_at: d.removed_at, removed_reason: d.removed_reason },
    );
  if (member.type !== "carbon") fail(403, "carbon_only", `${member.id} is a Silicon. Only the Carbons who paired a device manage it.`, "Ask the Carbon who paired it.");
  if (!d || d.removed || d.owner !== member.id) notFound(ctx, null);
  return d!;
}

/** A device the caller may read: a Carbon's own pair (paired or removed), or a Silicon's grant in its Team. */
function readableDevice(ctx: Ctx, member: Member, team: string | null): DeviceRec {
  team ??= teamOf(ctx, member, false);
  const d = scopedDevice(ctx.world.devices.get(ctx.params.device_id), team);
  if (!d || (d.owner !== member.id && (d.removed || d.visibility !== "team"))) notFound(ctx, team);
  return d!;
}

function logActivity(world: World, deviceId: string, a: Omit<Activity, "id" | "at" | "files" | "args" | "command" | "outcome" | "session_id" | "details" | "team"> & Partial<Activity>) {
  if (!world.activity.has(deviceId)) world.activity.set(deviceId, []);
  world.activity.get(deviceId)!.unshift({ id: randomUUID(), at: iso(now()), files: [], args: null, command: null, outcome: null, session_id: null, details: {}, team: null, ...a });
}

function touch(d: DeviceRec) {
  d.last_activity_at = now();
  d.version += 1;
}

/** A session as api.yaml shapes it (the takeover is its own resource). */
function publicSession(s: SessionRec) {
  const { takeover: _takeover, ...rest } = s;
  return rest;
}

function endSession(world: World, s: SessionRec, reason: string, details: Record<string, unknown> = {}) {
  if (s.state === "ended") return s;
  s.state = "ended";
  s.ended_at = iso(now());
  s.idle_ends_at = null;
  s.end_reason = reason;
  s.takeover = null;
  const byOther = details.stopped_by === "another_carbon";
  logActivity(world, s.device_id, {
    actor: byOther ? { type: "carbon", id: "extend" } : { type: "silicon", id: s.silicon_id },
    action: "session_ended",
    session_id: s.session_id,
    team: s.team,
    details: { reason, ...details },
  });
  return s;
}

/** Ends open wake requests (on a pair, or on the whole instance) with a reason. */
function endWakes(world: World, match: (w: WakeRec) => boolean, state: WakeRec["state"], reason: string): WakeRec[] {
  const ended: WakeRec[] = [];
  for (const w of world.wakes)
    if (w.state === "open" && match(w)) {
      w.state = state;
      w.ended_at = iso(now());
      w.end_reason = reason;
      if (state === "woken" || state === "declined") w.answer_ting = "delivered";
      ended.push(w);
    }
  return ended;
}

function newSessionId(world: World): string {
  for (let len = 3; ; len++) {
    for (let tries = 0; tries < 50; tries++) {
      const id = hex(len);
      if (!world.sessions.has(id)) return id;
    }
  }
}

function paginate<T>(items: T[], url: URL): { items: T[]; next_cursor: string | null } {
  const limitRaw = url.searchParams.get("limit");
  const limit = limitRaw === null ? 50 : Number(limitRaw);
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) fail(400, "invalid_input", `limit must be 1–100; got ${limitRaw}.`);
  const start = Number(url.searchParams.get("cursor") || 0);
  const page = items.slice(start, start + limit);
  return { items: page, next_cursor: start + limit < items.length ? String(start + limit) : null };
}

function issueTokens(world: World, member: Member, family: string = randomUUID(), org = member.teams[0]) {
  const access = `oat_${b64(32)}`;
  const refresh = `ort_${b64(32)}`;
  world.accessTokens.set(access, { member: member.id, expires: now() + config.accessTtlS * 1000, family, org });
  world.refreshTokens.set(refresh, { member: member.id, family, used: false, org });
  return {
    access_token: access,
    refresh_token: refresh,
    token_type: "Bearer",
    expires_in: config.accessTtlS,
    member: { type: member.type, id: member.id, display_name: member.display_name },
    teams: [org],
    testing_environment: environmentView(world),
  };
}

// ───────────── Routes ─────────────

type Handler = (ctx: Ctx) => Reply | Promise<Reply>;
const routes: [string, RegExp, string[], Handler][] = [];
function route(method: string, pattern: string, handler: Handler) {
  const names: string[] = [];
  const re = new RegExp(`^${pattern.replace(/:([a-z_]+)/g, (_m, n) => (names.push(n), "([^/]+)"))}$`);
  routes.push([method, re, names, handler]);
}

route("GET", "/live", () => none());
route("GET", "/ready", () => none());

route("GET", "/api/version", (ctx) => {
  const raw = header(ctx, "silicon-extend-supported-api-versions");
  if (!raw || !/^[1-9][0-9]*(, ?[1-9][0-9]*)*$/.test(raw))
    fail(400, "invalid_input", "Silicon-Extend-Supported-API-Versions is required, e.g. \"1, 2\".");
  const versions = raw!.split(",").map((v) => Number(v.trim()));
  if (!versions.includes(1)) fail(400, "api_version_unsupported", `No API version in common: you support ${versions.join(", ")}, Extend supports 1.`, "Update with `honeycomb install 'extend'`.", { client: versions, service: [1] });
  return ok(200, "version", { api_version: 1, supported: [1], service_version: "1.0.0-mock", deprecated: [] }, { "Silicon-Extend-API-Version": "1", Vary: "Silicon-Extend-Supported-API-Versions" });
});

route("GET", "/api/v1/contracts", () =>
  ok(200, "contracts", { versions: [{ api_version: 1, state: "current", deprecated_at: null, sunset_rule: "Sunset after 7 consecutive days with zero requests", compatible: { client_crate: ">=1.0.0, <2.0.0", cli: ">=1.0.0, <2.0.0", device_app_min: "1.0.0" } }] }),
);

route("GET", "/api/v1/iam", (ctx) =>
  ok(200, "iam", {
    app_id: "extend",
    iam_base_url: `${ctx.origin}/__mock/iam`,
    iam_login_url: `${ctx.origin}/__mock/iam/login`,
    api_base_url: ctx.origin,
    website_url: ctx.origin,
    docs_url: `${ctx.origin}/docs`,
    repository_url: "https://github.com/teamofsilicons/silicon-extend",
    testing_environment: environmentView(ctx.world),
  }),
);

route("GET", "/api/v1/testing-environment", (ctx) => {
  if (!ctx.world.environment) fail(401, "testing_secret_invalid", "X-Testing-Application-Secret is required here.", "Send the test application's app_secret.");
  return ok(200, "testing_environment", environmentView(ctx.world));
});

// Auth
route("POST", "/api/v1/auth/login", (ctx) => {
  requireKey(ctx);
  const data = envelope(ctx, "login");
  onlyKeys(data, ["slt"]);
  const slt = data.slt;
  if (typeof slt !== "string" || !slt || slt.length > 4096) fail(422, "invalid_input", "slt must be a non-empty string of at most 4096 characters.");
  const [s, requestedOrg] = (slt as string).trim().split("@", 2);
  const w = ctx.world;
  let member: Member | undefined;
  if (/^(c|si):/.test(s)) {
    if (!w.environment)
      fail(401, "slt_invalid", "Signing in with a member id works only in a test environment. In production, use a short-lived token from Silicon IAM.", "Get an SLT from the IAM consent screen or `iam token`, or enter a test environment first.");
    member = w.members.get(s);
    if (!member) fail(401, "slt_invalid", `${s} is not an active member of test environment ${w.environment!.name}.`, "Use an existing test Carbon or Silicon id, such as c:alice.");
  } else {
    const minted = mintedSlts.get(s);
    if (minted) {
      mintedSlts.delete(s);
      if (minted.expires < now()) fail(401, "slt_invalid", "IAM rejected the short-lived token: it expired (they last 2 minutes).", "Sign in again to get a fresh one.");
      if (minted.world !== w.key) fail(401, "slt_invalid", "IAM rejected the short-lived token: it was issued for a different environment.", "Sign in again from this environment.");
      member = w.members.get(minted.member);
    } else {
      const m = /^oac_(si_)?([a-z0-9_-]+)$/.exec(s);
      if (m) member = w.members.get(`${m[1] ? "si" : "c"}:${m[2]}`);
    }
    if (!member) fail(401, "slt_invalid", "IAM rejected the short-lived token: expired, used, or for another application.", "Get a new SLT and sign in again.");
  }
  if (requestedOrg && !member!.teams.includes(requestedOrg)) fail(403, "not_a_team_member", "Select an organization you belong to.");
  return ok(200, "login", issueTokens(w, member!, undefined, requestedOrg), { "Cache-Control": "no-store" });
});

route("POST", "/api/v1/auth/refresh", (ctx) => {
  requireKey(ctx);
  const data = envelope(ctx, "refresh");
  onlyKeys(data, ["refresh_token"]);
  const token = String(data.refresh_token ?? "");
  const rec = ctx.world.refreshTokens.get(token);
  if (!rec || ctx.world.revokedFamilies.has(rec.family))
    fail(401, "token_expired", "The refresh token is unknown or its session ended.", "Get a new SLT and sign in again.");
  if (rec!.used) {
    ctx.world.revokedFamilies.add(rec!.family);
    fail(401, "token_expired", "This refresh token was already used, so the whole session was revoked (a reused refresh token means it may have leaked).", "Sign in again.");
  }
  rec!.used = true;
  const member = ctx.world.members.get(rec!.member)!;
  return ok(200, "refresh", issueTokens(ctx.world, member, rec!.family, rec!.org), { "Cache-Control": "no-store" });
});

route("POST", "/api/v1/auth/logout", (ctx) => {
  requireKey(ctx);
  const data = envelope(ctx, "logout");
  const token = String(data.token ?? "");
  const rec = ctx.world.refreshTokens.get(token);
  if (rec) ctx.world.revokedFamilies.add(rec.family);
  return none();
});

route("GET", "/api/v1/auth/me", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member, false);
  return ok(200, "me", {
    authenticated: true,
    member: { type: member.type, id: member.id, display_name: member.display_name },
    teams: member.teams,
    team,
    team_role: null,
    testing_environment: environmentView(ctx.world),
  });
});

// Enrollment (the device app)
route("POST", "/api/v1/enrollments", (ctx) => {
  const data = envelope(ctx, "enrollment");
  const os = data.os as Os;
  if (!(os in SETUP_STEPS)) fail(422, "invalid_input", `os must be one of ${Object.keys(SETUP_STEPS).join(", ")}.`);
  const e = newEnrollment(os, (data.model as string) ?? null);
  e.os_version = (data.os_version as string) ?? null;
  e.app_version = String(data.app_version ?? "1.0.0");
  return ok(201, "enrollment", { enrollment_id: e.enrollment_id, enrollment_secret: e.secret, pairing_code: e.code, code_expires_at: iso(e.code_expires), rotates_every_s: 300 });
});

route("GET", "/api/v1/enrollments/:enrollment_id", (ctx) => {
  const e = enrollments.get(ctx.params.enrollment_id);
  if (!e) fail(404, "enrollment_not_found", "No such enrollment. It paired already or was discarded.");
  if (header(ctx, "authorization") !== `Extend-Enrollment ${e!.secret}`) fail(401, "enrollment_secret_invalid", "Authorization must be Extend-Enrollment <enrollment_secret>.");
  if (e!.paired) {
    enrollments.delete(e!.enrollment_id);
    const w = worlds.get(e!.paired.world)!;
    return ok(200, "enrollment", { state: "paired", device_id: e!.paired.device_id, device_credential: `edc_${b64(32)}`, environment: environmentView(w) });
  }
  if (e!.code_expires < now()) {
    e!.code = hex(6).toUpperCase();
    e!.code_expires = now() + 300_000;
  }
  return ok(200, "enrollment", { state: "waiting", pairing_code: e!.code, code_expires_at: iso(e!.code_expires) });
});

// Pairing
route("POST", "/api/v1/pairings", (ctx) => {
  const member = caller(ctx);
  // 1.1: X-Org-ID isn't needed; devices belong to the Carbon. It is only the pair's informational Team.
  const team = teamOf(ctx, member, false) ?? member.teams[0] ?? TEAM;
  carbonOnly(member);
  requireKey(ctx);
  const data = envelope(ctx, "pairing");
  onlyKeys(data, ["pairing_code", "name", "visibility", "pair_ttl_days", "silicon_ids"]);
  const code = data.pairing_code;
  if (typeof code !== "string" || !/^[0-9A-Fa-f]{6}$/.test(code))
    fail(400, "invalid_input", `pairing_code must be 6 hexadecimal characters; got ${JSON.stringify(code)}.`, "Enter the 6 characters the Extend app shows, in any case.", { field: "pairing_code" });
  const name = checkName(data.name);
  if (data.visibility !== undefined) checkVisibility(data.visibility); // accepted and ignored (1.1)
  const ttl = data.pair_ttl_days === undefined ? 14 : checkTtl(data.pair_ttl_days);
  const siliconIds = (data.silicon_ids ?? []) as string[];
  if (!Array.isArray(siliconIds)) fail(422, "invalid_input", "silicon_ids must be a list of si: ids.");
  if (siliconIds.length && !header(ctx, "x-org-id"))
    fail(422, "invalid_input", "silicon_ids gives access in the X-Org-ID Team, and none was sent.", "Pair first, then give access per Team.");
  siliconIds.forEach((id) => checkSilicon(ctx.world, team, id));

  const w = ctx.world;
  const failures = (w.failedClaims.get(member.id) ?? []).filter((t) => t > now() - 10 * MIN);
  if (failures.length >= 5)
    fail(429, "rate_limited", "Too many wrong pairing codes: 5 in the last 10 minutes.", `Try again after ${new Date(failures[0] + 10 * MIN).toISOString()}.`);
  const enrollment = [...enrollments.values()].find((e) => !e.paired && e.code === (code as string).toUpperCase() && e.code_expires > now());
  if (!enrollment) {
    w.failedClaims.set(member.id, [...failures, now()]);
    fail(404, "pairing_code_invalid", "That pairing code is wrong, expired or already used.", "Codes rotate every 5 minutes; enter the one the Extend app shows now.");
  }
  // "Pair with another Carbon": the enrollment names a live pair, so this adds a pair to that device.
  const sibling = enrollment!.from_device_id ? w.devices.get(enrollment!.from_device_id) : undefined;
  const liveSibling = sibling && !sibling.removed ? sibling : undefined;
  if (liveSibling) {
    const mine = pairsOf(w, liveSibling.instance_id).find((x) => x.owner === member.id);
    if (mine) fail(409, "conflict", `You already paired this device: it's ${mine.name} (${mine.device_id}) in your devices.`, "Open it from your device list; each Carbon pairs a device once.");
  }
  // The test limit counts physical devices: a new pair of one already paired doesn't count.
  if (w.environment && !liveSibling && new Set([...w.devices.values()].filter((d) => !d.removed).map((d) => d.instance_id)).size >= 5)
    fail(409, "test_device_limit", "In test environment you are limited to 5 paired devices per environment.", "Remove a device from this test environment, or clean it in Honeycomb.", { device_limit: 5 });

  const device_id = hex(8);
  const d: DeviceRec = {
    device_id,
    instance_id: liveSibling?.instance_id ?? randomUUID(),
    name,
    os: enrollment!.os,
    os_version: enrollment!.os_version,
    model: enrollment!.model,
    kind: kindFor(enrollment!.os, enrollment!.model),
    owner: member.id,
    team,
    visibility: data.visibility === undefined ? "personal" : checkVisibility(data.visibility),
    host_device_id: null,
    online: true,
    last_seen_at: iso(now()),
    last_used_at: null,
    last_activity_at: now(),
    paired_at: iso(now()),
    pair_ttl_days: ttl,
    app_version: enrollment!.app_version,
    version: 1,
    removed: false,
    removed_at: null,
    removed_reason: null,
    // A new pair of a device already set up is ready at once.
    setup: liveSibling ? null : { steps: SETUP_STEPS[enrollment!.os], started: now(), codeEnteredAt: null, failures: { ...(pendingFailures.get(enrollment!.os) ?? {}) }, lastRetryAt: null },
    wake_muted: false,
    duplicate: null,
    recognising: false,
  };
  w.devices.set(device_id, d);
  if (!w.instances.has(d.instance_id)) w.instances.set(d.instance_id, { instance_id: d.instance_id, awake: enrollment!.app_version >= "1.1" ? true : null, sleep_state: null, awake_changed_at: iso(now()) });
  enrollment!.paired = { device_id, world: w.key };
  logActivity(w, device_id, { actor: { type: "carbon", id: member.id }, action: "paired", details: { name, access: siliconIds, ...(liveSibling ? { with_existing_pairs: true } : {}) } });
  if (liveSibling)
    for (const other of pairsOf(w, d.instance_id))
      if (other.device_id !== device_id) logActivity(w, other.device_id, { actor: { type: "carbon", id: "extend" }, action: "another_carbon_paired", details: {} });
  for (const id of siliconIds) grantAccess(w, d, id, team, member.id);
  return ok(201, "device", deviceView(w, d, { member, team: null }), { ETag: `"${d.version}"` });
});

function grantAccess(w: World, d: DeviceRec, siliconId: string, team: string, by: string): Grant {
  if (!w.access.has(d.device_id)) w.access.set(d.device_id, new Map());
  const existing = w.access.get(d.device_id)!.get(gkey(team, siliconId));
  if (existing) return existing;
  const g: Grant = { device_id: d.device_id, silicon_id: siliconId, team, granted_by: by, granted_at: iso(now()), last_used_at: null, wake_muted: false };
  w.access.get(d.device_id)!.set(gkey(team, siliconId), g);
  logActivity(w, d.device_id, { actor: { type: "carbon", id: by }, action: "access_granted", team, details: { silicon_id: siliconId } });
  return g;
}

route("POST", "/api/v1/devices/:device_id/attachments", (ctx) => {
  const member = caller(ctx);
  carbonOnly(member);
  requireKey(ctx);
  const host = ownedDevice(ctx, member);
  const data = envelope(ctx, "attachment");
  onlyKeys(data, ["os", "name", "visibility", "pair_ttl_days"]);
  const os = data.os as Os;
  if (!["ios", "ipados", "tvos", "samsung_tv", "lg_tv"].includes(os))
    fail(422, "invalid_input", "os must be ios, ipados, tvos, samsung_tv or lg_tv.", null, { field: "os" });
  const name = checkName(data.name);
  if (data.visibility !== undefined) checkVisibility(data.visibility);
  const ttl = data.pair_ttl_days === undefined ? 14 : checkTtl(data.pair_ttl_days);
  const needsMac = os === "ios" || os === "ipados" || os === "tvos";
  if (needsMac && host.os !== "macos")
    fail(422, "host_not_eligible", `${host.name} (${host.os}) can't host ${os}: iPhones, iPads and Apple TVs pair through a Mac.`, "Pick a paired Mac.");
  if (!needsMac && !["macos", "windows", "linux"].includes(host.os))
    fail(422, "host_not_eligible", `${host.name} (${host.os}) is not a computer, so it can't host a TV.`, "Pick a paired Mac, Windows or Linux computer.");
  if (host.host_device_id) fail(422, "host_not_eligible", `${host.name} is itself paired through another device.`, "Pick a computer running the Extend app.");
  settle(ctx.world, host);
  if (host.setup) fail(409, "device_not_ready", `${host.name} hasn't finished its own setup yet.`, "Finish its setup first.");
  if (!host.online)
    fail(503, "device_offline", `${host.name} is offline (last seen ${host.last_seen_at ?? "never"}), so it can't set up a new device.`, "Wake the computer and open the Extend app, then try again.");
  const w = ctx.world;
  if (w.environment && new Set([...w.devices.values()].filter((d) => !d.removed).map((d) => d.instance_id)).size >= 5)
    fail(409, "test_device_limit", "In test environment you are limited to 5 paired devices per environment.", "Remove a device from this test environment, or clean it in Honeycomb.", { device_limit: 5 });
  const device_id = hex(8);
  const d: DeviceRec = {
    device_id,
    instance_id: randomUUID(),
    name,
    os,
    os_version: null,
    model: null,
    kind: kindFor(os, null),
    owner: member.id,
    team: host.team,
    visibility: data.visibility === undefined ? "personal" : checkVisibility(data.visibility),
    host_device_id: host.device_id,
    online: false,
    last_seen_at: null,
    last_used_at: null,
    last_activity_at: now(),
    paired_at: iso(now()),
    pair_ttl_days: ttl,
    app_version: null,
    version: 1,
    removed: false,
    removed_at: null,
    removed_reason: null,
    setup: { steps: SETUP_STEPS[os], started: now(), codeEnteredAt: null, failures: { ...(pendingFailures.get(os) ?? {}) }, lastRetryAt: null },
    wake_muted: false,
    duplicate: null,
    recognising: false,
  };
  w.devices.set(device_id, d);
  w.instances.set(d.instance_id, { instance_id: d.instance_id, awake: null, sleep_state: null, awake_changed_at: null });
  logActivity(w, device_id, { actor: { type: "carbon", id: member.id }, action: "paired", details: { name, through: host.device_id } });
  return ok(201, "device", deviceView(w, d, { member, team: null }), { ETag: `"${d.version}"` });
});

route("GET", "/api/v1/devices/:device_id/setup", (ctx) => {
  const member = caller(ctx);
  const d = readableDevice(ctx, member, teamOf(ctx, member, member.type === "silicon"));
  const view = setupView(ctx.world, d);
  settle(ctx.world, d);
  return ok(200, "setup", view);
});

route("POST", "/api/v1/devices/:device_id/setup/code", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  const data = envelope(ctx, "setup_code");
  onlyKeys(data, ["code"]);
  if (typeof data.code !== "string" || !/^[0-9]{4}$/.test(data.code)) fail(422, "invalid_input", "code must be the 4 digits the Apple TV shows.", null, { field: "code" });
  if (!d.setup || !d.setup.steps.some((s) => s.code) || d.setup.codeEnteredAt !== null)
    fail(409, "setup_code_not_expected", `${d.name} isn't waiting for a setup code.`, "Only an Apple TV asks for one, once, during setup.");
  if (data.code === "0000") fail(422, "setup_code_invalid", "The Apple TV didn't accept that code.", "Enter the code the TV shows now; it changes if you wait too long.");
  d.setup!.codeEnteredAt = now();
  return ok(200, "setup", setupView(ctx.world, d));
});

/**
 * Contract A: run a failed setup step again. The service changes no step itself; here the "device"
 * picks the step up at once and reports it in progress, then done (unless a failure is queued again).
 */
route("POST", "/api/v1/devices/:device_id/setup/retry", (ctx) => {
  const member = caller(ctx);
  if (member.type !== "carbon") fail(403, "carbon_only", `${member.id} is a Silicon. Only a Carbon who paired the device can retry its setup.`, "Ask the Carbon who paired it.");
  const d = ownedDevice(ctx, member);
  // The body may be bare ({"step"}) or an envelope ({"type":"setup_retry","data":{"step"}}), or absent.
  const raw = (ctx.body && typeof ctx.body === "object" && "data" in ctx.body ? ctx.body.data : ctx.body) as { step?: unknown } | null;
  const step = raw && typeof raw.step === "string" ? raw.step : null;
  const view = setupView(ctx.world, d);
  if (step && !view.steps.some((s) => s.key === step))
    fail(400, "invalid_input", `${d.name} has no setup step "${step}". Its steps: ${view.steps.map((s) => s.key).join(", ") || "none"}.`, "Retry one of those steps, or leave out the step to retry every failed one.");
  const failed = view.steps.filter((s) => s.status === "failed" && (!step || s.key === step));
  if (!failed.length)
    fail(409, "conflict", step ? `Nothing to retry: the step "${step}" hasn't failed.` : "Nothing to retry: no setup step has failed.", "Retry a step once it shows it failed.");
  const host = d.host_device_id ? ctx.world.devices.get(d.host_device_id) : null;
  if (!(host ?? d).online) fail(409, "device_offline", `${d.name} is offline. Setup carries on when it reconnects.`, "Open the Extend app on it, or wake the computer it pairs through.");
  const app = (host ?? d).app_version;
  if (!app || app < "1.1")
    fail(426, "upgrade_required", `${d.name} runs Silicon Extend ${app ?? "an older version"}, which can't retry from here. Update it to 1.1, or tap Retry on the device.`, "Update the Extend app on the device, then retry.");
  const t = now();
  if (d.setup?.lastRetryAt && t - d.setup.lastRetryAt < 5000) {
    const retry = Math.ceil((5000 - (t - d.setup.lastRetryAt)) / 1000);
    fail(429, "rate_limited", `A retry was just sent to ${d.name}. Wait ${retry} s before retrying again.`, `Try again in ${retry} s.`, { retry_after_s: retry });
  }
  const keys = failed.map((s) => s.key);
  if (d.setup) {
    // The retried steps run again from now: the steps before them stay done.
    const index = d.setup.steps.findIndex((s) => s.key === keys[0]);
    for (const key of keys) delete d.setup.failures[key];
    const again = retryFailures.get(d.device_id);
    if (again) {
      d.setup.failures[again.step] = again.error;
      retryFailures.delete(d.device_id);
    }
    d.setup.started = t - index * config.stepMs;
    d.setup.lastRetryAt = t;
  }
  retryLog.push({ device_id: d.device_id, step });
  return ok(202, "setup_retry", { retrying: keys });
});

// Devices
route("GET", "/api/v1/devices", (ctx) => {
  const member = caller(ctx);
  const scope = ctx.url.searchParams.get("scope") ?? (member.type === "carbon" ? "mine" : "accessible");
  if (!["mine", "accessible", "team"].includes(scope)) fail(400, "invalid_input", `scope must be mine, accessible or team; got ${scope}.`);
  // 1.1: X-Org-ID is a Silicon's Team; a Carbon's own devices don't need it.
  const team = teamOf(ctx, member, scope === "accessible");
  const online = ctx.url.searchParams.get("online");
  const os = ctx.url.searchParams.get("os");
  const includeRaw = ctx.url.searchParams.get("include_removed");
  if (includeRaw !== null && includeRaw !== "true" && includeRaw !== "false")
    fail(422, "invalid_input", `include_removed must be true or false; got ${JSON.stringify(includeRaw)}.`, "Send include_removed=true with scope=mine to list your removed devices too.");
  const includeRemoved = includeRaw === "true";
  if (includeRemoved && (scope !== "mine" || member.type !== "carbon"))
    fail(
      422,
      "invalid_input",
      `include_removed=true works only with scope=mine: a Carbon can list the devices they paired after they're removed, to read their activity log. This request lists scope=${scope}, which shows paired devices only.`,
      "Drop include_removed, or, as the Carbon who paired the devices, send scope=mine&include_removed=true.",
    );
  let list = [...ctx.world.devices.values()].map(d => scopedDevice(d, team)).filter((d): d is DeviceRec => !!d && (includeRemoved || !d.removed));
  if (scope === "mine") list = list.filter((d) => d.owner === member.id);
  // Kept for the 1.0 website's "Team devices" tab: devices are never visible to Team colleagues since 1.1.
  else if (scope === "team") list = list.filter(d => d.visibility === "team");
  else list = list.filter((d) => d.visibility === "team" && ctx.world.access.get(d.device_id)?.has(gkey(team!, member.id)));
  if (online !== null) list = list.filter((d) => String(d.online && !d.removed) === online);
  if (os) list = list.filter((d) => d.os === os);
  // The service's order: online first, then paired before removed, then name.
  list.sort((a, b) => Number(b.online && !b.removed) - Number(a.online && !a.removed) || Number(a.removed) - Number(b.removed) || a.name.localeCompare(b.name));
  const page = paginate(list, ctx.url);
  return ok(200, "devices", { items: page.items.map((d) => deviceView(ctx.world, d, { member, team })), next_cursor: page.next_cursor });
});

route("GET", "/api/v1/devices/importable", ctx => {
  const member = caller(ctx); carbonOnly(member); const org = teamOf(ctx, member)!;
  const items = [...ctx.world.devices.values()].filter(d => d.owner === member.id && (!scopedDevice(d, org) || scopedDevice(d, org)!.removed))
    .map(d => ({device_id:d.device_id,name:d.name,os:d.os,model:d.model,host_device_id:d.host_device_id}));
  return ok(200,"devices",paginate(items,ctx.url));
});
route("POST", "/api/v1/devices/:device_id/import", ctx => {
  const member = caller(ctx); carbonOnly(member); const org = teamOf(ctx, member)!; requireKey(ctx);
  const data = envelope(ctx,"device_import"); onlyKeys(data,["visibility"]);
  const visibility = data.visibility === undefined ? "personal" : checkVisibility(data.visibility);
  const d = ctx.world.devices.get(ctx.params.device_id);
  if (!d || d.owner !== member.id) notFound(ctx,org);
  if (scopedDevice(d,org) && !scopedDevice(d,org)!.removed) fail(409,"conflict","This device is already in this organization.");
  bindDevice(d!,org,visibility);
  const host = d!.host_device_id ? ctx.world.devices.get(d!.host_device_id) : null;
  if (host && (!scopedDevice(host,org) || scopedDevice(host,org)!.removed)) bindDevice(host,org,"personal");
  return ok(200,"device",deviceView(ctx.world,scopedDevice(d,org)!,{member,team:org}));
});

route("GET", "/api/v1/devices/:device_id", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member, member.type === "silicon");
  const d = readableDevice(ctx, member, team);
  const view = deviceView(ctx.world, d, { member, team }, true);
  if (d.removed)
    return ok(
      200,
      "device",
      { ...view, capabilities: [], missing: [{ capability: "screen.read", reason: "The device was removed, so nothing works on it any more. Pair it again to use it." }], commands: [] },
      { ETag: `"${d.version}"` },
    );
  let caps = CAPABILITIES[d.os];
  const missing: { capability: string; reason: string }[] = [];
  if (d.setup) {
    missing.push(...caps.filter((c) => c.startsWith("screen")).map((c) => ({ capability: c, reason: `Setup isn't finished on ${d.name}.` })));
    caps = caps.filter((c) => !c.startsWith("screen"));
  } else if (d.os === "macos" && d.device_id === "2e7f00d1") {
    missing.push({ capability: "screen.record", reason: "Screen Recording permission is off on this Mac" });
    caps = caps.filter((c) => c !== "screen.record");
  }
  // Carbon decision 3: on a computer several Carbons paired, only the first pair (the Carbon who
  // installed Extend there) gets the terminal.
  const pairs = pairsOf(ctx.world, d.instance_id);
  if (caps.includes("terminal") && new Set(pairs.map((x) => x.owner)).size > 1) {
    const first = [...pairs].sort((a, b) => Date.parse(a.paired_at) - Date.parse(b.paired_at))[0];
    if (first.device_id !== d.device_id) {
      caps = caps.filter((c) => c !== "terminal");
      missing.push({ capability: "terminal", reason: TERMINAL_NOT_SHARED_REASON });
    }
  }
  const commands = [...new Set(caps.flatMap((c) => COMMANDS[c] ?? []))];
  return ok(200, "device", { ...view, capabilities: caps, missing, commands }, { ETag: `"${d.version}"` });
});

function checkIfMatch(ctx: Ctx, d: DeviceRec) {
  const raw = header(ctx, "if-match");
  if (!raw || !/^"?[1-9][0-9]*"?$/.test(raw))
    fail(400, "invalid_input", "If-Match is required: send the device version from its ETag.", "Read the device again and send its ETag as If-Match.");
  const version = Number(raw!.replace(/"/g, ""));
  if (version !== d.version)
    fail(412, "version_conflict", `${d.name} changed since you read it (you sent version ${version}; it is now ${d.version}).`, "Read the device again and retry.", { current_version: d.version });
}

route("PATCH", "/api/v1/devices/:device_id", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  checkIfMatch(ctx, d);
  const data = envelope(ctx, "device");
  onlyKeys(data, ["name", "visibility", "pair_ttl_days", "in_use_indicator"]);
  if (!Object.keys(data).length) fail(422, "invalid_input", "Send at least one of name, visibility, pair_ttl_days, in_use_indicator.");
  // Logged like the service: one entry, "renamed" when only the name changed, else "settings_changed".
  // Visibility is accepted and ignored since 1.1: the device stays personal.
  const changes: Record<string, unknown> = {};
  if ("name" in data) {
    const name = checkName(data.name);
    changes.name = { from: d.name, to: name };
    d.name = name;
  }
  if ("visibility" in data) changes.visibility = d.visibility = checkVisibility(data.visibility);
  if ("pair_ttl_days" in data) changes.pair_ttl_days = d.pair_ttl_days = checkTtl(data.pair_ttl_days);
  // One setting for the physical device: every Carbon's pair of it reads the new value (and a new version).
  if ("in_use_indicator" in data) {
    const value = checkIndicator(data.in_use_indicator);
    const instance = ctx.world.instances.get(d.instance_id)!;
    if ((instance.in_use_indicator ?? "shown") !== value) {
      instance.in_use_indicator = value;
      changes.in_use_indicator = value;
      for (const other of pairsOf(ctx.world, d.instance_id)) if (other.device_id !== d.device_id) other.version += 1;
    }
  }
  if (Object.keys(changes).length) {
    const action = "name" in changes && Object.keys(changes).length === 1 ? "renamed" : "settings_changed";
    logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action, details: changes });
  }
  touch(d);
  return ok(200, "device", deviceView(ctx.world, d, { member, team: null }), { ETag: `"${d.version}"` });
});

/** Ends one Carbon's pair; the device's other pairs, and their sessions, are untouched. */
function removeDevice(w: World, d: DeviceRec, by: string, reason = "device_removed", org?: string) {
  for (const sess of w.sessions.values()) if (sess.device_id === d.device_id && sess.state !== "ended" && (!org || sess.team === org)) endSession(w, sess, reason);
  for (const raw of w.devices.values()) {
    const child = org ? scopedDevice(raw,org) : raw;
    if (child && child.host_device_id === d.device_id && !child.removed) removeDevice(w,child,by,reason,org);
  }
  endWakes(w, x => x.device_id === d.device_id && (!org || x.team === org), "withdrawn", "device_removed");
  const grants=w.access.get(d.device_id);
  if (!org) w.access.delete(d.device_id);
  else for(const [key,grant] of grants ?? []) if(grant.team===org) grants!.delete(key);
  d.removed = true; d.removed_at = iso(now()); d.removed_reason = reason;
  logActivity(w,d.device_id,{actor:{type:"carbon",id:by},action:"removed",team:org,details:{reason}});
}

route("DELETE", "/api/v1/devices/:device_id", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  checkIfMatch(ctx, d);
  removeDevice(ctx.world, d, member.id, "device_removed", member.teams[0]);
  return none();
});

/**
 * Stop, by the owner of pair Q: the session holding Q's device through any pair; for a computer also
 * the sessions on carried devices the caller paired. Another side's session is answered as
 * device_stopped (the Silicon isn't named); a carried device the caller didn't pair is 409.
 */
route("POST", "/api/v1/devices/:device_id/stop", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  const w = ctx.world;
  const active = holderOf(w, d);
  const holder = active && active.team === member.teams[0] && w.devices.get(active.device_id)?.owner === member.id ? active : null;
  const stoppable = carriedSessions(w, d).filter((x) => x.team === member.teams[0] && w.devices.get(x.device_id)?.owner === member.id);
  const unstoppable = carriedSessions(w, d).filter((x) => !stoppable.includes(x));
  if (!holder && !stoppable.length) {
    if (unstoppable.length)
      fail(409, "conflict", `A device carried by ${d.name} is in use. It can be stopped by the Carbon who paired it, or from ${d.name}'s Extend app.`, `Open the Extend app on ${d.name} and choose Stop.`);
    fail(409, "device_not_in_use", `Nothing is running on ${d.name}.`, "There is nothing to stop.");
  }
  const ended = [holder, ...stoppable].filter((x): x is SessionRec => !!x);
  let own: SessionRec | null = null;
  for (const s of ended) {
    const pair = w.devices.get(s.device_id)!;
    const other = pair.owner !== member.id;
    endSession(w, s, "stopped_by_carbon", other ? { stopped_by: "another_carbon" } : {});
    if (!other) logActivity(w, s.device_id, { actor: { type: "carbon", id: member.id }, action: "stopped", session_id: s.session_id, team: s.team, details: { silicon_id: s.silicon_id } });
    if (s.device_id === d.device_id) own = s;
  }
  if (own) return ok(200, "session", publicSession(own));
  logActivity(w, d.device_id, { actor: { type: "carbon", id: member.id }, action: "stopped", details: {} });
  return ok(200, "device_stopped", { device_id: d.device_id, stopped_at: iso(now()), in_use_by_other: true });
});

// Access
route("GET", "/api/v1/devices/:device_id/access", (ctx) => {
  const member = caller(ctx);
  const d = readableDevice(ctx, member, teamOf(ctx, member, member.type === "silicon"));
  const items = [...(ctx.world.access.get(d.device_id)?.values() ?? [])].filter(g => g.team === member.teams[0]).sort((a, b) => a.team.localeCompare(b.team) || a.silicon_id.localeCompare(b.silicon_id));
  return ok(200, "access", { items });
});

route("PUT", "/api/v1/devices/:device_id/access/:silicon_id", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  // The Silicon's Team: ?team=, else X-Org-ID. The Carbon's Extend login must reach it.
  const team = ctx.url.searchParams.get("team") ?? teamOf(ctx, member, false);
  if (!team) fail(422, "invalid_input", "Say which Team the Silicon is in: --team <handle>.", "Pick the Team in the access picker.");
  if (!member.teams.includes(team!))
    fail(403, "not_a_team_member", `${member.id}'s Extend login doesn't reach ${team}.`, `Sign in to Extend again and select ${team} (approve Extend for ${team} in Silicon IAM), then retry.`);
  if (d.visibility !== "team") fail(409,"conflict","Make this device visible to the organization before granting control.");
  checkSilicon(ctx.world, team!, ctx.params.silicon_id);
  if (!ctx.world.ting.has(`${member.id}\n${team}`)) ctx.world.ting.set(`${member.id}\n${team}`, { member: member.id, team: team!, status: "on", last_error: null });
  return ok(200, "access_grant", grantAccess(ctx.world, d, ctx.params.silicon_id, team!, member.id));
});

route("DELETE", "/api/v1/devices/:device_id/access/:silicon_id", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  // With ?team= that Team's grant; without it, every Team's (the 1.0 meaning). Ownership alone decides.
  const team = ctx.url.searchParams.get("team") ?? member.teams[0];
  if (team !== member.teams[0]) fail(403,"not_a_team_member","Select this organization first.");
  const grants = ctx.world.access.get(d.device_id);
  for (const g of [...(grants?.values() ?? [])]) {
    if (g.silicon_id !== ctx.params.silicon_id || (team && g.team !== team)) continue;
    grants!.delete(gkey(g.team, g.silicon_id));
    logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action: "access_revoked", team: g.team, details: { silicon_id: g.silicon_id } });
    for (const s of ctx.world.sessions.values())
      if (s.device_id === d.device_id && s.silicon_id === g.silicon_id && s.team === g.team && s.state !== "ended") endSession(ctx.world, s, "access_removed");
    endWakes(ctx.world, (w) => w.device_id === d.device_id && w.from === g.silicon_id && w.team === g.team, "withdrawn", "access_removed");
  }
  return none();
});

// Sessions (a Carbon reads them; Silicons start them)
route("GET", "/api/v1/sessions", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member, member.type === "silicon");
  const deviceId = ctx.url.searchParams.get("device_id");
  const state = ctx.url.searchParams.get("state");
  let list = [...ctx.world.sessions.values()].filter((s) => {
    const d = ctx.world.devices.get(s.device_id);
    if (!d) return false;
    return s.team === team && (member.type === "carbon" ? d.owner === member.id : s.silicon_id === member.id);
  });
  if (deviceId) list = list.filter((s) => s.device_id === deviceId);
  if (state) list = list.filter((s) => s.state === state);
  list.sort((a, b) => Date.parse(b.started_at) - Date.parse(a.started_at));
  const page = paginate(list, ctx.url);
  return ok(200, "sessions", { items: page.items.map(publicSession), next_cursor: page.next_cursor });
});

route("POST", "/api/v1/sessions", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  requireKey(ctx);
  if (member.type !== "silicon") fail(403, "silicon_only", "Only Silicons start sessions. Carbons manage devices.", "Ask your Silicon to run `extend session new <device_id>`.");
  const data = envelope(ctx, "session");
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d || d.removed) fail(404, "device_not_found", `No device ${String(data.device_id)} is visible to you in team ${team}.`);
  return startSession(ctx.world, d!, member.id, team);
});

function startSession(w: World, d: DeviceRec, siliconId: string, team: string): Reply {
  if (!w.access.get(d.device_id)?.has(gkey(team, siliconId)))
    fail(403, "no_access", `${siliconId} has no access to ${d.device_id} (${d.name}) in ${team}.`, "The Carbon who paired it can give access with `extend device access grant`.");
  const holder = holderOf(w, d);
  if (holder) {
    const sameSide = holder.device_id === d.device_id && holder.team === team;
    if (sameSide)
      fail(409, "device_in_use", `Device ${d.device_id} (${d.name}) is being used by ${holder.silicon_id} in session ${holder.session_id} since ${holder.started_at}. Only one Silicon can use a device at a time.`, `Ask for it: extend --team ${team} request send ${d.device_id} --reason "<why, up to 300 characters>"`, { in_use: inUseView(holder, false) });
    fail(409, "device_in_use", `Device ${d.device_id} (${d.name}) is in use. Only one Silicon can use a device at a time.`, `Ask for it: extend --team ${team} request send ${d.device_id} --reason "..."`, { in_use: { hidden: true } });
  }
  const t = now();
  const s: SessionRec = { session_id: newSessionId(w), device_id: d.device_id, silicon_id: siliconId, team, state: "active", started_at: iso(t), last_command_at: null, idle_ends_at: iso(t + 300_000), ended_at: null, end_reason: null, command_count: 0 };
  w.sessions.set(s.session_id, s);
  d.last_used_at = iso(t);
  // The pair lifetime counts the device's activity: every pair of it stays alive.
  for (const p of pairsOf(w, d.instance_id)) p.last_activity_at = t;
  const grant = w.access.get(d.device_id)!.get(gkey(team, siliconId))!;
  grant.last_used_at = iso(t);
  logActivity(w, d.device_id, { actor: { type: "silicon", id: siliconId }, action: "session_started", session_id: s.session_id, team });
  return ok(201, "session", publicSession(s));
}

// Takeovers: the Silicon hands the device to its Carbon; the owner Carbon reads and ends them.
function sessionFor(ctx: Ctx, member: Member): SessionRec {
  const sess = ctx.world.sessions.get(ctx.params.session_id);
  const d = sess ? ctx.world.devices.get(sess.device_id) : undefined;
  const team = member.type === "silicon" ? teamOf(ctx, member) : null;
  if (!sess || !d || !(member.type === "carbon" ? d.owner === member.id : sess.silicon_id === member.id && sess.team === team))
    fail(404, "session_not_found", `No session ${ctx.params.session_id} is visible to you.`, "List sessions with `extend session ls`.");
  return sess!;
}
const takeoverView = (s: SessionRec) => s.takeover ?? null;

route("POST", "/api/v1/sessions/:session_id/takeover", (ctx) => {
  const member = caller(ctx);
  const sess = sessionFor(ctx, member);
  if (sess.silicon_id !== member.id) fail(403, "not_session_owner", "Only the Silicon using the session can hand the device over.");
  const data = envelope(ctx, "takeover");
  const reason = String(data.reason ?? "").trim();
  if (!reason || [...reason].length > 300) fail(422, "invalid_input", "reason must be 1–300 characters.");
  if (sess.state !== "active") fail(409, "session_ended", `Session ${sess.session_id} is ${sess.state}.`);
  const t = now();
  sess.state = "paused";
  sess.takeover = { takeover_id: randomUUID(), session_id: sess.session_id, reason, started_at: iso(t), expires_at: iso(t + 30 * MIN) };
  logActivity(ctx.world, sess.device_id, { actor: { type: "silicon", id: member.id }, action: "takeover_started", session_id: sess.session_id, team: sess.team, details: { reason } });
  return ok(201, "takeover", sess.takeover);
});

route("GET", "/api/v1/sessions/:session_id/takeover", (ctx) => {
  const member = caller(ctx);
  return ok(200, "takeover", takeoverView(sessionFor(ctx, member)));
});

route("DELETE", "/api/v1/sessions/:session_id/takeover", (ctx) => {
  const member = caller(ctx);
  const sess = sessionFor(ctx, member);
  if (sess.state !== "paused" || !sess.takeover) fail(409, "not_paused", `Session ${sess.session_id} isn't handed over to you.`, "Nothing to release.");
  sess.state = "active";
  sess.takeover = null;
  sess.idle_ends_at = iso(now() + 300_000);
  logActivity(ctx.world, sess.device_id, { actor: { type: member.type, id: member.id }, action: "takeover_released", session_id: sess.session_id });
  return none();
});

// Team directory: X-Org-ID's Silicons (1.0), or with team=any every Team the login reaches (1.1).
route("GET", "/api/v1/team/silicons", (ctx) => {
  const member = caller(ctx);
  const silicons = (team: string) =>
    [...ctx.world.members.values()]
      .filter((m) => m.type === "silicon" && m.teams.includes(team))
      .map((m) => ({ id: m.id, display_name: m.display_name ?? null }))
      .sort((a, b) => a.id.localeCompare(b.id));
  if (ctx.url.searchParams.get("team") === "any") {
    const teams = member.teams.filter((t) => !failingDirectories.has(t));
    return ok(200, "team_silicons", {
      items: teams.flatMap((team) => silicons(team).map((m) => ({ ...m, team }))),
      teams: member.teams.map((team) =>
        failingDirectories.has(team) ? { team, ok: false, error: { code: "service_unavailable", message: `Silicon IAM didn't answer for ${team}.`, hint: "Try again in a minute." } } : { team, ok: true },
      ),
    });
  }
  const team = teamOf(ctx, member)!;
  return ok(200, "team_silicons", { items: silicons(team) });
});

// Requests and activity
route("GET", "/api/v1/devices/:device_id/requests", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member, member.type === "silicon");
  const d = readableDevice(ctx, member, team);
  const rows = ctx.world.requests
    .filter((r) =>
      member.type === "carbon"
        ? r.device_id === d.device_id || (r.routed_to === "carbon" && r.routed_to_id === member.id && r.holder_device_id === d.device_id)
        : r.device_id === d.device_id && r.team === team && (r.from === member.id || (r.to === member.id && r.routed_to === "holder")),
    )
    .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))
    .map((r) => requestView(ctx.world, r, member, member.type === "carbon" ? d.device_id : null));
  return ok(200, "requests", paginate(rows, ctx.url));
});

route("GET", "/api/v1/devices/:device_id/activity", (ctx) => {
  const member = caller(ctx);
  const d = readableDevice(ctx, member, teamOf(ctx, member, member.type === "silicon"));
  const q = ctx.url.searchParams;
  let list = (ctx.world.activity.get(d.device_id) ?? []).filter(a => !a.team || a.team === member.teams[0]);
  const silicon = q.get("silicon_id");
  const session = q.get("session_id");
  const since = q.get("since");
  const until = q.get("until");
  if (silicon && !/^si:/.test(silicon)) fail(400, "invalid_input", `silicon_id must start with si:; got ${silicon}.`);
  if (session && !/^[0-9a-f]{3,}$/.test(session)) fail(400, "invalid_input", `session_id is 3 or more lowercase hexadecimal characters; got ${session}.`);
  for (const [name, v] of [["since", since], ["until", until]] as const)
    if (v && Number.isNaN(Date.parse(v))) fail(400, "invalid_input", `${name} must be an RFC 3339 time; got ${v}.`);
  if (silicon) list = list.filter((a) => a.actor.id === silicon);
  if (session) list = list.filter((a) => a.session_id === session);
  if (since) list = list.filter((a) => Date.parse(a.at) >= Date.parse(since));
  if (until) list = list.filter((a) => Date.parse(a.at) <= Date.parse(until));
  return ok(200, "activity", paginate(list, ctx.url));
});

// Waking (1.1)
route("POST", "/api/v1/devices/:device_id/wake-requests", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  if (member.type !== "silicon") fail(403, "silicon_only", "Only Silicons ask to wake a device.", "Wake it yourself, then tell your Silicon.");
  const d = readableDevice(ctx, member, team);
  const data = envelope(ctx, "wake_request");
  const reason = String(data.reason ?? "");
  if (!reason.trim() || [...reason.trim()].length > 300) fail(422, "invalid_input", "reason must be 1–300 characters.");
  if (d.wake_muted) fail(409, "conflict", `${d.owner} has turned off wake requests for ${d.name}.`);
  const instance = ctx.world.instances.get(d.instance_id)!;
  if (d.online && instance.awake) fail(409, "conflict", `${d.name} is already awake.`, `extend --team ${team} session new ${d.device_id}`);
  const t = now();
  const w: WakeRec = {
    wake_id: randomUUID(),
    device_id: d.device_id,
    instance_id: d.instance_id,
    team,
    from: member.id,
    to: d.owner,
    reason,
    created_at: iso(t),
    last_asked_at: iso(t),
    asks: 1,
    expires_at: iso(t + 30 * MIN),
    state: "open",
    ended_at: null,
    end_reason: null,
    device_notice: !d.online ? "offline" : d.host_device_id || !d.app_version || d.app_version < "1.1" ? "unsupported" : "sent",
    device_notice_note: null,
    ting: (ctx.world.tingMissing.get(team) ?? []).includes("extend.device.wake_requested") ? "failed" : "delivered",
    ting_last_error: (ctx.world.tingMissing.get(team) ?? []).includes("extend.device.wake_requested")
      ? `Ting doesn't know the app type extend.device.wake_requested; its notification to ${team} stays pending. A Ting manager in the Team that owns Extend registers it once. Replace <owning-team> with that Team: ting --org '<owning-team>' types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'`
      : null,
    answer_ting: null,
  };
  ctx.world.wakes.push(w);
  logActivity(ctx.world, d.device_id, { actor: { type: "silicon", id: member.id }, action: "wake_requested", team, details: { reason } });
  return ok(201, "wake_request", wakeView(ctx.world, w, { member, team }));
});

route("GET", "/api/v1/devices/:device_id/wake-requests", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member, member.type === "silicon");
  const d = readableDevice(ctx, member, team);
  const state = ctx.url.searchParams.get("state") ?? "all";
  const list = ctx.world.wakes
    .filter((w) => w.device_id === d.device_id && (member.type === "carbon" || (w.from === member.id && w.team === team)) && (state === "all" || w.state === "open"))
    .sort((a, b) => Date.parse(b.last_asked_at) - Date.parse(a.last_asked_at))
    .map((w) => wakeView(ctx.world, w, { member, team }));
  return ok(200, "wake_requests", paginate(list, ctx.url));
});

route("POST", "/api/v1/devices/:device_id/wake-requests/answer", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  const data = envelope(ctx, "wake_answer");
  const answer = data.answer;
  if (answer !== "woken" && answer !== "declined") fail(422, "invalid_input", 'answer must be "woken" or "declined".');
  const ids = Array.isArray(data.wake_ids) ? (data.wake_ids as string[]) : null;
  let ended: WakeRec[];
  if (answer === "woken") {
    // "It's awake" is a fact about the device: every open request on it ends, through every pair and Team.
    ended = endWakes(ctx.world, (w) => w.instance_id === d.instance_id, "woken", "confirmed_by_carbon");
    if (!ended.length) fail(409, "conflict", `Nothing to answer: no Silicon asked to wake ${d.name}.`);
    const instance = ctx.world.instances.get(d.instance_id)!;
    instance.awake = true;
    instance.sleep_state = null;
    instance.awake_changed_at = iso(now());
    for (const pair of new Set(ended.map((w) => w.device_id))) logActivity(ctx.world, pair, { actor: pair === d.device_id ? { type: "carbon", id: member.id } : { type: "carbon", id: "extend" }, action: "wake_confirmed", details: {} });
  } else {
    // "Decline" answers only this Carbon's own pair.
    ended = endWakes(ctx.world, (w) => w.device_id === d.device_id && (!ids || ids.includes(w.wake_id)), "declined", "declined");
    if (!ended.length) fail(409, "conflict", `Nothing to decline on ${d.name}.`);
    for (const w of ended) logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action: "wake_declined", team: w.team, details: { silicon_id: w.from } });
  }
  return ok(200, "wake_answer", { answer, ended: ended.filter((w) => w.device_id === d.device_id).map((w) => wakeView(ctx.world, w, { member, team: null })) });
});

route("PUT", "/api/v1/devices/:device_id/wake-settings", (ctx) => {
  const member = caller(ctx);
  const d = ownedDevice(ctx, member);
  const data = envelope(ctx, "wake_settings");
  if (typeof data.muted !== "boolean") fail(422, "invalid_input", "muted must be true or false.");
  const muted = data.muted as boolean;
  const silicon = typeof data.silicon_id === "string" ? data.silicon_id : null;
  const team = typeof data.team === "string" ? data.team : null;
  if (silicon) {
    const grants = [...(ctx.world.access.get(d.device_id)?.values() ?? [])].filter((g) => g.silicon_id === silicon && (!team || g.team === team));
    if (!grants.length) fail(422, "invalid_input", `${silicon} has no access to ${d.name}${team ? ` in ${team}` : ""}.`);
    for (const g of grants) g.wake_muted = muted;
    if (muted) endWakes(ctx.world, (w) => w.device_id === d.device_id && w.from === silicon && (!team || w.team === team), "withdrawn", "muted");
  } else {
    d.wake_muted = muted;
    if (muted) endWakes(ctx.world, (w) => w.device_id === d.device_id, "withdrawn", "muted");
  }
  logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action: muted ? "wake_muted" : "wake_unmuted", team, details: silicon ? { silicon_id: silicon } : {} });
  const silicons_muted = [...(ctx.world.access.get(d.device_id)?.values() ?? [])].filter((g) => g.wake_muted).map((g) => ({ silicon_id: g.silicon_id, team: g.team }));
  return ok(200, "wake_settings", { device_id: d.device_id, muted: d.wake_muted, silicons_muted });
});

// Ting (1.1)
function tingView(world: World, member: Member, team: string) {
  const rec = world.ting.get(`${member.id}\n${team}`);
  const reached = member.teams.includes(team);
  return {
    team,
    member: member.id,
    status: rec?.status ?? "pending",
    ...(rec?.status === "on" ? { registered_at: iso(now() - DAY) } : {}),
    ...(!reached ? { last_error: `Sign in to Extend for ${team}` } : rec?.last_error ? { last_error: rec.last_error } : {}),
    missing_types: world.tingMissing.get(team) ?? [],
  };
}

route("GET", "/api/v1/permissions", (ctx) => {
  const member = caller(ctx), team = teamOf(ctx, member)!;
  return ok(200, "permissions", { items: ctx.world.featureGrants.get(gkey(team, member.id)) ?? [] });
});

route("POST", "/api/v1/permissions", (ctx) => {
  requireKey(ctx);
  const member = caller(ctx), team = teamOf(ctx, member)!;
  const data = envelope(ctx, "permission");
  const endpoints = data.endpoints as { audience: string; endpoint_id: string }[];
  if (!Array.isArray(endpoints) || !endpoints.length || endpoints.length > 16 || endpoints.some((e) => !e.audience || !e.endpoint_id))
    fail(422, "invalid_input", "Choose between one and sixteen endpoints.");
  const id = randomUUID(), expires = now() + 10 * MIN;
  ctx.world.featureRequests.set(id, { member: member.id, team, endpoints, expires, approved: false });
  return ok(200, "permission", { id, consent_url: `${ctx.origin}/__mock/iam/feature?request=${id}`, expires_at: iso(expires) });
});

route("POST", "/api/v1/permissions/:id/complete", (ctx) => {
  requireKey(ctx);
  const member = caller(ctx), team = teamOf(ctx, member)!;
  const request = ctx.world.featureRequests.get(ctx.params.id);
  if (!request || request.member !== member.id || request.team !== team) fail(404, "not_found", "Approval request not found for this account and organization.");
  const data = envelope(ctx, "permission");
  if (!request!.approved || request!.expires < now() || data.code !== `obc_mock_${ctx.params.id}`)
    fail(422, "confirmation_required", "Approve this request and copy its demo code.", "Your login remains active.");
  const key = gkey(team, member.id);
  const items = request!.endpoints.map((endpoint) => ({ ...endpoint, grant_id: ctx.params.id, actor: { public_id: member.id, type: member.type }, org_id: team, expires_at: iso(now() + 30 * MIN) }));
  const previous = ctx.world.featureGrants.get(key) as { audience: string; endpoint_id: string }[] | undefined;
  ctx.world.featureGrants.set(key, [...(previous ?? []).filter((old) => !items.some((item) => item.audience === old.audience && item.endpoint_id === old.endpoint_id)), ...items]);
  return ok(200, "permissions", { items: ctx.world.featureGrants.get(key) });
});

route("GET", "/__mock/iam/feature", (ctx) => {
  const id = ctx.url.searchParams.get("request") ?? "";
  const request = [...worlds.values()].map((world) => world.featureRequests.get(id)).find(Boolean);
  if (!request || request.expires < now()) fail(404, "not_found", "This demo approval expired.");
  if (ctx.url.searchParams.get("approve") === "yes") {
    request!.approved = true;
    return { status: 200, html: `<h1>Demo feature approval</h1><p>This is local test data. Copy this single-use demo code into Extend:</p><code>obc_mock_${escapeHtml(id)}</code>` };
  }
  return { status: 200, html: `<h1>Demo feature approval</h1><p>Local test data only. Extend requests access for ${escapeHtml(request!.member)} in ${escapeHtml(request!.team)}.</p><ul>${request!.endpoints.map((e) => `<li>${escapeHtml(e.audience)} · ${escapeHtml(e.endpoint_id)}</li>`).join("")}</ul><form><input type="hidden" name="request" value="${escapeHtml(id)}"><button name="approve" value="yes">Approve demo request</button></form>` };
});

route("GET", "/api/v1/ting-registration", (ctx) => {
  const member = caller(ctx);
  const team = ctx.url.searchParams.get("team");
  if (!team) fail(422, "invalid_input", "Say which Team: ?team=<handle>, or ?team=any.");
  if (team === "any") {
    const teams = member.teams;
    return ok(200, "ting_registrations", { items: teams.map((t) => tingView(ctx.world, member, t)) });
  }
  if (!member.teams.includes(team!)) fail(403, "not_a_team_member", `${member.id}'s Extend login doesn't reach ${team}.`, `Sign in to Extend again and select ${team}.`);
  return ok(200, "ting_registration", tingView(ctx.world, member, team!));
});

route("PUT", "/api/v1/ting-registration", (ctx) => {
  const member = caller(ctx);
  const team = ctx.url.searchParams.get("team");
  if (!team || team === "any") fail(422, "invalid_input", "Say which Team to turn Tings on in: ?team=<handle>.");
  if (!member.teams.includes(team!)) fail(403, "not_a_team_member", `${member.id}'s Extend login doesn't reach ${team}.`, `Sign in to Extend again and select ${team}.`);
  ctx.world.ting.set(`${member.id}\n${team}`, { member: member.id, team: team!, status: "on", last_error: null });
  return ok(200, "ting_registration", tingView(ctx.world, member, team!));
});

route("GET", "/api/v1/files", (ctx) => {
  const member = caller(ctx);
  teamOf(ctx, member);
  return ok(200, "files", { items: [], next_cursor: null });
});

route("POST", "/api/v1/telemetry", (ctx) => {
  envelope(ctx, "telemetry");
  telemetryLog.push({ at: iso(now()), off: header(ctx, "x-extend-telemetry") === "off", body: ctx.body });
  return none();
});
const telemetryLog: unknown[] = [];

route("POST", "/api/v1/reports", (ctx) => {
  caller(ctx);
  requireKey(ctx);
  envelope(ctx, "report");
  return ok(202, "report", { report_id: randomUUID(), notification: ctx.world.environment ? "simulated" : "queued", repository_url: "https://github.com/teamofsilicons/silicon-extend" });
});

// ───────────── Mock controls and the stand-in consent screen ─────────────

route("POST", "/__mock/reset", () => {
  seed();
  pendingFailures.clear();
  retryFailures.clear();
  failingDirectories.clear();
  return ok(200, "reset", { ok: true });
});

route("POST", "/__mock/config", (ctx) => {
  const data = (ctx.body ?? {}) as Record<string, unknown>;
  if (typeof data.step_ms === "number") config.stepMs = data.step_ms;
  if (typeof data.access_ttl_s === "number") config.accessTtlS = data.access_ttl_s;
  return ok(200, "config", config);
});

/**
 * Simulates an Extend app showing a pairing code. With `instance_of` (a live pair's id), it is "Pair
 * with another Carbon" on that device; `app_version` (default 1.1.0) lets a 1.0 app pair.
 */
route("POST", "/__mock/enroll", (ctx) => {
  const data = (ctx.body ?? {}) as Record<string, unknown>;
  const sibling = typeof data.instance_of === "string" ? ctx.world.devices.get(data.instance_of) : undefined;
  const os = (data.os as Os) ?? sibling?.os ?? "android";
  const e = newEnrollment(os, (data.model as string) ?? sibling?.model ?? null, undefined, {
    app_version: typeof data.app_version === "string" ? data.app_version : undefined,
    from_device_id: sibling?.device_id,
  });
  return ok(201, "enrollment", { pairing_code: e.code, enrollment_id: e.enrollment_id });
});

/** Simulates a Silicon starting a session (in the world the secret header picks), in `team` (default acme). */
route("POST", "/__mock/sessions", (ctx) => {
  const data = (ctx.body ?? {}) as Record<string, unknown>;
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d) fail(404, "device_not_found", "No such device.");
  return startSession(ctx.world, d!, String(data.silicon_id), typeof data.team === "string" ? data.team : TEAM);
});

/**
 * Makes a setup step fail with a plain-language error: on a device now (`device_id`), or on the next
 * device paired of an OS (`os`). `again` makes the step fail once more after the next retry.
 */
route("POST", "/__mock/fail-step", (ctx) => {
  const data = (ctx.body ?? {}) as { device_id?: string; os?: string; step: string; error: string; again?: boolean };
  if (data.os) {
    pendingFailures.set(data.os, { ...(pendingFailures.get(data.os) ?? {}), [data.step]: data.error });
    return ok(200, "fail_step", { os: data.os });
  }
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d) fail(404, "device_not_found", "No such device.");
  if (!d!.setup) d!.setup = { steps: SETUP_STEPS[d!.os], started: now() - 60_000, codeEnteredAt: now() - 60_000, failures: {}, lastRetryAt: null };
  d!.setup!.failures[data.step] = data.error;
  if (data.again) retryFailures.set(d!.device_id, { step: data.step, error: data.error });
  d!.version += 1;
  return ok(200, "fail_step", setupView(ctx.world, d!));
});

/** Named scenarios for tests. `carried_busy`: Alice's si:pilot moves from the Studio Mac to her iPad it carries. */
route("POST", "/__mock/scenario", (ctx) => {
  const name = String(((ctx.body ?? {}) as Record<string, unknown>).name ?? "");
  const w = ctx.world;
  if (name === "carried_busy") {
    const s = w.sessions.get("f0c");
    if (s) endSession(w, s, "ended_by_silicon");
    return startSession(w, w.devices.get("a9d2c4e7")!, "si:pilot", TEAM);
  }
  return fail(404, "not_found", `No scenario ${name}.`);
});

/** A carried pair held back by the service (1.1): `duplicate_own` (with `of`), `duplicate_other`, `recognising`, or `linked`. */
route("POST", "/__mock/carried", (ctx) => {
  const data = (ctx.body ?? {}) as { device_id: string; state: string; of?: string };
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d || !d.host_device_id) fail(404, "device_not_found", "No such carried device.");
  d!.duplicate = data.state === "duplicate_own" ? { kind: "own", of: String(data.of) } : data.state === "duplicate_other" ? { kind: "other_computer" } : null;
  d!.recognising = data.state === "recognising";
  if (d!.duplicate) logActivity(ctx.world, d!.device_id, { actor: { type: "carbon", id: "extend" }, action: "duplicate_device", details: { kind: data.state === "duplicate_own" ? "same_carbon" : "other_computer" } });
  d!.version += 1;
  return ok(200, "carried", setupView(ctx.world, d!));
});

/** Sets what a device (its instance, so every pair of it) shows while a Silicon uses it, as its Extend app would. */
route("POST", "/__mock/indicator", (ctx) => {
  const data = (ctx.body ?? {}) as { device_id: string; in_use_indicator: string };
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d) fail(404, "device_not_found", "No such device.");
  ctx.world.instances.get(d!.instance_id)!.in_use_indicator = checkIndicator(data.in_use_indicator);
  for (const pair of pairsOf(ctx.world, d!.instance_id)) pair.version += 1;
  return ok(200, "indicator", { device_id: d!.device_id, in_use_indicator: data.in_use_indicator });
});

/** Sets whether a device (its instance, so every pair of it) is awake. */
route("POST", "/__mock/awake", (ctx) => {
  const data = (ctx.body ?? {}) as { device_id: string; awake: boolean | null; sleep_state?: string };
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d) fail(404, "device_not_found", "No such device.");
  const instance = ctx.world.instances.get(d!.instance_id)!;
  instance.awake = data.awake;
  instance.sleep_state = data.awake === false ? (data.sleep_state ?? "screen_off") : null;
  instance.awake_changed_at = iso(now());
  if (data.awake) endWakes(ctx.world, (w) => w.instance_id === d!.instance_id, "woken", "woken_on_device");
  return ok(200, "awake", instance);
});

/** Makes a Team's directory read fail in team=any. */
route("POST", "/__mock/directory-fails", (ctx) => {
  failingDirectories.add(String(((ctx.body ?? {}) as Record<string, unknown>).team));
  return ok(200, "directory", { failing: [...failingDirectories] });
});
/** A later successful notification observes externally registered app types. */
route("POST", "/__mock/ting-types-known", (ctx) => {
  const data = (ctx.body ?? {}) as { team: string };
  ctx.world.tingMissing.delete(data.team);
  return ok(200, "ting_types", { ok: true });
});

route("GET", "/__mock/retries", () => ok(200, "retries", { items: retryLog }));

route("GET", "/__mock/telemetry", () => ok(200, "telemetry", { items: telemetryLog }));

const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

route("GET", "/__mock/iam/login", (ctx) => {
  const appId = ctx.url.searchParams.get("app_id") ?? "";
  const redirect = ctx.url.searchParams.get("redirect_uri") ?? "";
  const carbons = [...worlds.get("production")!.members.values()].filter((m) => m.type === "carbon");
  const buttons = carbons
    .map((m) => `<button name="member" value="${escapeHtml(m.id)}">Continue as ${escapeHtml(m.display_name)} <small>${escapeHtml(m.id)} · ${escapeHtml(m.teams.join(", "))}</small></button>`)
    .join("");
  return {
    status: 200,
    html: `<!doctype html><meta name="viewport" content="width=device-width"><title>Silicon IAM (mock)</title>
<style>body{font:15px system-ui;max-width:420px;margin:60px auto;padding:0 16px;color:#262a29;background:#fffdf9}button{display:block;width:100%;margin:8px 0;padding:14px;border:1px solid #ccd4ef;border-radius:8px;background:#f0f3fd;text-align:left;font:inherit;cursor:pointer}small{display:block;color:#777a74}code{font-size:12px}</style>
<h1>Silicon IAM <small>mock consent screen</small></h1>
<p><b>${escapeHtml(appId)}</b> asks to sign you in and read your identity and teams.</p>
<form method="get" action="/__mock/iam/approve"><input type="hidden" name="redirect_uri" value="${escapeHtml(redirect)}">${buttons}</form>
<p><small>Returns to <code>${escapeHtml(redirect)}</code></small></p>`,
  };
});

route("GET", "/__mock/iam/approve", (ctx) => {
  const member = ctx.url.searchParams.get("member") ?? "c:saket";
  const redirect = new URL(ctx.url.searchParams.get("redirect_uri") ?? "/");
  const slt = `oac_${b64(32)}`;
  mintedSlts.set(slt, { member, world: "production", expires: now() + 120_000 });
  redirect.searchParams.set("slt", slt);
  return { status: 302, redirect: redirect.toString() };
});

// ───────────── Server ─────────────

const ALLOWED_HEADERS = "Authorization, Content-Type, X-Org-ID, X-Testing-Application-Secret, Idempotency-Key, If-Match, X-Extend-Telemetry, Silicon-Extend-API-Version, Silicon-Extend-Supported-API-Versions";

async function handle(req: http.IncomingMessage, res: http.ServerResponse) {
  const requestId = randomUUID();
  const host = (req.headers["x-forwarded-host"] as string) || req.headers.host || `127.0.0.1:${MOCK_PORT}`;
  const proto = (req.headers["x-forwarded-proto"] as string) || "http";
  const origin = `${proto}://${host}`;
  const url = new URL(req.url ?? "/", origin);
  const cors: Record<string, string> = {
    "Access-Control-Allow-Origin": (req.headers.origin as string) || "*",
    "Access-Control-Allow-Methods": "GET, POST, PUT, PATCH, DELETE, OPTIONS",
    "Access-Control-Allow-Headers": ALLOWED_HEADERS,
    "Access-Control-Expose-Headers": "ETag, X-Request-ID, Silicon-Extend-API-Version",
    "Access-Control-Max-Age": "600",
    Vary: "Origin",
  };
  if (req.method === "OPTIONS") {
    res.writeHead(204, cors);
    res.end();
    return;
  }
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk as Buffer);
  const rawBody = Buffer.concat(chunks).toString("utf8");
  const send = (status: number, body: unknown, headers: Record<string, string> = {}) => {
    const h: Record<string, string> = { ...cors, "X-Request-ID": requestId, "Silicon-Extend-API-Version": "1", ...headers };
    if (status === 204) {
      res.writeHead(204, h);
      res.end();
      return;
    }
    res.writeHead(status, { ...h, "Content-Type": "application/json" });
    res.end(JSON.stringify(body));
  };

  const found = routes
    .filter(([, re]) => re.test(url.pathname))
    .map(([method, re, names, handler]) => ({ method, re, names, handler }));
  const r = found.find((f) => f.method === req.method);
  try {
    if (!r) {
      if (found.length) fail(405, "method_not_allowed", `${req.method} is not allowed on ${url.pathname}.`);
      fail(404, "not_found", `No route ${req.method} ${url.pathname} in the mock Extend service.`, "Check api.yaml for the path.");
    }
    const m = r!.re.exec(url.pathname)!;
    const params: Record<string, string> = {};
    r!.names.forEach((n, i) => (params[n] = decodeURIComponent(m[i + 1])));
    let body: Ctx["body"] = null;
    if (rawBody) {
      try {
        body = JSON.parse(rawBody);
      } catch {
        fail(400, "invalid_input", "The body is not valid JSON.");
      }
    }
    const world = url.pathname.startsWith("/__mock/iam") ? worlds.get("production")! : worldFor(req);
    const ctx: Ctx = { req, url, params, body, rawBody, world, requestId, origin };

    // Idempotent replay: same key and body → same answer.
    const key = req.method === "POST" ? header(ctx, "idempotency-key") : null;
    const replayKey = key ? `${world.key}|${header(ctx,"authorization") ?? "anonymous"}|${header(ctx,"x-org-id") ?? ""}|${req.method}|${url.pathname}|${key}` : null;
    const hash = createHash("sha256").update(rawBody).digest("hex");
    if (replayKey && idempotency.has(replayKey)) {
      const prior = idempotency.get(replayKey)!;
      if (prior.hash !== hash) fail(409, "idempotency_conflict", "This Idempotency-Key was used with a different body.", "Use a new key for a new request.");
      send(prior.status, prior.body, { ...prior.headers, "Idempotent-Replayed": "true" });
      return;
    }

    const reply = await r!.handler(ctx);
    if (reply.redirect) {
      res.writeHead(302, { Location: reply.redirect });
      res.end();
      return;
    }
    if (reply.html !== undefined) {
      res.writeHead(reply.status, { "Content-Type": "text/html; charset=utf-8" });
      res.end(reply.html);
      return;
    }
    const payload = reply.status === 204 ? null : { type: reply.type, data: reply.data };
    if (replayKey) idempotency.set(replayKey, { hash, status: reply.status, body: payload, headers: reply.headers ?? {} });
    send(reply.status, payload, reply.headers);
  } catch (error) {
    if (error instanceof MockError) {
      send(error.status, {
        type: "error",
        data: {
          code: error.code,
          message: error.message,
          hint: error.hint,
          docs_url: `https://extend.teamofsilicons.com/docs/cli#error-${error.code}`,
          request_id: requestId,
          details: error.details,
        },
      });
      return;
    }
    console.error(error);
    send(500, { type: "error", data: { code: "internal", message: `The mock failed: ${(error as Error).message}`, hint: "This is a bug in web/mock/server.ts.", request_id: requestId, details: {} } });
  }
}

seed();
const server = http.createServer((req, res) => {
  handle(req, res).catch((error) => {
    console.error(error);
    res.writeHead(500);
    res.end();
  });
});
server.listen(MOCK_PORT, "127.0.0.1", () => {
  console.log(`mock Extend service on http://127.0.0.1:${MOCK_PORT}  (test app_secret: ${TEST_SECRET})`);
});
