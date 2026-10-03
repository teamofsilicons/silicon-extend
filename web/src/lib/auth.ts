/**
 * Signing in through the Silicon IAM consent screen, the way IAM's client docs describe it
 * (silicon-iam/docs/client/login.html):
 *
 * 1. Send the browser to `<iam_login_url>?app_id=<app_id>&redirect_uri=<callback>`.
 *    IAM shows Extend's permissions and asks the account to pick one organization. The website never sends
 *    a team (`org_id`): IAM refuses it.
 * 2. IAM returns to `<callback>?slt=…` (it keeps the callback's own query, so our `state` survives).
 *    The SLT lives two minutes and works once.
 * 3. The website posts it to Extend's `POST /api/v1/auth/login`, which does the secret-authenticated
 *    exchange with IAM. The website never holds Extend's app secret.
 *
 * IAM's SLT exchange has no PKCE, so the callback is bound to the attempt this tab started with a
 * random `state` kept in sessionStorage.
 *
 * Signing up goes through IAM too. IAM's auth site serves `/login` and `/signup` side by side, and
 * its sign-up page carries `app_id` and `redirect_uri` through: it verifies the new Carbon's email,
 * creates the account, signs them in, then shows the same consent screen and returns here.
 */
import { IAM_LOGIN_URL_OVERRIDE } from "../config";
import { ApiError } from "./api";
import type { IamInfo } from "./types";
import { readJson, remove, writeJson } from "./storage";
import { KEYS } from "./session";

const ATTEMPT_TTL_MS = 10 * 60_000;
/** Creating an account verifies an email first, so a sign-up attempt gets longer. */
const SIGNUP_TTL_MS = 30 * 60_000;

export type IdentityKind = "carbon" | "silicon";
interface LoginAttempt {
  kind?: IdentityKind;
  display?: "popup" | "redirect";
  context?: string;
  state: string;
  /** "production" or the test environment id the attempt started in. */
  world: string;
  created: number;
  /** A sign-up attempt (IAM's /signup, then its consent screen); absent for sign-in. */
  signup?: boolean;
}

/**
 * The full URL of IAM's consent screen. `GET /api/v1/iam` gives it as `iam_login_url`; only when that
 * is missing does the website fall back to `VITE_IAM_LOGIN_URL` (an origin), then to deriving IAM's
 * sign-in origin from `iam_base_url` (`backend.<domain>` → `auth.<domain>`), each plus `/login`.
 */
export function iamLoginUrl(info: Pick<IamInfo, "iam_base_url" | "iam_login_url">, override = IAM_LOGIN_URL_OVERRIDE): string {
  if (info.iam_login_url) return info.iam_login_url;
  if (override) return `${override.replace(/\/+$/, "")}/login`;
  try {
    const url = new URL(info.iam_base_url);
    if (url.hostname.startsWith("backend.")) url.hostname = `auth.${url.hostname.slice("backend.".length)}`;
    return `${url.toString().replace(/\/+$/, "")}/login`;
  } catch {
    return `${info.iam_base_url.replace(/\/+$/, "")}/login`;
  }
}

/**
 * IAM's sign-up page for Extend, or null when Extend can't tell where it is. `GET /api/v1/iam` may
 * name it as `iam_signup_url`; otherwise, when the consent screen is IAM's own `<auth origin>/login`,
 * sign-up is `<auth origin>/signup` (IAM serves both). A login URL laid out any other way, such as a
 * local stand-in, gives null: the website then offers the sign-in page and says so, rather than guess.
 */
export function iamSignupUrl(info: Pick<IamInfo, "iam_base_url" | "iam_login_url" | "iam_signup_url">, override = IAM_LOGIN_URL_OVERRIDE): string | null {
  if (info.iam_signup_url) return info.iam_signup_url;
  try {
    const url = new URL(iamLoginUrl(info, override));
    if (url.pathname.replace(/\/+$/, "") !== "/login") return null;
    url.pathname = "/signup";
    return url.toString();
  } catch {
    return null;
  }
}

