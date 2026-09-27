/**
 * An in-memory mock of the Extend service, following understanding/api.yaml, so the website runs
 * and is tested without the Rust service. It checks what the real service checks where that
 * matters to the website: envelopes, X-Org-ID, X-Testing-Application-Secret, Idempotency-Key,
 * If-Match versions, owner-only actions, the test-environment device limit, and token rotation.
 *
 * It also serves a stand-in for the IAM consent screen under /__mock/iam/login, and control
 * endpoints under /__mock/* for tests. Run: `pnpm mock` (port 8490) or `pnpm dev:mock`.
 */
import http from "node:http";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import {
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

interface DeviceRec {
  device_id: string;
  name: string;
  os: Os;
  os_version: string | null;
  model: string | null;
  kind: "phone" | "tablet" | "tv" | "computer";
  owner: string;
  team: string;
  visibility: "team" | "personal";
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
  setup: { steps: StepDef[]; started: number; codeEnteredAt: number | null } | null;
}

interface SessionRec {
  session_id: string;
  device_id: string;
  silicon_id: string;
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
}

interface RequestRec {
  request_id: string;
  device_id: string;
  from: string;
  to: string;
  session_id?: string;
  reason: string;
  created_at: string;
  delivery: "pending" | "delivered" | "failed";
}

interface Environment {
  environment_id: string;
  name: string;
  state: "preparing" | "ready" | "cleaning" | "disabled" | "removed";
}

interface World {
  key: string;
  environment: Environment | null;
  members: Map<string, Member>;
  devices: Map<string, DeviceRec>;
  access: Map<string, Map<string, Grant>>;
  sessions: Map<string, SessionRec>;
  activity: Map<string, Activity[]>;
  requests: Map<string, RequestRec[]>;
  accessTokens: Map<string, { member: string; expires: number; family: string }>;
  refreshTokens: Map<string, { member: string; family: string; used: boolean }>;
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

const hex = (n: number) => randomBytes(Math.ceil(n / 2)).toString("hex").slice(0, n);
const b64 = (n: number) => randomBytes(n).toString("base64url");

function newWorld(key: string, environment: Environment | null): World {
  return {
    key,
    environment,
    members: new Map(),
    devices: new Map(),
    access: new Map(),
    sessions: new Map(),
    activity: new Map(),
    requests: new Map(),
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

function newEnrollment(os: Os, model: string | null, code?: string): Enrollment {
  const e: Enrollment = {
    enrollment_id: randomUUID(),
    secret: `ees_${b64(32)}`,
    os,
    model,
    os_version: null,
    app_version: "1.0.0",
    code: code ?? hex(6).toUpperCase(),
    code_expires: now() + 300_000,
  };
  enrollments.set(e.enrollment_id, e);
  return e;
}

function seed() {
  worlds = new Map();
  enrollments = new Map();
  mintedSlts = new Map();
  idempotency = new Map();
  const t = now();

  // Production
  const prod = newWorld("production", null);
  addMember(prod, { type: "carbon", id: "c:saket", display_name: "Saket", teams: [TEAM, OTHER_TEAM] });
  addMember(prod, { type: "carbon", id: "c:alice", display_name: "Alice", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:chef", display_name: "Chef", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:scout", display_name: "Scout", teams: [TEAM] });
  addMember(prod, { type: "silicon", id: "si:atlas", display_name: "Atlas", teams: [TEAM, OTHER_TEAM] });
  addMember(prod, { type: "silicon", id: "si:juniper", display_name: "Juniper", teams: [OTHER_TEAM] });
  worlds.set(prod.key, prod);

  const device = (d: Partial<DeviceRec> & Pick<DeviceRec, "device_id" | "name" | "os" | "owner" | "team">): DeviceRec => {
    const rec: DeviceRec = {
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
      app_version: "1.0.0",
      version: 1,
      removed: false,
      removed_at: null,
      removed_reason: null,
      setup: null,
      ...d,
    };
    return rec;
  };
  const put = (w: World, d: DeviceRec) => w.devices.set(d.device_id, d);

  put(prod, device({ device_id: "7c1e09ab", name: "Saket's Pixel", os: "android", os_version: "15", model: "Pixel 9", owner: "c:saket", team: TEAM, last_used_at: iso(t - 10_000), last_activity_at: t - 10_000, version: 3 }));
  put(prod, device({ device_id: "2e7f00d1", name: "MacBook Pro", os: "macos", os_version: "15.4", model: "MacBookPro18,3", owner: "c:saket", team: TEAM, last_used_at: iso(t - 2 * 3600_000), last_activity_at: t - 2 * 3600_000, pair_ttl_days: 30, version: 2 }));
  put(prod, device({ device_id: "0d44e1f2", name: "Living room TV", os: "android_tv", os_version: "12", model: "Chromecast with Google TV", owner: "c:saket", team: TEAM, online: false, last_seen_at: iso(t - 2 * DAY), last_used_at: iso(t - 5 * DAY), last_activity_at: t - 5 * DAY }));
  put(prod, device({ device_id: "51ab93c0", name: "Saket's iPhone", os: "ios", os_version: "18.1", model: "iPhone 16", owner: "c:saket", team: TEAM, visibility: "personal", host_device_id: "2e7f00d1", app_version: null, last_used_at: iso(t - 3 * DAY), last_activity_at: t - 3 * DAY }));
  put(prod, device({ device_id: "a9d2c4e7", name: "Alice's iPad", os: "ipados", os_version: "18.0", owner: "c:alice", team: TEAM, host_device_id: null }));
  put(prod, device({ device_id: "b3f81c20", name: "Alice's Windows PC", os: "windows", owner: "c:alice", team: TEAM, visibility: "personal" }));
  put(prod, device({ device_id: "c0ffee42", name: "Lab Linux box", os: "linux", os_version: "Ubuntu 24.04", owner: "c:saket", team: OTHER_TEAM, online: false, last_seen_at: iso(t - 6 * 3600_000) }));

  const grant = (w: World, device_id: string, silicon_id: string, ago: number, lastUsed: number | null = null) => {
    if (!w.access.has(device_id)) w.access.set(device_id, new Map());
    w.access.get(device_id)!.set(silicon_id, {
      device_id,
      silicon_id,
      granted_by: w.devices.get(device_id)!.owner,
      granted_at: iso(t - ago),
      last_used_at: lastUsed === null ? null : iso(t - lastUsed),
    });
  };
  grant(prod, "7c1e09ab", "si:chef", 12 * DAY, 10_000);
  grant(prod, "7c1e09ab", "si:scout", 6 * DAY, 2 * DAY);
  grant(prod, "2e7f00d1", "si:atlas", 3 * DAY, 2 * 3600_000);
  grant(prod, "0d44e1f2", "si:chef", 9 * DAY, 5 * DAY);

  // si:chef is using the Pixel right now, in session a3f.
  prod.sessions.set("a3f", {
    session_id: "a3f",
    device_id: "7c1e09ab",
    silicon_id: "si:chef",
    state: "active",
    started_at: iso(t - 4 * MIN),
    last_command_at: iso(t - 10_000),
    idle_ends_at: iso(t - 10_000 + 300_000),
    ended_at: null,
    end_reason: null,
    command_count: 23,
  });
  prod.sessions.set("7d2", {
    session_id: "7d2",
    device_id: "7c1e09ab",
    silicon_id: "si:scout",
    state: "ended",
    started_at: iso(t - 2 * DAY - 20 * MIN),
    last_command_at: iso(t - 2 * DAY - 6 * MIN),
    idle_ends_at: null,
    ended_at: iso(t - 2 * DAY - MIN),
    end_reason: "idle_timeout",
    command_count: 9,
  });

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
    ...extra,
  });
  pixelLog.push(entry(t - 20 * DAY, "c:saket", "paired", { details: { name: "Saket's Pixel", access: [] } }));
  pixelLog.push(entry(t - 19 * DAY, "c:saket", "settings_changed", { details: { pair_ttl_days: 14, visibility: "team" } }));
  pixelLog.push(entry(t - 12 * DAY, "c:saket", "access_granted", { details: { silicon_id: "si:chef" } }));
  pixelLog.push(entry(t - 6 * DAY, "c:saket", "access_granted", { details: { silicon_id: "si:scout" } }));
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
  prod.activity.set("2e7f00d1", [entry(t - 30 * DAY, "c:saket", "paired"), entry(t - 3 * DAY, "c:saket", "access_granted", { details: { silicon_id: "si:atlas" } })].reverse());

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

  prod.requests.set("7c1e09ab", [
    { request_id: randomUUID(), device_id: "7c1e09ab", from: "si:scout", to: "si:chef", session_id: "a3f", reason: "I need to check the order confirmation in the Swiggy app, 2 minutes", created_at: iso(t - 3 * MIN), delivery: "delivered" },
    { request_id: randomUUID(), device_id: "7c1e09ab", from: "si:chef", to: "si:scout", session_id: "7d2", reason: "Need 2 minutes to check an OTP for the vendor portal", created_at: iso(t - 2 * DAY - 10 * MIN), delivery: "delivered" },
  ]);

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
  const paired = [...world.devices.values()].filter((d) => !d.removed).length;
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
  return member!;
}

function teamOf(ctx: Ctx, member: Member, required = true): string | null {
  const team = header(ctx, "x-org-id");
  if (!team) {
    if (required) fail(400, "invalid_input", "X-Org-ID is required: it names the team (IAM handle) this request is for.", "Send the team handle in X-Org-ID, or pass --team.");
    return null;
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
function setupView(d: DeviceRec): { state: "in_progress" | "needs_carbon" | "complete"; steps: StepView[] } {
  if (!d.setup) return { state: "complete", steps: [] };
  const { steps, started, codeEnteredAt } = d.setup;
  const out: StepView[] = [];
  const push = (step: StepDef, status: StepStatus) => out.push({ key: step.key, title: step.title, status, help: step.help, error: null, input: step.code ? "code" : null });
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
    if (now() >= doneAt) {
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
  if (d.setup && setupView(d).state === "complete") {
    d.setup = null;
    d.version += 1;
    const host = d.host_device_id ? world.devices.get(d.host_device_id) : null;
    d.online = host ? host.online : true;
    d.last_seen_at = iso(now());
  }
}

function inUse(world: World, deviceId: string) {
  const s = [...world.sessions.values()].find((x) => x.device_id === deviceId && x.state !== "ended");
  return s ? { silicon_id: s.silicon_id, session_id: s.session_id, since: s.started_at, paused: s.state === "paused" } : null;
}

function deviceView(world: World, d: DeviceRec, limited = false) {
  settle(world, d);
  const owner = world.members.get(d.owner);
  const base = {
    device_id: d.device_id,
    name: d.name,
    os: d.os,
    kind: d.kind,
    owner: { type: "carbon", id: d.owner, display_name: owner?.display_name ?? null },
    visibility: d.visibility,
    online: d.removed ? false : d.online,
  };
  if (limited) return base;
  if (d.removed) {
    // Like the service: a removed device reads offline, with no in_use, pair_expires_at or days_left.
    return {
      ...base,
      os_version: d.os_version,
      model: d.model,
      team: d.team,
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
    };
  }
  const expires = d.last_activity_at + d.pair_ttl_days * DAY;
  return {
    ...base,
    os_version: d.os_version,
    model: d.model,
    team: d.team,
    host_device_id: d.host_device_id,
    state: d.setup ? "setup" : "ready",
    last_seen_at: d.last_seen_at,
    in_use: inUse(world, d.device_id),
    last_used_at: d.last_used_at,
    paired_at: d.paired_at,
    pair_ttl_days: d.pair_ttl_days,
    pair_expires_at: iso(expires),
    days_left: Math.max(0, Math.ceil((expires - now()) / DAY)),
    access_count: world.access.get(d.device_id)?.size ?? 0,
    app_version: d.app_version,
    version: d.version,
  };
}

function limitedView(world: World, d: DeviceRec) {
  const v = deviceView(world, d, true) as Record<string, unknown>;
  return { ...v, state: d.setup ? "setup" : "ready" };
}

const REMOVED_WHY: Record<string, string> = {
  device_removed: "its Carbon removed it",
  pair_revoked: "the pair was revoked on the device",
  pair_expired: "it went unused for longer than its pairing lasts",
  left_team: "its Carbon left the team",
};

function notFound(ctx: Ctx, team: string): never {
  return fail(404, "device_not_found", `No device ${ctx.params.device_id} is visible to you in team ${team} and ${ctx.world.environment ? `test environment ${ctx.world.environment.name}` : "production"}.`, "List your devices with `extend device ls`.");
}

/**
 * A device the caller owns, for changing it. A removed one is refused like the service refuses it:
 * its Carbon hears when and why it was removed; anyone else gets the plain device_not_found.
 */
function ownedDevice(ctx: Ctx, member: Member, team: string): DeviceRec {
  const d = ctx.world.devices.get(ctx.params.device_id);
  if (!/^[0-9a-f]{8}$/.test(ctx.params.device_id))
    fail(400, "invalid_input", `${ctx.params.device_id} is not a device id (8 lowercase hexadecimal characters).`);
  if (d && d.removed && d.team === team && d.owner === member.id)
    fail(
      404,
      "device_not_found",
      `Device ${d.device_id} (${d.name}) was removed at ${d.removed_at}: ${REMOVED_WHY[d.removed_reason ?? "device_removed"] ?? d.removed_reason}. A removed device can't be changed or used.`,
      `Its activity log stays readable: \`extend device activity ${d.device_id}\`, or the device's page on the website. To use the device again, pair it again.`,
      { removed_at: d.removed_at, removed_reason: d.removed_reason },
    );
  if (!d || d.removed || d.team !== team || (d.owner !== member.id && d.visibility === "personal")) notFound(ctx, team);
  if (d!.owner !== member.id)
    fail(403, "not_owner", `Only ${d!.owner}, who paired ${d!.device_id} (${d!.name}), can do this.`, "Ask them to change it.");
  return d!;
}

/** A device the caller owns, paired or removed, for reading it and its logs. */
function readableDevice(ctx: Ctx, member: Member, team: string): DeviceRec {
  const d = ctx.world.devices.get(ctx.params.device_id);
  if (d && d.removed) {
    if (d.team === team && d.owner === member.id && member.type === "carbon") return d;
    notFound(ctx, team);
  }
  return ownedDevice(ctx, member, team);
}

function logActivity(world: World, deviceId: string, a: Omit<Activity, "id" | "at" | "files" | "args" | "command" | "outcome" | "session_id" | "details"> & Partial<Activity>) {
  if (!world.activity.has(deviceId)) world.activity.set(deviceId, []);
  world.activity.get(deviceId)!.unshift({ id: randomUUID(), at: iso(now()), files: [], args: null, command: null, outcome: null, session_id: null, details: {}, ...a });
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

function endSession(world: World, s: SessionRec, reason: string) {
  if (s.state === "ended") return s;
  s.state = "ended";
  s.ended_at = iso(now());
  s.idle_ends_at = null;
  s.end_reason = reason;
  s.takeover = null;
  logActivity(world, s.device_id, { actor: { type: "silicon", id: s.silicon_id }, action: "session_ended", session_id: s.session_id, details: { reason } });
  return s;
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

function issueTokens(world: World, member: Member, family: string = randomUUID()) {
  const access = `oat_${b64(32)}`;
  const refresh = `ort_${b64(32)}`;
  world.accessTokens.set(access, { member: member.id, expires: now() + config.accessTtlS * 1000, family });
  world.refreshTokens.set(refresh, { member: member.id, family, used: false });
  return {
    access_token: access,
    refresh_token: refresh,
    token_type: "Bearer",
    expires_in: config.accessTtlS,
    member: { type: member.type, id: member.id, display_name: member.display_name },
    teams: member.teams,
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
  const s = (slt as string).trim();
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
  return ok(200, "login", issueTokens(w, member!), { "Cache-Control": "no-store" });
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
  return ok(200, "refresh", issueTokens(ctx.world, member, rec!.family), { "Cache-Control": "no-store" });
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
  const team = teamOf(ctx, member)!;
  carbonOnly(member);
  requireKey(ctx);
  const data = envelope(ctx, "pairing");
  onlyKeys(data, ["pairing_code", "name", "visibility", "pair_ttl_days", "silicon_ids"]);
  const code = data.pairing_code;
  if (typeof code !== "string" || !/^[0-9A-Fa-f]{6}$/.test(code))
    fail(400, "invalid_input", `pairing_code must be 6 hexadecimal characters; got ${JSON.stringify(code)}.`, "Enter the 6 characters the Extend app shows, in any case.", { field: "pairing_code" });
  const name = checkName(data.name);
  const visibility = data.visibility === undefined ? "team" : checkVisibility(data.visibility);
  const ttl = data.pair_ttl_days === undefined ? 14 : checkTtl(data.pair_ttl_days);
  const siliconIds = (data.silicon_ids ?? []) as string[];
  if (!Array.isArray(siliconIds)) fail(422, "invalid_input", "silicon_ids must be a list of si: ids.");
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
  if (w.environment && [...w.devices.values()].filter((d) => !d.removed).length >= 5)
    fail(409, "test_device_limit", "In test environment you are limited to 5 paired devices per environment.", "Remove a device from this test environment, or clean it in Honeycomb.", { device_limit: 5 });

  const device_id = hex(8);
  const d: DeviceRec = {
    device_id,
    name,
    os: enrollment!.os,
    os_version: enrollment!.os_version,
    model: enrollment!.model,
    kind: kindFor(enrollment!.os, enrollment!.model),
    owner: member.id,
    team,
    visibility,
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
    setup: { steps: SETUP_STEPS[enrollment!.os], started: now(), codeEnteredAt: null },
  };
  w.devices.set(device_id, d);
  enrollment!.paired = { device_id, world: w.key };
  logActivity(w, device_id, { actor: { type: "carbon", id: member.id }, action: "paired", details: { name, access: siliconIds } });
  for (const id of siliconIds) grantAccess(w, d, id, member.id);
  return ok(201, "device", deviceView(w, d), { ETag: `"${d.version}"` });
});

function grantAccess(w: World, d: DeviceRec, siliconId: string, by: string): Grant {
  if (!w.access.has(d.device_id)) w.access.set(d.device_id, new Map());
  const existing = w.access.get(d.device_id)!.get(siliconId);
  if (existing) return existing;
  const g: Grant = { device_id: d.device_id, silicon_id: siliconId, granted_by: by, granted_at: iso(now()), last_used_at: null };
  w.access.get(d.device_id)!.set(siliconId, g);
  logActivity(w, d.device_id, { actor: { type: "carbon", id: by }, action: "access_granted", details: { silicon_id: siliconId } });
  return g;
}

route("POST", "/api/v1/devices/:device_id/attachments", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  carbonOnly(member);
  requireKey(ctx);
  const host = ownedDevice(ctx, member, team);
  const data = envelope(ctx, "attachment");
  onlyKeys(data, ["os", "name", "visibility", "pair_ttl_days"]);
  const os = data.os as Os;
  if (!["ios", "ipados", "tvos", "samsung_tv", "lg_tv"].includes(os))
    fail(422, "invalid_input", "os must be ios, ipados, tvos, samsung_tv or lg_tv.", null, { field: "os" });
  const name = checkName(data.name);
  const visibility = data.visibility === undefined ? "team" : checkVisibility(data.visibility);
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
  if (w.environment && [...w.devices.values()].filter((d) => !d.removed).length >= 5)
    fail(409, "test_device_limit", "In test environment you are limited to 5 paired devices per environment.", "Remove a device from this test environment, or clean it in Honeycomb.", { device_limit: 5 });
  const device_id = hex(8);
  const d: DeviceRec = {
    device_id,
    name,
    os,
    os_version: null,
    model: null,
    kind: kindFor(os, null),
    owner: member.id,
    team,
    visibility,
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
    setup: { steps: SETUP_STEPS[os], started: now(), codeEnteredAt: null },
  };
  w.devices.set(device_id, d);
  logActivity(w, device_id, { actor: { type: "carbon", id: member.id }, action: "paired", details: { name, through: host.device_id } });
  return ok(201, "device", deviceView(w, d), { ETag: `"${d.version}"` });
});

route("GET", "/api/v1/devices/:device_id/setup", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = readableDevice(ctx, member, team);
  const view = setupView(d);
  settle(ctx.world, d);
  return ok(200, "setup", view);
});

route("POST", "/api/v1/devices/:device_id/setup/code", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = ownedDevice(ctx, member, team);
  const data = envelope(ctx, "setup_code");
  onlyKeys(data, ["code"]);
  if (typeof data.code !== "string" || !/^[0-9]{4}$/.test(data.code)) fail(422, "invalid_input", "code must be the 4 digits the Apple TV shows.", null, { field: "code" });
  if (!d.setup || !d.setup.steps.some((s) => s.code) || d.setup.codeEnteredAt !== null)
    fail(409, "setup_code_not_expected", `${d.name} isn't waiting for a setup code.`, "Only an Apple TV asks for one, once, during setup.");
  if (data.code === "0000") fail(422, "setup_code_invalid", "The Apple TV didn't accept that code.", "Enter the code the TV shows now; it changes if you wait too long.");
  d.setup!.codeEnteredAt = now();
  return ok(200, "setup", setupView(d));
});

// Devices
route("GET", "/api/v1/devices", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const scope = ctx.url.searchParams.get("scope") ?? (member.type === "carbon" ? "mine" : "accessible");
  if (!["mine", "accessible", "team"].includes(scope)) fail(400, "invalid_input", `scope must be mine, accessible or team; got ${scope}.`);
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
  let list = [...ctx.world.devices.values()].filter((d) => (includeRemoved || !d.removed) && d.team === team);
  if (scope === "mine") list = list.filter((d) => d.owner === member.id);
  else if (scope === "team") list = list.filter((d) => d.owner !== member.id && d.visibility === "team");
  else list = list.filter((d) => ctx.world.access.get(d.device_id)?.has(member.id));
  if (online !== null) list = list.filter((d) => String(d.online && !d.removed) === online);
  if (os) list = list.filter((d) => d.os === os);
  // The service's order: online first, then paired before removed, then name.
  list.sort((a, b) => Number(b.online && !b.removed) - Number(a.online && !a.removed) || Number(a.removed) - Number(b.removed) || a.name.localeCompare(b.name));
  const page = paginate(list, ctx.url);
  return ok(200, "devices", { items: page.items.map((d) => (scope === "team" ? limitedView(ctx.world, d) : deviceView(ctx.world, d))), next_cursor: page.next_cursor });
});

route("GET", "/api/v1/devices/:device_id", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = readableDevice(ctx, member, team);
  const view = deviceView(ctx.world, d);
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
  const team = teamOf(ctx, member)!;
  const d = ownedDevice(ctx, member, team);
  checkIfMatch(ctx, d);
  const data = envelope(ctx, "device");
  onlyKeys(data, ["name", "visibility", "pair_ttl_days"]);
  if (!Object.keys(data).length) fail(422, "invalid_input", "Send at least one of name, visibility, pair_ttl_days.");
  // Logged like the service: one entry, "renamed" when only the name changed, else "settings_changed".
  const changes: Record<string, unknown> = {};
  if ("name" in data) {
    const name = checkName(data.name);
    changes.name = { from: d.name, to: name };
    d.name = name;
  }
  if ("visibility" in data) changes.visibility = d.visibility = checkVisibility(data.visibility);
  if ("pair_ttl_days" in data) changes.pair_ttl_days = d.pair_ttl_days = checkTtl(data.pair_ttl_days);
  const action = "name" in changes && Object.keys(changes).length === 1 ? "renamed" : "settings_changed";
  logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action, details: changes });
  touch(d);
  return ok(200, "device", deviceView(ctx.world, d), { ETag: `"${d.version}"` });
});

