import { createEffect, createSignal, For, on, onMount, Show } from "solid-js";
import { ArrowLeft, Check, CircleStop, Pencil, Trash2 } from "lucide-solid";
import { session } from "../lib/session";
import { ifMatchValue, toApiError, type ApiError } from "../lib/api";
import type { AccessGrant, ActivityEntry, BridgeRequest, Device, DeviceDetail, Takeover, Visibility } from "../lib/types";
import { usePoll } from "../lib/poll";
import { Link, navigate } from "../lib/router";
import { OS_LABEL, POLL_MS } from "../config";
import { activitySummary, clock, dateTime, day, duration, plural, relativeTime } from "../lib/format";
import { Button, DeviceIcon, ErrorNote, Modal, OnlineDot, Spinner, toast } from "../components/ui";
import { TtlSlider } from "../components/TtlSlider";
import { AccessPicker } from "../components/AccessPicker";
import { SetupSteps } from "../components/SetupSteps";

export default function DevicePage(props: { id: string }) {
  const s = session();
  const [device, setDevice] = createSignal<DeviceDetail | null>(null);
  const [etag, setEtag] = createSignal<string | null>(null);
  const [loadError, setLoadError] = createSignal<ApiError | null>(null);
  const [now, setNow] = createSignal(Date.now());

  async function load() {
    try {
      const { device: d, etag: tag } = await s.client().getDevice(props.id);
      setDevice(d);
      setEtag(tag);
      setLoadError(null);
      setNow(Date.now());
    } catch (e) {
      setLoadError(toApiError(e));
    }
  }
  createEffect(on([() => props.id, s.team, s.world], () => {
    setDevice(null);
    load();
  }));
  usePoll(load, POLL_MS);

  /** Sends a settings change with If-Match; on a stale version, reloads so the Carbon sees what changed. */
  async function patch(change: { name?: string; visibility?: Visibility; pair_ttl_days?: number }): Promise<ApiError | null> {
    const d = device();
    if (!d) return null;
    const started = performance.now();
    try {
      const { device: updated, etag: tag } = await s.client().updateDevice(d.device_id, change, ifMatchValue(etag(), d.version));
      setDevice({ ...d, ...updated });
      setEtag(tag);
      void s.client().telemetry({ event: "device_update", step: `web.device.${Object.keys(change).join("+")}`, success: true, duration_ms: performance.now() - started, device_os: d.os });
      return null;
    } catch (e) {
      const error = toApiError(e);
      if (error.status === 412) await load();
      void s.client().telemetry({ event: "device_update", step: `web.device.${Object.keys(change).join("+")}`, success: false, duration_ms: performance.now() - started, error_code: error.code, request_id: error.requestId, device_os: d.os });
      return error;
    }
  }

  return (
    <section class="page device-page" data-testid="device-page">
      <Link href="/devices" class="back-link">
        <ArrowLeft size={15} aria-hidden="true" /> Devices
      </Link>
      <ErrorNote error={loadError()} testid="device-load-error" />
      <Show when={device()} fallback={<Show when={!loadError()}><Spinner label="Loading the device…" /></Show>}>
        {(d) => (
          <>
            <Header device={d()} patch={patch} />
            <Show when={d().state === "setup"}>
              <div class="card">
                <h2 class="card-title">Setup isn't finished</h2>
                <p class="fine">Until it is, Silicons can't use every part of the device.</p>
                <SetupSteps device={d()} onComplete={load} />
              </div>
            </Show>
            <InUse device={d()} now={now()} onStopped={load} />
            <Access device={d()} onChanged={load} />
            <Settings device={d()} patch={patch} />
            <Capabilities device={d()} />
            <Activity device={d()} />
            <Requests device={d()} />
            <DangerZone device={d()} etag={etag()} />
          </>
        )}
      </Show>
    </section>
  );
}

