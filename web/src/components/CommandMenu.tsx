import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { ArrowRight, BookOpen, CornerDownLeft, Plus, Search, Settings, Smartphone } from "lucide-solid";
import { session } from "../lib/session";
import { toApiError, type ApiError } from "../lib/api";
import type { Device } from "../lib/types";
import { navigate } from "../lib/router";
import { DEVICE_KINDS, OS_LABEL } from "../config";
import { deviceStatus, ErrorNote, StatusDot } from "./ui";

interface Item {
  id: string;
  group: "Devices" | "Go to" | "Pair";
  label: string;
  detail?: string;
  href: string;
  icon?: () => JSX.Element;
}

const [open, setOpen] = createSignal(false);
/** Opens the ⌘K menu from anywhere (the top bar's search button uses it). */
export const openCommandMenu = () => setOpen(true);

/**
 * ⌘K: find a device by name, id, system or the Silicon using it, or jump to a page. Devices are
 * read from Extend when the menu opens; pages work signed out too.
 */
export function CommandMenu() {
  const s = session();
  let dialog!: HTMLDialogElement;
  let input!: HTMLInputElement;
  const [query, setQuery] = createSignal("");
  const [devices, setDevices] = createSignal<Device[] | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [active, setActive] = createSignal(0);
  const isCarbon = () => s.member()?.type === "carbon";

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen(!open());
      }
    };
    document.addEventListener("keydown", key);
    onCleanup(() => document.removeEventListener("keydown", key));
  });

  createEffect(
    on(open, async (isOpen) => {
      if (isOpen && !dialog.open) {
        dialog.showModal();
        setQuery("");
        setActive(0);
        setTimeout(() => input?.focus());
        if (!s.pair()) return setDevices(null);
        try {
          const page = await s.client().listDevices({ scope: s.member()?.type === "silicon" ? "accessible" : "mine", limit: 100 });
          setDevices(page.items);
          setError(null);
        } catch (e) {
          setError(toApiError(e));
        }
      } else if (!isOpen && dialog.open) dialog.close();
    }),
  );

  const items = createMemo<Item[]>(() => {
    const out: Item[] = [];
    for (const d of devices() ?? [])
      out.push({
        id: `device-${d.device_id}`,
        group: "Devices",
        label: d.name,
        detail: `${OS_LABEL[d.os] ?? d.os} · ${d.device_id}${d.in_use ? ` · ${d.in_use.silicon_id}` : ""}`,
        href: `/devices/${d.device_id}`,
        icon: () => <StatusDot status={deviceStatus(d)} />,
      });
    if (s.pair()) out.push({ id: "go-devices", group: "Go to", label: "Devices", href: "/devices", icon: () => <Smartphone size={15} /> });
    if (isCarbon()) out.push({ id: "go-add", group: "Go to", label: "Add a device", href: "/devices/new", icon: () => <Plus size={15} /> });
    out.push(
      { id: "go-docs", group: "Go to", label: "Docs: Start here", href: "/docs", icon: () => <BookOpen size={15} /> },
      { id: "go-docs-devices", group: "Go to", label: "Docs: Pairing each kind of device", href: "/docs/devices", icon: () => <BookOpen size={15} /> },
      { id: "go-docs-cli", group: "Go to", label: "Docs: CLI reference", href: "/docs/cli", icon: () => <BookOpen size={15} /> },
      { id: "go-docs-how", group: "Go to", label: "Docs: How Extend works", href: "/docs/how-it-works", icon: () => <BookOpen size={15} /> },
      { id: "go-settings", group: "Go to", label: "Settings", href: "/settings", icon: () => <Settings size={15} /> },
    );
    if (isCarbon())
      for (const k of DEVICE_KINDS)
        out.push({ id: `pair-${k.id}`, group: "Pair", label: `Pair ${/^[aeiou]/i.test(k.label) || k.id === "lg_tv" ? "an" : "a"} ${k.label}`, detail: k.via === "app" ? "Extend app" : "through a computer", href: `/devices/new?kind=${k.id}`, icon: () => <Plus size={15} /> });
    return out;
  });

  const results = createMemo(() => {
    const words = query().trim().toLowerCase().split(/\s+/).filter(Boolean);
    const list = words.length ? items().filter((it) => words.every((w) => `${it.label} ${it.detail ?? ""}`.toLowerCase().includes(w))) : items().filter((it) => it.group !== "Pair");
    return list.slice(0, 40);
  });
  createEffect(on(results, () => setActive(0)));

  function go(item: Item | undefined) {
    if (!item) return;
    setOpen(false);
    navigate(item.href);
  }

  function onKey(e: KeyboardEvent) {
    const n = results().length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => (n ? (i + 1) % n : 0));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => (n ? (i - 1 + n) % n : 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      go(results()[active()]);
    }
  }

  createEffect(() => {
    const i = active();
    document.getElementById(`command-option-${i}`)?.scrollIntoView({ block: "nearest" });
  });

  return (
    <dialog
      ref={dialog}
      class="modal command-menu"
      aria-label="Search devices and pages"
      data-testid="command-menu"
      onCancel={(e) => {
        e.preventDefault();
        setOpen(false);
      }}
      onClick={(e) => {
        if (e.target === dialog) setOpen(false);
      }}
    >
      <div class="command-search search-field">
        <Search size={16} aria-hidden="true" />
        <input
          ref={input}
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-controls="command-results"
          aria-activedescendant={results().length ? `command-option-${active()}` : undefined}
          aria-label="Search devices and pages"
          placeholder="Find a device or a page"
          autocomplete="off"
          spellcheck={false}
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
          onKeyDown={onKey}
          data-testid="command-input"
        />
        <kbd>esc</kbd>
      </div>
      <ErrorNote error={error()} compact />
      <ul class="command-results" id="command-results" role="listbox" aria-label="Results">
        <For each={results()}>
          {(item, i) => (
            <>
              <Show when={i() === 0 || results()[i() - 1].group !== item.group}>
                <li class="command-group" role="presentation">
                  {item.group}
                </li>
              </Show>
              <li
                id={`command-option-${i()}`}
                role="option"
                aria-selected={active() === i()}
                class={active() === i() ? "active" : ""}
                onMouseMove={() => setActive(i())}
                onClick={() => go(item)}
                data-testid="command-option"
              >
                <span class="command-icon" aria-hidden="true">
                  {item.icon?.()}
                </span>
                <span class="command-label">{item.label}</span>
                <Show when={item.detail}>
                  <small>{item.detail}</small>
                </Show>
                <Show when={active() === i()}>
                  <ArrowRight size={14} aria-hidden="true" class="command-go" />
                </Show>
              </li>
            </>
          )}
        </For>
      </ul>
      <Show when={!results().length}>
        <p class="command-empty">Nothing matches “{query().trim()}”. Try a device name, a device id, a Silicon such as si:chef, or a page.</p>
      </Show>
      <p class="command-hint">
        <kbd>↑</kbd> <kbd>↓</kbd> to move · <kbd>
          <CornerDownLeft size={10} aria-hidden="true" />
        </kbd>{" "}
        to open · <kbd>⌘ K</kbd> to close
      </p>
    </dialog>
  );
}
