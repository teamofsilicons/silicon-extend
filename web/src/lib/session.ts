/**
 * Who is signed in, in which world, and which team: the website's whole session state.
 *
 * Production and each test environment are separate worlds with separate token pairs. Entering a
 * test environment never touches the production login; exiting returns to it (or to sign-in).
 */
import { createMemo, createRoot, createSignal } from "solid-js";
import { apiBaseUrl } from "../config";
import { ApiError, ExtendClient, type TokenPair, type TokenStore } from "./api";
import { read, readJson, remove, write, writeJson } from "./storage";
import type { TestingEnvironment } from "./types";

export type World =
  | { kind: "production" }
  | { kind: "testing"; secret: string; environment: TestingEnvironment };

export const KEYS = {
  productionAuth: "extend.auth.production",
  testing: "extend.testing",
  testingAuth: (environmentId: string) => `extend.auth.testing.${environmentId}`,
  team: (world: World) =>
    world.kind === "production" ? "extend.team.production" : `extend.team.testing.${world.environment.environment_id}`,
  telemetry: "extend.telemetry",
  theme: "extend.theme",
  loginState: "extend.login.state",
};

export const APP_SECRET = /^ask_[A-Za-z0-9_-]{43}$/;

function areaFor(world: World): "local" | "session" {
  return world.kind === "production" ? "local" : "session";
}
function authKey(world: World): string {
  return world.kind === "production" ? KEYS.productionAuth : KEYS.testingAuth(world.environment.environment_id);
}

/** A token store over browser storage; `changed` fires after every save or clear. */
export function storageTokenStore(kind: "local" | "session", key: string, changed: () => void): TokenStore {
  return {
    load: () => {
      const pair = readJson<TokenPair>(kind, key);
      return pair && typeof pair.access_token === "string" && typeof pair.refresh_token === "string" ? pair : null;
    },
    save: (pair) => {
      // One write replaces the whole pair, so a reader never sees a new access token with an old refresh token.
      writeJson(kind, key, pair);
      changed();
    },
    clear: () => {
      remove(kind, key);
      changed();
    },
  };
}

function webLock<T>(name: string, fn: () => Promise<T>): Promise<T> {
  const locks = typeof navigator !== "undefined" ? (navigator as Navigator & { locks?: LockManager }).locks : undefined;
  if (!locks?.request) return fn();
  return locks.request(name, fn) as Promise<T>;
}

function loadWorld(): World {
  const saved = readJson<{ secret: string; environment: TestingEnvironment }>("session", KEYS.testing);
  if (saved && APP_SECRET.test(saved.secret) && saved.environment?.environment_id)
    return { kind: "testing", secret: saved.secret, environment: saved.environment };
  return { kind: "production" };
}

function createSession() {
  const [world, setWorld] = createSignal<World>(loadWorld());
  const [authTick, setAuthTick] = createSignal(0);
  const [teamTick, setTeamTick] = createSignal(0);
  const [telemetryOff, setTelemetryOff] = createSignal(read("local", KEYS.telemetry) === "off");
  const [signedOutReason, setSignedOutReason] = createSignal<ApiError | null>(null);
  const bump = () => setAuthTick((n) => n + 1);

  if (typeof window !== "undefined")
    window.addEventListener("storage", (event) => {
      if (event.key === KEYS.productionAuth) bump();
    });

  const store = createMemo(() => {
    const w = world();
    return storageTokenStore(areaFor(w), authKey(w), bump);
  });

  const pair = createMemo(() => {
    authTick();
    return store().load();
  });

  const team = createMemo(() => {
    teamTick();
    const p = pair();
    if (!p) return null;
    const saved = read(areaFor(world()), KEYS.team(world()));
    if (saved && p.teams.includes(saved)) return saved;
    return p.teams[0] ?? null;
  });

  const client = createMemo(() => {
    const w = world();
    const tokens = store();
    return new ExtendClient({
      baseUrl: apiBaseUrl(),
      tokens,
      worldKey: w.kind === "production" ? "production" : `testing:${w.environment.environment_id}`,
      testingSecret: () => (w.kind === "testing" ? w.secret : null),
      team: () => team(),
      telemetryOff: () => telemetryOff(),
      lock: webLock,
      onSignedOut: (error) => setSignedOutReason(error),
    });
  });

  return {
    world,
    client,
    pair,
    team,
    telemetryOff,
    signedOutReason,
    clearSignedOutReason: () => setSignedOutReason(null),
    member: () => pair()?.member ?? null,
    teams: () => pair()?.teams ?? [],
    isTesting: () => world().kind === "testing",

    setTeam(handle: string) {
      write(areaFor(world()), KEYS.team(world()), handle);
      setTeamTick((n) => n + 1);
    },

    /** Replaces the team list with the live one from `/auth/me`, keeping the rest of the pair. */
    updateTeams(teams: string[]) {
      const p = store().load();
      if (!p || JSON.stringify(p.teams) === JSON.stringify(teams)) return;
      store().save({ ...p, teams });
    },

    setTelemetry(on: boolean) {
      write("local", KEYS.telemetry, on ? "on" : "off");
      setTelemetryOff(!on);
    },

    /** Selects a validated test environment. Its login is separate from production's. */
    enterTesting(secret: string, environment: TestingEnvironment) {
      writeJson("session", KEYS.testing, { secret, environment });
      setSignedOutReason(null);
      setWorld({ kind: "testing", secret, environment });
    },

    /** Refreshes the environment's name and state from the service. */
    updateEnvironment(environment: TestingEnvironment) {
      const w = world();
      if (w.kind !== "testing" || w.environment.environment_id !== environment.environment_id) return;
      writeJson("session", KEYS.testing, { secret: w.secret, environment });
      setWorld({ ...w, environment });
    },

    /**
     * Leaves the test environment: signs its test identity out (best effort) and forgets the secret
     * and that world's tokens. The production login, if any, is untouched.
     */
    async exitTesting() {
      const w = world();
      if (w.kind !== "testing") return;
      const c = client();
      try {
        await c.logout();
      } catch {
        /* the test tokens are forgotten either way */
      }
      remove("session", KEYS.testingAuth(w.environment.environment_id));
      remove("session", KEYS.testing);
      remove("session", KEYS.team(w));
      setSignedOutReason(null);
      setWorld({ kind: "production" });
    },

    async signOut() {
      try {
        await client().logout();
      } finally {
        setSignedOutReason(null);
        bump();
      }
    },
  };
}

export type SessionState = ReturnType<typeof createSession>;

let instance: SessionState | undefined;
export function session(): SessionState {
  if (!instance) instance = createRoot(() => createSession());
  return instance;
}
