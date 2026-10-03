import { ExtendClient, type ClientContext, type TokenPair, type TokenStore } from "../../src/lib/api";

export interface Call {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: unknown;
}

export type Handler = (call: Call, index: number) => Response | Promise<Response> | Error;

export function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json", "X-Request-ID": "req-1", ...headers },
  });
}

export function fakeFetch(handler: Handler) {
  const calls: Call[] = [];
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call: Call = {
      url: String(input),
      method: init?.method ?? "GET",
      headers: { ...(init?.headers as Record<string, string>) },
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
    };
    calls.push(call);
    const result = await handler(call, calls.length - 1);
    if (result instanceof Error) throw result;
    return result;
  }) as typeof fetch;
  return { fetchImpl, calls };
}

export function memoryStore(initial: TokenPair | null = null): TokenStore & { value: TokenPair | null; saves: number } {
  const store = {
    value: initial,
    saves: 0,
    load: () => store.value,
    save: (pair: TokenPair) => {
      store.value = pair;
      store.saves++;
    },
    clear: () => {
      store.value = null;
    },
  };
  return store;
}

export const NOW = 1_800_000_000_000;

export function pair(overrides: Partial<TokenPair> = {}): TokenPair {
  return {
    access_token: "oat_old",
    refresh_token: "ort_old",
    expires_at: NOW + 3600_000,
    member: { type: "carbon", id: "c:saket" },
    teams: ["acme"],
    ...overrides,
  };
}

export function client(handler: Handler, overrides: Partial<ClientContext> = {}) {
  const { fetchImpl, calls } = fakeFetch(handler);
  const tokens = (overrides.tokens as ReturnType<typeof memoryStore>) ?? memoryStore(pair());
  let key = 0;
  const c = new ExtendClient({
    baseUrl: "https://api.test",
    tokens,
    worldKey: "production",
    testingSecret: () => null,
    team: () => "acme",
    telemetryOff: () => false,
    fetch: fetchImpl,
    now: () => NOW,
    newKey: () => `key-${++key}`,
    ...overrides,
  });
  return { client: c, calls, tokens };
}

export function session(access: string, refresh: string, expiresIn = 1800) {
  return {
    access_token: access,
    refresh_token: refresh,
    token_type: "Bearer",
    expires_in: expiresIn,
    member: { type: "carbon", id: "c:saket" },
    teams: ["acme"],
  };
}