function removeDevice(w: World, d: DeviceRec, by: string, reason = "device_removed") {
  for (const s of w.sessions.values()) if (s.device_id === d.device_id && s.state !== "ended") endSession(w, s, reason);
  for (const child of w.devices.values()) if (child.host_device_id === d.device_id && !child.removed) removeDevice(w, child, by, reason);
  w.access.delete(d.device_id);
  d.removed = true;
  d.removed_at = iso(now());
  d.removed_reason = reason;
  d.online = false;
  logActivity(w, d.device_id, { actor: { type: "carbon", id: by }, action: "removed", details: { reason } });
}

route("DELETE", "/api/v1/devices/:device_id", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = ownedDevice(ctx, member, team);
  checkIfMatch(ctx, d);
  removeDevice(ctx.world, d, member.id);
  return none();
});

route("POST", "/api/v1/devices/:device_id/stop", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = ownedDevice(ctx, member, team);
  const s = [...ctx.world.sessions.values()].find((x) => x.device_id === d.device_id && x.state !== "ended");
  if (!s) fail(409, "device_not_in_use", `Nothing is running on ${d.name}.`, "There is nothing to stop.");
  logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action: "stopped", session_id: s!.session_id, details: { silicon_id: s!.silicon_id } });
  return ok(200, "session", publicSession(endSession(ctx.world, s!, "stopped_by_carbon")));
});

