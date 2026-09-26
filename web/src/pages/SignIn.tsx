import { createSignal, Show } from "solid-js";
import { ArrowRight, FlaskConical, KeyRound } from "lucide-solid";
import { session } from "../lib/session";
import { ApiError, toApiError } from "../lib/api";
import { beginIamLogin } from "../lib/auth";
import { Button, ErrorNote } from "../components/ui";
import { ExtendMark } from "../components/ExtendMark";
import { TestingSecretForm } from "../components/TestingSecretForm";
import { navigate } from "../lib/router";
import { write } from "../lib/storage";

export default function SignIn(props: { reason?: ApiError | null; next?: string; onSignedIn?: () => void }) {
  const s = session();
  const [slt, setSlt] = createSignal("");
  const [busy, setBusy] = createSignal<"iam" | "slt" | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [showTesting, setShowTesting] = createSignal(false);
  const testing = () => s.world().kind === "testing";
  const worldKey = () => {
    const w = s.world();
    return w.kind === "production" ? "production" : w.environment.environment_id;
  };

  async function withIam() {
    setBusy("iam");
    setError(null);
    try {
      const info = await s.client().iam();
      write("session", "extend.next", props.next ?? "/devices");
      const url = beginIamLogin(info, location.origin, worldKey());
      location.assign(url);
    } catch (e) {
      setError(toApiError(e));
      setBusy(null);
    }
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
    <section class="page sign-in" data-testid="sign-in">
      <div class="sign-in-card">
        <ExtendMark size={40} />
        <h1 class="page-title">
          Let your Silicons use <em>your devices</em>
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

        <Button variant="primary" class="wide" onClick={withIam} busy={busy() === "iam"} data-testid="sign-in-iam">
          Continue with Silicon IAM <ArrowRight size={16} aria-hidden="true" />
        </Button>
        <p class="fine">IAM asks you to approve Extend and pick your teams, then sends you back here. Extend never sees your password.</p>

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
    </section>
  );
}
