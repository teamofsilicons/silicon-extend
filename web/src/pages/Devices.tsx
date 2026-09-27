import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import { Plus, RefreshCw, Search } from "lucide-solid";
import { session } from "../lib/session";
import { toApiError, type ApiError } from "../lib/api";
import type { Device } from "../lib/types";
import { usePoll } from "../lib/poll";
import { Link } from "../lib/router";
import { devicesTick } from "../lib/refresh";
import { OS_LABEL, POLL_MS } from "../config";
import { duration, plural, relativeTime, removedWhy } from "../lib/format";
import { Button, DeviceIcon, Empty, ErrorNote, MemberTag, OnlineDot, Spinner } from "../components/ui";
import Shader from "../components/Shader";
import DevicePage from "./DevicePage";

/** The list's tabs. "removed" is the Carbon's own devices that were removed (scope=mine&include_removed=true). */
type Scope = "mine" | "team" | "removed" | "accessible";

const EYEBROW: Record<Scope, string> = {
  mine: "Your paired devices",
  team: "Your team's devices",
  removed: "Your removed devices",
  accessible: "Devices you can use",
};

const EMPTY: Record<Scope, string> = {
  mine: "Nothing paired yet.",
  team: "No other Carbon in this team has made a device visible.",
  removed: "Nothing removed in this team.",
  accessible: "No Carbon has given you a device in this team yet.",
};

/**
 * Devices, Interface-style: the list on the left (the Carbon's devices, or the team's), and the
 * selected device's page on the right. With nothing selected the right pane is a short overview.
 * At phone width only one of the two shows: the list, or the device.
 */