// Access
route("GET", "/api/v1/devices/:device_id/access", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = readableDevice(ctx, member, team);
  const items = [...(ctx.world.access.get(d.device_id)?.values() ?? [])].sort((a, b) => a.silicon_id.localeCompare(b.silicon_id));
  return ok(200, "access", { items });
});

route("PUT", "/api/v1/devices/:device_id/access/:silicon_id", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = ownedDevice(ctx, member, team);
  checkSilicon(ctx.world, team, ctx.params.silicon_id);
  return ok(200, "access_grant", grantAccess(ctx.world, d, ctx.params.silicon_id, member.id));
});

route("DELETE", "/api/v1/devices/:device_id/access/:silicon_id", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = ownedDevice(ctx, member, team);
  const grants = ctx.world.access.get(d.device_id);
  if (grants?.delete(ctx.params.silicon_id)) {
    logActivity(ctx.world, d.device_id, { actor: { type: "carbon", id: member.id }, action: "access_revoked", details: { silicon_id: ctx.params.silicon_id } });
    for (const s of ctx.world.sessions.values())
      if (s.device_id === d.device_id && s.silicon_id === ctx.params.silicon_id && s.state !== "ended") endSession(ctx.world, s, "access_removed");
  }
  return none();
});

// Sessions (a Carbon reads them; Silicons start them)
route("GET", "/api/v1/sessions", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const deviceId = ctx.url.searchParams.get("device_id");
  const state = ctx.url.searchParams.get("state");
  let list = [...ctx.world.sessions.values()].filter((s) => {
    const d = ctx.world.devices.get(s.device_id);
    if (!d || d.team !== team) return false;
    return member.type === "carbon" ? d.owner === member.id : s.silicon_id === member.id;
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
  if (!d || d.removed || d.team !== team) fail(404, "device_not_found", `No device ${String(data.device_id)} is visible to you.`);
  return startSession(ctx.world, d!, member.id);
});

function startSession(w: World, d: DeviceRec, siliconId: string): Reply {
  if (!w.access.get(d.device_id)?.has(siliconId)) fail(403, "no_access", `${siliconId} has no access to ${d.device_id} (${d.name}).`, "The owner can grant it with `extend device access grant`.");
  const current = inUse(w, d.device_id);
  if (current)
    fail(409, "device_in_use", `Device ${d.device_id} (${d.name}) is being used by ${current.silicon_id} in session ${current.session_id} since ${current.since}. Only one Silicon can use a device at a time.`, `Ask for it with: extend request send ${d.device_id} --reason "<why, up to 300 characters>"`, { in_use: current });
  const t = now();
  const s: SessionRec = { session_id: newSessionId(w), device_id: d.device_id, silicon_id: siliconId, state: "active", started_at: iso(t), last_command_at: null, idle_ends_at: iso(t + 300_000), ended_at: null, end_reason: null, command_count: 0 };
  w.sessions.set(s.session_id, s);
  d.last_used_at = iso(t);
  d.last_activity_at = t;
  logActivity(w, d.device_id, { actor: { type: "silicon", id: siliconId }, action: "session_started", session_id: s.session_id });
  return ok(201, "session", publicSession(s));
}

// Takeovers: the Silicon hands the device to its Carbon; the owner Carbon reads and ends them.
function sessionFor(ctx: Ctx, member: Member, team: string): SessionRec {
  const sess = ctx.world.sessions.get(ctx.params.session_id);
  const d = sess ? ctx.world.devices.get(sess.device_id) : undefined;
  if (!sess || !d || d.team !== team || (sess.silicon_id !== member.id && d.owner !== member.id))
    fail(404, "session_not_found", `No session ${ctx.params.session_id} is visible to you.`, "List sessions with `extend session ls`.");
  return sess!;
}
const takeoverView = (s: SessionRec) => s.takeover ?? null;

route("POST", "/api/v1/sessions/:session_id/takeover", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const sess = sessionFor(ctx, member, team);
  if (sess.silicon_id !== member.id) fail(403, "not_session_owner", "Only the Silicon using the session can hand the device over.");
  const data = envelope(ctx, "takeover");
  const reason = String(data.reason ?? "").trim();
  if (!reason || [...reason].length > 300) fail(422, "invalid_input", "reason must be 1–300 characters.");
  if (sess.state !== "active") fail(409, "session_ended", `Session ${sess.session_id} is ${sess.state}.`);
  const t = now();
  sess.state = "paused";
  sess.takeover = { takeover_id: randomUUID(), session_id: sess.session_id, reason, started_at: iso(t), expires_at: iso(t + 30 * MIN) };
  logActivity(ctx.world, sess.device_id, { actor: { type: "silicon", id: member.id }, action: "takeover_started", session_id: sess.session_id, details: { reason } });
  return ok(201, "takeover", sess.takeover);
});

