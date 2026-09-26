import { createEffect, createSignal, For, onCleanup, Show, splitProps, type JSX } from "solid-js";
import { Check, CircleAlert, Laptop, LoaderCircle, Monitor, Smartphone, Tablet, Tv, X } from "lucide-solid";
import type { ApiError } from "../lib/api";
import type { Device } from "../lib/types";

/** Shows exactly what Bridge said: the message, the hint, and the ids to report it with. */
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

export function OnlineDot(props: { online: boolean; label?: boolean }) {
  return (
    <span class={`online ${props.online ? "is-online" : "is-offline"}`}>
      <span class="dot" aria-hidden="true" />
      <Show when={props.label !== false}>{props.online ? "Online" : "Offline"}</Show>
    </span>
  );
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

export function Empty(props: { title: string; children?: JSX.Element }) {
  return (
    <div class="empty">
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
