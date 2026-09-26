import { createEffect, createSignal, For, on, Show } from "solid-js";
import { Plus, RefreshCw } from "lucide-solid";
import { session } from "../lib/session";
import { toApiError, type ApiError } from "../lib/api";
import type { Device } from "../lib/types";
import { usePoll } from "../lib/poll";
import { Link } from "../lib/router";
import { OS_LABEL, POLL_MS } from "../config";
import { duration, plural, relativeTime } from "../lib/format";
import { Button, DeviceIcon, Empty, ErrorNote, OnlineDot, Spinner } from "../components/ui";

type Scope = "mine" | "team" | "accessible";

export default function Devices() {
  const s = session();
  const isSilicon = () => s.member()?.type === "silicon";
  const [scope, setScope] = createSignal<Scope>(isSilicon() ? "accessible" : "mine");
  const [items, setItems] = createSignal<Device[] | null>(null);
  const [next, setNext] = createSignal<string | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [loadingMore, setLoadingMore] = createSignal(false);
  const [refreshedAt, setRefreshedAt] = createSignal<number | null>(null);
  const [now, setNow] = createSignal(Date.now());

  /** Reloads everything shown so far in one request (up to 100), so polling doesn't drop pages. */
  async function load(reset = false) {
    const current = scope();
    const shown = reset ? 0 : (items()?.length ?? 0);
    try {
      const page = await s.client().listDevices({ scope: current, limit: Math.min(100, Math.max(50, shown)) });
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
    if (!cursor) return;
    setLoadingMore(true);
    try {
      const page = await s.client().listDevices({ scope: scope(), cursor });
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
  usePoll(() => load(), POLL_MS);

  const hostName = (id: string | null | undefined) => (id ? (items()?.find((d) => d.device_id === id)?.name ?? id) : null);

  return (
    <section class="page" data-testid="devices-page">
      <div class="page-head">
        <div>
          <h1 class="page-title">Devices</h1>
          <p class="subtitle">
            {isSilicon() ? "Devices you can use" : "Devices you paired"} in team <strong>{s.team()}</strong>
            <Show when={refreshedAt()}>
              <span class="muted"> · updated {relativeTime(new Date(refreshedAt()!).toISOString(), now())}</span>
            </Show>
          </p>
        </div>
        <div class="page-actions">
          <button class="icon-button" aria-label="Refresh" title="Refresh" onClick={() => load()}>
            <RefreshCw size={16} />
          </button>
          <Show when={!isSilicon()}>
            <Link href="/devices/new" class="button primary" data-testid="add-device">
              <Plus size={16} aria-hidden="true" /> Add a device
            </Link>
          </Show>
        </div>
      </div>

      <Show when={isSilicon()}>
        <p class="notice">
          You are signed in as a Silicon. This website is where Carbons pair and manage devices; Silicons use them through the <code>extend</code> CLI. See the{" "}
          <Link href="/docs">docs</Link>.
        </p>
      </Show>

      <Show when={!isSilicon()}>
        <div class="tabs" role="tablist">
          <button role="tab" aria-selected={scope() === "mine"} class={scope() === "mine" ? "active" : ""} onClick={() => setScope("mine")} data-testid="tab-mine">
            My devices
          </button>
          <button role="tab" aria-selected={scope() === "team"} class={scope() === "team" ? "active" : ""} onClick={() => setScope("team")} data-testid="tab-team">
            Team devices
          </button>
        </div>
      </Show>

      <ErrorNote error={error()} />

      <Show when={items()} fallback={<Show when={!error()}><Spinner label="Loading devices…" /></Show>}>
        {(list) => (
          <Show
            when={list().length}
            fallback={
              <Show
                when={scope() === "mine"}
                fallback={
                  <Empty title={scope() === "team" ? "No team devices to show" : "No devices yet"}>
                    <p>
                      {scope() === "team"
                        ? "Other Carbons in this team haven't made any of their devices visible to the team."
                        : "No Carbon has given you access to a device in this team yet."}
                    </p>
                  </Empty>
                }
              >
                <Empty title="No devices paired yet">
                  <p>Pair a phone, computer or TV, then choose which Silicons can use it. It takes a few minutes.</p>
                  <Link href="/devices/new" class="button primary">
                    <Plus size={16} aria-hidden="true" /> Add your first device
                  </Link>
                </Empty>
              </Show>
            }
          >
            <Show
              when={scope() !== "team"}
              fallback={
                <ul class="device-list team" data-testid="team-device-list">
                  <For each={list()}>
                    {(d) => (
                      <li class="device-row readonly" data-testid="device-row">
                        <DeviceIcon device={d} />
                        <div class="device-main">
                          <span class="device-name">{d.name}</span>
                          <span class="device-sub">
                            {OS_LABEL[d.os] ?? d.os} · paired by {d.owner.display_name ? `${d.owner.display_name} (${d.owner.id})` : d.owner.id}
                          </span>
                        </div>
                        <OnlineDot online={d.online} />
                      </li>
                    )}
                  </For>
                </ul>
              }
            >
              <div class="device-table" role="table" aria-label="Devices" data-testid="device-list">
                <div class="device-table-head" role="row">
                  <span role="columnheader">Device</span>
                  <span role="columnheader">Status</span>
                  <span role="columnheader">In use by</span>
                  <span role="columnheader">Last used</span>
                  <span role="columnheader">Pair ends</span>
                </div>
                <For each={list()}>
                  {(d) => (
                    <Link href={`/devices/${d.device_id}`} class="device-row" role="row" data-testid="device-row" data-device-id={d.device_id}>
                      <span class="cell device-cell" role="cell">
                        <DeviceIcon device={d} />
                        <span class="device-main">
                          <span class="device-name">{d.name}</span>
                          <span class="device-sub">
                            {OS_LABEL[d.os] ?? d.os}
                            {d.os_version ? ` ${d.os_version}` : ""}
                            <Show when={d.host_device_id}> · through {hostName(d.host_device_id)}</Show>
                            <Show when={d.visibility === "personal"}> · personal</Show>
                          </span>
                        </span>
                      </span>
                      <span class="cell" role="cell">
                        <OnlineDot online={d.online} />
                        <Show when={d.state === "setup"}>
                          <span class="badge warn">Setup unfinished</span>
                        </Show>
                      </span>
                      <span class="cell in-use-cell" role="cell">
                        <span class="cell-label">In use by</span>
                        <Show when={d.in_use} fallback={<span class="muted">Nobody</span>}>
                          {(u) => (
                            <span class="in-use" data-testid="in-use">
                              <strong>{u().silicon_id}</strong>
                              <span class="muted">
                                {" "}
                                · {duration(u().since, now())}
                                {u().paused ? " · paused for you" : ""}
                              </span>
                            </span>
                          )}
                        </Show>
                      </span>
                      <span class="cell" role="cell">
                        <span class="cell-label">Last used</span>
                        {relativeTime(d.last_used_at, now())}
                      </span>
                      <span class="cell" role="cell">
                        <span class="cell-label">Pair ends</span>
                        <Show when={d.days_left !== undefined} fallback="—">
                          <span class={d.days_left! <= 3 ? "days-left warn" : "days-left"} title={d.pair_expires_at ? `Unpairs on ${new Date(d.pair_expires_at).toLocaleString()} unless used` : undefined}>
                            {d.days_left === 0 ? "today" : `in ${plural(d.days_left!, "day")}`}
                          </span>
                        </Show>
                      </span>
                    </Link>
                  )}
                </For>
              </div>
            </Show>
            <Show when={next()}>
              <Button onClick={loadMore} busy={loadingMore()} class="load-more">
                Load more
              </Button>
            </Show>
          </Show>
        )}
      </Show>
    </section>
  );
}