route("GET", "/api/v1/sessions/:session_id/takeover", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  return ok(200, "takeover", takeoverView(sessionFor(ctx, member, team)));
});

route("DELETE", "/api/v1/sessions/:session_id/takeover", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const sess = sessionFor(ctx, member, team);
  if (sess.state !== "paused" || !sess.takeover) fail(409, "not_paused", `Session ${sess.session_id} isn't handed over to you.`, "Nothing to release.");
  sess.state = "active";
  sess.takeover = null;
  sess.idle_ends_at = iso(now() + 300_000);
  logActivity(ctx.world, sess.device_id, { actor: { type: member.type, id: member.id }, action: "takeover_released", session_id: sess.session_id });
  return none();
});

// Team directory
route("GET", "/api/v1/team/silicons", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const items = [...ctx.world.members.values()]
    .filter((m) => m.type === "silicon" && m.teams.includes(team))
    .map((m) => ({ id: m.id, display_name: m.display_name ?? null }))
    .sort((a, b) => a.id.localeCompare(b.id));
  return ok(200, "team_silicons", { items });
});

// Requests and activity
route("GET", "/api/v1/devices/:device_id/requests", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = readableDevice(ctx, member, team);
  return ok(200, "requests", paginate(ctx.world.requests.get(d.device_id) ?? [], ctx.url));
});

