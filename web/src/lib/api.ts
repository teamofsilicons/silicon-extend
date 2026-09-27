/**
 * The Extend API client used by the website. It speaks exactly `understanding/api.yaml`:
 * every body is an envelope `{type, data}`, every failure becomes an `ApiError` carrying the
 * service's `code`, `message` and `hint`, and nothing is ever reduced to "something went wrong".
 *
 * One client serves one world (production, or one test environment): it owns that world's token
 * pair and, for a test environment, its `X-Testing-Application-Secret`.
 */
import type {
  AccessGrant,
  ActivityEntry,
  AttachOs,
  AuthSession,
  ExtendRequest,
  Device,
  DeviceDetail,
  ErrorBody,
  IamInfo,
  Me,
  Member,
  Page,
  Session,
  Setup,
  Takeover,
  TeamSilicon,
  TestingEnvironment,
  Visibility,
} from "./types";

export const API_MAJOR = 1;

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly hint: string | null;
  readonly docsUrl: string | null;
  readonly requestId: string | null;
  readonly details: Record<string, unknown>;
  constructor(status: number, body: ErrorBody) {
    super(body.message);
    this.name = "ApiError";
    this.status = status;
    this.code = body.code;
    this.hint = body.hint ?? null;
    this.docsUrl = body.docs_url ?? null;
    this.requestId = body.request_id ?? null;
    this.details = body.details ?? {};
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError;
}

/** Turns anything thrown into an ApiError so the UI can always show a message and a hint. */
export function toApiError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  const message = error instanceof Error ? error.message : String(error);
  return new ApiError(0, {
    code: "client_error",
    message: `The website failed before it could talk to Extend: ${message}`,
    hint: "Reload the page. If it happens again, report it with the steps you took.",
  });
}

export interface TokenPair {
  access_token: string;
  refresh_token: string;
  /** Epoch milliseconds when the access token expires. */
  expires_at: number;
  member: Member;
  teams: string[];
  testing_environment?: TestingEnvironment | null;
}

/** Where one world's token pair lives. `save` must replace the whole pair in one write. */
export interface TokenStore {
  load(): TokenPair | null;
  save(pair: TokenPair): void;
  clear(): void;
}

export interface ClientContext {
  /** API origin without trailing slash; "" for same origin. */
  baseUrl: string;
  tokens: TokenStore;
  /** The test application's app_secret, or null for production. */
  testingSecret: () => string | null;
  /** The team handle sent as X-Org-ID. */
  team: () => string | null;
  telemetryOff: () => boolean;
  /** Names the world for cross-tab refresh locking. */
  worldKey: string;
  fetch?: typeof fetch;
  now?: () => number;
  newKey?: () => string;
  /** Runs `fn` while holding a lock shared by every tab (Web Locks); defaults to no cross-tab lock. */
  lock?: <T>(name: string, fn: () => Promise<T>) => Promise<T>;
  /** Called when refreshing fails for good, after the tokens are cleared. */
  onSignedOut?: (error: ApiError) => void;
}

type TeamMode = "required" | "optional" | "none";

interface RequestOptions {
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  path: string;
  query?: Record<string, string | number | boolean | null | undefined>;
  body?: { type: string; data: unknown };
  /** Send the bearer token (default true). */
  auth?: boolean;
  team?: TeamMode;
  /** Send an Idempotency-Key, reused on the one retry after a network failure. */
  idempotent?: boolean;
  ifMatch?: string;
  /** Expected envelope `type` of a successful body. */
  expect?: string;
  headers?: Record<string, string>;
}

export interface ApiResponse<T> {
  status: number;
  data: T;
  headers: Headers;
}

/** Refresh this long before the access token expires, so requests don't race the expiry. */
const REFRESH_EARLY_MS = 30_000;

