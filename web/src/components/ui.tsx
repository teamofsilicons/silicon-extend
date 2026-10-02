import { createEffect, createSignal, For, onCleanup, Show, splitProps, type JSX } from "solid-js";
import { Check, CircleAlert, Laptop, LoaderCircle, Monitor, Smartphone, Tablet, Tv, X } from "lucide-solid";
import type { ApiError } from "../lib/api";
import type { Device } from "../lib/types";
import { statusLabel } from "../lib/format";
import { Link } from "../lib/router";
import Shader from "./Shader";

/** Shows exactly what Extend said: the message, the hint, and the ids to report it with. */
export function ErrorNote(props: { error: ApiError | null | undefined; compact?: boolean; testid?: string }) {
  return (
    <Show when={props.error}>
      {(error) => (
        <div class={`error-note ${props.compact ? "compact" : ""}`} role="alert" data-testid={props.testid ?? "error"} data-code={error().code}>
          <CircleAlert size={18} aria-hidden="true" />
          <div>
            <p class="error-message">{error().message}</p>
            <Show when={error().hint}>
              <p class="error-hint">{error().hint}</p>
            </Show>
            <Show when={error().details.permission_required === true}>
              <p><Link href="/settings#feature-permissions">Review feature access in Settings</Link></p>
            </Show>
            <p class="error-meta">
              <code>{error().code}</code>
              <Show when={error().status}>
                <span> · HTTP {error().status}</span>
              </Show>
              <Show when={error().requestId}>
                <span> · request {error().requestId}</span>
              </Show>
              <Show when={error().docsUrl}>
                {" · "}
                <a href={error().docsUrl!} target="_blank" rel="noopener noreferrer">
                  about this error
                </a>
              </Show>
            </p>
          </div>
        </div>
      )}
    </Show>
  );
}

export function Spinner(props: { label?: string; inline?: boolean }) {
  return (
    <span class={props.inline ? "spinner-inline" : "loading-state"} role="status">
      <LoaderCircle class="spin" size={props.inline ? 15 : 20} aria-hidden="true" />
      <span>{props.label ?? "Loading…"}</span>
    </span>
  );
}

export function Button(
  props: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "primary" | "secondary" | "ghost" | "danger"; busy?: boolean; small?: boolean },
) {
  const [local, rest] = splitProps(props, ["variant", "busy", "small", "class", "children", "disabled"]);
  return (
    <button
      type="button"
      {...rest}
      class={`button ${local.variant ?? "secondary"} ${local.small ? "small" : ""} ${local.class ?? ""}`}
      disabled={local.disabled || local.busy}
      aria-busy={local.busy ? "true" : undefined}
    >
      <Show when={local.busy}>
        <LoaderCircle class="spin" size={15} aria-hidden="true" />
      </Show>
      {local.children}
    </button>
  );
}

export function Modal(props: { open: boolean; title: string; onClose: () => void; children: JSX.Element; testid?: string }) {
  let dialog!: HTMLDialogElement;
  createEffect(() => {
    if (props.open && !dialog.open) dialog.showModal();
    else if (!props.open && dialog.open) dialog.close();
  });
  onCleanup(() => {
    if (dialog?.open) dialog.close();
  });
  return (
    <dialog
      ref={dialog}
      class="modal"
      aria-label={props.title}
      data-testid={props.testid}
      onCancel={(e) => {
        e.preventDefault();
        props.onClose();
      }}
    >
      <header class="modal-head">
        <h2>{props.title}</h2>
        <button class="icon-button" onClick={props.onClose} aria-label="Close">
          <X size={18} />
        </button>
      </header>
      <div class="modal-body">{props.children}</div>
    </dialog>
  );
}

export type DeviceStatus = "online" | "in-use" | "offline";

/**
 * A 5×5 pixel dot, printed rather than drawn: solid for online, cobalt with a shimmering dithered
 * rim for in use (still with reduced motion), and a 50% checker for offline. The colour is never
 * the only signal; the label beside it says the same thing.
 */
const DOT_PIXELS: Record<DeviceStatus, string[]> = {
  online: [".oXo.", "oXXXo", "XXXXX", "oXXXo", ".oXo."],
  "in-use": [".oXo.", "oXXXo", "XXXXX", "oXXXo", ".oXo."],
  offline: ["..X..", ".X.X.", "X.X.X", ".X.X.", "..X.."],
};