function Header(props: { device: DeviceDetail; patch: (c: { name: string }) => Promise<ApiError | null> }) {
  const [editing, setEditing] = createSignal(false);
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);
  const d = () => props.device;
  async function save(e: Event) {
    e.preventDefault();
    const value = name().trim();
    if (!value || value === d().name) {
      setEditing(false);
      return;
    }
    setBusy(true);
    const err = await props.patch({ name: value });
    setBusy(false);
    setError(err);
    if (!err) {
      setEditing(false);
      toast(`Renamed to ${value}`);
    }
  }
  return (
    <div class="device-header">
      <DeviceIcon device={d()} size={28} />
      <div class="device-header-main">
        <Show
          when={editing()}
          fallback={
            <h1 class="page-title device-title">
              <span data-testid="device-name">{d().name}</span>
              <button class="icon-button" aria-label="Rename" title="Rename" data-testid="rename" onClick={() => (setName(d().name), setEditing(true), setError(null))}>
                <Pencil size={16} />
              </button>
            </h1>
          }
        >
          <form class="rename" onSubmit={save}>
            <input value={name()} maxLength={64} aria-label="Device name" data-testid="rename-input" onInput={(e) => setName(e.currentTarget.value)} ref={(el) => setTimeout(() => el.select())} />
            <Button type="submit" variant="primary" busy={busy()} data-testid="rename-save">
              Save
            </Button>
            <Button variant="ghost" onClick={() => setEditing(false)}>
              Cancel
            </Button>
          </form>
        </Show>
        <p class="device-meta">
          <OnlineDot online={d().online} />
          <span>
            {OS_LABEL[d().os] ?? d().os}
            {d().os_version ? ` ${d().os_version}` : ""}
            {d().model ? ` · ${d().model}` : ""}
          </span>
          <span class="mono">{d().device_id}</span>
          <Show when={d().host_device_id}>
            <span>
              through <Link href={`/devices/${d().host_device_id}`}>{d().host_device_id}</Link>
            </span>
          </Show>
          <Show when={!d().online && d().last_seen_at}>
            <span>last seen {relativeTime(d().last_seen_at)}</span>
          </Show>
        </p>
        <ErrorNote error={error()} compact />
      </div>
    </div>
  );
}

function InUse(props: { device: DeviceDetail; now: number; onStopped: () => void }) {
  const s = session();
  const [busy, setBusy] = createSignal<"stop" | "done" | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [takeover, setTakeover] = createSignal<Takeover | null>(null);

  // A paused session means the Silicon handed the device to the Carbon: read why.
  createEffect(
    on(
      () => (props.device.in_use?.paused ? props.device.in_use.session_id : null),
      async (sessionId) => {
        setTakeover(null);
        if (!sessionId) return;
        try {
          setTakeover(await s.client().getTakeover(sessionId));
        } catch (e) {
          setError(toApiError(e));
        }
      },
    ),
  );

  async function stop() {
    setBusy("stop");
    setError(null);
    try {
      const ended = await s.client().stopDevice(props.device.device_id);
      toast(`Stopped ${ended.silicon_id} (session ${ended.session_id})`);
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
      props.onStopped();
    }
  }

  async function done(sessionId: string, siliconId: string) {
    setBusy("done");
    setError(null);
    try {
      await s.client().releaseTakeover(sessionId);
      setTakeover(null);
      toast(`${siliconId} can carry on`);
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
      props.onStopped();
    }
  }

  return (
    <div class={`card in-use-card ${props.device.in_use ? "active" : ""} ${props.device.in_use?.paused ? "paused" : ""}`} data-testid="in-use-card">
      <Show
        when={props.device.in_use}
        fallback={
          <p class="muted">
            No Silicon is using {props.device.name} right now.
            <Show when={props.device.last_used_at}> Last used {relativeTime(props.device.last_used_at, props.now)}.</Show>
          </p>
        }
      >
        {(u) => (
          <>
            <div class="in-use-row">
              <span class="pulse" aria-hidden="true" />
              <div>
                <p>
                  <strong data-testid="in-use-silicon">{u().silicon_id}</strong> {u().paused ? "handed the device to you" : "is using it now"}
                </p>
                <p class="fine">
                  Since {clock(u().since)} ({duration(u().since, props.now)}) · session <span class="mono">{u().session_id}</span>
                </p>
              </div>
              <Button variant="danger" onClick={stop} busy={busy() === "stop"} data-testid="stop-session">
                <CircleStop size={16} aria-hidden="true" /> Stop
              </Button>
            </div>
            <Show when={u().paused}>
              <div class="takeover" data-testid="takeover">
                <Show when={takeover()} fallback={<p class="fine">The Silicon paused its session and is waiting for you on the device.</p>}>
                  {(t) => (
                    <>
                      <p class="takeover-label">It needs you to:</p>
                      <blockquote data-testid="takeover-reason">{t().reason}</blockquote>
                      <p class="fine">
                        Do it on the device, then choose Done so {u().silicon_id} can carry on. If you don't, the session ends at {clock(t().expires_at)}.
                      </p>
                    </>
                  )}
                </Show>
                <Button variant="primary" onClick={() => done(u().session_id, u().silicon_id)} busy={busy() === "done"} data-testid="takeover-done">
                  <Check size={16} aria-hidden="true" /> Done
                </Button>
              </div>
            </Show>
          </>
        )}
      </Show>
      <ErrorNote error={error()} compact />
    </div>
  );
}