/** Builds the If-Match value from an ETag if the browser could read it, else the device version. */
export function ifMatchValue(etag: string | null | undefined, version: number | undefined): string {
  if (etag && /^(W\/)?"?[1-9][0-9]*"?$/.test(etag)) return etag.replace(/^W\//, "");
  if (version && version >= 1) return `"${version}"`;
  throw new ApiError(0, {
    code: "version_unknown",
    message: "The device's version is unknown, so the change can't be sent safely.",
    hint: "Reload the device page and try again.",
  });
}

function defaultKey(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `k-${Date.now().toString(16)}-${Math.random().toString(16).slice(2)}`;
}

export function pairFromSession(session: AuthSession, now: number): TokenPair {
  return {
    access_token: session.access_token,
    refresh_token: session.refresh_token,
    expires_at: now + Math.max(1, session.expires_in) * 1000,
    member: session.member,
    teams: session.teams,
    testing_environment: session.testing_environment ?? null,
  };
}

export class ExtendClient {
  private refreshing: Promise<TokenPair> | null = null;
  constructor(readonly ctx: ClientContext) {}

  private get fetchImpl(): typeof fetch {
    return this.ctx.fetch ?? ((...args) => fetch(...args));
  }
  private now(): number {
    return (this.ctx.now ?? Date.now)();
  }

  url(path: string, query?: RequestOptions["query"]): string {
    let url = `${this.ctx.baseUrl}${path}`;
    if (query) {
      const params = new URLSearchParams();
      for (const [key, value] of Object.entries(query))
        if (value !== undefined && value !== null && value !== "") params.set(key, String(value));
      const qs = params.toString();
      if (qs) url += `?${qs}`;
    }
    return url;
  }

  /** Every header a request carries, before auth. Exposed for tests. */
  baseHeaders(options: Pick<RequestOptions, "team" | "path" | "body" | "ifMatch" | "headers">): Record<string, string> {
    const headers: Record<string, string> = { Accept: "application/json" };
    if (options.body) headers["Content-Type"] = "application/json";
    if (options.path.startsWith("/api/v1/")) headers["Silicon-Extend-API-Version"] = String(API_MAJOR);
    const mode = options.team ?? "required";
    if (mode !== "none") {
      const team = this.ctx.team();
      if (team) headers["X-Org-ID"] = team;
      else if (mode === "required")
        throw new ApiError(0, {
          code: "no_team_selected",
          message: "No team is selected, and this request needs one (sent as X-Org-ID).",
          hint: "Pick a team from the team menu at the top of the page.",
        });
    }
    const secret = this.ctx.testingSecret();
    if (secret) headers["X-Testing-Application-Secret"] = secret;
    if (this.ctx.telemetryOff()) headers["X-Extend-Telemetry"] = "off";
    if (options.ifMatch) headers["If-Match"] = options.ifMatch;
    return { ...headers, ...options.headers };
  }

  private async send(url: string, init: RequestInit, retryOnNetwork: boolean): Promise<Response> {
    try {
      return await this.fetchImpl(url, init);
    } catch (error) {
      if (retryOnNetwork) return this.send(url, init, false);
      const where = this.ctx.baseUrl || (typeof location !== "undefined" ? location.origin : "the same origin");
      const reason = error instanceof Error ? error.message : String(error);
      throw new ApiError(0, {
        code: "network_error",
        message: `Could not reach Extend at ${where} (${reason}).`,
        hint: "Check your connection. If Extend is up, it may not allow this website's origin (CORS), or a local service may not be running.",
      });
    }
  }

  private async parse<T>(response: Response, expect?: string): Promise<T> {
    const text = await response.text();
    let json: unknown = null;
    if (text) {
      try {
        json = JSON.parse(text);
      } catch {
        json = undefined;
      }
    }
    const requestId = response.headers.get("X-Request-ID");
    const envelope = json as { type?: unknown; data?: unknown } | null | undefined;
    if (!response.ok || envelope?.type === "error") {
      const data = envelope?.type === "error" ? (envelope.data as ErrorBody | undefined) : undefined;
      if (data && typeof data.code === "string" && typeof data.message === "string")
        throw new ApiError(response.status, { ...data, request_id: data.request_id ?? requestId ?? undefined });
      throw new ApiError(response.status, {
        code: `http_${response.status}`,
        message: `Extend answered HTTP ${response.status}${response.statusText ? ` ${response.statusText}` : ""} without an error envelope.`,
        hint:
          response.status >= 500
            ? "Extend or something in front of it is failing. Try again in a minute."
            : "The website and the service disagree about this request. Report it with the request id.",
        request_id: requestId ?? undefined,
      });
    }
    if (response.status === 204) return null as T;
    if (!envelope || typeof envelope.type !== "string" || !("data" in envelope))
      throw new ApiError(response.status, {
        code: "unexpected_response",
        message: `Extend answered HTTP ${response.status} without the {"type", "data"} envelope.`,
        hint: "The website and the service disagree on the contract. Report it with the request id.",
        request_id: requestId ?? undefined,
      });
    if (expect && envelope.type !== expect)
      throw new ApiError(response.status, {
        code: "unexpected_response",
        message: `Expected a "${expect}" response from Extend, got "${envelope.type}".`,
        hint: "The website and the service disagree on the contract. Report it with the request id.",
        request_id: requestId ?? undefined,
      });
    return envelope.data as T;
  }

  /** Sends one request, refreshing the access token first if needed and once more after a 401. */
  async request<T>(options: RequestOptions): Promise<ApiResponse<T>> {
    const auth = options.auth ?? true;
    const headers = this.baseHeaders(options);
    if (options.idempotent) headers["Idempotency-Key"] = (this.ctx.newKey ?? defaultKey)();
    const init = (token?: string): RequestInit => ({
      method: options.method,
      headers: token ? { ...headers, Authorization: `Bearer ${token}` } : headers,
      body: options.body ? JSON.stringify(options.body) : undefined,
    });
    const url = this.url(options.path, options.query);
    const retry = !!options.idempotent || options.method === "GET";

    if (!auth) {
      const response = await this.send(url, init(), retry);
      return { status: response.status, headers: response.headers, data: await this.parse<T>(response, options.expect) };
    }

    let pair = this.ctx.tokens.load();
    if (!pair)
      throw new ApiError(401, {
        code: "not_signed_in",
        message: "You are not signed in here.",
        hint: "Sign in with Silicon IAM, or paste a short-lived token.",
      });
    if (pair.expires_at - this.now() < REFRESH_EARLY_MS) pair = await this.refresh(pair);

    let response = await this.send(url, init(pair.access_token), retry);
    if (response.status === 401) {
      const error = await this.parse<never>(response).catch((e: ApiError) => e);
      if (error.code === "testing_secret_invalid" || error.code === "testing_environment_not_ready") throw error;
      pair = await this.refresh(pair);
      response = await this.send(url, init(pair.access_token), retry);
    }
    return { status: response.status, headers: response.headers, data: await this.parse<T>(response, options.expect) };
  }

  /**
   * Rotates the refresh token. Only one refresh runs at a time: in this tab through a shared
   * promise, across tabs through a Web Lock, and a tab that waited re-reads storage first so it
   * never spends a refresh token another tab already rotated (a reused token revokes the family).
   */
  refresh(stale: TokenPair): Promise<TokenPair> {
    if (this.refreshing) return this.refreshing;
    const lock = this.ctx.lock ?? (<R,>(_name: string, fn: () => Promise<R>) => fn());
    this.refreshing = lock(`extend-refresh:${this.ctx.worldKey}`, async () => {
      const current = this.ctx.tokens.load();
      if (!current)
        throw new ApiError(401, {
          code: "not_signed_in",
          message: "You were signed out in another tab.",
          hint: "Sign in again.",
        });
      if (current.refresh_token !== stale.refresh_token && current.expires_at - this.now() >= REFRESH_EARLY_MS)
        return current;
      try {
        const { data } = await this.request<AuthSession>({
          method: "POST",
          path: "/api/v1/auth/refresh",
          auth: false,
          team: "none",
          idempotent: true,
          expect: "refresh",
          body: { type: "refresh", data: { refresh_token: current.refresh_token } },
        });
        const next = pairFromSession(data, this.now());
        this.ctx.tokens.save(next);
        return next;
      } catch (error) {
        const apiError = toApiError(error);
        if (apiError.status === 401 && apiError.code !== "testing_secret_invalid") {
          this.ctx.tokens.clear();
          this.ctx.onSignedOut?.(apiError);
        }
        throw apiError;
      }
    }).finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  // ───────────── System ─────────────

  async negotiateVersion(): Promise<{ api_version: number; supported: number[]; service_version: string }> {
    const { data } = await this.request<{ api_version: number; supported: number[]; service_version: string }>({
      method: "GET",
      path: "/api/version",
      auth: false,
      team: "none",
      expect: "version",
      headers: { "Silicon-Extend-Supported-API-Versions": String(API_MAJOR) },
    });
    return data;
  }

  async iam(): Promise<IamInfo> {
    return (await this.request<IamInfo>({ method: "GET", path: "/api/v1/iam", auth: false, team: "none", expect: "iam" })).data;
  }

  /** Validates a test app_secret and names its environment. Doesn't need a login. */
  async testingEnvironment(secret: string): Promise<TestingEnvironment> {
    return (
      await this.request<TestingEnvironment>({
        method: "GET",
        path: "/api/v1/testing-environment",
        auth: false,
        team: "none",
        expect: "testing_environment",
        headers: { "X-Testing-Application-Secret": secret },
      })
    ).data;
  }

  // ───────────── Auth ─────────────

  /** Exchanges an SLT (or, in a test environment, a test member id) and stores the pair. */
  async login(slt: string): Promise<TokenPair> {
    const { data } = await this.request<AuthSession>({
      method: "POST",
      path: "/api/v1/auth/login",
      auth: false,
      team: "none",
      idempotent: true,
      expect: "login",
      body: { type: "login", data: { slt } },
    });
    const pair = pairFromSession(data, this.now());
    this.ctx.tokens.save(pair);
    return pair;
  }

  /** Revokes the refresh-token family, then forgets the pair even if the service is unreachable. */
  async logout(): Promise<void> {
    const pair = this.ctx.tokens.load();
    this.ctx.tokens.clear();
    if (!pair) return;
    await this.request<null>({
      method: "POST",
      path: "/api/v1/auth/logout",
      auth: false,
      team: "none",
      idempotent: true,
      body: { type: "logout", data: { token: pair.refresh_token } },
    });
  }

  async me(): Promise<Me> {
    return (await this.request<Me>({ method: "GET", path: "/api/v1/auth/me", team: "optional", expect: "me" })).data;
  }

  // ───────────── Devices ─────────────

  /**
   * One page of devices. `include_removed` (Carbons, scope=mine only) also lists the Carbon's removed
   * devices, marked with `removed_at` and `removed_reason`.
   */
  async listDevices(params: {
    scope: "mine" | "team" | "accessible";
    cursor?: string | null;
    limit?: number;
    include_removed?: boolean;
  }): Promise<Page<Device>> {
    return (
      await this.request<Page<Device>>({
        method: "GET",
        path: "/api/v1/devices",
        query: { scope: params.scope, cursor: params.cursor, limit: params.limit, include_removed: params.include_removed ? "true" : undefined },
        expect: "devices",
      })
    ).data;
  }

  /**
   * The Carbon's removed devices in the selected team, newest removal first. The service pages
   * paired and removed devices together, so this reads every page (at most `maxPages`).
   */
  async listRemovedDevices(maxPages = 20): Promise<Device[]> {
    const removed: Device[] = [];
    let cursor: string | null = null;
    for (let page = 0; page < maxPages; page++) {
      const result: Page<Device> = await this.listDevices({ scope: "mine", include_removed: true, limit: 100, cursor });
      removed.push(...result.items.filter((d) => d.removed_at));
      cursor = result.next_cursor;
      if (!cursor) break;
    }
    return removed.sort((a, b) => Date.parse(b.removed_at!) - Date.parse(a.removed_at!) || a.name.localeCompare(b.name));
  }

  async getDevice(deviceId: string): Promise<{ device: DeviceDetail; etag: string | null }> {
    const res = await this.request<DeviceDetail>({
      method: "GET",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}`,
      expect: "device",
    });
    return { device: res.data, etag: res.headers.get("ETag") };
  }

  async updateDevice(
    deviceId: string,
    patch: { name?: string; visibility?: Visibility; pair_ttl_days?: number },
    ifMatch: string,
  ): Promise<{ device: Device; etag: string | null }> {
    const res = await this.request<Device>({
      method: "PATCH",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}`,
      ifMatch,
      expect: "device",
      body: { type: "device", data: patch },
    });
    return { device: res.data, etag: res.headers.get("ETag") };
  }

  async removeDevice(deviceId: string, ifMatch: string): Promise<void> {
    await this.request<null>({ method: "DELETE", path: `/api/v1/devices/${encodeURIComponent(deviceId)}`, ifMatch });
  }

  async stopDevice(deviceId: string): Promise<Session> {
    return (
      await this.request<Session>({
        method: "POST",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/stop`,
        expect: "session",
      })
    ).data;
  }

  // ───────────── Pairing ─────────────

  async claimPairing(input: {
    pairing_code: string;
    name: string;
    visibility?: Visibility;
    pair_ttl_days?: number;
    silicon_ids?: string[];
  }): Promise<{ device: Device; etag: string | null }> {
    const res = await this.request<Device>({
      method: "POST",
      path: "/api/v1/pairings",
      idempotent: true,
      expect: "device",
      body: { type: "pairing", data: input },
    });
    return { device: res.data, etag: res.headers.get("ETag") };
  }

  async attachDevice(
    hostId: string,
    input: { os: AttachOs; name: string; visibility?: Visibility; pair_ttl_days?: number },
  ): Promise<Device> {
    return (
      await this.request<Device>({
        method: "POST",
        path: `/api/v1/devices/${encodeURIComponent(hostId)}/attachments`,
        idempotent: true,
        expect: "device",
        body: { type: "attachment", data: input },
      })
    ).data;
  }

  async getSetup(deviceId: string): Promise<Setup> {
    return (
      await this.request<Setup>({ method: "GET", path: `/api/v1/devices/${encodeURIComponent(deviceId)}/setup`, expect: "setup" })
    ).data;
  }

  async enterSetupCode(deviceId: string, code: string): Promise<Setup> {
    return (
      await this.request<Setup>({
        method: "POST",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/setup/code`,
        expect: "setup",
        body: { type: "setup_code", data: { code } },
      })
    ).data;
  }

  // ───────────── Access ─────────────

  async listAccess(deviceId: string): Promise<AccessGrant[]> {
    return (
      await this.request<{ items: AccessGrant[] }>({
        method: "GET",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/access`,
        expect: "access",
      })
    ).data.items;
  }

  async grantAccess(deviceId: string, siliconId: string): Promise<AccessGrant> {
    return (
      await this.request<AccessGrant>({
        method: "PUT",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/access/${encodeURIComponent(siliconId)}`,
        expect: "access_grant",
      })
    ).data;
  }

  async revokeAccess(deviceId: string, siliconId: string): Promise<void> {
    await this.request<null>({
      method: "DELETE",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}/access/${encodeURIComponent(siliconId)}`,
    });
  }

  /** Silicons in the selected team, for choosing who gets access. */
  async listTeamSilicons(): Promise<TeamSilicon[]> {
    return (await this.request<{ items: TeamSilicon[] }>({ method: "GET", path: "/api/v1/team/silicons", expect: "team_silicons" })).data.items;
  }

  // ───────────── Takeovers (the owner Carbon reads and ends them) ─────────────

  async getTakeover(sessionId: string): Promise<Takeover | null> {
    return (
      await this.request<Takeover | null>({
        method: "GET",
        path: `/api/v1/sessions/${encodeURIComponent(sessionId)}/takeover`,
        expect: "takeover",
      })
    ).data;
  }

  /** The Carbon is done: the Silicon's session resumes. */
  async releaseTakeover(sessionId: string): Promise<void> {
    await this.request<null>({ method: "DELETE", path: `/api/v1/sessions/${encodeURIComponent(sessionId)}/takeover` });
  }

  // ───────────── Activity, requests, sessions ─────────────

  async listActivity(
    deviceId: string,
    filters: { silicon_id?: string; session_id?: string; since?: string; until?: string; cursor?: string | null; limit?: number },
  ): Promise<Page<ActivityEntry>> {
    return (
      await this.request<Page<ActivityEntry>>({
        method: "GET",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/activity`,
        query: filters,
        expect: "activity",
      })
    ).data;
  }

  async listDeviceRequests(deviceId: string, cursor?: string | null): Promise<Page<ExtendRequest>> {
    return (
      await this.request<Page<ExtendRequest>>({
        method: "GET",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/requests`,
        query: { cursor },
        expect: "requests",
      })
    ).data;
  }

  async listSessions(params: { device_id?: string; state?: "active" | "paused" | "ended"; cursor?: string | null }): Promise<Page<Session>> {
    return (
      await this.request<Page<Session>>({ method: "GET", path: "/api/v1/sessions", query: params, expect: "sessions" })
    ).data;
  }

  // ───────────── Operations ─────────────

  /** Fire-and-forget telemetry; never throws. Skipped entirely when telemetry is off. */
  async telemetry(event: {
    event: string;
    step: string;
    success: boolean;
    duration_ms: number;
    error_code?: string | null;
    request_id?: string | null;
    device_os?: string | null;
  }): Promise<void> {
    if (this.ctx.telemetryOff() || !this.ctx.tokens.load()) return;
    try {
      await this.request<null>({
        method: "POST",
        path: "/api/v1/telemetry",
        team: "optional",
        body: {
          type: "telemetry",
          data: {
            source: "web",
            event: event.event,
            step: event.step,
            success: event.success,
            duration_ms: Math.max(0, Math.round(event.duration_ms)),
            error_code: event.error_code ?? null,
            command: null,
            device_os: event.device_os ?? null,
            session_id: null,
            request_id: event.request_id ?? null,
            client_version: __APP_VERSION__,
          },
        },
      });
    } catch {
      /* telemetry never interrupts the Carbon */
    }
  }
}

declare const __APP_VERSION__: string;
