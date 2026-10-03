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

export type World = { kind: "production" } | { kind: "testing"; secret: string; environment: TestingEnvironment };

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

/** Stable account+organization binding; unscoped legacy credentials must be reauthenticated. */
export function contextId(pair: TokenPair): string | null {
  return pair.teams?.length === 1 && typeof pair.teams[0] === "string" && !!pair.teams[0].trim() && pair.member?.id && (pair.member?.type === "carbon" || pair.member?.type === "silicon")
    ? encodeURIComponent(JSON.stringify([pair.member.type, pair.member.id, pair.teams[0]]))
    : null;
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

export function createSession() {
  const [world, setWorld] = createSignal<World>(loadWorld());
  const [authTick, setAuthTick] = createSignal(0);
  const [contextRevision, setContextRevision] = createSignal(0);
  const changedContext = (persist = true) => {
    // Survives a full-page IAM round trip, including account A → B → A in another tab.
    if (persist) write("local", "extend.login.context-epoch", crypto.randomUUID());
    setContextRevision(n => n + 1);
  };
  const [telemetryOff, setTelemetryOff] = createSignal(read("local", KEYS.telemetry) === "off");
  const [signedOutReason, setSignedOutReason] = createSignal<ApiError | null>(null);
  const bump = () => setAuthTick((n) => n + 1);
  const selectedKey = (w: World) => `${authKey(w)}.selected`;
  const indexKey = (w: World) => `${authKey(w)}.contexts`;
  const contextKey = (w: World, id: string) => `${authKey(w)}.context.${id}`;
  function savedIds(w: World): string[] {
    return readJson<string[]>(areaFor(w), indexKey(w)) ?? [];
  }
  function remember(w: World, p: TokenPair, select = false) {
    const id = contextId(p);
    if (!id) throw new ApiError(401, { code: "context_required", message: "Sign in again and select exactly one organization." });
    const area = areaFor(w);
    writeJson(area, contextKey(w, id), p);
    const ids = savedIds(w);
    if (!ids.includes(id)) writeJson(area, indexKey(w), [...ids, id]);
    if (select) { write(area, selectedKey(w), id); changedContext(); }
    bump();
  }
  // Import only already scoped legacy sessions. IAM revokes older unscoped credentials.
  createMemo(() => {
    const w = world();
    const legacy = readJson<TokenPair>(areaFor(w), authKey(w));
    if (legacy && contextId(legacy)) remember(w, legacy, !read(areaFor(w), selectedKey(w)));
    remove(areaFor(w), authKey(w));
  });
  if (typeof window !== "undefined")
    window.addEventListener("storage", (event) => {
      if (!event.key || event.key.startsWith(KEYS.productionAuth)) { changedContext(false); bump(); }
    });
  const selected = createMemo(() => {
    authTick();
    return read(areaFor(world()), selectedKey(world()));
  });
  const contexts = createMemo(() => {
    authTick();
    const w = world();
    return savedIds(w)
      .map((id) => readJson<TokenPair>(areaFor(w), contextKey(w, id)))
      .filter((p): p is TokenPair => !!p && !!contextId(p));
  });
  const store = createMemo<TokenStore>(() => {
    const w = world(),
      id = selected(),
      area = areaFor(w);
    const key = id ? contextKey(w, id) : `${authKey(w)}.empty`;
    return {
      load: () => {
        const p = readJson<TokenPair>(area, key);
        return p && contextId(p) === id ? p : null;
      },
      save: (p, expectedRefreshToken) => {
        if (!id || contextId(p) !== id)
          throw new ApiError(401, { code: "context_changed", message: "The account or organization changed during this request." });
        if (expectedRefreshToken && readJson<TokenPair>(area, key)?.refresh_token !== expectedRefreshToken)
          throw new ApiError(409, { code: "context_changed", message: "This saved login changed while refreshing. Retry in its original context." });
        writeJson(area, key, p);
        bump();
      },
      clear: () => {
        remove(area, key);
        if (read(area, selectedKey(w)) === id) { remove(area, selectedKey(w)); changedContext(); }
        bump();
      },
    };
  });
  const pair = createMemo(() => {
    authTick();
    return store().load();
  });
  const team = createMemo(() => pair()?.teams[0] ?? null);
  const contextKeyValue = createMemo(() => `${authKey(world())}:${selected() ?? "signed-out"}`);
  const client = createMemo(() => {
    const w = world(),
      tokens = store(),
      id = selected(), revision = contextRevision();
    const org = id ? (contexts().find((p) => contextId(p) === id)?.teams[0] ?? null) : null;
    return new ExtendClient({
      baseUrl: apiBaseUrl(),
      tokens,
      worldKey: `${authKey(w)}:${id ?? "signed-out"}`,
      testingSecret: () => (w.kind === "testing" ? w.secret : null),
      team: () => org,
      telemetryOff: () => telemetryOff(),
      lock: webLock,
      onLogin: (p) => {
        if (revision !== contextRevision() || world() !== w || selected() !== id)
          throw new ApiError(409, { code: "context_changed", message: "The selected account or environment changed during sign-in. Start sign-in again." });
        remember(w, p, true);
      },
      onSignedOut: (error) => {
        if (world() === w && (!selected() || selected() === id)) setSignedOutReason(error);
      },
    });
  });

  return {
    world,
    client,
    pair,
    contexts,
    contextKey: contextKeyValue,
    contextRevision,
    invalidateLogin: () => changedContext(),
    loginContext: () => `${contextKeyValue()}:${read("local", "extend.login.context-epoch") ?? "initial"}`,
    selectContext(id: string) {
      if (!contexts().some((p) => contextId(p) === id)) return;
      changedContext();
      write(areaFor(world()), selectedKey(world()), id);
      setSignedOutReason(null);
      bump();
    },
    team,
    telemetryOff,
    signedOutReason,
    clearSignedOutReason: () => setSignedOutReason(null),
    member: () => pair()?.member ?? null,
    teams: () => pair()?.teams ?? [],
    isTesting: () => world().kind === "testing",

    setTeam(handle: string) {
      const found = contexts().find(
        (p) => p.member.id === pair()?.member.id && p.member.type === pair()?.member.type && p.teams[0] === handle,
      );
      if (found) {
        changedContext();
        write(areaFor(world()), selectedKey(world()), contextId(found)!);
        bump();
      }
    },

    /** IAM5 never broadens a stored application context with a directory result. */
    updateTeams(teams: string[]) {
      const p = store().load();
      if (p && (teams.length !== 1 || teams[0] !== p.teams[0])) {
        store().clear();
        setSignedOutReason(new ApiError(401, { code: "context_changed", message: "Choose your account and organization again in IAM." }));
      }
    },

    setTelemetry(on: boolean) {
      write("local", KEYS.telemetry, on ? "on" : "off");
      setTelemetryOff(!on);
    },

    /** Selects a validated test environment. Its login is separate from production's. */
    enterTesting(secret: string, environment: TestingEnvironment) {
      writeJson("session", KEYS.testing, { secret, environment });
      setSignedOutReason(null);
      changedContext();
      setWorld({ kind: "testing", secret, environment });
    },

    /** Refreshes the environment's name and state from the service. */
    updateEnvironment(environment: TestingEnvironment) {
      const w = world();
      if (w.kind !== "testing" || w.environment.environment_id !== environment.environment_id) return;
      // Unchanged: keep the same world. A new one would re-run everything that follows the world,
      // including the read that called this, and loop.
      if (JSON.stringify(w.environment) === JSON.stringify(environment)) return;
      writeJson("session", KEYS.testing, { secret: w.secret, environment });
      changedContext();
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
      for (const id of savedIds(w)) remove("session", contextKey(w, id));
      remove("session", indexKey(w));
      remove("session", selectedKey(w));
      remove("session", KEYS.testingAuth(w.environment.environment_id));
      remove("session", KEYS.testing);
      remove("session", KEYS.team(w));
      setSignedOutReason(null);
      changedContext();
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
