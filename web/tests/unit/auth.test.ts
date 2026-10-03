import { beforeEach, describe, expect, it } from "vitest";
import { beginIamLogin, beginIamSignup, finishIamLogin, iamLoginUrl, iamSignupUrl } from "../../src/lib/auth";

const info = { app_id: "extend", iam_base_url: "https://backend.iam.teamofsilicons.com", api_base_url: "", website_url: "", docs_url: "" };

beforeEach(() => sessionStorage.clear());

describe("IAM consent URL", () => {
  it("uses iam_login_url from Extend when it is there", () => {
    expect(iamLoginUrl({ iam_base_url: "https://backend.iam.x", iam_login_url: "http://127.0.0.1:8480/dev/iam/login" }, "https://override.example")).toBe("http://127.0.0.1:8480/dev/iam/login");
  });

  it("falls back to the override, then derives IAM's sign-in origin from its API origin", () => {
    expect(iamLoginUrl({ ...info, iam_login_url: null }, "https://override.example/")).toBe("https://override.example/login");
    expect(iamLoginUrl(info, "")).toBe("https://auth.iam.teamofsilicons.com/login");
    expect(iamLoginUrl({ iam_base_url: "http://localhost:5190/__mock/iam" }, "")).toBe("http://localhost:5190/__mock/iam/login");
  });

  it("keeps any query on iam_login_url and adds app_id and redirect_uri", () => {
    const url = new URL(beginIamLogin({ ...info, iam_login_url: "https://auth.iam.teamofsilicons.com/login?theme=extend" }, "https://b.test", "production"));
    expect(url.searchParams.get("theme")).toBe("extend");
    expect(url.searchParams.get("app_id")).toBe("extend");
  });

  it("sends app_id and a callback carrying state, never a team", () => {
    const url = new URL(beginIamLogin(info, "https://extend.teamofsilicons.com", "production"));
    expect(url.origin + url.pathname).toBe("https://auth.iam.teamofsilicons.com/login");
    expect(url.searchParams.get("app_id")).toBe("extend");
    expect(url.searchParams.has("org_id")).toBe(false);
    const callback = new URL(url.searchParams.get("redirect_uri")!);
    expect(callback.origin + callback.pathname).toBe("https://extend.teamofsilicons.com/auth/callback");
    expect(callback.searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{43}$/);
  });
});

