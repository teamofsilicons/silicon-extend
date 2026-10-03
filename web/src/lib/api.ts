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
  DeviceStopped,
  ErrorBody,
  IamInfo,
  InUseIndicator,
  Me,
  Member,
  Page,
  RetryResult,
  Session,
  Setup,
  Takeover,
  TeamSilicon,
  TeamSilicons,
  TestingEnvironment,
  TingRegistration,
  WakeAnswered,
  WakeRequest,
  WakeSettingsView,
} from "./types";

export const API_MAJOR = 1;

/** What PATCH /devices/{id} changes; send only what changed. */
export interface DevicePatch {
  name?: string;
  pair_ttl_days?: number;
  in_use_indicator?: InUseIndicator;
}

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

export interface PermissionEndpoint {
  audience: string;
  endpoint_id: string;
}

export interface FeaturePermission extends PermissionEndpoint {
  grant_id: string;
  org_id: string;
  actor: { public_id?: string; id?: string; kind?: string };
  expires_at: string;
}

export interface FeaturePermissionRequest {
  state?: string;
  id: string;
  consent_url: string;
  expires_at: string;
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
  /**
   * The team handle sent as X-Org-ID. Since 1.1 it is the default Team for grants and a Silicon's
   * Team; it no longer filters a Carbon's devices, grants or history.
   */
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
  /**
   * X-Org-ID: "required" fails before sending when no Team is selected; "optional" sends it when
   * one is. Routes of a Carbon's own devices are "optional" since 1.1: devices belong to the Carbon.
   */
  team?: TeamMode;
  /** Send an Idempotency-Key, reused on the one retry after a network failure. */
  idempotent?: boolean;
  /** Retain a mutation's key when the user explicitly retries the same request. */
  idempotencyKey?: string;
  ifMatch?: string;
  /** Expected envelope `type` of a successful body, or the types a route may answer with. */
  expect?: string | string[];
  headers?: Record<string, string>;
}

export interface ApiResponse<T> {
  status: number;
  /** The envelope's `type`, for routes that answer with more than one ("" for 204). */
  type: string;
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

  private async parse<T>(response: Response, expect?: string | string[]): Promise<{ type: string; data: T }> {
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
    if (response.status === 204) return { type: "", data: null as T };
    if (!envelope || typeof envelope.type !== "string" || !("data" in envelope))
      throw new ApiError(response.status, {
        code: "unexpected_response",
        message: `Extend answered HTTP ${response.status} without the {"type", "data"} envelope.`,
        hint: "The website and the service disagree on the contract. Report it with the request id.",
        request_id: requestId ?? undefined,
      });
    const expected = expect === undefined ? [] : Array.isArray(expect) ? expect : [expect];
    if (expected.length && !expected.includes(envelope.type))
      throw new ApiError(response.status, {
        code: "unexpected_response",
        message: `Expected a ${expected.map((t) => `"${t}"`).join(" or ")} response from Extend, got "${envelope.type}".`,
        hint: "The website and the service disagree on the contract. Report it with the request id.",
        request_id: requestId ?? undefined,
      });
    return { type: envelope.type, data: envelope.data as T };
  }

