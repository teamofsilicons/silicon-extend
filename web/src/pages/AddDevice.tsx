import { createMemo, createResource, createSignal, For, Match, Show, Switch } from "solid-js";
import { ArrowLeft, ArrowRight, Check, Download, Laptop, Monitor, Smartphone, Tablet, Tv } from "lucide-solid";
import { session } from "../lib/session";
import { toApiError, type ApiError } from "../lib/api";
import type { AttachOs, Device } from "../lib/types";
import { canAdvance, initialState, nameProblem, reduce, STEP_TITLE, stepsFor, type WizardEvent } from "../lib/wizard";
import { displayPairingCode, formatCodeInput, normalizePairingCode } from "../lib/pairing";
import { DEVICE_KINDS, DOWNLOADS, deviceKind, OS_LABEL, type DeviceKind, type DeviceKindId } from "../config";
import { Link, navigate, query } from "../lib/router";
import { Button, CopyText, ErrorNote, OnlineDot, Spinner } from "../components/ui";
import { TtlSlider } from "../components/TtlSlider";
import { SetupSteps } from "../components/SetupSteps";
import { AccessPicker } from "../components/AccessPicker";
import { relativeTime } from "../lib/format";

function KindIcon(props: { kind: DeviceKind; size?: number }) {
  const size = () => props.size ?? 22;
  return (
    <Switch fallback={<Monitor size={size()} />}>
      <Match when={props.kind.icon === "phone"}>
        <Smartphone size={size()} />
      </Match>
      <Match when={props.kind.icon === "tablet"}>
        <Tablet size={size()} />
      </Match>
      <Match when={props.kind.icon === "tv"}>
        <Tv size={size()} />
      </Match>
      <Match when={props.kind.icon === "laptop"}>
        <Laptop size={size()} />
      </Match>
    </Switch>
  );
}