export default function Devices(props: { selected?: string | null }) {
  const s = session();
  const isSilicon = () => s.member()?.type === "silicon";
  const [scope, setScope] = createSignal<Scope>(isSilicon() ? "accessible" : "mine");
  const [items, setItems] = createSignal<Device[] | null>(null);
  const [next, setNext] = createSignal<string | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [loadingMore, setLoadingMore] = createSignal(false);
  const [refreshedAt, setRefreshedAt] = createSignal<number | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const [filter, setFilter] = createSignal("");

  /**
   * Reloads everything shown so far in one request (up to 100), so polling doesn't drop pages. Removed
   * devices come mixed with paired ones, so that tab reads every page and keeps only the removed.
   */
  async function load(reset = false) {
    const current = scope();
    const shown = reset ? 0 : (items()?.length ?? 0);
    try {
      const page =
        current === "removed"
          ? { items: await s.client().listRemovedDevices(), next_cursor: null }
          : await s.client().listDevices({ scope: current, limit: Math.min(100, Math.max(50, shown)) });
      if (current !== scope()) return;
      setItems(page.items);
      setNext(page.next_cursor);
      setError(null);
      setRefreshedAt(Date.now());
      setNow(Date.now());
    } catch (e) {
      if (current === scope()) setError(toApiError(e));
    }
  }

  async function loadMore() {
    const cursor = next();
    const current = scope();
    if (!cursor || current === "removed") return;
    setLoadingMore(true);
    try {
      const page = await s.client().listDevices({ scope: current, cursor });
      setItems([...(items() ?? []), ...page.items]);
      setNext(page.next_cursor);
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setLoadingMore(false);
    }
  }

  createEffect(
    on([scope, s.team, s.world], () => {
      setItems(null);
      setError(null);
      load(true);
    }),
  );
  // Opening or leaving a device, or changing one, re-reads the list at once.
  createEffect(on([() => props.selected, devicesTick], () => load(), { defer: true }));
  usePoll(() => load(), POLL_MS);

  const hostName = (id: string | null | undefined) => (id ? (items()?.find((d) => d.device_id === id)?.name ?? id) : null);
  const shown = createMemo(() => {
    const words = filter().trim().toLowerCase().split(/\s+/).filter(Boolean);
    const list = items() ?? [];
    if (!words.length) return list;
    return list.filter((d) => {
      const text = `${d.name} ${d.device_id} ${OS_LABEL[d.os] ?? d.os} ${d.model ?? ""} ${d.in_use?.silicon_id ?? ""} ${d.owner.id}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
  });
  const empty = () => items() !== null && items()!.length === 0;
  /** At phone width: the device when one is open, or the empty state when there is nothing to list. */
  const showMain = () => !!props.selected || (empty() && !error());

  return (
    <div class={`devices-view ${showMain() ? "show-main" : "show-list"}`} data-testid={props.selected ? "devices-view" : "devices-page"}>
      <aside class="list-column" aria-label="Devices">
        <div class="list-inner">
          <header class="list-title">
            <div>
              <p class="eyebrow">{EYEBROW[scope()]}</p>
              <h1>Devices.</h1>
            </div>
            <Show when={!isSilicon()}>
              <Link href="/devices/new" class="icon-button outlined" aria-label="Add a device" title="Add a device" data-testid="add-device">
                <Plus size={17} aria-hidden="true" />
              </Link>
            </Show>
          </header>

          <Show when={isSilicon()}>
            <p class="notice list-notice">
              You are signed in as a Silicon. This website is where Carbons pair and manage devices; Silicons use them through the <code>extend</code> CLI. See the <Link href="/docs">docs</Link>.
            </p>
          </Show>

          <label class="search-field list-search">
            <Search size={15} aria-hidden="true" />
            <span class="visually-hidden">Find a device</span>
            <input type="search" placeholder="Find a device" value={filter()} onInput={(e) => setFilter(e.currentTarget.value)} data-testid="device-filter" />
          </label>

          <Show when={!isSilicon()}>
            <div class="filter-chips" role="tablist" aria-label="Whose devices">
              <button role="tab" aria-selected={scope() === "mine"} class={scope() === "mine" ? "selected" : ""} onClick={() => setScope("mine")} data-testid="tab-mine">
                My devices
              </button>
              <button role="tab" aria-selected={scope() === "team"} class={scope() === "team" ? "selected" : ""} onClick={() => setScope("team")} data-testid="tab-team">
                Team devices
              </button>
              <button
                role="tab"
                aria-selected={scope() === "removed"}
                class={scope() === "removed" ? "selected" : ""}
                onClick={() => setScope("removed")}
                title="Devices you removed, or whose pair ended. Their activity logs stay readable."
                data-testid="tab-removed"
              >
                Removed
              </button>
            </div>
          </Show>

          <div class="list-label">
            <span>
              {scope() === "team" ? "Visible in " : scope() === "removed" ? "Removed in " : "In "}
              {s.team()}
              <Show when={items()}> · {items()!.length}</Show>
            </span>
            <span class="list-label-tail">
              <Show when={refreshedAt()}>
                <span title="Refreshes every few seconds">{relativeTime(new Date(refreshedAt()!).toISOString(), now())}</span>
              </Show>
              <button class="icon-button small-icon" aria-label="Refresh" title="Refresh" onClick={() => load()}>
                <RefreshCw size={13} />
              </button>
            </span>
          </div>

          <div class="list-body">
            <ErrorNote error={error()} compact />
            <Show when={items()} fallback={<Show when={!error()}><Spinner label="Loading devices…" /></Show>}>
              {(list) => (
                <Show
                  when={list().length}
                  fallback={<p class="small-empty">{EMPTY[scope()]}</p>}
                >
                  <Show when={shown().length} fallback={<p class="small-empty">No device matches “{filter().trim()}”. Try a name, an id or a Silicon.</p>}>
                    <Show when={scope() === "removed"}>
                      <nav class="device-list removed" aria-label="Your removed devices" data-testid="removed-device-list">
                        <For each={shown()}>
                          {(d) => (
                            <Link
                              href={`/devices/${d.device_id}`}
                              class={`device-row removed ${props.selected === d.device_id ? "active" : ""}`}
                              aria-current={props.selected === d.device_id ? "page" : undefined}
                              data-testid="device-row"
                              data-device-id={d.device_id}
                              data-removed="true"
                            >
                              <DeviceIcon device={d} />
                              <span class="device-copy">
                                <span class="device-top">
                                  <strong class="device-name">{d.name}</strong>
                                  <small title={d.removed_at ? new Date(d.removed_at).toLocaleString() : undefined}>removed {relativeTime(d.removed_at, now())}</small>
                                </span>
                                <span class="device-sub">
                                  {OS_LABEL[d.os] ?? d.os}
                                  {d.os_version ? ` ${d.os_version}` : ""}
                                  <Show when={d.host_device_id}> · through {hostName(d.host_device_id)}</Show>
                                </span>
                                <span class="device-state">
                                  <span class="badge muted">Removed</span>
                                  <span class="removed-why">{removedWhy(d)}</span>
                                </span>
                              </span>
                            </Link>
                          )}
                        </For>
                      </nav>
                    </Show>
                    <Show
                      when={scope() !== "team"}
                      fallback={
                        <ul class="device-list team" data-testid="team-device-list">
                          <For each={shown()}>
                            {(d) => (
                              <li class="device-row readonly" data-testid="device-row">
                                <DeviceIcon device={d} />
                                <span class="device-copy">
                                  <span class="device-top">
                                    <strong class="device-name">{d.name}</strong>
                                  </span>
                                  <span class="device-sub">
                                    {OS_LABEL[d.os] ?? d.os} · paired by {d.owner.display_name ? `${d.owner.display_name} (${d.owner.id})` : d.owner.id}
                                  </span>
                                  <span class="device-state">
                                    <OnlineDot online={d.online} />
                                  </span>
                                </span>
                              </li>
                            )}
                          </For>
                        </ul>
                      }
                    >
                      <Show when={scope() !== "removed"}>
                      <nav class="device-list" aria-label="Your devices" data-testid="device-list">
                        <For each={shown()}>
                          {(d) => (
                            <Link
                              href={`/devices/${d.device_id}`}
                              class={`device-row ${props.selected === d.device_id ? "active" : ""}`}
                              aria-current={props.selected === d.device_id ? "page" : undefined}
                              data-testid="device-row"
                              data-device-id={d.device_id}
                            >
                              <DeviceIcon device={d} />
                              <span class="device-copy">
                                <span class="device-top">
                                  <strong class="device-name">{d.name}</strong>
                                  <small title="Last used">{d.last_used_at ? relativeTime(d.last_used_at, now()) : "never used"}</small>
                                </span>
                                <span class="device-sub">
                                  {OS_LABEL[d.os] ?? d.os}
                                  {d.os_version ? ` ${d.os_version}` : ""}
                                  <Show when={d.host_device_id}> · through {hostName(d.host_device_id)}</Show>
                                  <Show when={d.visibility === "personal"}> · personal</Show>
                                </span>
                                <span class="device-state">
                                  <OnlineDot online={d.online} inUse={!!d.in_use} paused={!!d.in_use?.paused} />
                                  <Show when={d.state === "setup"}>
                                    <span class="badge warn">Setup unfinished</span>
                                  </Show>
                                  <Show when={d.days_left !== undefined}>
                                    <span
                                      class={d.days_left! <= 3 ? "days-left warn" : "days-left"}
                                      title={d.pair_expires_at ? `Unpairs on ${new Date(d.pair_expires_at).toLocaleString()} unless used` : undefined}
                                    >
                                      Pair ends {d.days_left === 0 ? "today" : `in ${plural(d.days_left!, "day")}`}
                                    </span>
                                  </Show>
                                </span>
                                <Show when={d.in_use}>
                                  {(u) => (
                                    <span class="in-use" data-testid="in-use">
                                      <MemberTag type="silicon" />
                                      <strong>{u().silicon_id}</strong>
                                      <span> · {duration(u().since, now())}</span>
                                    </span>
                                  )}
                                </Show>
                              </span>
                            </Link>
                          )}
                        </For>
                      </nav>
                      </Show>
                    </Show>
                  </Show>
                  <Show when={next()}>
                    <Button onClick={loadMore} busy={loadingMore()} small class="load-more">
                      Load more
                    </Button>
                  </Show>
                </Show>
              )}
            </Show>
          </div>

          <p class="list-note">
            <span class="status-dot" aria-hidden="true" /> One Silicon at a time. Stop it any time.
          </p>
        </div>
      </aside>

      <div class="main-pane">
        <Show when={props.selected} keyed fallback={<Overview items={items()} scope={scope()} isSilicon={isSilicon()} />}>
          {(id) => <DevicePage id={id} />}
        </Show>
      </div>
    </div>
  );
}

/** The right pane with no device open: what is here at a glance, or where to start. */
function Overview(props: { items: Device[] | null; scope: Scope; isSilicon: boolean }) {
  const s = session();
  const count = (f: (d: Device) => boolean) => (props.items ?? []).filter(f).length;
  const two = (n: number) => String(n).padStart(2, "0");
  return (
    <Show when={props.scope !== "removed"} fallback={<RemovedOverview items={props.items} />}>
    <Show when={props.items}>
      {(list) => (
        <Show
          when={list().length}
          fallback={
            <Show
              when={props.scope === "mine"}
              fallback={
                <Empty eyebrow={s.team() ?? undefined} title={props.scope === "team" ? "No team devices to show." : "No devices yet."}>
                  <p>
                    {props.scope === "team"
                      ? "Other Carbons in this team haven't made any of their devices visible to the team."
                      : "No Carbon has given you access to a device in this team yet. Ask the Carbon who owns it."}
                  </p>
                </Empty>
              }
            >
              <Empty eyebrow={`Extend · ${s.team() ?? ""}`} title="No devices paired yet.">
                <p>Pair a phone, computer or TV, then choose which Silicons can use it. It takes a few minutes.</p>
                <Link href="/devices/new" class="button primary">
                  <Plus size={16} aria-hidden="true" /> Add your first device
                </Link>
              </Empty>
            </Show>
          }
        >
          <section class="overview" aria-label="At a glance">
            <p class="eyebrow">
              Extend · {s.team()}
            </p>
            <h2 class="overview-title">Pick a device.</h2>
            <p class="overview-lead">
              {props.isSilicon
                ? "Choose one on the left to see what you can do on it. You use it through the extend CLI."
                : "Choose one on the left to see who is using it, change which Silicons can, or stop a session."}
            </p>
            <figure class="tally" aria-label="Your devices at a glance">
              <div class="tally-print">
                <Shader variant="ticket" seed={2.4} />
                <span class="print-caption top">
                  {props.scope === "team" ? "Team" : props.scope === "accessible" ? "Yours to use" : "Paired"} · {s.team()}
                </span>
                <span class="print-caption top right">Silicon Extend</span>
              </div>
              <dl class="tally-numbers">
                <div>
                  <dt>{props.scope === "team" ? "Visible" : props.scope === "accessible" ? "Yours to use" : "Paired"}</dt>
                  <dd>{two(list().length)}</dd>
                </div>
                <div>
                  <dt>Online</dt>
                  <dd class={count((d) => d.online) ? "" : "zero"}>{two(count((d) => d.online))}</dd>
                </div>
                <div>
                  <dt>In use</dt>
                  {/* Cobalt only when a Silicon is actually at work. */}
                  <dd class={count((d) => !!d.in_use) ? "in-use-number" : "zero"}>{two(count((d) => !!d.in_use))}</dd>
                </div>
              </dl>
            </figure>
            <Show when={!props.isSilicon}>
              <div class="overview-actions">
                <Link href="/devices/new" class="button primary">
                  <Plus size={16} aria-hidden="true" /> Add a device
                </Link>
                <Link href="/docs" class="button secondary">
                  How it works
                </Link>
              </div>
            </Show>
          </section>
        </Show>
      )}
    </Show>
    </Show>
  );
}

/** The right pane on the Removed tab: what a removed device still offers (its log), and what it doesn't. */
function RemovedOverview(props: { items: Device[] | null }) {
  const s = session();
  return (
    <Show when={props.items}>
      {(list) => (
        <Show
          when={list().length}
          fallback={
            <Empty eyebrow={`Extend · ${s.team() ?? ""}`} title="Nothing removed yet." testid="removed-empty">
              <p>When you remove a device, or its pair ends, it moves here. You can still read its activity log.</p>
            </Empty>
          }
        >
          <section class="overview" aria-label="Removed devices" data-testid="removed-overview">
            <p class="eyebrow">Extend · {s.team()}</p>
            <h2 class="overview-title">Removed devices.</h2>
            <p class="overview-lead">
              Choose one on the left to read its activity log. A removed device can't be changed or used, and no Silicon can reach it. To use one again, pair it again.
            </p>
            <div class="overview-actions">
              <Link href="/devices/new" class="button primary">
                <Plus size={16} aria-hidden="true" /> Add a device
              </Link>
            </div>
          </section>
        </Show>
      )}
    </Show>
  );
}
