import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { ArrowRight, FlaskConical, KeyRound } from "lucide-solid";
import { session } from "../lib/session";
import { ApiError, toApiError } from "../lib/api";
import { beginIamLogin, beginIamSignup, iamSignupUrl, signInPopup, type IdentityKind } from "../lib/auth";
import type { IamInfo } from "../lib/types";
import { Button, ErrorNote } from "../components/ui";
import { ExtendMark } from "../components/ExtendMark";
import Shader from "../components/Shader";
import { TestingSecretForm } from "../components/TestingSecretForm";
import { navigate } from "../lib/router";
import { write } from "../lib/storage";

export default function SignIn(props: { reason?: ApiError | null; next?: string; onSignedIn?: () => void }) {
  const s = session();
  const [slt, setSlt] = createSignal("");
  const [busy, setBusy] = createSignal<"carbon" | "silicon" | "signup" | "slt" | "redirect" | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [showTesting, setShowTesting] = createSignal(false);
  const testing = () => s.world().kind === "testing";
  const worldKey = () => {
    const w = s.world();
    return w.kind === "production" ? "production" : w.environment.environment_id;
  };
  // Read early only to say where "Create an account" leads; each click reads it again.
  const [iamInfo, setIamInfo] = createSignal<IamInfo | null>(null);
  onMount(async () => {
    try {
      setIamInfo(await s.client().iam());
    } catch {
      /* the error shows if the Carbon chooses to continue */
    }
  });
  const signupPageKnown = () => {
    const info = iamInfo();
    return !info || iamSignupUrl(info) !== null;
  };

  let operation = 0;
  let popupController: AbortController | undefined;
  onCleanup(() => popupController?.abort());

  /** Full-page IAM remains available when a popup is blocked or cannot finish. */
  async function withIam(kind: IdentityKind, signup = false) {
    const current = ++operation;
    popupController?.abort();
    s.invalidateLogin();
    setBusy(signup ? "signup" : "redirect");
    setError(null);
    const client = s.client(), revision = s.contextRevision(), world = worldKey(), context = s.loginContext();
    try {
      const info = await client.iam();
      if (current !== operation || revision !== s.contextRevision() || context !== s.loginContext()) throw new Error("The selected account or environment changed. Start sign-in again.");
      setIamInfo(info);
      write("session", "extend.next", props.next ?? "/devices");
      const url = (signup ? beginIamSignup(info, location.origin, world, Date.now(), context) : null)
        ?? beginIamLogin(info, location.origin, world, Date.now(), kind, "redirect", context);
      location.assign(url);
    } catch (e) {
      if (current === operation) { setError(toApiError(e)); setBusy(null); }
    }
  }

  async function withKind(kind: IdentityKind) {
    const current = ++operation;
    popupController?.abort();
    s.invalidateLogin();
    popupController = new AbortController();
    setBusy(kind); setError(null);
    try {
      const client = s.client(), revision = s.contextRevision();
      const token = await signInPopup(() => client.iam(), kind, worldKey, popupController.signal);
      if (current !== operation || revision !== s.contextRevision()) throw new Error("The selected account or environment changed. Start sign-in again.");
      await client.login(token, kind);
      s.clearSignedOutReason(); props.onSignedIn?.();
      const next = props.next ?? "/devices";
      if (next.startsWith("/") && !next.startsWith("//")) navigate(next, { replace: true });
    } catch (error) { if (current === operation) setError(toApiError(error)); }
    finally { if (current === operation) setBusy(null); }
  }

  async function withSlt(event: Event) {
    event.preventDefault();
    const value = slt().trim();
    if (!value) {
      setError(
        new ApiError(0, {
          code: "slt_missing",
          message: testing() ? "Paste a short-lived token, or type a test member id such as c:alice." : "Paste a short-lived token first.",
          hint: "Short-lived tokens come from the Silicon IAM consent screen or the IAM CLI, and work once within 2 minutes.",
        }),
      );
      return;
    }
    ++operation;
    popupController?.abort();
    s.invalidateLogin();
    setBusy("slt");
    setError(null);
    try {
      await s.client().login(value);
      s.clearSignedOutReason();
      setSlt("");
      props.onSignedIn?.();
      if (props.next && location.pathname !== props.next && (location.pathname === "/" || location.pathname === "/sign-in")) navigate(props.next, { replace: true });
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section class="welcome sign-in" data-testid="sign-in">
      <div class="sign-in-card">
        <p class="eyebrow welcome-eyebrow">
          <ExtendMark size={16} /> Silicon Extend{testing() ? " · test environment" : ""}
        </p>
        <h1 class="welcome-title">
          Let your Silicons use <em>your devices.</em>
        </h1>
        <p class="lead">
          Pair your phones, computers and TVs with Silicon Extend, then choose which Silicons can use each one. You can stop a Silicon at any time.
        </p>

        <Show when={props.reason}>
          <div class="notice">
            <p>You were signed out.</p>
            <ErrorNote error={props.reason} compact />
          </div>
        </Show>

        <Show when={testing()}>
          <p class="notice testing-notice" data-testid="testing-sign-in-note">
            <FlaskConical size={16} aria-hidden="true" />
            <span>
              You are in a test environment. Sign in with a test SLT, or type the id of an existing test Carbon or Silicon (<code>c:alice</code>, <code>si:chef</code>).
            </span>
          </p>
        </Show>

        <div class="input-row"><Button variant="primary" onClick={() => withKind("carbon")} busy={busy() === "carbon"} disabled={busy() !== null} data-testid="sign-in-carbon">Continue as Carbon <ArrowRight size={16} aria-hidden="true" /></Button><Button onClick={() => withKind("silicon")} busy={busy() === "silicon"} disabled={busy() !== null} data-testid="sign-in-silicon">Continue as Silicon <ArrowRight size={16} aria-hidden="true" /></Button></div>
        <p class="fine">Choose your account and organization in the IAM popup. Feature access is approved separately when you need it. Extend never sees your password.</p>

        <p class="fine" data-testid="full-page-sign-in">
          Prefer this window?{" "}
          <button class="link-button" onClick={() => withIam("carbon")} disabled={busy() === "redirect" || busy() === "signup" || busy() === "slt"} data-testid="sign-in-carbon-page">Carbon full-page sign-in</button>
          {" · "}
          <button class="link-button" onClick={() => withIam("silicon")} disabled={busy() === "redirect" || busy() === "signup" || busy() === "slt"} data-testid="sign-in-silicon-page">Silicon full-page sign-in</button>
        </p>

        {/* Signing up is IAM's too. Test identities come from the test environment, not from sign-up. */}
        <Show when={!testing()}>
          <div class="signup" data-testid="signup">
            <p class="signup-line">
              New to Silicon IAM?{" "}
              <button class="link-button" onClick={() => withIam("carbon", true)} disabled={busy() !== null} data-testid="sign-up-iam">
                Create an account
              </button>
            </p>
            <p class="fine" data-testid="signup-note">
              {signupPageKnown()
                ? "Silicon IAM verifies your email, creates your Carbon account and signs you in with a code, then asks you to approve Extend and sends you back here."
                : "This Silicon IAM gives Extend no sign-up page, so this opens its sign-in page. Create your account there if it offers to; if it doesn't, ask someone in your Team to invite you."}
            </p>
          </div>
        </Show>

        <div class="divider">
          <span>or</span>
        </div>

        <form class="slt-form" onSubmit={withSlt}>
          <label for="slt">
            <KeyRound size={15} aria-hidden="true" /> {testing() ? "Short-lived token or test member id" : "Sign in with a short-lived token"}
          </label>
          <div class="input-row">
            <input
              id="slt"
              name="slt"
              data-testid="slt-input"
              autocomplete="off"
              spellcheck={false}
              placeholder={testing() ? "oac_… or c:alice" : "oac_…"}
              value={slt()}
              onInput={(e) => setSlt(e.currentTarget.value)}
            />
            <Button type="submit" busy={busy() === "slt"} data-testid="slt-submit">
              Sign in
            </Button>
          </div>
          <p class="fine">An SLT (short-lived token) from the IAM consent screen or the IAM CLI. It works once and expires after 2 minutes.</p>
        </form>

        <ErrorNote error={error()} />

        <Show
          when={!testing()}
          fallback={
            <p class="fine">
              To use production instead, choose <strong>Exit testing</strong> in the banner above.
            </p>
          }
        >
          <div class="testing-entry">
            <button class="link-button" onClick={() => setShowTesting(!showTesting())} aria-expanded={showTesting()} data-testid="use-test-environment">
              <FlaskConical size={15} aria-hidden="true" /> Use a test environment
            </button>
            <Show when={showTesting()}>
              <TestingSecretForm />
            </Show>
          </div>
        </Show>
      </div>
      {/* The credits sit on their own rules above and below the print, never on its pixels. */}
      <figure class="welcome-art" aria-hidden="true">
        <div class="print-credits top">
          <span>Extend / 01</span>
          <span>Phones · computers · TVs</span>
        </div>
        <div class="welcome-print">
          <Shader variant="field" seed={1.1} cell={3} />
        </div>
        <div class="print-credits bottom">
          <span>One Silicon at a time.</span>
          <span>Stop it any time.</span>
        </div>
      </figure>
    </section>
  );
}