route("GET", "/api/v1/devices/:device_id/activity", (ctx) => {
  const member = caller(ctx);
  const team = teamOf(ctx, member)!;
  const d = readableDevice(ctx, member, team);
  const q = ctx.url.searchParams;
  let list = ctx.world.activity.get(d.device_id) ?? [];
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
  return ok(200, "reset", { ok: true });
});

route("POST", "/__mock/config", (ctx) => {
  const data = (ctx.body ?? {}) as Record<string, unknown>;
  if (typeof data.step_ms === "number") config.stepMs = data.step_ms;
  if (typeof data.access_ttl_s === "number") config.accessTtlS = data.access_ttl_s;
  return ok(200, "config", config);
});

/** Simulates an Extend app showing a pairing code. */
route("POST", "/__mock/enroll", (ctx) => {
  const data = (ctx.body ?? {}) as Record<string, unknown>;
  const os = (data.os as Os) ?? "android";
  const e = newEnrollment(os, (data.model as string) ?? null);
  return ok(201, "enrollment", { pairing_code: e.code, enrollment_id: e.enrollment_id });
});

/** Simulates a Silicon starting a session (in the world the secret header picks). */
route("POST", "/__mock/sessions", (ctx) => {
  const data = (ctx.body ?? {}) as Record<string, unknown>;
  const d = ctx.world.devices.get(String(data.device_id));
  if (!d) fail(404, "device_not_found", "No such device.");
  return startSession(ctx.world, d!, String(data.silicon_id));
});

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
    const replayKey = key ? `${world.key}|${req.method}|${url.pathname}|${key}` : null;
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
