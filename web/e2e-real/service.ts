/** Talks to the real Extend service directly, and plays the part of an Extend app on a device. */
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import WebSocket from "ws";

export const REAL = process.env.EXTEND_REAL_URL || "http://127.0.0.1:8480";

async function call(method: string, path: string, init: { body?: unknown; headers?: Record<string, string> } = {}) {
  const res = await fetch(`${REAL}${path}`, {
    method,
    headers: { "Content-Type": "application/json", "Idempotency-Key": randomUUID(), ...init.headers },
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });
  const text = await res.text();
  const json = text ? JSON.parse(text) : null;
  if (!res.ok) throw new Error(`${method} ${path} → ${res.status} ${text}`);
  return json;
}

export async function login(slt: string, secret?: string) {
  const r = await call("POST", "/api/v1/auth/login", { body: { type: "login", data: { slt } }, headers: secret ? { "X-Testing-Application-Secret": secret } : {} });
  return r.data.access_token as string;
}

export function headers(token: string, secret?: string): Record<string, string> {
  return { Authorization: `Bearer ${token}`, "X-Org-ID": "acme", ...(secret ? { "X-Testing-Application-Secret": secret } : {}) };
}

export async function startSession(siliconToken: string, deviceId: string) {
  return (await call("POST", "/api/v1/sessions", { body: { type: "session", data: { device_id: deviceId } }, headers: headers(siliconToken) })).data;
}

export async function takeover(siliconToken: string, sessionId: string, reason: string) {
  return (await call("POST", `/api/v1/sessions/${sessionId}/takeover`, { body: { type: "takeover", data: { reason } }, headers: headers(siliconToken) })).data;
}

/**
 * Whether this service lets a Carbon read removed devices (GET /devices?include_removed=…). A service
 * that knows the parameter refuses a value that isn't true or false; an older one ignores it.
 */
export async function supportsRemovedDevices(token: string): Promise<boolean> {
  const res = await fetch(`${REAL}/api/v1/devices?scope=mine&include_removed=probe`, { headers: headers(token) });
  return res.status === 422;
}

export async function pairViaApi(token: string, code: string, name: string, secret?: string) {
  return (await call("POST", "/api/v1/pairings", { body: { type: "pairing", data: { pairing_code: code, name } }, headers: headers(token, secret) })).data;
}

/** A pretend Extend app: enrolls, waits to be paired, then keeps the device socket open. */
export class FakeDevice {
  enrollmentId = "";
  secret = "";
  code = "";
  deviceId = "";
  credential = "";
  socket: WebSocket | null = null;
  /** Every frame the service sent, oldest first. */
  frames: { type: string; [key: string]: unknown }[] = [];

  /** Starts pairing like an Extend app. With `secret`, the app pairs into that test environment (the service refuses cross-world claims). */
  static async enroll(os = "android", secret?: string) {
    const d = new FakeDevice();
    const r = await call("POST", "/api/v1/enrollments", {
      body: { type: "enrollment", data: { os, os_version: "15", model: "Pixel 9", app_version: "1.0.0" } },
      headers: secret ? { "X-Testing-Application-Secret": secret } : {},
    });
    d.enrollmentId = r.data.enrollment_id;
    d.secret = r.data.enrollment_secret;
    d.code = r.data.pairing_code;
    return d;
  }

  async waitPaired(timeoutMs = 10_000) {
    const until = Date.now() + timeoutMs;
    while (Date.now() < until) {
      const r = await call("GET", `/api/v1/enrollments/${this.enrollmentId}`, { headers: { Authorization: `Extend-Enrollment ${this.secret}` } });
      if (r.data.state === "paired") {
        this.deviceId = r.data.device_id;
        this.credential = r.data.device_credential;
        return;
      }
      await new Promise((r) => setTimeout(r, 300));
    }
    throw new Error("the device was never paired");
  }

  /** Opens the device socket and reports setup: waiting on the Carbon, then finished. */
  async connect(setupDone: boolean) {
    const ws = new WebSocket(`${REAL.replace(/^http/, "ws")}/api/v1/device/connect`, { headers: { Authorization: `Extend-Device ${this.credential}` } });
    this.socket = ws;
    ws.on("message", (raw) => {
      const frame = JSON.parse(String(raw));
      this.frames.push(frame);
      if (frame.type === "ping") ws.send(JSON.stringify({ type: "pong", nonce: frame.nonce }));
    });
    await new Promise<void>((resolve, reject) => {
      ws.once("open", () => resolve());
      ws.once("error", reject);
    });
    this.hello(setupDone);
  }

  hello(setupDone: boolean) {
    this.socket!.send(
      JSON.stringify({
        type: "hello",
        app_version: "1.0.0",
        os: "android",
        os_version: "15",
        model: "Pixel 9",
        agent_device_version: null,
        capabilities: ["screen.read", "screen.capture", "input.touch", "input.text", "nav.system", "apps.launch", "apps.list", "takeover", "notifications", "links"],
        missing: setupDone ? [] : [{ capability: "adb", reason: "Wireless debugging is off. Turn it on in Developer options." }],
        setup: {
          state: setupDone ? "complete" : "needs_carbon",
          steps: [
            { key: "developer_options", title: "Turn on Developer options", status: "done" },
            { key: "wireless_debugging", title: "Turn on wireless debugging", status: setupDone ? "done" : "needs_carbon", help: "Settings › System › Developer options › Wireless debugging" },
          ],
        },
      }),
    );
  }

  close() {
    this.socket?.close();
  }
}

/** Sends one Honeycomb lifecycle instruction for a test environment, as Honeycomb would. */
async function lifecycle(environmentId: string, action: string, name?: string) {
  const env = readFileSync(new URL("../../e2e/dev.env", import.meta.url), "utf8");
  const token = /^EXTEND_HONEYCOMB_SERVICE_TOKEN=(.+)$/m.exec(env)?.[1]?.trim();
  if (!token) throw new Error("e2e/dev.env has no EXTEND_HONEYCOMB_SERVICE_TOKEN");
  const op = randomUUID();
  const res = await fetch(`${REAL}/internal/honeycomb/organizations/acme/testing-environments/${environmentId}/operations/${op}`, {
    method: "PUT",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ operation_id: op, environment_id: environmentId, org_id: "acme", app_id: "extend", environment_revision: 1, generation: 1, key_version: 1, action, testing_key: "abcdefghijklmnopqrstuvwxyz012345", name }),
  });
  if (!res.ok) throw new Error(`${action} → ${res.status} ${await res.text()}`);
}

/** Frees a test environment's slot: the service allows 10 across all of Extend, and each run makes one. */
export async function purgeTestEnvironment(environmentId: string) {
  await lifecycle(environmentId, "purge");
}

/** Creates a ready test environment through Honeycomb's lifecycle endpoint and registers its app secret with local IAM. */
export async function createTestEnvironment(name: string) {
  const environmentId = randomUUID();
  await lifecycle(environmentId, "prepare", name);
  const secret = "ask_" + randomUUID().replace(/-/g, "") + "abcdefghijk";
  await call("POST", "/dev/iam/test-apps", { body: { type: "test_app", data: { secret, environment_id: environmentId } } });
  return { environmentId, secret };
}