export default function AddDevice() {
  const s = session();
  const preset = deviceKind(query().get("kind") ?? undefined)?.id ?? null;
  const [state, setState] = createSignal(initialState(preset));
  const dispatch = (event: WizardEvent) => setState((current) => reduce(current, event));
  const [lastError, setLastError] = createSignal<ApiError | null>(null);
  const kind = createMemo(() => deviceKind(state().kind ?? undefined));
  const order = createMemo(() => stepsFor(kind()).filter((step) => step !== "kind" || !state().device));

  const defaultName = () => {
    const k = kind();
    const owner = s.member()?.display_name;
    return k ? (owner ? `${owner}'s ${k.short}` : k.short) : "";
  };

  async function create() {
    const st = state();
    const k = kind();
    if (!k) return;
    dispatch({ type: "submit" });
    if (!state().submitting) return;
    const started = performance.now();
    try {
      let device: Device;
      if (k.via === "app") {
        device = (
          await s.client().claimPairing({
            pairing_code: normalizePairingCode(st.code).code,
            name: st.name.trim(),
            visibility: st.visibility,
            pair_ttl_days: st.ttlDays,
          })
        ).device;
      } else {
        device = await s.client().attachDevice(st.hostId!, {
          os: k.os as AttachOs,
          name: st.name.trim(),
          visibility: st.visibility,
          pair_ttl_days: st.ttlDays,
        });
      }
      dispatch({ type: "created", device });
      void s.client().telemetry({ event: "pairing", step: k.via === "app" ? "web.pairing.claim" : "web.pairing.attach", success: true, duration_ms: performance.now() - started, device_os: k.os });
    } catch (e) {
      const error = toApiError(e);
      setLastError(error);
      dispatch({ type: "failed", error: { code: error.code, message: error.message, hint: error.hint } });
      void s.client().telemetry({ event: "pairing", step: k.via === "app" ? "web.pairing.claim" : "web.pairing.attach", success: false, duration_ms: performance.now() - started, error_code: error.code, request_id: error.requestId, device_os: k.os });
    }
  }
  /** The wizard keeps the error's text; the full ApiError (hint, request id) is shown from here. */
  const shownError = () => (state().error ? lastError() : null);

  return (
    <section class="page wizard" data-testid="add-device" data-step={state().step}>
      <div class="page-head">
        <div>
          <p class="eyebrow">Add a device</p>
          <h1 class="page-title">{kind() ? `Pair ${articleFor(kind()!)}` : "What are you pairing?"}</h1>
        </div>
        <Show when={s.world().kind === "testing"}>
          <span class="badge testing">Pairs into the test environment</span>
        </Show>
      </div>

      <Show when={kind()}>
        <ol class="rail" aria-label="Steps">
          <For each={order().filter((step) => step !== "kind")}>
            {(step, i) => {
              const position = () => order().indexOf(step);
              const current = () => order().indexOf(state().step);
              return (
                <li class={position() < current() ? "done" : position() === current() ? "current" : ""} aria-current={position() === current() ? "step" : undefined}>
                  <span class="rail-dot">{position() < current() ? <Check size={12} /> : i() + 1}</span>
                  <span class="rail-label">{STEP_TITLE[step]}</span>
                </li>
              );
            }}
          </For>
        </ol>
      </Show>

      <div class="wizard-body">
        <Switch>
          {/* 1. Kind */}
          <Match when={state().step === "kind"}>
            <div class="kind-grid" role="list">
              <For each={DEVICE_KINDS}>
                {(k) => (
                  <button role="listitem" class="kind-card" data-testid={`kind-${k.id}`} onClick={() => dispatch({ type: "choose_kind", kind: k.id })}>
                    <KindIcon kind={k} />
                    <span class="kind-label">{k.label}</span>
                    <span class="kind-via">{k.via === "app" ? "Bridge app on the device" : k.host === "mac" ? "Through your paired Mac" : "Through a paired computer"}</span>
                  </button>
                )}
              </For>
            </div>
          </Match>

          {/* 2. Guide */}
          <Match when={state().step === "guide" && kind()}>
            {(k) => (
              <div class="card guide" data-testid="guide">
                <Show when={k().download}>
                  {(platform) => (
                    <div class="download">
                      <div>
                        <p class="download-app">{DOWNLOADS[platform()].app}</p>
                        <p class="fine">{DOWNLOADS[platform()].note}</p>
                      </div>
                      <a class="button primary" href={DOWNLOADS[platform()].href} target="_blank" rel="noopener" data-testid="download-link">
                        <Download size={16} aria-hidden="true" /> Download
                      </a>
                    </div>
                  )}
                </Show>
                <ol class="numbered">
                  <For each={k().guide}>{(line) => <li>{line}</li>}</For>
                </ol>
                <details class="preview">
                  <summary>What happens on the device after that</summary>
                  <ol class="numbered small">
                    <For each={k().setup}>{(line) => <li>{line}</li>}</For>
                  </ol>
                  <p class="fine">
                    <strong>A Silicon can:</strong> {k().canDo}
                  </p>
                  <Show when={k().goodToKnow}>
                    <p class="fine">
                      <strong>Good to know:</strong> {k().goodToKnow}
                    </p>
                  </Show>
                </details>
                <Nav onBack={() => dispatch({ type: "reset" })} backLabel="Other device" onNext={() => dispatch({ type: "next" })} nextLabel={k().via === "app" ? "I have the code" : k().host === "mac" ? "Pick the Mac" : "Pick the computer"} />
              </div>
            )}
          </Match>

          {/* 3a. Pairing code */}
          <Match when={state().step === "code"}>
            <CodeStep
              code={state().code}
              onInput={(code) => dispatch({ type: "set_code", code })}
              onBack={() => dispatch({ type: "back" })}
              onNext={() => {
                dispatch({ type: "next" });
                if (!state().name) dispatch({ type: "set_name", name: defaultName() });
              }}
              error={state().step === "code" ? shownError() : null}
            />
          </Match>

          {/* 3b. Host computer */}
          <Match when={state().step === "host" && kind()}>
            {(k) => (
              <HostStep
                kind={k()}
                hostId={state().hostId}
                onPick={(hostId) => dispatch({ type: "set_host", hostId })}
                onBack={() => dispatch({ type: "back" })}
                onNext={() => {
                  dispatch({ type: "next" });
                  if (!state().name) dispatch({ type: "set_name", name: defaultName() });
                }}
                onAddHost={() => dispatch({ type: "choose_kind", kind: "mac" })}
                error={shownError()}
                canNext={canAdvance(state())}
              />
            )}
          </Match>

          {/* 4. Name */}
          <Match when={state().step === "name" && kind()}>
            {(k) => (
              <form
                class="card"
                data-testid="name-step"
                onSubmit={(e) => {
                  e.preventDefault();
                  void create();
                }}
              >
                <Show when={k().via === "app"}>
                  <p class="fine">
                    Pairing code <strong class="mono">{displayPairingCode(state().code)}</strong>
                  </p>
                </Show>
                <label for="device-name">Name</label>
                <input
                  id="device-name"
                  maxLength={64}
                  value={state().name}
                  data-testid="device-name-input"
                  onInput={(e) => dispatch({ type: "set_name", name: e.currentTarget.value })}
                />
                <Show when={state().name && nameProblem(state().name)}>
                  <p class="field-problem">{nameProblem(state().name)}</p>
                </Show>
                <fieldset class="radio-group">
                  <legend>Who can see it exists</legend>
                  <label>
                    <input type="radio" name="visibility" checked={state().visibility === "team"} onChange={() => dispatch({ type: "set_visibility", visibility: "team" })} />
                    <span>
                      <strong>Team</strong> — other Carbons in {s.team()} see its name, kind and whether it's online. Only you manage it.
                    </span>
                  </label>
                  <label>
                    <input type="radio" name="visibility" checked={state().visibility === "personal"} onChange={() => dispatch({ type: "set_visibility", visibility: "personal" })} />
                    <span>
                      <strong>Personal</strong> — only you see it.
                    </span>
                  </label>
                </fieldset>
                <TtlSlider value={state().ttlDays} onInput={(days) => dispatch({ type: "set_ttl", days })} />
                <ErrorNote error={shownError()} testid="pairing-error" />
                <Nav
                  onBack={() => dispatch({ type: "back" })}
                  submit
                  nextLabel={k().via === "app" ? "Pair device" : "Add device"}
                  busy={state().submitting}
                  disabled={!canAdvance(state())}
                  nextTestId="pair-submit"
                />
              </form>
            )}
          </Match>

          {/* 5. Device setup */}
          <Match when={state().step === "setup" && state().device}>
            {(device) => (
              <div class="card" data-testid="setup-step-card">
                <p class="success-line">
                  <Check size={16} aria-hidden="true" /> <strong>{device().name}</strong> is paired{s.world().kind === "testing" ? " in the test environment" : ""}. Now finish its setup
                  {kind()?.via === "host" ? " — the host computer's Bridge app walks you through it." : " on the device."}
                </p>
                <SetupSteps device={device()} onComplete={() => dispatch({ type: "setup_complete" })} />
                <div class="wizard-nav">
                  <span />
                  <Button variant={state().setupComplete ? "primary" : "secondary"} onClick={() => dispatch({ type: "next" })} data-testid="setup-next">
                    {state().setupComplete ? "Choose Silicons" : "Continue, finish setup later"} <ArrowRight size={16} aria-hidden="true" />
                  </Button>
                </div>
              </div>
            )}
          </Match>

          {/* 6. Silicons */}
          <Match when={state().step === "access" && state().device}>
            {(device) => (
              <div class="card" data-testid="access-step">
                <h2 class="card-title">Which Silicons can use {device().name}?</h2>
                <p class="fine">You can change this any time on the device's page. Only one Silicon uses the device at a time; the others can ask it for a turn.</p>
                <AccessPicker deviceId={device().device_id} onGranted={(ids) => dispatch({ type: "access_done", granted: [...state().granted, ...ids] })} />
                <div class="wizard-nav">
                  <span />
                  <Button variant="ghost" onClick={() => dispatch({ type: "skip_access" })} data-testid="skip-access">
                    Skip for now
                  </Button>
                </div>
              </div>
            )}
          </Match>

          {/* 7. Done */}
          <Match when={state().step === "done" && state().device}>
            {(device) => (
              <div class="card done" data-testid="wizard-done">
                <h2 class="card-title">
                  <Check size={18} aria-hidden="true" /> {device().name} is paired
                </h2>
                <Show when={state().granted.length} fallback={<p>No Silicon can use it yet. Give access from its page when you're ready.</p>}>
                  <p>
                    {state().granted.join(", ")} can use it now. Tell {state().granted.length === 1 ? "it" : "them"} the device id, or ask in plain words:
                  </p>
                  <blockquote class="ask">“Use my {kind()?.short ?? "device"} ({device().device_id}) through Bridge to …”</blockquote>
                  <p class="fine">A Silicon finds it with:</p>
                  <CopyText text={`bridge device show ${device().device_id}`} />
                </Show>
                <div class="wizard-nav">
                  <Button onClick={() => setState(initialState())}>Add another device</Button>
                  <Link href={`/devices/${device().device_id}`} class="button primary" data-testid="open-device">
                    Open device page <ArrowRight size={16} aria-hidden="true" />
                  </Link>
                </div>
              </div>
            )}
          </Match>
        </Switch>
      </div>
    </section>
  );
}

function articleFor(kind: DeviceKind): string {
  if (kind.id === "android") return "an Android phone or tablet";
  if (kind.id === "android_tv") return "an Android TV, Google TV or Fire TV";
  if (/^[AEIOU]/.test(kind.label) || kind.id === "lg_tv") return `an ${kind.label}`;
  return `a ${kind.label}`;
}

function Nav(props: {
  onBack?: () => void;
  backLabel?: string;
  onNext?: () => void;
  nextLabel: string;
  submit?: boolean;
  busy?: boolean;
  disabled?: boolean;
  nextTestId?: string;
}) {
  return (
    <div class="wizard-nav">
      <Show when={props.onBack} fallback={<span />}>
        <Button variant="ghost" onClick={props.onBack} data-testid="wizard-back">
          <ArrowLeft size={16} aria-hidden="true" /> {props.backLabel ?? "Back"}
        </Button>
      </Show>
      <Button
        variant="primary"
        type={props.submit ? "submit" : "button"}
        onClick={props.submit ? undefined : props.onNext}
        busy={props.busy}
        disabled={props.disabled}
        data-testid={props.nextTestId ?? "wizard-next"}
      >
        {props.nextLabel} <ArrowRight size={16} aria-hidden="true" />
      </Button>
    </div>
  );
}

function CodeStep(props: { code: string; onInput: (code: string) => void; onBack: () => void; onNext: () => void; error: ReturnType<typeof toApiError> | null }) {
  const normalized = () => normalizePairingCode(props.code);
  return (
    <form
      class="card code-step"
      data-testid="code-step"
      onSubmit={(e) => {
        e.preventDefault();
        if (normalized().valid) props.onNext();
      }}
    >
      <label for="pairing-code">Pairing code shown in the Bridge app</label>
      <input
        id="pairing-code"
        class="code-input"
        autocomplete="off"
        autocapitalize="characters"
        spellcheck={false}
        inputmode="text"
        placeholder="4F9 C2A"
        value={props.code}
        data-testid="pairing-code-input"
        aria-describedby="code-help"
        onInput={(e) => props.onInput(formatCodeInput(e.currentTarget.value))}
        ref={(el) => setTimeout(() => el.focus())}
      />
      <p id="code-help" class={normalized().problem ? "field-problem" : "fine"} data-testid="code-help">
        {normalized().valid ? (
          <>
            Looks right: <strong class="mono">{displayPairingCode(normalized().code)}</strong>
          </>
        ) : (
          (normalized().problem ?? "6 characters, 0–9 and A–F. Spaces, dashes and lowercase are fine.")
        )}
      </p>
      <p class="fine">The code changes every 5 minutes and works once. If it just changed, use the new one.</p>
      <ErrorNote error={props.error} testid="code-error" />
      <Nav onBack={props.onBack} submit nextLabel="Next" disabled={!normalized().valid} />
    </form>
  );
}

function HostStep(props: {
  kind: DeviceKind;
  hostId: string | null;
  onPick: (id: string) => void;
  onBack: () => void;
  onNext: () => void;
  onAddHost: () => void;
  error: ReturnType<typeof toApiError> | null;
  canNext: boolean;
}) {
  const s = session();
  const [hosts] = createResource(
    () => s.team(),
    async () => {
      const page = await s.client().listDevices({ scope: "mine", limit: 100 });
      return page.items.filter(
        (d) => !d.host_device_id && (props.kind.host === "mac" ? d.os === "macos" : d.os === "macos" || d.os === "windows" || d.os === "linux"),
      );
    },
  );
  const what = () => (props.kind.host === "mac" ? "Mac" : "computer");
  return (
    <div class="card" data-testid="host-step">
      <p>
        {props.kind.label} {props.kind.host === "mac" ? "pairs through a Mac you already paired" : "pairs through a Mac, Windows or Linux computer you already paired"}. It must be online
        {props.kind.os === "ios" || props.kind.os === "ipados" ? " and, for the first setup, within reach of a cable" : " and on the same network as the TV"}.
      </p>
      <Show when={!hosts.error} fallback={<ErrorNote error={toApiError(hosts.error)} />}>
        <Show when={hosts()} fallback={<Spinner label={`Finding your paired ${what()}s…`} />}>
          {(list) => (
            <Show
              when={list().length}
              fallback={
                <div class="notice">
                  <p>You haven't paired a {what()} yet. Pair one first, then come back to add the {props.kind.short}.</p>
                  <Button onClick={props.onAddHost}>Pair a Mac</Button>
                </div>
              }
            >
              <div class="host-list" role="radiogroup" aria-label={`Paired ${what()}s`}>
                <For each={list()}>
                  {(d) => {
                    const blocked = () => (!d.online ? `Offline, last seen ${relativeTime(d.last_seen_at)}. Open the Bridge app on it.` : d.state !== "ready" ? "Its own setup isn't finished." : null);
                    return (
                      <label class={`host-option ${props.hostId === d.device_id ? "selected" : ""} ${blocked() ? "blocked" : ""}`} data-testid="host-option">
                        <input type="radio" name="host" value={d.device_id} checked={props.hostId === d.device_id} disabled={!!blocked()} onChange={() => props.onPick(d.device_id)} />
                        <span class="host-main">
                          <strong>{d.name}</strong>
                          <span class="fine">
                            {OS_LABEL[d.os]} · {d.device_id}
                          </span>
                          <Show when={blocked()}>
                            <span class="fine warn-text">{blocked()}</span>
                          </Show>
                        </span>
                        <OnlineDot online={d.online} />
                      </label>
                    );
                  }}
                </For>
              </div>
            </Show>
          )}
        </Show>
      </Show>
      <ErrorNote error={props.error} />
      <Nav onBack={props.onBack} onNext={props.onNext} nextLabel="Next" disabled={!props.canNext} />
    </div>
  );
}

export type { DeviceKindId };
