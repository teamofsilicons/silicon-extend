import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Check, CircleAlert, Circle, Hand, LoaderCircle } from "lucide-solid";
import { session } from "../lib/session";
import { ApiError, toApiError } from "../lib/api";
import type { Device, Setup } from "../lib/types";
import { normalizeSetupCode } from "../lib/pairing";
import { SETUP_POLL_MS } from "../config";
import { Button, ErrorNote } from "./ui";

/**
 * The device's own setup steps with live status (GET /devices/{id}/setup, polled). For an Apple TV
 * waiting on the Carbon, it also takes the 4-digit code the TV shows.
 */
export function SetupSteps(props: { device: Device; onComplete?: (setup: Setup) => void }) {
  const s = session();
  const [setup, setSetup] = createSignal<Setup | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [code, setCode] = createSignal("");
  const [codeError, setCodeError] = createSignal<ApiError | null>(null);
  const [sending, setSending] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let done = false;
  let disposed = false;

  async function poll() {
    try {
      const next = await s.client().getSetup(props.device.device_id);
      if (disposed) return;
      setSetup(next);
      setError(null);
      if (next.state === "complete" && !done) {
        done = true;
        props.onComplete?.(next);
      }
    } catch (e) {
      if (!disposed) setError(toApiError(e));
    }
    if (!done && !disposed) timer = setTimeout(poll, SETUP_POLL_MS);
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
                      <Show when={step.error}>
                        <p class="step-error">{step.error}</p>
                      </Show>
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
