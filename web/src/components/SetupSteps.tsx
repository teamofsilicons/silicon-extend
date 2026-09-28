import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Check, CircleAlert, Circle, Hand, LoaderCircle, RotateCw } from "lucide-solid";
import { session } from "../lib/session";
import { ApiError, toApiError } from "../lib/api";
import type { Device, Setup, SetupStep } from "../lib/types";
import { normalizeSetupCode } from "../lib/pairing";
import { SETUP_POLL_MS } from "../config";
import { Button, ErrorNote } from "./ui";

/**
 * Steps the service itself reports for a device a computer carries: a duplicate of a device already
 * added elsewhere, or one waiting for the computer to recognise it. The device can't run them
 * again, so they have no Retry; their text says what to do.
 */
const SERVICE_STEPS = new Set(["duplicate_device", "recognising"]);

/** A retry the Carbon asked for, until the step reports back (or says nothing for a while). */
interface Retrying {
  since: number;
  /** The step left `failed` since the retry was sent: the device picked it up. */
  moved: boolean;
}
/**
 * How long "Retrying…" waits for the device to report the step moving before it gives the button
 * back. A step the device fails again between two reads never shows moving, so this also bounds that.
 */
const RETRY_QUIET_MS = 15_000;
/** The first read after a retry comes sooner than the usual poll, so the step's progress shows at once. */
const RETRY_READ_MS = 400;

/**
 * The device's own setup steps with live status (GET /devices/{id}/setup, polled). For an Apple TV
 * waiting on the Carbon, it also takes the 4-digit code the TV shows. A failed step shows its
 * plain-language error and a Retry button (POST /setup/retry, contract A): the device runs the step
 * again at once and reports as usual, and a refusal (nothing to retry, offline, an app older than
 * 1.1, too soon) shows the service's own message under the step.
 */
