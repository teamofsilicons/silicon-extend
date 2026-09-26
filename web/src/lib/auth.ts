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
 */
import { IAM_LOGIN_URL_OVERRIDE } from "../config";
import { ApiError } from "./api";
import type { IamInfo } from "./types";
import { readJson, remove, writeJson } from "./storage";
import { KEYS } from "./session";

const ATTEMPT_TTL_MS = 10 * 60_000;

interface LoginAttempt {
  state: string;
  /** "production" or the test environment id the attempt started in. */
  world: string;
  created: number;
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

export function randomState(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Builds the consent URL and remembers the attempt. Returns the URL to navigate to. */
export function beginIamLogin(info: IamInfo, origin: string, world: string, now = Date.now()): string {
  const state = randomState();
  const attempt: LoginAttempt = { state, world, created: now };
  writeJson("session", KEYS.loginState, attempt);
  const callback = new URL("/auth/callback", origin);
  callback.searchParams.set("state", state);
  const url = new URL(iamLoginUrl(info));
  url.searchParams.set("app_id", info.app_id);
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
  if (!attempt || !state || attempt.state !== state || now - attempt.created > ATTEMPT_TTL_MS)
    throw new ApiError(0, {
      code: "invalid_login_state",
      message: "This sign-in result doesn't belong to a sign-in started in this tab, or it is older than 10 minutes.",
      hint: "Start sign-in again from this tab. Nothing was signed in.",
    });
  if (attempt.world !== world)
    throw new ApiError(0, {
      code: "login_world_changed",
      message: "Sign-in started in a different environment than the one this tab is in now.",
      hint: "Start sign-in again from this tab.",
    });
  return slt;
}
