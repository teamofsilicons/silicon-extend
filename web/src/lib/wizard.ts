/**
 * The "Add a device" wizard as a pure state machine, so every transition is testable without a
 * browser. The page renders `state.step` and turns clicks and API results into events.
 *
 * Devices with a Bridge app:   kind → guide → code → name → setup → access → done
 * Devices through a computer:  kind → guide → host → name → setup → access → done
 *
 * The device is created when the name is submitted (the pairing endpoint needs code and name
 * together; attaching needs host and name). From then on there is no going back past `setup`:
 * the device exists, and leaving the wizard leaves it paired.
 */
import { deviceKind, type DeviceKind, type DeviceKindId } from "../config";
import type { Device, Visibility } from "./types";
import { normalizePairingCode } from "./pairing";

export type Step = "kind" | "guide" | "code" | "host" | "name" | "setup" | "access" | "done";

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
  visibility: Visibility;
  ttlDays: number;
  /** Set once Bridge created the device. */
  device: Device | null;
  /** True while the pairing or attachment request is in flight. */
  submitting: boolean;
  error: WizardError | null;
  /** Silicons given access in the last step. */
  granted: string[];
  setupComplete: boolean;
}

export type WizardEvent =
  | { type: "choose_kind"; kind: DeviceKindId }
  | { type: "next" }
  | { type: "back" }
  | { type: "set_code"; code: string }
  | { type: "set_host"; hostId: string }
  | { type: "set_name"; name: string }
  | { type: "set_visibility"; visibility: Visibility }
  | { type: "set_ttl"; days: number }
  | { type: "submit" }
  | { type: "created"; device: Device }
  | { type: "failed"; error: WizardError }
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
    visibility: "team",
    ttlDays: 14,
    device: null,
    submitting: false,
    error: null,
    granted: [],
    setupComplete: false,
  };
}

export function stepsFor(kind: DeviceKind | undefined): Step[] {
  const pick: Step = kind?.via === "host" ? "host" : "code";
  return ["kind", "guide", pick, "name", "setup", "access", "done"];
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
    case "set_visibility":
      return { ...state, visibility: event.visibility };
    case "set_ttl":
      return { ...state, ttlDays: Math.min(30, Math.max(1, Math.round(event.days))) };
    case "next":
      if (!canAdvance(state) || state.step === "name") return state;
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
      return { ...state, submitting: false, device: event.device, step: "setup", error: null, setupComplete: event.device.state === "ready" };
    case "failed": {
      const next: WizardState = { ...state, submitting: false, error: event.error };
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
  setup: "Device setup",
  access: "Silicons",
  done: "Done",
};
