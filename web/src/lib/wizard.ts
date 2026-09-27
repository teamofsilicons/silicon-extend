/**
 * The "Add a device" wizard as a pure state machine, so every transition is testable without a
 * browser. The page renders `state.step` and turns clicks and API results into events.
 *
 * Devices with an Extend app:   kind → guide → code → name → banner → setup → access → done
 * Devices through a computer:  kind → guide → host → name → banner → setup → access → done
 *
 * The device is created when the name is submitted (the pairing endpoint needs code and name
 * together; attaching needs host and name). From then on there is no going back past `banner`:
 * the device exists, and leaving the wizard leaves it paired. Since 1.1 there is no visibility
 * choice (a device is only ever visible to the Carbons who paired it), and access is given after
 * the claim, per Team.
 *
 * `banner` asks whether the device shows that a Silicon is using it (UNDERSTANDING, "Add a device"
 * step 5). Neither the claim nor the attach carries it, so the choice is saved with a PATCH of the
 * device just created. The switch starts from the device's own value: on for a new device, and
 * whatever another Carbon chose for a device they paired first, since it is one setting per device.
 */
import { deviceKind, type DeviceKind, type DeviceKindId } from "../config";
import type { Device } from "./types";
import { normalizePairingCode } from "./pairing";

export type Step = "kind" | "guide" | "code" | "host" | "name" | "banner" | "setup" | "access" | "done";

export interface WizardError {
  code: string;
  message: string;
  hint: string | null;
}

export interface WizardState {
  step: Step;
  kind: DeviceKindId | null;
  code: string;
  hostId: string | null;
  name: string;
  ttlDays: number;
  /** Set once Extend created the device. */
  device: Device | null;
  /** True while the pairing or attachment request is in flight. */
  submitting: boolean;
  error: WizardError | null;
  /** Silicons given access in the last step. */
  granted: string[];
  setupComplete: boolean;
  /** The banner step's switch: whether the device shows that a Silicon is using it. */
  banner: boolean;
  /** True while the banner choice is being saved. */
  savingBanner: boolean;
}

export type WizardEvent =
  | { type: "choose_kind"; kind: DeviceKindId }
  | { type: "next" }
  | { type: "back" }
  | { type: "set_code"; code: string }
  | { type: "set_host"; hostId: string }
  | { type: "set_name"; name: string }
  | { type: "set_ttl"; days: number }
  | { type: "submit" }
  | { type: "created"; device: Device }
  | { type: "failed"; error: WizardError }
  | { type: "set_banner"; shown: boolean }
  | { type: "save_banner" }
  | { type: "banner_saved"; device: Device }
  | { type: "setup_complete" }
  | { type: "access_done"; granted: string[] }
  | { type: "skip_access" }
  | { type: "reset" };

export function initialState(kind: DeviceKindId | null = null): WizardState {
  return {
    step: kind ? "guide" : "kind",
    kind,
    code: "",
    hostId: null,
    name: "",
    ttlDays: 14,
    device: null,
    submitting: false,
    error: null,
    granted: [],
    setupComplete: false,
    banner: true,
    savingBanner: false,
  };
}

/** Whether a device shows that a Silicon is using it: anything but "hidden" (absent means shown). */
export function indicatorShown(device: Pick<Device, "in_use_indicator"> | null | undefined): boolean {
  return device?.in_use_indicator !== "hidden";
}

/** Whether the banner step has a change to save: the switch differs from the device's own value. */
export function bannerChanged(state: WizardState): boolean {
  return !!state.device && state.banner !== indicatorShown(state.device);
}

export function stepsFor(kind: DeviceKind | undefined): Step[] {
  const pick: Step = kind?.via === "host" ? "host" : "code";
  return ["kind", "guide", pick, "name", "banner", "setup", "access", "done"];
}

export function nameProblem(name: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "Give the device a name, so you and your Silicons can tell it apart.";
  if ([...trimmed].length > 64) return `A device name is at most 64 characters; this has ${[...trimmed].length}.`;
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(trimmed)) return "A device name can't contain control characters.";
  return null;
}

