import { createEffect, createMemo, createResource, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { ArrowLeft, ArrowRight, Check, ChevronRight, Download, Laptop, Monitor, Smartphone, Tablet, Tv, Users } from "lucide-solid";
import { session } from "../lib/session";
import { ifMatchValue, toApiError, type ApiError } from "../lib/api";
import type { AttachOs, Device } from "../lib/types";
import { bannerChanged, canAdvance, initialState, nameProblem, reduce, STEP_TITLE, stepsFor, type Step, type WizardEvent } from "../lib/wizard";
import { displayPairingCode, formatCodeInput, normalizePairingCode } from "../lib/pairing";
import { DEVICE_KINDS, DOWNLOADS, deviceKind, MULTI_CARBON_APP_VERSION, OS_LABEL, type DeviceKind, type DeviceKindId } from "../config";
import { Link, navigate, query } from "../lib/router";
import { Button, CopyText, ErrorNote, OnlineDot, Spinner } from "../components/ui";
import Shader from "../components/Shader";
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
  const client = s.client();
  const [shared, setShared] = createSignal(true);
  const preset = deviceKind(query().get("kind") ?? undefined)?.id ?? null;
  const [state, setState] = createSignal(initialState(preset));
  const dispatch = (event: WizardEvent) => setState((current) => reduce(current, event));
  const [lastError, setLastError] = createSignal<ApiError | null>(null);
  /** The Team the last Silicons were given access in, for the command the done step suggests. */
  const [grantTeam, setGrantTeam] = createSignal<string | null>(null);
  const kind = createMemo(() => deviceKind(state().kind ?? undefined));
  const order = createMemo(() => stepsFor(kind()).filter((step) => step !== "kind" || !state().device));
  /** The steps the rail shows (everything after choosing the kind), and where the Carbon is in them. */
  const railSteps = createMemo<Step[]>(() => order().filter((step) => step !== "kind"));
  const railIndex = () => railSteps().indexOf(state().step);

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
          await client.claimPairing({
            pairing_code: normalizePairingCode(st.code).code,
            name: st.name.trim(),
            pair_ttl_days: st.ttlDays,
            visibility: shared() ? "team" : "personal",
          })
        ).device;
      } else {
        device = await client.attachDevice(st.hostId!, {
          os: k.os as AttachOs,
          name: st.name.trim(),
          pair_ttl_days: st.ttlDays,
          visibility: shared() ? "team" : "personal",
        });
      }
      dispatch({ type: "created", device });
      void client.telemetry({ event: "pairing", step: k.via === "app" ? "web.pairing.claim" : "web.pairing.attach", success: true, duration_ms: performance.now() - started, device_os: k.os });
    } catch (e) {
      const error = toApiError(e);
      setLastError(error);
      dispatch({ type: "failed", error: { code: error.code, message: error.message, hint: error.hint } });
      void client.telemetry({ event: "pairing", step: k.via === "app" ? "web.pairing.claim" : "web.pairing.attach", success: false, duration_ms: performance.now() - started, error_code: error.code, request_id: error.requestId, device_os: k.os });
    }
  }
  /**
   * The banner step: saves the choice on the device just created, when it differs from the device's
   * own value (a new device shows it). If-Match comes from the claim's version; if the device changed
   * since (its setup reports move it on), it is read again and the change sent once more.
   */
  async function saveBanner() {
    const device = state().device;
    if (!device) return;
    if (!bannerChanged(state())) return dispatch({ type: "next" });
    dispatch({ type: "save_banner" });
    if (!state().savingBanner) return;
    const change = { in_use_indicator: state().banner ? "shown" : "hidden" } as const;
    const started = performance.now();
    const send = async (ifMatch: string) => (await client.updateDevice(device.device_id, change, ifMatch)).device;
    try {
      let updated;
      try {
        updated = await send(ifMatchValue(null, device.version));
      } catch (e) {
        const error = toApiError(e);
        if (error.status !== 412 && error.code !== "version_unknown") throw e;
        const fresh = await client.getDevice(device.device_id);
        updated = await send(ifMatchValue(fresh.etag, fresh.device.version));
      }
      dispatch({ type: "banner_saved", device: updated });
      void client.telemetry({ event: "device_update", step: "web.pairing.in_use_indicator", success: true, duration_ms: performance.now() - started, device_os: device.os });
    } catch (e) {
      const error = toApiError(e);
      setLastError(error);
      dispatch({ type: "failed", error: { code: error.code, message: error.message, hint: error.hint } });
      void client.telemetry({ event: "device_update", step: "web.pairing.in_use_indicator", success: false, duration_ms: performance.now() - started, error_code: error.code, request_id: error.requestId, device_os: device.os });
    }
  }

  /** The wizard keeps the error's text; the full ApiError (hint, request id) is shown from here. */
  const shownError = () => (state().error ? lastError() : null);

  return (
    <section class="page-main wizard" data-testid="add-device" data-step={state().step}>
      <header class="page-heading">
        <div>
          <p class="eyebrow">Add a device</p>
          <h1 class="page-title">{kind() ? `Pair ${articleFor(kind()!)}.` : "What are you pairing?"}</h1>
          <Show when={!kind()}>
            <p class="lead">Pick the kind of device. Phones, computers and some TVs run the Extend app; the rest pair through a computer you already paired.</p>
          </Show>
        </div>
        <Show when={s.world().kind === "testing"}>
          <span class="badge testing">Pairs into the test environment</span>
        </Show>
      </header>

      <Show when={kind()}>
        <StepRail steps={railSteps()} current={railIndex()} />
        <div class="step-progress" data-testid="step-progress">
          <p class="step-progress-label">
            Step <strong>{railIndex() + 1}</strong> of {railSteps().length} · {STEP_TITLE[state().step]}.
          </p>
          <span class="step-progress-bar" aria-hidden="true">
            <For each={railSteps()}>{(_, i) => <span class={i() < railIndex() ? "done" : i() === railIndex() ? "current" : ""} />}</For>
          </span>
        </div>
      </Show>

      <div class="wizard-body">
        <Switch>
          {/* 1. Kind */}
          <Match when={state().step === "kind"}>
            <div class="kind-groups">
              <For each={KIND_GROUPS}>
                {(group) => (
                  <section class="kind-group" aria-labelledby={`kind-group-${group.via}`}>
                    <header class="kind-group-head">
                      <h2 class="eyebrow" id={`kind-group-${group.via}`}>
                        {group.title}
                      </h2>
                      <p class="fine">{group.note}</p>
                    </header>
                    <ul class="kind-list">
                      <For each={DEVICE_KINDS.filter((k) => k.via === group.via)}>
                        {(k) => (
                          <li>
                            <button class="kind-option" data-testid={`kind-${k.id}`} onClick={() => dispatch({ type: "choose_kind", kind: k.id })}>
                              <span class="kind-icon" aria-hidden="true">
                                <KindIcon kind={k} size={19} />
                              </span>
                              <span class="kind-copy">
                                <span class="kind-label">{k.label}</span>
                                <Show when={k.via === "host"}>
                                  <span class="kind-via">{k.host === "mac" ? "Needs a paired Mac" : "Any paired computer"}</span>
                                </Show>
                              </span>
                              <ChevronRight size={16} class="kind-go" aria-hidden="true" />
                            </button>
                          </li>
                        )}
                      </For>
                    </ul>
                  </section>
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
                      {/* Secondary: the step's one primary action is going on to the code. */}
                      <a class="button secondary" href={DOWNLOADS[platform()].href} target="_blank" rel="noopener" data-testid="download-link">
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
                  <p class="code-stub">
                    <span>Pairing code</span>
                    <strong class="mono">{displayPairingCode(state().code)}</strong>
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
                <label class="checkbox-row"><input type="checkbox" checked={shared()} onChange={e=>setShared(e.currentTarget.checked)}/>Visible to members of {s.team()}</label>
                <p class="fine">Off hides this device from other Carbons and Silicons without access. Silicons you explicitly grant access in this organization can still see and use it.</p>
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

          {/* 5. Banner while in use */}
          <Match when={state().step === "banner" && state().device}>
            {(device) => (
              <form
                class="card"
                data-testid="banner-step"
                onSubmit={(e) => {
                  e.preventDefault();
                  void saveBanner();
                }}
              >
                <p class="eyebrow">While a Silicon uses it</p>
                <p class="success-line">
                  <Check size={16} aria-hidden="true" /> <span><strong>{device().name}</strong> is paired{s.world().kind === "testing" ? " in the test environment" : ""}.</span>
                </p>
                <label class="switch">
                  <input
                    type="checkbox"
                    checked={state().banner}
                    disabled={state().savingBanner}
                    onChange={(e) => dispatch({ type: "set_banner", shown: e.currentTarget.checked })}
                    data-testid="banner-toggle"
                  />
                  <span>Show a banner while a Silicon is using this device</span>
                </label>
                <Show
                  when={kind()?.via !== "host"}
                  fallback={
                    <p class="fine" data-testid="banner-explain">
                      {kind()?.inUse} {kind()?.inUseStays ?? ""} The computer it pairs through and this website show which Silicon is using it, with Stop.
                    </p>
                  }
                >
                  <p class="fine" data-testid="banner-explain">
                    When a Silicon starts using it, the device shows the Silicon's name for 10 seconds. Turned off, the device shows nothing while a Silicon uses it; the Extend app and
                    this website still show who is using it, with Stop.
                  </p>
                </Show>
                <Show when={device().paired_by_others && !state().banner && !bannerChanged(state())}>
                  <p class="fine" data-testid="banner-shared">It's already off on this device: it's one setting for the whole device, shared with the other Carbons who paired it.</p>
                </Show>
                <p class="fine">You can change this later on the device's page.</p>
                <ErrorNote error={shownError()} testid="banner-error" />
                <div class="wizard-nav">
                  <Show when={state().error} fallback={<span />}>
                    <Button variant="ghost" onClick={() => dispatch({ type: "next" })} data-testid="banner-skip">
                      Continue without saving
                    </Button>
                  </Show>
                  <Button variant="primary" type="submit" busy={state().savingBanner} data-testid="banner-next">
                    Continue <ArrowRight size={16} aria-hidden="true" />
                  </Button>
                </div>
              </form>
            )}
          </Match>

          {/* 6. Device setup */}
          <Match when={state().step === "setup" && state().device}>
            {(device) => (
              <div class="card" data-testid="setup-step-card">
                <p class="eyebrow">Setup · on the device</p>
                <p class="success-line">
                  <Check size={16} aria-hidden="true" /> <span><strong>{device().name}</strong> is paired{s.world().kind === "testing" ? " in the test environment" : ""}. Now finish its setup
                  {kind()?.via === "host" ? " — the host computer's Extend app walks you through it." : " on the device."}</span>
                </p>
                <SharedDeviceNote device={device()} />
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

          {/* 7. Silicons */}
          <Match when={state().step === "access" && state().device}>
            {(device) => (
              <div class="card" data-testid="access-step">
                <p class="eyebrow">Access</p>
                <h2 class="card-title">Which Silicons can use {device().name}?</h2>
                <p class="fine">
                  Choose Silicons in {s.team()}. Only one Silicon uses the device at a time. Granted Silicons can see and use this device even when it is hidden from other organization members.
                </p>
                <AccessPicker
                  deviceId={device().device_id}
                  onGranted={(ids, team) => {
                    setGrantTeam(team);
                    dispatch({ type: "access_done", granted: [...state().granted, ...ids] });
                  }}
                />
                <div class="wizard-nav">
                  <span />
                  <Button variant="ghost" onClick={() => dispatch({ type: "skip_access" })} data-testid="skip-access">
                    Skip for now
                  </Button>
                </div>
              </div>
            )}
          </Match>

          {/* 8. Done */}
          <Match when={state().step === "done" && state().device}>
            {(device) => (
              <div class="card done" data-testid="wizard-done">
                <h2 class="card-title">
                  <Check size={18} aria-hidden="true" /> {device().name} is paired.
                </h2>
                <Show when={state().granted.length} fallback={<p>No Silicon can use it yet. Give access from its page when you're ready.</p>}>
                  <p>
                    {state().granted.join(", ")} can use it now. Tell {state().granted.length === 1 ? "it" : "them"} the device id, or ask in plain words:
                  </p>
                  <blockquote class="ask">“Use my {kind()?.short ?? "device"} ({device().device_id}) through Extend to …”</blockquote>
                  <p class="fine">A Silicon finds it with:</p>
                  <CopyText text={`extend ${grantTeam() ? `--team ${grantTeam()} ` : ""}device show ${device().device_id}`} />
                </Show>
                <div class="wizard-nav">
                  <Button onClick={() => (setGrantTeam(null), setShared(true), setState(initialState()))}>Add another device</Button>
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

const KIND_GROUPS: { via: DeviceKind["via"]; title: string; note: string }[] = [
  { via: "app", title: "Runs the Extend app", note: "Install the app on it; it shows a pairing code." },
  { via: "host", title: "Pairs through a computer", note: "Pair the Mac or computer first." },
];

/**
 * The steps as a strip of chips. When the strip is wider than the page it keeps the current step
 * in view (scrolling only itself, never the page) and fades the edge that has more.
 */
function StepRail(props: { steps: Step[]; current: number }) {
  let rail!: HTMLOListElement;
  const [edges, setEdges] = createSignal({ left: false, right: false });
  const measure = () => setEdges({ left: rail.scrollLeft > 2, right: rail.scrollLeft + rail.clientWidth < rail.scrollWidth - 2 });
  const centre = () => {
    const item = rail.children[props.current] as HTMLElement | undefined;
    if (!item || rail.scrollWidth <= rail.clientWidth) return measure();
    const still = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
    rail.scrollTo({ left: item.offsetLeft - (rail.clientWidth - item.offsetWidth) / 2, behavior: still ? "auto" : "smooth" });
    measure();
  };
  createEffect(on(() => props.current, () => requestAnimationFrame(centre)));
  onMount(() => {
    const resize = new ResizeObserver(centre);
    resize.observe(rail);
    onCleanup(() => resize.disconnect());
  });
  return (
    <ol ref={rail} class={`step-rail ${edges().left ? "more-left" : ""} ${edges().right ? "more-right" : ""}`} aria-label="Steps" onScroll={measure}>
      <For each={props.steps}>
        {(step, i) => (
          <li class={i() < props.current ? "done" : i() === props.current ? "current" : ""} aria-current={i() === props.current ? "step" : undefined}>
            <span class="step-rail-dot">{i() < props.current ? <Check size={12} aria-hidden="true" /> : String(i() + 1).padStart(2, "0")}</span>
            <span class="step-rail-label">{STEP_TITLE[step]}</span>
          </li>
        )}
      </For>
    </ol>
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

/**
 * The pairing code as a printed ticket: a dithered colour block with poster credits, a tear line,
 * then the code itself in poster-scale mono type.
 */
function CodeStep(props: { code: string; onInput: (code: string) => void; onBack: () => void; onNext: () => void; error: ReturnType<typeof toApiError> | null }) {
  const normalized = () => normalizePairingCode(props.code);
  return (
    <form
      class="card code-step ticket"
      data-testid="code-step"
      data-valid={normalized().valid ? "true" : "false"}
      onSubmit={(e) => {
        e.preventDefault();
        if (normalized().valid) props.onNext();
      }}
    >
      <div class="ticket-print" aria-hidden="true">
        <Shader variant="ticket" seed={4.2} />
        <span class="print-caption top">Pairing code</span>
        <span class="print-caption top right">Admit one device</span>
      </div>
      <div class="ticket-tear" aria-hidden="true" />
      <div class="ticket-body">
        <label for="pairing-code">Pairing code shown in the Extend app</label>
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
        <p class="fine" data-testid="already-paired-note">
          Someone else already paired this device? On the device, open Extend and choose <strong>Pair with another Carbon</strong> (Extend {MULTI_CARBON_APP_VERSION} or later), then enter
          the code it shows. Your pair stays separate from theirs.
        </p>
        <ErrorNote error={props.error} testid="code-error" />
        <Nav onBack={props.onBack} submit nextLabel="Next" disabled={!normalized().valid} />
        <p class="ticket-credits" aria-hidden="true">
          <span>6 characters · 0–9 A–F</span>
          <span>New every 5 min · works once</span>
        </p>
      </div>
    </form>
  );
}

/**
 * After a claim or an attach, when other Carbons paired the same device: each pair is separate, and
 * Silicons any Carbon gives access to share the device. On a computer the terminal runs as its own
 * account, so only the Carbon who installed Extend there can give terminal use (Carbon decision 3).
 */
function SharedDeviceNote(props: { device: Device }) {
  const computer = () => props.device.kind === "computer" && !props.device.host_device_id;
  return (
    <Show when={props.device.paired_by_others}>
      <div class="notice shared-note" data-testid="wizard-shared-note">
        <p class="shared-head">
          <Users size={15} aria-hidden="true" /> <strong>Another Carbon paired this {computer() ? "computer" : "device"} too.</strong>
        </p>
        <p>Your pair is separate: its own name, Silicons and pairing time. You see only your own Silicons, and only one Silicon uses the device at a time.</p>
        <Show
          when={computer()}
          fallback={<p data-testid="wizard-shared-device-warning">Silicons any Carbon gives access to can use this whole device, including what others leave on it.</p>}
        >
          <p data-testid="wizard-shared-computer-warning">
            Only Silicons given access by the Carbon who installed Silicon Extend on this computer can use its terminal, which runs as the computer's own account. Share a computer
            only with Carbons you trust: a Silicon using it can reach what that account can, including what other Silicons leave on it.
          </p>
        </Show>
      </div>
    </Show>
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
  const client = s.client();
  // Every computer the Carbon paired, whichever Team is selected (1.1: devices belong to the Carbon).
  const [hosts] = createResource(
    () => s.world(),
    async () => {
      const page = await client.listDevices({ scope: "mine", limit: 100 });
      return page.items.filter(
        (d) => !d.host_device_id && (props.kind.host === "mac" ? d.os === "macos" : d.os === "macos" || d.os === "windows" || d.os === "linux"),
      );
    },
  );
  const what = () => (props.kind.host === "mac" ? "Mac" : "computer");
  return (
    <div class="card" data-testid="host-step">
      <p class="fine">
        If another Carbon already added this {props.kind.short} through a computer, add it through that same computer: pair the computer first (on it, choose Pair with another
        Carbon), then pick your pair of it here.
      </p>
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
                    const blocked = () => (!d.online ? `Offline, last seen ${relativeTime(d.last_seen_at)}. Open the Extend app on it.` : d.state !== "ready" ? "Its own setup isn't finished." : null);
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