describe("callback", () => {
  const start = (now = 1000) => new URL(new URL(beginIamLogin(info, "https://b.test", "production", now)).searchParams.get("redirect_uri")!).searchParams.get("state")!;

  it("returns the SLT for the attempt this tab started, once", () => {
    const state = start();
    const params = new URLSearchParams({ state, slt: "oac_abc" });
    expect(finishIamLogin(params, "production", 2000)).toBe("oac_abc");
    expect(() => finishIamLogin(params, "production", 2000)).toThrow(/doesn't belong/);
  });

  it("refuses a callback with another state", () => {
    start();
    expect(() => finishIamLogin(new URLSearchParams({ state: "forged", slt: "oac_abc" }), "production")).toThrow(/doesn't belong/);
  });

  it("refuses an attempt older than 10 minutes", () => {
    const state = start(0);
    expect(() => finishIamLogin(new URLSearchParams({ state, slt: "oac_abc" }), "production", 11 * 60_000)).toThrow(/older than 10 minutes/);
  });

  it("refuses a result from a different environment than the tab is in now", () => {
    const state = start();
    expect(() => finishIamLogin(new URLSearchParams({ state, slt: "oac_abc" }), "9b3e0c1a", 2000)).toThrow(/different environment/);
  });

  it("reports IAM's own error", () => {
    start();
    expect(() => finishIamLogin(new URLSearchParams({ error: "access_denied" }), "production")).toThrow(/did not sign you in: access_denied/);
  });
});

describe("IAM sign-up", () => {
  it("uses iam_signup_url when Extend names one", () => {
    expect(iamSignupUrl({ ...info, iam_login_url: "https://auth.iam.teamofsilicons.com/login", iam_signup_url: "https://join.example/start" }, "")).toBe("https://join.example/start");
  });

  it("derives IAM's /signup beside its /login, keeping the login URL's query", () => {
    expect(iamSignupUrl({ ...info, iam_login_url: "https://auth.iam.teamofsilicons.com/login" }, "")).toBe("https://auth.iam.teamofsilicons.com/signup");
    expect(iamSignupUrl({ ...info, iam_login_url: "https://auth.iam.teamofsilicons.com/login/?theme=extend" }, "")).toBe("https://auth.iam.teamofsilicons.com/signup?theme=extend");
    // Without iam_login_url: the derived sign-in origin (`backend.` host → `auth.` host).
    expect(iamSignupUrl(info, "")).toBe("https://auth.iam.teamofsilicons.com/signup");
  });

  it("doesn't guess for a login page laid out another way (the local stand-in, the mock)", () => {
    expect(iamSignupUrl({ ...info, iam_login_url: "http://127.0.0.1:8480/dev/iam/login" }, "")).toBeNull();
    expect(iamSignupUrl({ ...info, iam_login_url: "http://localhost:5190/__mock/iam/login" }, "")).toBeNull();
    expect(beginIamSignup({ ...info, iam_login_url: "http://127.0.0.1:8480/dev/iam/login" }, "https://b.test", "production")).toBeNull();
  });

  it("sends app_id and the same state-bound callback as sign-in, so IAM brings the new Carbon back signed in", () => {
    const url = new URL(beginIamSignup(info, "https://extend.teamofsilicons.com", "production")!);
    expect(url.origin + url.pathname).toBe("https://auth.iam.teamofsilicons.com/signup");
    expect(url.searchParams.get("app_id")).toBe("extend");
    expect(url.searchParams.has("org_id")).toBe(false);
    const callback = new URL(url.searchParams.get("redirect_uri")!);
    expect(callback.origin + callback.pathname).toBe("https://extend.teamofsilicons.com/auth/callback");
    const state = callback.searchParams.get("state")!;
    expect(finishIamLogin(new URLSearchParams({ state, slt: "oac_new" }), "production")).toBe("oac_new");
  });

  it("gives a sign-up 30 minutes (email and phone checks come first), and a sign-in still 10", () => {
    const signup = new URL(new URL(beginIamSignup(info, "https://b.test", "production", 0)!).searchParams.get("redirect_uri")!).searchParams.get("state")!;
    expect(finishIamLogin(new URLSearchParams({ state: signup, slt: "oac_new" }), "production", 25 * 60_000)).toBe("oac_new");
    const late = new URL(new URL(beginIamSignup(info, "https://b.test", "production", 0)!).searchParams.get("redirect_uri")!).searchParams.get("state")!;
    expect(() => finishIamLogin(new URLSearchParams({ state: late, slt: "oac_new" }), "production", 31 * 60_000)).toThrow(/older than 30 minutes/);
    const login = new URL(new URL(beginIamLogin(info, "https://b.test", "production", 0)).searchParams.get("redirect_uri")!).searchParams.get("state")!;
    expect(() => finishIamLogin(new URLSearchParams({ state: login, slt: "oac_abc" }), "production", 25 * 60_000)).toThrow(/older than 10 minutes/);
  });
});

it("locks each popup account kind into the saved attempt and IAM URL", () => {
  for (const kind of ["carbon", "silicon"] as const) {
    const url = new URL(beginIamLogin(info, "https://extend.test", "production", 100, kind));
    expect(url.searchParams.get("identity_kind")).toBe(kind); expect(url.searchParams.get("display")).toBe("popup");
    const params = new URL(url.searchParams.get("redirect_uri")!).searchParams; params.set("slt", "oac_fresh");
    expect(() => finishIamLogin(params, "production", 101, kind === "carbon" ? "silicon" : "carbon")).toThrow();
  }
});