export function randomState(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Builds the consent URL and remembers the attempt. Returns the URL to navigate to. */
export function beginIamLogin(info: IamInfo, origin: string, world: string, now = Date.now(), kind?: IdentityKind, display: "popup" | "redirect" = "redirect", context?: string): string {
  return begin(iamLoginUrl(info), info.app_id, origin, { state: randomState(), world, created: now, kind, display, context });
}

/**
 * Like `beginIamLogin`, for a Carbon who has no Silicon IAM account yet: IAM's sign-up page with the
 * same app_id and callback, so after creating the account IAM asks them to approve Extend and sends
 * them back signed in. Returns null when there is no sign-up page to send them to (see `iamSignupUrl`).
 */
export function beginIamSignup(info: IamInfo, origin: string, world: string, now = Date.now(), context?: string): string | null {
  const signup = iamSignupUrl(info);
  return signup ? begin(signup, info.app_id, origin, { state: randomState(), world, created: now, signup: true, kind: "carbon", display: "redirect", context }) : null;
}

function begin(page: string, appId: string, origin: string, attempt: LoginAttempt): string {
  writeJson("session", KEYS.loginState, attempt);
  const callback = new URL("/auth/callback", origin);
  callback.searchParams.set("state", attempt.state);
  const url = new URL(page);
  if (attempt.kind) url.searchParams.set("identity_kind", attempt.kind);
  url.searchParams.delete("display");
  if (attempt.display === "popup") {
    url.searchParams.set("display", "popup");
    callback.searchParams.set("display", "popup");
  }
  url.searchParams.set("app_id", appId);
  url.searchParams.set("redirect_uri", callback.toString());
  return url.toString();
}

/** Checks the callback belongs to this tab's attempt, and returns the SLT to exchange. */
export function finishIamLogin(params: URLSearchParams, world: string, now = Date.now(), kind?: IdentityKind): string {
  const attempt = readJson<LoginAttempt>("session", KEYS.loginState);
  remove("session", KEYS.loginState);
  const error = params.get("error");
  if (error)
    throw new ApiError(0, {
      code: `iam_${error}`,
      message: `Silicon IAM did not sign you in: ${params.get("error_description") || error}.`,
      hint: "Start sign-in again. If IAM keeps refusing, check that you approved Extend's permissions and picked a team.",
    });
  const slt = params.get("slt");
  const state = params.get("state");
  if (!slt)
    throw new ApiError(0, {
      code: "callback_without_slt",
      message: "IAM came back without a short-lived token.",
      hint: "Start sign-in again from this page.",
    });
  const ttl = attempt?.signup ? SIGNUP_TTL_MS : ATTEMPT_TTL_MS;
  if (!attempt || !state || attempt.state !== state || attempt.kind !== kind || now - attempt.created > ttl)
    throw new ApiError(0, {
      code: "invalid_login_state",
      message: `This sign-in result doesn't belong to a sign-in started in this tab, or it is older than ${ttl / 60_000} minutes.`,
      hint: attempt?.signup
        ? "Your Silicon IAM account exists now, so sign in again from this tab. Nothing was signed in here."
        : "Start sign-in again from this tab. Nothing was signed in.",
    });
  if (attempt.world !== world)
    throw new ApiError(0, {
      code: "login_world_changed",
      message: "Sign-in started in a different environment than the one this tab is in now.",
      hint: "Start sign-in again from this tab.",
    });
  return slt;
}

/** Full-page return consumes only its own typed, context-bound redirect attempt. */
export function finishIamRedirect(params: URLSearchParams, world: string, context: string, now = Date.now()): { slt: string; kind: IdentityKind } {
  const attempt = readJson<LoginAttempt>("session", KEYS.loginState);
  if (!attempt || attempt.display !== "redirect" || (attempt.kind !== "carbon" && attempt.kind !== "silicon") || attempt.context !== context) {
    remove("session", KEYS.loginState);
    throw new ApiError(409, { code: "invalid_login_state", message: "The selected account or environment changed, or this return does not belong to a full-page sign-in. Start sign-in again." });
  }
  return { slt: finishIamLogin(params, world, now, attempt.kind), kind: attempt.kind };
}

const POPUP_MESSAGE = "silicon-extend:sign-in";
export function completeIamPopup(params: URLSearchParams): boolean {
  if (!window.opener || params.get("display") !== "popup" || !params.get("state")) return false;
  window.opener.postMessage({ type: POPUP_MESSAGE, state: params.get("state"), slt: params.get("slt"), error: params.get("error"), error_description: params.get("error_description") }, location.origin);
  window.close();
  return true;
}
export async function signInPopup(info: () => Promise<IamInfo>, kind: IdentityKind, world: () => string, signal?: AbortSignal): Promise<string> {
  const popup = window.open("about:blank", `extend-login-${randomState()}`, "popup,width=520,height=720");
  if (!popup) throw new Error("The popup was blocked. Use one of the full-page sign-in options below.");
  const startedWorld = world();
  let url: URL;
  try {
    const metadata = await info();
    if (signal?.aborted) throw new Error("Sign-in replaced by another attempt.");
    url = new URL(beginIamLogin(metadata, location.origin, startedWorld, Date.now(), kind, "popup"));
  }
  catch (error) { popup.close(); throw error; }
  const state = new URL(url.searchParams.get("redirect_uri")!).searchParams.get("state")!;
  return new Promise((resolve, reject) => {
    const cleanup = () => { signal?.removeEventListener("abort", aborted); window.removeEventListener("message", receive); clearTimeout(timeout); clearInterval(closed); popup.close(); };
    const fail = (error: unknown) => { cleanup(); if (readJson<LoginAttempt>("session", KEYS.loginState)?.state === state) remove("session", KEYS.loginState); reject(error); };
    const aborted = () => fail(new Error("Sign-in replaced by another attempt."));
    const receive = (event: MessageEvent) => {
      if (event.origin !== location.origin || event.source !== popup || event.data?.type !== POPUP_MESSAGE || event.data.state !== state) return;
      const params = new URLSearchParams({ state });
      for (const key of ["slt", "error", "error_description"]) if (typeof event.data[key] === "string") params.set(key, event.data[key]);
      try { const token = finishIamLogin(params, world(), Date.now(), kind); cleanup(); resolve(token); }
      catch (error) { fail(error); }
    };
    const timeout = setTimeout(() => fail(new Error("Sign-in timed out. Please try again.")), ATTEMPT_TTL_MS);
    const closed = setInterval(() => { if (popup.closed) fail(new Error("Sign-in cancelled.")); }, 500);
    signal?.addEventListener("abort", aborted, { once: true });
    window.addEventListener("message", receive);
    popup.location.href = url.href; popup.focus();
  });
}