export function SetupSteps(props: { device: Device; onComplete?: (setup: Setup) => void }) {
  const s = session();
  const [setup, setSetup] = createSignal<Setup | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [code, setCode] = createSignal("");
  const [codeError, setCodeError] = createSignal<ApiError | null>(null);
  const [sending, setSending] = createSignal(false);
  const [retrying, setRetrying] = createSignal<Record<string, Retrying>>({});
  const [retryErrors, setRetryErrors] = createSignal<Record<string, ApiError>>({});
  const [sendingRetry, setSendingRetry] = createSignal<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let done = false;
  let disposed = false;

  /** Folds a fresh report into the retries in flight: a step that moved and then settled is done retrying. */
  function settleRetries(steps: SetupStep[]) {
    const current = retrying();
    if (!Object.keys(current).length) return;
    const next: Record<string, Retrying> = {};
    for (const [key, r] of Object.entries(current)) {
      const step = steps.find((st) => st.key === key);
      if (!step || step.status === "done") continue;
      if (step.status !== "failed") {
        next[key] = { ...r, moved: true };
        continue;
      }
      // Still failed: either the device hasn't picked it up yet, or it ran and failed again.
      if (!r.moved && Date.now() - r.since < RETRY_QUIET_MS) next[key] = r;
    }
    setRetrying(next);
  }

  async function poll() {
    try {
      const next = await s.client().getSetup(props.device.device_id);
      if (disposed) return;
      settleRetries(next.steps);
      setSetup(next);
      setError(null);
      if (next.state === "complete" && !done) {
        done = true;
        props.onComplete?.(next);
      }
    } catch (e) {
      if (!disposed) setError(toApiError(e));
    }
    if (!done && !disposed) schedule(SETUP_POLL_MS);
  }
  function schedule(ms: number) {
    clearTimeout(timer);
    timer = setTimeout(poll, ms);
  }
  onMount(poll);
  onCleanup(() => {
    disposed = true;
    clearTimeout(timer);
  });

  /**
   * A step with `input: "code"` asks the Carbon for a code on the website (an Apple TV's PIN). Only
   * when the service sends no `input` field at all does the website infer it: a tvOS step waiting on
   * the Carbon.
   */
  const codeStep = () => {
    const steps = setup()?.steps ?? [];
    const declared = steps.find((st) => st.input === "code" && st.status !== "done");
    if (declared) return declared;
    if (steps.some((st) => st.input !== undefined)) return undefined;
    return props.device.os === "tvos" ? steps.find((st) => st.status === "needs_carbon") : undefined;
  };
  const wantsCode = () => !!codeStep();

  const canRetry = (step: SetupStep) => step.status === "failed" && !SERVICE_STEPS.has(step.key);

  async function retry(step: SetupStep) {
    setSendingRetry(step.key);
    setRetryErrors(({ [step.key]: _gone, ...rest }) => rest);
    try {
      const { retrying: keys } = await s.client().retrySetup(props.device.device_id, step.key);
      const since = Date.now();
      setRetrying((current) => {
        const next = { ...current };
        for (const key of keys.length ? keys : [step.key]) next[key] = { since, moved: false };
        return next;
      });
      if (!done && !disposed) schedule(RETRY_READ_MS);
    } catch (e) {
      setRetryErrors((current) => ({ ...current, [step.key]: toApiError(e) }));
    } finally {
      setSendingRetry(null);
    }
  }

  async function sendCode(event: Event) {
    event.preventDefault();
    const { code: value, valid } = normalizeSetupCode(code());
    if (!valid) {
      setCodeError(new ApiError(0, { code: "invalid_input", message: "The Apple TV shows 4 digits; enter all four.", hint: "Look at the TV: the code is on screen during setup." }));
      return;
    }
    setSending(true);
    setCodeError(null);
    try {
      setSetup(await s.client().enterSetupCode(props.device.device_id, value));
      setCode("");
    } catch (e) {
      setCodeError(toApiError(e));
    } finally {
      setSending(false);
    }
  }

  return (
    <div class="setup" data-testid="setup-steps" data-state={setup()?.state ?? "loading"}>
      <ErrorNote error={error()} />
      <Show when={setup()} fallback={<p class="muted">Asking the device how far it got…</p>}>
        {(st) => (
          <>
            <Show when={!st().steps.length && st().state !== "complete"}>
              <p class="waiting" data-testid="setup-waiting">
                <LoaderCircle class="spin" size={16} aria-hidden="true" />
                <span>
                  Waiting for {props.device.name} to connect and report its setup.{" "}
                  {props.device.host_device_id
                    ? "Keep the host computer awake with its Extend app open."
                    : "Keep the Extend app open on the device; it connects on its own once the code is accepted."}
                </span>
              </p>
            </Show>
            <ol class="steps">
              <For each={st().steps}>
                {(step) => (
                  <li class={`step ${step.status}`} data-testid="setup-step" data-status={step.status}>
                    <span class="step-icon" aria-hidden="true">
                      {step.status === "done" ? (
                        <Check size={16} />
                      ) : step.status === "failed" ? (
                        <CircleAlert size={16} />
                      ) : step.status === "needs_carbon" ? (
                        <Hand size={16} />
                      ) : step.status === "in_progress" ? (
                        <LoaderCircle class="spin" size={16} />
                      ) : (
                        <Circle size={16} />
                      )}
                    </span>
                    <div>
                      <p class="step-title">
                        {step.title}
                        <span class="visually-hidden"> — {step.status.replace("_", " ")}</span>
                        <Show when={step.status === "needs_carbon"}>
                          <span class="badge action">Needs you</span>
                        </Show>
                      </p>
                      <Show when={step.help && step.status !== "done"}>
                        <p class="step-help">{step.help}</p>
                      </Show>
                      <Show when={step.error && step.status !== "done"}>
                        {/* Plain text on purpose: the device writes one or two Carbon-facing sentences here. */}
                        <p class="step-error" data-testid="setup-step-error">
                          {step.error}
                        </p>
                      </Show>
                      <Show when={canRetry(step) || retrying()[step.key]}>
                        <div class="step-retry">
                          <Show
                            when={!retrying()[step.key]}
                            fallback={
                              <p class="step-retrying" role="status" data-testid="setup-retrying">
                                <LoaderCircle class="spin" size={14} aria-hidden="true" /> Retrying on {props.device.name}…
                              </p>
                            }
                          >
                            <Button small busy={sendingRetry() === step.key} onClick={() => retry(step)} data-testid="setup-retry">
                              <RotateCw size={14} aria-hidden="true" /> Retry
                            </Button>
                          </Show>
                        </div>
                      </Show>
                      <ErrorNote error={retryErrors()[step.key]} compact testid="setup-retry-error" />
                    </div>
                  </li>
                )}
              </For>
            </ol>
            <Show when={st().state === "complete"}>
              <p class="success-line" data-testid="setup-complete">
                <Check size={16} aria-hidden="true" /> Setup is finished. {props.device.name} is ready for Silicons.
              </p>
            </Show>
          </>
        )}
      </Show>
      <Show when={wantsCode()}>
        <form class="setup-code" onSubmit={sendCode}>
          <label for="setup-code">{codeStep()?.title ?? "Code on the Apple TV"}</label>
          <div class="input-row">
            <input
              id="setup-code"
              inputmode="numeric"
              autocomplete="one-time-code"
              maxLength={4}
              placeholder="0000"
              value={code()}
              data-testid="setup-code-input"
              onInput={(e) => setCode(normalizeSetupCode(e.currentTarget.value).code)}
            />
            <Button type="submit" busy={sending()} data-testid="setup-code-submit">
              Send code
            </Button>
          </div>
          <ErrorNote error={codeError()} compact />
        </form>
      </Show>
    </div>
  );
}
