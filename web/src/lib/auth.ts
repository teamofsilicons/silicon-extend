/**
 * Signing in through the Silicon IAM consent screen, the way IAM's client docs describe it
 * (silicon-iam/docs/client/login.html):
 *
 * 1. Send the browser to `<iam_login_url>?app_id=<app_id>&redirect_uri=<callback>`.
 *    IAM shows Extend's permissions and asks the Carbon to pick teams. The website never sends
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
 * its sign-up page carries `app_id` and `redirect_uri` through: it verifies the new Carbon's email and
 * phone, creates the account, signs them in, then shows the same consent screen and returns here.
 */
import { IAM_LOGIN_URL_OVERRIDE } from "../config";
import { ApiError } from "./api";
import type { IamInfo } from "./types";
import { readJson, remove, writeJson } from "./storage";
import { KEYS } from "./session";

const ATTEMPT_TTL_MS = 10 * 60_000;
/** Creating an account verifies an email and a phone first, so a sign-up attempt gets longer. */
const SIGNUP_TTL_MS = 30 * 60_000;

interface LoginAttempt {
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
export function beginIamLogin(info: IamInfo, origin: string, world: string, now = Date.now()): string {
  return begin(iamLoginUrl(info), info.app_id, origin, { state: randomState(), world, created: now });
}

/**
 * Like `beginIamLogin`, for a Carbon who has no Silicon IAM account yet: IAM's sign-up page with the
 * same app_id and callback, so after creating the account IAM asks them to approve Extend and sends
 * them back signed in. Returns null when there is no sign-up page to send them to (see `iamSignupUrl`).
 */
export function beginIamSignup(info: IamInfo, origin: string, world: string, now = Date.now()): string | null {
  const signup = iamSignupUrl(info);
  return signup ? begin(signup, info.app_id, origin, { state: randomState(), world, created: now, signup: true }) : null;
}

function begin(page: string, appId: string, origin: string, attempt: LoginAttempt): string {
  writeJson("session", KEYS.loginState, attempt);
  const callback = new URL("/auth/callback", origin);
  callback.searchParams.set("state", attempt.state);
  const url = new URL(page);
  url.searchParams.set("app_id", appId);
  url.searchParams.set("redirect_uri", callback.toString());
  return url.toString();
}

/** Checks the callback belongs to this tab's attempt, and returns the SLT to exchange. */
export function finishIamLogin(params: URLSearchParams, world: string, now = Date.now()): string {
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
  if (!attempt || !state || attempt.state !== state || now - attempt.created > ttl)
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