/** Whether `next` (or `submit` on the name step) is allowed from the current step. */
export function canAdvance(state: WizardState): boolean {
  switch (state.step) {
    case "kind":
      return !!state.kind;
    case "guide":
      return true;
    case "code":
      return normalizePairingCode(state.code).valid;
    case "host":
      return !!state.hostId;
    case "name":
      return !state.submitting && !nameProblem(state.name) && state.ttlDays >= 1 && state.ttlDays <= 30;
    case "banner":
      return !!state.device && !state.savingBanner;
    case "setup":
      return !!state.device;
    case "access":
      return true;
    case "done":
      return false;
  }
}

/** Errors that mean the pairing code itself was wrong, so the Carbon goes back to enter it again. */
const CODE_ERRORS = new Set(["pairing_code_invalid", "invalid_pairing_code"]);
const HOST_ERRORS = new Set(["device_offline", "host_offline", "host_not_eligible"]);

export function reduce(state: WizardState, event: WizardEvent): WizardState {
  const kind = deviceKind(state.kind ?? undefined);
  const order = stepsFor(kind);
  const index = order.indexOf(state.step);
  switch (event.type) {
    case "reset":
      return initialState();
    case "choose_kind":
      if (state.device) return state;
      return { ...initialState(event.kind), step: "guide" };
    case "set_code":
      return { ...state, code: event.code, error: state.step === "code" ? null : state.error };
    case "set_host":
      return { ...state, hostId: event.hostId, error: null };
    case "set_name":
      return { ...state, name: event.name, error: null };
    case "set_ttl":
      return { ...state, ttlDays: Math.min(30, Math.max(1, Math.round(event.days))) };
    case "next":
      if (!canAdvance(state) || state.step === "name") return state;
      // Going on from the banner step without saving leaves the device's own value as it is.
      if (state.step === "banner") return { ...state, step: "setup", error: null };
      if (state.step === "setup") return { ...state, step: "access", error: null };
      if (state.step === "access") return { ...state, step: "done" };
      return { ...state, step: order[index + 1] ?? state.step, error: null };
    case "back":
      if (state.device || state.submitting || index <= 0) return state;
      return { ...state, step: order[index - 1], error: null };
    case "submit":
      if (state.step !== "name" || !canAdvance(state)) return state;
      return { ...state, submitting: true, error: null };
    case "created":
      return {
        ...state,
        submitting: false,
        device: event.device,
        step: "banner",
        error: null,
        setupComplete: event.device.state === "ready",
        banner: indicatorShown(event.device),
      };
    case "set_banner":
      return state.step === "banner" && !state.savingBanner ? { ...state, banner: event.shown, error: null } : state;
    case "save_banner":
      if (state.step !== "banner" || !canAdvance(state) || !bannerChanged(state)) return state;
      return { ...state, savingBanner: true, error: null };
    case "banner_saved":
      // The PATCH answers with the whole device; keep what the claim said about its setup.
      return { ...state, savingBanner: false, device: { ...state.device!, ...event.device }, step: state.step === "banner" ? "setup" : state.step, error: null };
    case "failed": {
      const next: WizardState = { ...state, submitting: false, savingBanner: false, error: event.error };
      if (state.step === "name" && CODE_ERRORS.has(event.error.code)) return { ...next, step: "code" };
      if (state.step === "name" && HOST_ERRORS.has(event.error.code) && kind?.via === "host") return { ...next, step: "host" };
      return next;
    }
    case "setup_complete":
      return { ...state, setupComplete: true };
    case "access_done":
      return { ...state, granted: event.granted, step: "done", error: null };
    case "skip_access":
      return state.step === "access" ? { ...state, step: "done", error: null } : state;
  }
}

/** Titles for the progress rail. */
export const STEP_TITLE: Record<Step, string> = {
  kind: "Kind of device",
  guide: "Get the app",
  code: "Pairing code",
  host: "Host computer",
  name: "Name",
  banner: "Banner",
  setup: "Device setup",
  access: "Silicons",
  done: "Done",
};