export function StatusDot(props: { status: DeviceStatus }) {
  const pixels = () => {
    const out: { x: number; y: number; rim: boolean; i: number }[] = [];
    let i = 0;
    DOT_PIXELS[props.status].forEach((row, y) =>
      [...row].forEach((c, x) => {
        if (c !== ".") out.push({ x, y, rim: c === "o", i: i++ });
      }),
    );
    return out;
  };
  return (
    <svg class={`pixel-dot ${props.status}`} width="10" height="10" viewBox="0 0 5 5" shape-rendering="crispEdges" aria-hidden="true">
      <For each={pixels()}>{(p) => <rect x={p.x} y={p.y} width="1" height="1" class={p.rim ? "rim" : undefined} style={p.rim ? { "animation-delay": `${(p.i * 137) % 900}ms` } : undefined} />}</For>
    </svg>
  );
}

export function deviceStatus(device: { online: boolean; in_use?: unknown }): DeviceStatus {
  return device.in_use ? "in-use" : device.online ? "online" : "offline";
}

/**
 * "Online", "In use", "Paused for you" or "Offline" with its pixel dot: the words always say what the
 * colour says. A paused session is still a Silicon's session (cobalt), waiting for the Carbon.
 */
export function OnlineDot(props: { online: boolean; inUse?: boolean; paused?: boolean; label?: boolean }) {
  const status = (): DeviceStatus => (props.inUse ? "in-use" : props.online ? "online" : "offline");
  return (
    <span class={`online ${props.online ? "is-online" : "is-offline"} ${props.inUse ? "is-in-use" : ""}`} data-testid="device-status">
      <StatusDot status={status()} />
      <Show when={props.label !== false}>{statusLabel({ online: props.online, inUse: props.inUse, paused: props.paused })}</Show>
    </span>
  );
}

/** The SILICON / CARBON tag Interface puts beside a member's name. */
export function MemberTag(props: { type: "carbon" | "silicon" | string | undefined }) {
  return (
    <Show when={props.type === "carbon" || props.type === "silicon"}>
      <span class={`member-tag ${props.type}`}>{props.type === "carbon" ? "Carbon" : "Silicon"}</span>
    </Show>
  );
}

/** Whether an id names a Silicon (si:…) or a Carbon (c:…), for the tag. */
export function memberType(id: string | null | undefined): "carbon" | "silicon" | undefined {
  if (!id) return undefined;
  if (id.startsWith("si:")) return "silicon";
  if (id.startsWith("c:")) return "carbon";
  return undefined;
}

export function DeviceIcon(props: { device: Pick<Device, "kind" | "os">; size?: number }) {
  const size = () => props.size ?? 20;
  return (
    <span class="device-icon" aria-hidden="true">
      {props.device.kind === "tv" ? (
        <Tv size={size()} />
      ) : props.device.kind === "tablet" ? (
        <Tablet size={size()} />
      ) : props.device.kind === "phone" ? (
        <Smartphone size={size()} />
      ) : props.device.os === "macos" ? (
        <Laptop size={size()} />
      ) : (
        <Monitor size={size()} />
      )}
    </span>
  );
}

/** An empty state: a dithered colour study, a serif sentence, a line of help and what to do. */
export function Empty(props: { title: string; eyebrow?: string; children?: JSX.Element; testid?: string }) {
  return (
    <div class="empty" data-testid={props.testid}>
      {/* The same 3 px cell as the sign-in print, so the two read as one press run. */}
      <Shader variant="orb" class="empty-orb" cell={3} />
      <Show when={props.eyebrow}>
        <p class="eyebrow">{props.eyebrow}</p>
      </Show>
      <h2>{props.title}</h2>
      {props.children}
    </div>
  );
}

// ───────────── Toasts ─────────────

interface Toast {
  id: number;
  text: string;
  kind: "ok" | "info";
}
const [toasts, setToasts] = createSignal<Toast[]>([]);
let toastId = 0;
export function toast(text: string, kind: Toast["kind"] = "ok") {
  const id = ++toastId;
  setToasts((list) => [...list, { id, text, kind }]);
  setTimeout(() => setToasts((list) => list.filter((t) => t.id !== id)), 4200);
}
export function Toasts() {
  return (
    <div class="toasts" aria-live="polite">
      <For each={toasts()}>
        {(t) => (
          <div class={`toast ${t.kind}`} data-testid="toast">
            <Check size={16} aria-hidden="true" />
            <span>{t.text}</span>
          </div>
        )}
      </For>
    </div>
  );
}

export function CopyText(props: { text: string; label?: string }) {
  const [copied, setCopied] = createSignal(false);
  return (
    <span class="copy-line">
      <code>{props.text}</code>
      <button
        class="icon-button small"
        aria-label={props.label ?? `Copy ${props.text}`}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(props.text);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          } catch {
            /* clipboard blocked; the text stays selectable */
          }
        }}
      >
        {copied() ? "Copied" : "Copy"}
      </button>
    </span>
  );
}