  /** Sends one request, refreshing the access token first if needed and once more after a 401. */
  async request<T>(options: RequestOptions): Promise<ApiResponse<T>> {
    const auth = options.auth ?? true;
    const headers = this.baseHeaders(options);
    if (options.idempotent) headers["Idempotency-Key"] = options.idempotencyKey ?? (this.ctx.newKey ?? defaultKey)();
    const init = (token?: string): RequestInit => ({
      method: options.method,
      headers: token ? { ...headers, Authorization: `Bearer ${token}` } : headers,
      body: options.body ? JSON.stringify(options.body) : undefined,
    });
    const url = this.url(options.path, options.query);
    const retry = !!options.idempotent || options.method === "GET";

    if (!auth) {
      const response = await this.send(url, init(), retry);
      return { status: response.status, headers: response.headers, ...(await this.parse<T>(response, options.expect)) };
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
      const error = await this.parse<never>(response).then(
        () => null,
        (e: ApiError) => e,
      );
      if (error && (error.code === "testing_secret_invalid" || error.code === "testing_environment_not_ready")) throw error;
      pair = await this.refresh(pair);
      response = await this.send(url, init(pair.access_token), retry);
    }
    return { status: response.status, headers: response.headers, ...(await this.parse<T>(response, options.expect)) };
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
  async login(slt: string, kind?: "carbon" | "silicon"): Promise<TokenPair> {
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
    if (kind) {
      const { data: verified } = await this.request<Me>({ method: "GET", path: "/api/v1/auth/me", auth: false, team: "none", expect: "me", headers: { Authorization: `Bearer ${pair.access_token}` } });
      if (!verified.authenticated || verified.member.type !== kind || pair.member.type !== kind || verified.member.id !== pair.member.id) throw new ApiError(401, { code: "identity_kind_mismatch", message: `This sign-in did not return a ${kind} account. Start again with the matching account button.` });
    }
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
   * One page of devices. A Carbon's own devices (scope=mine) are every device they paired, whichever
   * Team is selected, so X-Org-ID is only sent along; a Silicon's (scope=accessible) are the ones it
   * may use in its selected Team. `include_removed` (Carbons, scope=mine only) also lists the
   * Carbon's removed devices, marked with `removed_at` and `removed_reason`.
   */
  async listDevices(params: {
    scope: "mine" | "accessible";
    cursor?: string | null;
    limit?: number;
    include_removed?: boolean;
  }): Promise<Page<Device>> {
    return (
      await this.request<Page<Device>>({
        method: "GET",
        path: "/api/v1/devices",
        team: params.scope === "accessible" ? "required" : "optional",
        query: { scope: params.scope, cursor: params.cursor, limit: params.limit, include_removed: params.include_removed ? "true" : undefined },
        expect: "devices",
      })
    ).data;
  }

  /**
   * The Carbon's removed devices, newest removal first. The service pages paired and removed devices
   * together, so this reads every page (at most `maxPages`).
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
      team: "optional",
      expect: "device",
    });
    return { device: res.data, etag: res.headers.get("ETag") };
  }

  /**
   * Renames the Carbon's pair of a device, sets how long it stays paired, or shows or hides what the
   * device itself shows while a Silicon uses it (`in_use_indicator`, one setting for the whole
   * device, shared by every Carbon who paired it). Visibility is gone in 1.1.
   */
  async updateDevice(deviceId: string, patch: DevicePatch, ifMatch: string): Promise<{ device: Device; etag: string | null }> {
    const res = await this.request<Device>({
      method: "PATCH",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}`,
      team: "optional",
      ifMatch,
      expect: "device",
      body: { type: "device", data: patch },
    });
    return { device: res.data, etag: res.headers.get("ETag") };
  }

  async removeDevice(deviceId: string, ifMatch: string): Promise<void> {
    await this.request<null>({ method: "DELETE", path: `/api/v1/devices/${encodeURIComponent(deviceId)}`, team: "optional", ifMatch });
  }

  /**
   * Stops the Silicon using the device. When its session ran through the Carbon's own pair the answer
   * is that session; when it ran through another Carbon's pair of the same device, the Silicon isn't
   * named (`device_stopped`). A device a computer carries that the Carbon didn't pair answers 409
   * conflict, saying to stop it at the computer.
   */
  async stopDevice(deviceId: string): Promise<{ kind: "session"; session: Session } | { kind: "other"; stopped: DeviceStopped }> {
    const res = await this.request<Session | DeviceStopped>({
      method: "POST",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}/stop`,
      team: "optional",
      expect: ["session", "device_stopped"],
    });
    return res.type === "device_stopped" ? { kind: "other", stopped: res.data as DeviceStopped } : { kind: "session", session: res.data as Session };
  }

  // ───────────── Pairing ─────────────

  /**
   * Claims a pairing code, the first pair of a device or "Pair with another Carbon". Access is given
   * afterwards, per Team, so no `silicon_ids` go with it (1.1 refuses them without X-Org-ID).
   */
  async claimPairing(input: { pairing_code: string; name: string; pair_ttl_days?: number }): Promise<{ device: Device; etag: string | null }> {
    const res = await this.request<Device>({
      method: "POST",
      path: "/api/v1/pairings",
      team: "optional",
      idempotent: true,
      expect: "device",
      body: { type: "pairing", data: input },
    });
    return { device: res.data, etag: res.headers.get("ETag") };
  }

  /** Adds a device that pairs through one of the Carbon's computers. */
  async attachDevice(hostId: string, input: { os: AttachOs; name: string; pair_ttl_days?: number }): Promise<Device> {
    return (
      await this.request<Device>({
        method: "POST",
        path: `/api/v1/devices/${encodeURIComponent(hostId)}/attachments`,
        team: "optional",
        idempotent: true,
        expect: "device",
        body: { type: "attachment", data: input },
      })
    ).data;
  }

  async getSetup(deviceId: string): Promise<Setup> {
    return (
      await this.request<Setup>({ method: "GET", path: `/api/v1/devices/${encodeURIComponent(deviceId)}/setup`, team: "optional", expect: "setup" })
    ).data;
  }

  async enterSetupCode(deviceId: string, code: string): Promise<Setup> {
    return (
      await this.request<Setup>({
        method: "POST",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/setup/code`,
        team: "optional",
        expect: "setup",
        body: { type: "setup_code", data: { code } },
      })
    ).data;
  }

  /**
   * Asks the device to run a failed setup step again now (every failed step without `step`). The
   * service changes no step itself: the device reports progress as usual, so keep reading getSetup.
   * Refusals carry the service's own words: 400 when the device has no such step, 409 when nothing
   * failed or the device is offline, 426 when its app is older than 1.1, 429 at most once every 5 s.
   */
  async retrySetup(deviceId: string, step?: string | null): Promise<RetryResult> {
    const res = await this.request<RetryResult>({
      method: "POST",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}/setup/retry`,
      team: "optional",
      expect: "setup_retry",
      body: { type: "setup_retry", data: step ? { step } : {} },
    });
    return { retrying: Array.isArray(res.data?.retrying) ? res.data.retrying : step ? [step] : [] };
  }

  // ───────────── Access ─────────────

  /** Every grant on the Carbon's pair, in every Team, each with its `team`. */
  async listAccess(deviceId: string): Promise<AccessGrant[]> {
    return (
      await this.request<{ items: AccessGrant[] }>({
        method: "GET",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/access`,
        team: "optional",
        expect: "access",
      })
    ).data.items;
  }

  /**
   * Gives a Silicon access in one of the Carbon's Teams: `team` goes as ?team= (the selected Team,
   * X-Org-ID, when absent). The Carbon's Extend login must reach that Team.
   */
  async grantAccess(deviceId: string, siliconId: string, team?: string | null): Promise<AccessGrant> {
    return (
      await this.request<AccessGrant>({
        method: "PUT",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/access/${encodeURIComponent(siliconId)}`,
        query: { team: team ?? undefined },
        team: team ? "optional" : "required",
        expect: "access_grant",
      })
    ).data;
  }

  /** Takes a Silicon's access away: in one Team with `team`, in every Team without it. Works on ownership alone. */
  async revokeAccess(deviceId: string, siliconId: string, team?: string | null): Promise<void> {
    await this.request<null>({
      method: "DELETE",
      path: `/api/v1/devices/${encodeURIComponent(deviceId)}/access/${encodeURIComponent(siliconId)}`,
      query: { team: team ?? undefined },
      team: "optional",
    });
  }

  /** Silicons in the selected Team (X-Org-ID, or `team` in its place), for choosing who gets access. */
  async listTeamSilicons(team?: string | null): Promise<TeamSilicon[]> {
    return (
      await this.request<{ items: TeamSilicon[] }>({
        method: "GET",
        path: "/api/v1/team/silicons",
        headers: team ? { "X-Org-ID": team } : undefined,
        expect: "team_silicons",
      })
    ).data.items;
  }

  /**
   * The Silicons of every Team the Carbon's login reaches (team=any), each tagged with its Team, and
   * which Teams couldn't be read and why. A 1.0 service ignores team=any and answers the selected
   * Team's Silicons without `teams`; `across` is false then.
   */
  async listAllTeamSilicons(): Promise<{ items: TeamSilicon[]; teams: NonNullable<TeamSilicons["teams"]>; across: boolean }> {
    const data = (
      await this.request<TeamSilicons>({ method: "GET", path: "/api/v1/team/silicons", query: { team: "any" }, team: "optional", expect: "team_silicons" })
    ).data;
    return { items: data.items ?? [], teams: data.teams ?? [], across: Array.isArray(data.teams) };
  }

  // ───────────── Waking ─────────────

  /** Wake requests on the Carbon's pair (every Team's, tagged), or a Silicon's own. */
  async listWakeRequests(deviceId: string, params: { state?: "open" | "all"; cursor?: string | null; limit?: number } = {}): Promise<Page<WakeRequest>> {
    return (
      await this.request<Page<WakeRequest>>({
        method: "GET",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/wake-requests`,
        query: params,
        team: "optional",
        expect: "wake_requests",
      })
    ).data;
  }

  /**
   * The Carbon's answer. "woken" ("It's awake") is a fact about the device: it ends every open wake
   * request on it, through every Carbon's pair and in every Team. "declined" ends only this pair's
   * requests (or the ones listed).
   */
  async answerWake(deviceId: string, answer: "woken" | "declined", wakeIds?: string[]): Promise<WakeAnswered> {
    return (
      await this.request<WakeAnswered>({
        method: "POST",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/wake-requests/answer`,
        team: "optional",
        idempotent: true,
        expect: "wake_answer",
        body: { type: "wake_answer", data: wakeIds?.length ? { answer, wake_ids: wakeIds } : { answer } },
      })
    ).data;
  }

  /** Turns wake requests off or on for the pair, or for one Silicon (in every Team, or only in `team`). */
  async setWakeSettings(deviceId: string, settings: { muted: boolean; silicon_id?: string; team?: string }): Promise<WakeSettingsView> {
    return (
      await this.request<WakeSettingsView>({
        method: "PUT",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/wake-settings`,
        team: "optional",
        expect: "wake_settings",
        body: { type: "wake_settings", data: settings },
      })
    ).data;
  }

  // ───────────── Ting ─────────────

  /**
   * Whether Extend's Tings reach the member in a Team, and which of Extend's Ting types the Team is
   * missing. "any" (Carbons) lists every Team of the login plus the Teams of the Carbon's grants.
   */
  async permissions(): Promise<FeaturePermission[]> {
    return (await this.request<{ items: FeaturePermission[] }>({ method: "GET", path: "/api/v1/permissions", expect: "permissions" })).data.items;
  }

  async requestPermissions(endpoints: PermissionEndpoint[], idempotencyKey: string, callback?: { redirect_uri: string; state: string }): Promise<FeaturePermissionRequest> {
    return (await this.request<FeaturePermissionRequest>({
      method: "POST", path: "/api/v1/permissions", body: { type: "permission", data: { endpoints, ...(callback ? { callback } : {}) } },
      idempotent: true, idempotencyKey, expect: "permission",
    })).data;
  }

  async completePermissions(id: string, code: string, idempotencyKey: string, state?: string): Promise<FeaturePermission[]> {
    return (await this.request<{ items: FeaturePermission[] }>({
      method: "POST", path: `/api/v1/permissions/${encodeURIComponent(id)}/complete`, body: { type: "permission", data: { code, ...(state ? { state } : {}) } },
      idempotent: true, idempotencyKey, expect: "permissions",
    })).data.items;
  }

  async getTingRegistrations(team: string | "any" = "any"): Promise<TingRegistration[]> {
    const res = await this.request<TingRegistration | Page<TingRegistration> | TingRegistration[]>({
      method: "GET",
      path: "/api/v1/ting-registration",
      query: { team },
      team: "optional",
      expect: ["ting_registration", "ting_registrations"],
    });
    const data = res.data;
    if (Array.isArray(data)) return data;
    if (data && "items" in data && Array.isArray(data.items)) return data.items;
    return data ? [data as TingRegistration] : [];
  }

  /**
   * "Turn on": registers the member with Ting in that Team now, with their own login, and sends the
   * Tings waiting for them there. Where they are that Team's Ting manager, Extend also registers its
   * missing Ting types there (Carbon decision 4).
   */
  async turnOnTing(team: string): Promise<TingRegistration> {
    return (
      await this.request<TingRegistration>({
        method: "PUT",
        path: "/api/v1/ting-registration",
        query: { team },
        team: "optional",
        expect: "ting_registration",
      })
    ).data;
  }

  // ───────────── Takeovers (the owner Carbon reads and ends them) ─────────────

  async getTakeover(sessionId: string): Promise<Takeover | null> {
    return (
      await this.request<Takeover | null>({
        method: "GET",
        path: `/api/v1/sessions/${encodeURIComponent(sessionId)}/takeover`,
        team: "optional",
        expect: "takeover",
      })
    ).data;
  }

  /** The Carbon is done: the Silicon's session resumes. */
  async releaseTakeover(sessionId: string): Promise<void> {
    await this.request<null>({ method: "DELETE", path: `/api/v1/sessions/${encodeURIComponent(sessionId)}/takeover`, team: "optional" });
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
        team: "optional",
        expect: "activity",
      })
    ).data;
  }

  /**
   * Requests on the Carbon's pair: the ones its Silicons sent (through this pair), and the ones routed
   * to the Carbon because a Silicon they gave access to holds the device (`routed_to: "carbon"`).
   */
  async listDeviceRequests(deviceId: string, cursor?: string | null): Promise<Page<ExtendRequest>> {
    return (
      await this.request<Page<ExtendRequest>>({
        method: "GET",
        path: `/api/v1/devices/${encodeURIComponent(deviceId)}/requests`,
        query: { cursor },
        team: "optional",
        expect: "requests",
      })
    ).data;
  }

  async listSessions(params: { device_id?: string; state?: "active" | "paused" | "ended"; cursor?: string | null }): Promise<Page<Session>> {
    return (
      await this.request<Page<Session>>({ method: "GET", path: "/api/v1/sessions", query: params, team: "optional", expect: "sessions" })
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