function Access(props: { device: DeviceDetail; onChanged: () => void }) {
  const s = session();
  const [grants, setGrants] = createSignal<AccessGrant[] | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [confirm, setConfirm] = createSignal<string | null>(null);
  const [revoking, setRevoking] = createSignal<string | null>(null);
  async function load() {
    try {
      setGrants(await s.client().listAccess(props.device.device_id));
      setError(null);
    } catch (e) {
      setError(toApiError(e));
    }
  }
  onMount(load);
  usePoll(load, POLL_MS * 3);

  async function revoke(id: string) {
    setConfirm(null);
    setRevoking(id);
    try {
      await s.client().revokeAccess(props.device.device_id, id);
      toast(`${id} can no longer use ${props.device.name}`);
      await load();
      props.onChanged();
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setRevoking(null);
    }
  }

  return (
    <div class="card" data-testid="access-card">
      <h2 class="card-title">Silicons with access</h2>
      <ErrorNote error={error()} compact />
      <Show when={grants()} fallback={<Show when={!error()}><Spinner inline label="Loading access…" /></Show>}>
        {(list) => (
          <Show when={list().length} fallback={<p class="muted">No Silicon can use this device yet.</p>}>
            <ul class="grant-list">
              <For each={list()}>
                {(g) => (
                  <li class="grant" data-testid="grant" data-silicon={g.silicon_id}>
                    <div>
                      <strong>{g.silicon_id}</strong>
                      <Show when={props.device.in_use?.silicon_id === g.silicon_id}>
                        <span class="badge live">Using it now</span>
                      </Show>
                      <p class="fine">
                        Given by {g.granted_by} on {day(g.granted_at)} · last used {relativeTime(g.last_used_at)}
                      </p>
                    </div>
                    <Button
                      small
                      variant="ghost"
                      busy={revoking() === g.silicon_id}
                      data-testid="revoke"
                      onClick={() => (props.device.in_use?.silicon_id === g.silicon_id ? setConfirm(g.silicon_id) : revoke(g.silicon_id))}
                    >
                      Take away
                    </Button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        )}
      </Show>
      <details class="grant-more" open={grants()?.length === 0}>
        <summary>Give another Silicon access</summary>
        <AccessPicker
          deviceId={props.device.device_id}
          existing={(grants() ?? []).map((g) => g.silicon_id)}
          onGranted={async (ids) => {
            toast(`${ids.join(", ")} can now use ${props.device.name}`);
            await load();
            props.onChanged();
          }}
        />
      </details>
      <Modal open={!!confirm()} title="Take access away now?" onClose={() => setConfirm(null)} testid="revoke-confirm">
        <p>
          <strong>{confirm()}</strong> is using {props.device.name} right now. Taking access away ends its session immediately.
        </p>
        <div class="modal-actions">
          <Button variant="ghost" onClick={() => setConfirm(null)}>
            Keep access
          </Button>
          <Button variant="danger" onClick={() => revoke(confirm()!)} data-testid="revoke-confirm-button">
            Take access away
          </Button>
        </div>
      </Modal>
    </div>
  );
}

function Settings(props: { device: DeviceDetail; patch: (c: { visibility?: Visibility; pair_ttl_days?: number }) => Promise<ApiError | null> }) {
  const [ttl, setTtl] = createSignal(props.device.pair_ttl_days ?? 14);
  const [dirty, setDirty] = createSignal(false);
  const [busy, setBusy] = createSignal<"ttl" | "visibility" | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  // Follow the server's value unless the Carbon is mid-change.
  createEffect(on(() => props.device.pair_ttl_days, (v) => !dirty() && setTtl(v ?? 14)));

  async function saveTtl() {
    setBusy("ttl");
    const err = await props.patch({ pair_ttl_days: ttl() });
    setBusy(null);
    setError(err);
    if (!err) {
      setDirty(false);
      toast(`${props.device.name} now stays paired for ${plural(ttl(), "day")} without activity`);
    }
  }
  async function setVisibility(v: Visibility) {
    if (v === props.device.visibility) return;
    setBusy("visibility");
    const err = await props.patch({ visibility: v });
    setBusy(null);
    setError(err);
    if (!err) toast(v === "team" ? "Visible to your team" : "Visible only to you");
  }
  return (
    <div class="card" data-testid="settings-card">
      <h2 class="card-title">Pairing</h2>
      <TtlSlider
        id="device-ttl"
        value={ttl()}
        onInput={(days) => {
          setTtl(days);
          setDirty(days !== props.device.pair_ttl_days);
        }}
      />
      <div class="ttl-actions">
        <Show when={props.device.pair_expires_at}>
          <p class="fine" data-testid="pair-expires">
            {dirty()
              ? "Saving restarts the count from now."
              : `Unpairs on ${dateTime(props.device.pair_expires_at)} unless used (${props.device.days_left === 0 ? "today" : `in ${plural(props.device.days_left ?? 0, "day")}`}).`}
          </p>
        </Show>
        <Show when={dirty()}>
          <Button small onClick={() => (setTtl(props.device.pair_ttl_days ?? 14), setDirty(false))}>
            Undo
          </Button>
          <Button small variant="primary" busy={busy() === "ttl"} onClick={saveTtl} data-testid="ttl-save">
            Save
          </Button>
        </Show>
      </div>
      <fieldset class="radio-group" disabled={busy() === "visibility"}>
        <legend>Who can see it exists</legend>
        <label>
          <input type="radio" name="device-visibility" checked={props.device.visibility === "team"} onChange={() => setVisibility("team")} data-testid="visibility-team" />
          <span>
            <strong>Team</strong> — other Carbons in {props.device.team ?? "the team"} see its name, kind and whether it's online.
          </span>
        </label>
        <label>
          <input type="radio" name="device-visibility" checked={props.device.visibility === "personal"} onChange={() => setVisibility("personal")} data-testid="visibility-personal" />
          <span>
            <strong>Personal</strong> — only you see it.
          </span>
        </label>
      </fieldset>
      <ErrorNote error={error()} compact testid="settings-error" />
    </div>
  );
}

const CAPABILITY_LABEL: Record<string, string> = {
  "screen.read": "Read the screen",
  "screen.capture": "Screenshots",
  "screen.record": "Screen recordings",
  "input.touch": "Touch",
  "input.pointer": "Mouse",
  "input.text": "Typing",
  "input.keyboard": "Keyboard",
  "input.remote": "Remote buttons",
  "nav.system": "Back and home",
  "apps.launch": "Open and close apps",
  "apps.list": "List apps",
  "apps.install": "Install apps",
  alerts: "Pop-ups",
  clipboard: "Clipboard",
  logs: "Device logs",
  replay: "Replay saved steps",
  takeover: "Hand the device to you",
  notifications: "Notifications",
  adb: "Android debugging",
  terminal: "Terminal",
  display: "Show things full screen",
  links: "Open links",
};

function Capabilities(props: { device: DeviceDetail }) {
  return (
    <details class="card" data-testid="capabilities">
      <summary class="card-title">What a Silicon can do here</summary>
      <ul class="capabilities">
        <For each={props.device.capabilities}>{(c) => <li title={c}>{CAPABILITY_LABEL[c] ?? c}</li>}</For>
      </ul>
      <Show when={props.device.missing?.length}>
        <p class="fine">Not available right now:</p>
        <ul class="missing">
          <For each={props.device.missing}>
            {(m) => (
              <li>
                <strong>{CAPABILITY_LABEL[m.capability] ?? m.capability}</strong> — {m.reason}
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={props.device.commands?.length}>
        <p class="fine">
          Commands: <span class="mono">{props.device.commands.join(", ")}</span>
        </p>
      </Show>
    </details>
  );
}

function Activity(props: { device: DeviceDetail }) {
  const s = session();
  const [items, setItems] = createSignal<ActivityEntry[] | null>(null);
  const [next, setNext] = createSignal<string | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [silicon, setSilicon] = createSignal("");
  const [sessionId, setSessionId] = createSignal("");
  const [since, setSince] = createSignal("");
  const [until, setUntil] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const filters = () => ({
    silicon_id: silicon().trim() || undefined,
    session_id: sessionId().trim() || undefined,
    since: since() ? new Date(since()).toISOString() : undefined,
    until: until() ? new Date(until()).toISOString() : undefined,
    limit: 20,
  });

  async function load(cursor?: string | null) {
    setBusy(true);
    try {
      const page = await s.client().listActivity(props.device.device_id, { ...filters(), cursor });
      setItems(cursor ? [...(items() ?? []), ...page.items] : page.items);
      setNext(page.next_cursor);
      setError(null);
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(false);
    }
  }
  onMount(() => load());
  // Reload the newest page whenever the device changes (settings, access, a session starting or ending).
  createEffect(
    on(
      () => [props.device.version, props.device.in_use?.session_id ?? null, props.device.access_count] as const,
      () => {
        if (!busy() && (items()?.length ?? 0) <= 20) load();
      },
      { defer: true },
    ),
  );

  return (
    <div class="card" data-testid="activity">
      <h2 class="card-title">Activity</h2>
      <p class="fine">Every action on this device, who did it and when. Typed text is redacted.</p>
      <form
        class="filters"
        onSubmit={(e) => {
          e.preventDefault();
          load();
        }}
      >
        <label>
          <span>Silicon</span>
          <input placeholder="si:chef" value={silicon()} onInput={(e) => setSilicon(e.currentTarget.value)} data-testid="filter-silicon" />
        </label>
        <label>
          <span>Session</span>
          <input placeholder="a3f" value={sessionId()} onInput={(e) => setSessionId(e.currentTarget.value)} data-testid="filter-session" />
        </label>
        <label>
          <span>From</span>
          <input type="datetime-local" value={since()} onInput={(e) => setSince(e.currentTarget.value)} />
        </label>
        <label>
          <span>To</span>
          <input type="datetime-local" value={until()} onInput={(e) => setUntil(e.currentTarget.value)} />
        </label>
        <div class="filter-actions">
          <Button type="submit" small busy={busy() && !next()} data-testid="filter-apply">
            Filter
          </Button>
          <Button
            small
            variant="ghost"
            onClick={() => {
              setSilicon("");
              setSessionId("");
              setSince("");
              setUntil("");
              load();
            }}
          >
            Clear
          </Button>
        </div>
      </form>
      <ErrorNote error={error()} compact />
      <Show when={items()} fallback={<Show when={!error()}><Spinner inline label="Loading activity…" /></Show>}>
        {(list) => (
          <Show when={list().length} fallback={<p class="muted">Nothing matches.</p>}>
            <ol class="activity-list">
              <For each={list()}>
                {(a) => (
                  <li class="activity-item" data-testid="activity-item">
                    <time datetime={a.at} title={dateTime(a.at)}>
                      {relativeTime(a.at)}
                    </time>
                    <span class="actor">{a.actor.id}</span>
                    <span class="action">
                      {a.action === "command" && a.command ? (
                        <span class="mono">
                          {a.command} {(a.args ?? []).join(" ")}
                        </span>
                      ) : (
                        <span data-testid="activity-summary">{activitySummary(a.action, a.details)}</span>
                      )}
                    </span>
                    <span class="activity-tail">
                      <Show when={a.outcome && a.outcome !== "ok"}>
                        <span class={`badge ${a.outcome === "failed" ? "warn" : "muted"}`}>{a.outcome}</span>
                      </Show>
                      <Show when={a.files?.length}>
                        <span class="badge muted">{plural(a.files!.length, "file")}</span>
                      </Show>
                      <Show when={a.session_id}>
                        <button type="button" class="link-button mono" title="Show only this session" onClick={() => (setSessionId(a.session_id!), load())}>
                          {a.session_id}
                        </button>
                      </Show>
                    </span>
                  </li>
                )}
              </For>
            </ol>
            <Show when={next()}>
              <Button small onClick={() => load(next())} busy={busy()} data-testid="activity-more">
                Load older
              </Button>
            </Show>
          </Show>
        )}
      </Show>
    </div>
  );
}

function Requests(props: { device: DeviceDetail }) {
  const s = session();
  const [items, setItems] = createSignal<BridgeRequest[] | null>(null);
  const [next, setNext] = createSignal<string | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  async function load(cursor?: string | null) {
    try {
      const page = await s.client().listDeviceRequests(props.device.device_id, cursor);
      setItems(cursor ? [...(items() ?? []), ...page.items] : page.items);
      setNext(page.next_cursor);
      setError(null);
    } catch (e) {
      setError(toApiError(e));
    }
  }
  onMount(() => load());
  return (
    <div class="card" data-testid="requests">
      <h2 class="card-title">Requests between Silicons</h2>
      <p class="fine">When a Silicon wants the device while another one is using it, it asks with a reason. Bridge delivers it through Ting.</p>
      <ErrorNote error={error()} compact />
      <Show when={items()} fallback={<Show when={!error()}><Spinner inline label="Loading requests…" /></Show>}>
        {(list) => (
          <Show when={list().length} fallback={<p class="muted">No Silicon has asked for this device.</p>}>
            <ul class="request-list">
              <For each={list()}>
                {(r) => (
                  <li class="request" data-testid="request">
                    <p>
                      <strong>{r.from}</strong> asked <strong>{r.to}</strong> <span class="muted">· {relativeTime(r.created_at)}</span>
                      <span class={`badge ${r.delivery === "failed" ? "warn" : "muted"}`}>{r.delivery}</span>
                    </p>
                    <blockquote>{r.reason}</blockquote>
                  </li>
                )}
              </For>
            </ul>
            <Show when={next()}>
              <Button small onClick={() => load(next())}>
                Load older
              </Button>
            </Show>
          </Show>
        )}
      </Show>
    </div>
  );
}

function DangerZone(props: { device: DeviceDetail; etag: string | null }) {
  const s = session();
  const [open, setOpen] = createSignal(false);
  const [typed, setTyped] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [children, setChildren] = createSignal<Device[]>([]);
  const matches = () => typed().trim() === props.device.name.trim();

  async function openDialog() {
    setTyped("");
    setError(null);
    setOpen(true);
    try {
      const page = await s.client().listDevices({ scope: "mine", limit: 100 });
      setChildren(page.items.filter((d) => d.host_device_id === props.device.device_id));
    } catch {
      setChildren([]);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      await s.client().removeDevice(props.device.device_id, ifMatchValue(props.etag, props.device.version));
      setOpen(false);
      toast(`Removed ${props.device.name}`);
      navigate("/devices", { replace: true });
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="card danger" data-testid="danger-zone">
      <h2 class="card-title">Remove device</h2>
      <p class="fine">Ends any session, takes access away from every Silicon and unpairs the device. The activity log stays readable. To use it again you pair it again.</p>
      <Button variant="danger" onClick={openDialog} data-testid="remove-device">
        <Trash2 size={16} aria-hidden="true" /> Remove {props.device.name}
      </Button>
      <Modal open={open()} title={`Remove ${props.device.name}?`} onClose={() => setOpen(false)} testid="remove-dialog">
        <ul class="consequences">
          <Show when={props.device.in_use}>
            <li>
              <strong>{props.device.in_use!.silicon_id}</strong> is using it now; its session ends immediately.
            </li>
          </Show>
          <li>{props.device.access_count ? `${plural(props.device.access_count, "Silicon")} lose access.` : "Every Silicon loses access."}</li>
          <li>The device unpairs and returns to its pairing screen.</li>
          <Show when={children().length}>
            <li>
              Also removed, because they pair through it: <strong>{children().map((c) => c.name).join(", ")}</strong>.
            </li>
          </Show>
        </ul>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (matches()) remove();
          }}
        >
          <label for="confirm-name">
            Type <strong class="mono">{props.device.name}</strong> to confirm
          </label>
          <input id="confirm-name" autocomplete="off" value={typed()} onInput={(e) => setTyped(e.currentTarget.value)} data-testid="remove-confirm-input" />
          <ErrorNote error={error()} compact />
          <div class="modal-actions">
            <Button variant="ghost" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" variant="danger" disabled={!matches()} busy={busy()} data-testid="remove-confirm">
              Remove device
            </Button>
          </div>
        </form>
      </Modal>
    </div>
  );
}

export type { Device };
