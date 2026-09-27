import { createEffect, createMemo, createSignal, For, on, onMount, Show } from "solid-js";
import { ArrowLeft, Check, CircleStop, Hand, Pencil, Trash2, Users } from "lucide-solid";
import { session } from "../lib/session";
import { ifMatchValue, toApiError, type ApiError } from "../lib/api";
import type { AccessGrant, ActivityEntry, ExtendRequest, Device, DeviceDetail, Takeover, TingRegistration, WakeRequest } from "../lib/types";
import { usePoll } from "../lib/poll";
import { Link, navigate } from "../lib/router";
import { DEVICE_KINDS, OS_LABEL, POLL_MS } from "../config";
import { activitySummary, awakeLabel, clock, dateTime, day, duration, plural, relativeTime, removedWhy, wakeEnd } from "../lib/format";
import { Button, DeviceIcon, ErrorNote, MemberTag, memberType, Modal, OnlineDot, Spinner, StatusDot, toast } from "../components/ui";
import { devicesChanged } from "../lib/refresh";
import { TtlSlider } from "../components/TtlSlider";
import { AccessPicker } from "../components/AccessPicker";
import { SetupSteps } from "../components/SetupSteps";
import { TingBanner } from "../components/Ting";
import { WakeBanner } from "../components/WakeRequests";

export default function DevicePage(props: { id: string }) {
  const s = session();
  const [device, setDevice] = createSignal<DeviceDetail | null>(null);
  const [etag, setEtag] = createSignal<string | null>(null);
  const [loadError, setLoadError] = createSignal<ApiError | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const [ting, setTing] = createSignal<TingRegistration[]>([]);
  const [grantTeams, setGrantTeams] = createSignal<string[]>([]);
  const isSilicon = () => s.member()?.type === "silicon";

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
  // A Carbon's device doesn't depend on the selected Team (1.1): only a Silicon's view does.
  createEffect(
    on([() => props.id, () => (isSilicon() ? s.team() : null), s.world], () => {
      setDevice(null);
      load();
    }),
  );
  // A removed device never changes again, so its page stops polling.
  usePoll(load, POLL_MS, () => !device()?.removed_at);

  /**
   * Ting types missing in the Teams of this device's grants. Read once per device, and again when its
   * grants change; a service without ting-registration shows nothing.
   */
  // A memo, so the 15 s re-read of the grants asks Ting again only when their Teams changed.
  const teamsKey = createMemo(() => grantTeams().join(","));
  createEffect(
    on([() => props.id, teamsKey, s.world], async ([, teams]) => {
      if (isSilicon()) return setTing([]);
      try {
        const all = await s.client().getTingRegistrations("any");
        const relevant = new Set((teams as string).split(",").filter(Boolean));
        setTing(all.filter((r) => r.missing_types?.length && relevant.has(r.team)));
      } catch {
        setTing([]);
      }
    }),
  );

  /** Sends a settings change with If-Match; on a stale version, reloads so the Carbon sees what changed. */
  async function patch(change: { name?: string; pair_ttl_days?: number }): Promise<ApiError | null> {
    const d = device();
    if (!d) return null;
    const started = performance.now();
    try {
      const { device: updated, etag: tag } = await s.client().updateDevice(d.device_id, change, ifMatchValue(etag(), d.version));
      setDevice({ ...d, ...updated });
      setEtag(tag);
      devicesChanged();
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
    <section class={`page device-page ${device()?.removed_at ? "removed" : ""}`} data-testid="device-page">
      <Link href="/devices" class="back-link">
        <ArrowLeft size={15} aria-hidden="true" /> All devices
      </Link>
      <ErrorNote error={loadError()} testid="device-load-error" />
      <Show when={device()} fallback={<Show when={!loadError()}><Spinner label="Loading the device…" /></Show>}>
        {(d) => (
          <Show when={!d().removed_at} fallback={<RemovedDevice device={d()} />}>
            <Header device={d()} patch={patch} />
            <SharedNote device={d()} />
            <WakeBanner device={d()} requests={d().wake_requests ?? []} onChanged={load} />
            <Show when={d().state === "setup"}>
              <div class="card">
                <h2 class="card-title">Setup isn't finished</h2>
                <p class="fine">Until it is, Silicons can't use every part of the device.</p>
                <SetupSteps device={d()} onComplete={load} />
              </div>
            </Show>
            <InUse device={d()} now={now()} onStopped={load} />
            <TingBanner registrations={ting()} />
            <Access device={d()} onChanged={load} onTeams={setGrantTeams} />
            <Settings device={d()} patch={patch} onChanged={load} />
            <Capabilities device={d()} />
            <Activity device={d()} />
            <Requests device={d()} />
            <DangerZone device={d()} etag={etag()} />
          </Show>
        )}
      </Show>
    </section>
  );
}

const KIND_WORD: Record<Device["kind"], string> = { tv: "TV", computer: "Computer", tablet: "Tablet", phone: "Phone" };

function Header(props: { device: DeviceDetail; patch?: (c: { name: string }) => Promise<ApiError | null> }) {
  const [editing, setEditing] = createSignal(false);
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);
  const d = () => props.device;
  const awake = () => awakeLabel(d());
  async function save(e: Event) {
    e.preventDefault();
    const value = name().trim();
    if (!value || value === d().name) {
      setEditing(false);
      return;
    }
    if (!props.patch) return;
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
    <header class="device-header">
      <DeviceIcon device={d()} size={26} />
      <div class="device-header-main">
        <p class="eyebrow" data-testid="device-eyebrow">
          {KIND_WORD[d().kind] ?? "Device"} · {d().removed_at ? "Removed" : d().paired_by_others ? "Also paired by another Carbon" : "Only you see it"}
        </p>
        <Show
          when={editing()}
          fallback={
            <h1 class="page-title device-title">
              <span data-testid="device-name">{d().name}</span>
              <Show when={props.patch}>
                <button class="icon-button" aria-label="Rename" title="Rename" data-testid="rename" onClick={() => (setName(d().name), setEditing(true), setError(null))}>
                  <Pencil size={16} />
                </button>
              </Show>
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
          <Show when={!d().removed_at} fallback={<span class="badge muted" data-testid="removed-badge">Removed</span>}>
            <OnlineDot online={d().online} inUse={!!d().in_use || !!d().in_use_by_other} paused={!!d().in_use?.paused} />
          </Show>
          <Show when={awake()}>
            {(a) => (
              <span class={`awake ${a().state}`} data-testid="device-awake" title={d().awake_changed_at ? `Since ${dateTime(d().awake_changed_at)}` : undefined}>
                {a().text}
                <Show when={d().awake_changed_at && d().online && d().awake !== undefined && d().awake !== null}> since {clock(d().awake_changed_at)}</Show>
              </span>
            )}
          </Show>
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
          <Show when={!d().removed_at && !d().online && d().last_seen_at}>
            <span>last seen {relativeTime(d().last_seen_at)}</span>
          </Show>
        </p>
        <ErrorNote error={error()} compact />
      </div>
    </header>
  );
}

/**
 * When other Carbons paired this device too. Each pair is separate, and each Carbon sees only their
 * own side. On a computer the terminal runs as the computer's own account, so (Carbon decision 3)
 * only Silicons given access by the Carbon who installed Extend on it get the terminal; the service
 * reports it for the others as a missing capability, which is how this page knows which it is.
 */
function SharedNote(props: { device: DeviceDetail }) {
  const d = () => props.device;
  const computer = () => d().kind === "computer" && !d().host_device_id;
  const terminal = () => {
    if (d().missing?.some((m) => m.capability === "terminal")) return "not_yours";
    if (d().capabilities?.includes("terminal")) return "yours";
    return "none";
  };
  return (
    <Show when={d().paired_by_others}>
      <div class="notice shared-note" data-testid="shared-note">
        <p class="shared-head">
          <Users size={15} aria-hidden="true" /> <strong>Another Carbon paired this {computer() ? "computer" : "device"} too.</strong>
        </p>
        <p>
          Your pair is separate: its own name, Silicons and pairing time. You see only your own Silicons and their activity; when a Silicon another Carbon gave access to is
          using it, you see only that it is in use, and you can stop it.
        </p>
        <Show
          when={computer()}
          fallback={<p data-testid="shared-device-warning">Silicons any Carbon gives access to can use this whole device, including what others leave on it.</p>}
        >
          <p data-testid="shared-computer-terminal">
            Only Silicons given access by the Carbon who installed Silicon Extend on this computer can use its terminal.{" "}
            {terminal() === "not_yours"
              ? "That isn't you, so your Silicons use the screen, the keyboard and the apps."
              : terminal() === "yours"
                ? "That's you, so your Silicons can use it; the other Carbons' Silicons use the screen, the keyboard and the apps."
                : "Silicons the other Carbons give access to use the screen, the keyboard and the apps."}
          </p>
          <p data-testid="shared-computer-warning">
            The terminal runs as the computer's own account. Share a computer only with Carbons you trust: a Silicon using it can reach what that account can, including what other
            Silicons leave on it.
          </p>
        </Show>
      </div>
    </Show>
  );
}

/**
 * A removed device, as the Carbon who paired it still sees it: when and why it was removed, and its
 * activity log and requests, read-only. Every change to it is refused by Extend, so none is offered.
 */
function RemovedDevice(props: { device: DeviceDetail }) {
  const d = () => props.device;
  const kind = () => DEVICE_KINDS.find((k) => k.os === d().os);
  return (
    <>
      <Header device={d()} />
      <div class="card removed-card" data-testid="removed-card">
        <p class="eyebrow">Removed</p>
        <p class="removed-line" data-testid="removed-why">
          Removed on {dateTime(d().removed_at)} ({relativeTime(d().removed_at)}). {removedWhy(d())}.
        </p>
        <p class="fine">
          Nothing on it can be changed or used any more, and no Silicon can reach it. Its activity log and requests below stay readable. To use the device again, pair it again.
        </p>
        <div class="removed-actions">
          <Link href={kind() ? `/devices/new?kind=${kind()!.id}` : "/devices/new"} class="button secondary" data-testid="pair-again">
            Pair it again
          </Link>
        </div>
      </div>
      <Activity device={d()} />
      <Requests device={d()} />
    </>
  );
}

function InUse(props: { device: DeviceDetail; now: number; onStopped: () => void }) {
  const s = session();
  const [busy, setBusy] = createSignal<"stop" | "done" | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [takeover, setTakeover] = createSignal<Takeover | null>(null);
  const d = () => props.device;
  /** In use on another Carbon's side (with Stop), or only a carried device the Carbon didn't pair (no Stop). */
  const other = () => (d().in_use ? null : d().in_use_by_other_carried ? "carried" : d().in_use_by_other ? "other" : null);

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
      toast(
        ended.kind === "session"
          ? `Stopped ${ended.session.silicon_id} (session ${ended.session.session_id})`
          : `Stopped the Silicon using ${props.device.name} (another Carbon gave it access)`,
      );
      devicesChanged();
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
      devicesChanged();
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
      props.onStopped();
    }
  }

  return (
    <div
      class={`card in-use-card ${d().in_use || other() === "other" ? "active" : ""} ${d().in_use?.paused ? "paused" : ""}`}
      data-testid="in-use-card"
      data-side={d().in_use ? "own" : (other() ?? "free")}
    >
      <Show
        when={d().in_use}
        fallback={
          <Show
            when={other()}
            fallback={
              <div class="in-use-idle">
                <p class="eyebrow">Not in use</p>
                <p class="muted">
                  No Silicon is using {d().name} right now.
                  <Show when={d().last_used_at}> Last used {relativeTime(d().last_used_at, props.now)}.</Show>
                </p>
              </div>
            }
          >
            <Show
              when={other() === "other"}
              fallback={
                <div class="in-use-idle" data-testid="in-use-carried">
                  <p class="eyebrow in-use-eyebrow">
                    <StatusDot status="in-use" /> A carried device is in use
                  </p>
                  <p class="in-use-line">A device this computer carries is in use.</p>
                  <p class="fine">
                    Another Carbon added it through their own pair of {d().name}, so you can't stop it from here. The Stop in {d().name}'s own Extend app stops it, and it stops
                    everything on the computer.
                  </p>
                </div>
              }
            >
              <div class="in-use-row" data-testid="in-use-other">
                <div>
                  <p class="eyebrow in-use-eyebrow">
                    <StatusDot status="in-use" /> In use
                  </p>
                  <p class="in-use-line">A Silicon another Carbon gave access to is using it.</p>
                  <p class="fine">Only one Silicon uses a device at a time, whoever gave it access. You can stop it, because the device is yours too.</p>
                </div>
                <Button variant="danger" class="stop" onClick={stop} busy={busy() === "stop"} data-testid="stop-session">
                  <CircleStop size={16} aria-hidden="true" /> Stop
                </Button>
              </div>
            </Show>
          </Show>
        }
      >
        {(u) => (
          <>
            <div class="in-use-row">
              <div>
                <p class="eyebrow in-use-eyebrow">
                  <StatusDot status="in-use" /> {u().paused ? "Paused for you" : "In use"}
                </p>
                <p class="in-use-line">
                  <MemberTag type="silicon" /> <strong data-testid="in-use-silicon">{u().silicon_id}</strong>
                  <Show when={u().team}>
                    <span class="team-chip" data-testid="in-use-team" title="The Silicon's Team">
                      {u().team}
                    </span>
                  </Show>{" "}
                  {u().paused ? "handed the device to you" : "is using it now"}
                </p>
                <p class="fine">
                  Since {clock(u().since)} ({duration(u().since, props.now)}) · session <span class="mono">{u().session_id}</span>
                </p>
              </div>
              <Button variant="danger" class="stop" onClick={stop} busy={busy() === "stop"} data-testid="stop-session">
                <CircleStop size={16} aria-hidden="true" /> Stop
              </Button>
            </div>
            <Show when={u().paused}>
              <div class="takeover" data-testid="takeover">
                <Show when={takeover()} fallback={<p class="fine">The Silicon paused its session and is waiting for you on the device.</p>}>
                  {(t) => (
                    <>
                      <p class="takeover-label eyebrow">
                        <Hand size={13} aria-hidden="true" /> It needs you to
                      </p>
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

/** The Team a grant belongs to. A 1.0 service leaves `team` out: its grants are in the device's Team. */
const grantTeam = (g: AccessGrant, device: Device, fallback: string | null) => g.team ?? device.team ?? fallback ?? "";

function Access(props: { device: DeviceDetail; onChanged: () => void; onTeams?: (teams: string[]) => void }) {
  const s = session();
  const [grants, setGrants] = createSignal<AccessGrant[] | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [confirm, setConfirm] = createSignal<AccessGrant | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  async function load() {
    try {
      const list = await s.client().listAccess(props.device.device_id);
      setGrants(list);
      setError(null);
      props.onTeams?.([...new Set(list.map((g) => grantTeam(g, props.device, s.team())))].sort());
    } catch (e) {
      setError(toApiError(e));
    }
  }
  onMount(load);
  usePoll(load, POLL_MS * 3);
  // Re-read when the device changes in a way grants follow: a rename or TTL (version), access given or
  // taken, or wake requests answered or turned off (a Silicon's wake_muted).
  // (A memo: the device object is replaced on every poll, and only a changed value should re-read.)
  const changeKey = createMemo(() => `${props.device.version}/${props.device.access_count}/${props.device.open_wake_requests}`);
  createEffect(on(changeKey, () => load(), { defer: true }));

  const key = (g: AccessGrant) => `${grantTeam(g, props.device, s.team())}\n${g.silicon_id}`;
  /** Grants by Team: the Carbon's own Teams in the menu's order, then any other Team. */
  const groups = createMemo(() => {
    const byTeam = new Map<string, AccessGrant[]>();
    for (const g of grants() ?? []) {
      const team = grantTeam(g, props.device, s.team());
      byTeam.set(team, [...(byTeam.get(team) ?? []), g]);
    }
    const order = s.teams();
    return [...byTeam.entries()]
      .sort(([a], [b]) => (order.includes(a) ? order.indexOf(a) : 1e6) - (order.includes(b) ? order.indexOf(b) : 1e6) || a.localeCompare(b))
      .map(([team, list]) => ({ team, list, reached: order.includes(team) }));
  });
  const usingNow = (g: AccessGrant) => {
    const u = props.device.in_use;
    return !!u && u.silicon_id === g.silicon_id && (!u.team || !g.team || u.team === g.team);
  };

  async function revoke(g: AccessGrant) {
    setConfirm(null);
    setBusy(`revoke:${key(g)}`);
    try {
      await s.client().revokeAccess(props.device.device_id, g.silicon_id, g.team ?? null);
      toast(`${g.silicon_id} can no longer use ${props.device.name}${g.team ? ` (in ${g.team})` : ""}`);
      devicesChanged();
      await load();
      props.onChanged();
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
    }
  }

  async function unmute(g: AccessGrant) {
    setBusy(`unmute:${key(g)}`);
    try {
      await s.client().setWakeSettings(props.device.device_id, { muted: false, silicon_id: g.silicon_id, team: g.team ?? undefined });
      toast(`${g.silicon_id} can ask you to wake ${props.device.name} again`);
      await load();
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div class="card" data-testid="access-card">
      <h2 class="card-title">Silicons with access.</h2>
      <p class="fine">Each Silicon uses the device as a member of its own Team, and what it does stays in that Team. Every Silicon with access can do the same things.</p>
      <ErrorNote error={error()} compact />
      <Show when={grants()} fallback={<Show when={!error()}><Spinner inline label="Loading access…" /></Show>}>
        {(list) => (
          <Show when={list().length} fallback={<p class="muted">No Silicon can use this device yet.</p>}>
            <For each={groups()}>
              {(group) => (
                <section class="grant-team" data-testid="grant-team" data-team={group.team}>
                  <p class="grant-team-head">
                    <span class="team-chip">{group.team}</span>
                    <span class="fine">{plural(group.list.length, "Silicon")}</span>
                  </p>
                  <Show when={!group.reached}>
                    <p class="fine sign-in-marker" data-testid="sign-in-marker">
                      Sign in to Extend for {group.team} to see names, add Silicons from it, open their files and get Tings there. You can still take access away.
                    </p>
                  </Show>
                  <ul class="grant-list">
                    <For each={group.list}>
                      {(g) => (
                        <li class="grant" data-testid="grant" data-silicon={g.silicon_id} data-team={group.team}>
                          <div>
                            <p class="grant-who">
                              <MemberTag type="silicon" />
                              <strong>{g.silicon_id}</strong>
                              <Show when={usingNow(g)}>
                                <span class="badge live">Using it now</span>
                              </Show>
                              <Show when={g.wake_muted}>
                                <span class="badge muted" data-testid="grant-wake-muted">
                                  Wake requests off
                                </span>
                              </Show>
                            </p>
                            <p class="fine">
                              Given by {g.granted_by} on {day(g.granted_at)} · last used {relativeTime(g.last_used_at)}
                              <Show when={g.wake_muted}>
                                {" · "}
                                <button type="button" class="link-button" disabled={!!busy()} onClick={() => unmute(g)} data-testid="grant-unmute">
                                  let it ask to wake again
                                </button>
                              </Show>
                            </p>
                          </div>
                          <Button
                            small
                            variant="ghost"
                            busy={busy() === `revoke:${key(g)}`}
                            data-testid="revoke"
                            onClick={() => (usingNow(g) ? setConfirm(g) : revoke(g))}
                          >
                            Take away
                          </Button>
                        </li>
                      )}
                    </For>
                  </ul>
                </section>
              )}
            </For>
          </Show>
        )}
      </Show>
      <details class="grant-more" open={grants()?.length === 0}>
        <summary>Give another Silicon access</summary>
        <AccessPicker
          deviceId={props.device.device_id}
          existing={(grants() ?? []).map((g) => ({ silicon_id: g.silicon_id, team: grantTeam(g, props.device, s.team()) }))}
          fallbackTeam={props.device.team ?? s.team()}
          onGranted={async (ids, team) => {
            toast(`${ids.join(", ")} can now use ${props.device.name}${team ? ` (in ${team})` : ""}`);
            await load();
            props.onChanged();
          }}
        />
      </details>
      <Modal open={!!confirm()} title="Take access away now?" onClose={() => setConfirm(null)} testid="revoke-confirm">
        <p>
          <strong>{confirm()?.silicon_id}</strong> is using {props.device.name} right now. Taking access away ends its session immediately.
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

function Settings(props: { device: DeviceDetail; patch: (c: { pair_ttl_days?: number }) => Promise<ApiError | null>; onChanged: () => void }) {
  const s = session();
  const [ttl, setTtl] = createSignal(props.device.pair_ttl_days ?? 14);
  const [dirty, setDirty] = createSignal(false);
  const [busy, setBusy] = createSignal<"ttl" | "wake" | null>(null);
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
  async function setWake(on: boolean) {
    setBusy("wake");
    setError(null);
    try {
      await s.client().setWakeSettings(props.device.device_id, { muted: !on });
      toast(on ? `Silicons can ask you to wake ${props.device.name}` : `Wake requests for ${props.device.name} are off`);
      props.onChanged();
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(null);
    }
  }
  return (
    <div class="card" data-testid="settings-card">
      <h2 class="card-title">Pairing.</h2>
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
      <Show when={props.device.paired_by_others}>
        <p class="fine">Any Silicon using the device, through any Carbon's pair, counts as activity for your pair too.</p>
      </Show>
      <Show when={props.device.wake_muted !== undefined && props.device.wake_muted !== null}>
        <label class="switch">
          <input type="checkbox" checked={!props.device.wake_muted} disabled={busy() === "wake"} onChange={(e) => setWake(e.currentTarget.checked)} data-testid="wake-toggle" />
          <span>Silicons can ask you to wake it</span>
        </label>
        <p class="fine">
          A Silicon that needs the screen of a device that isn't awake asks you, with a reason: the device shows it where it can, and you get it through Ting. Extend never wakes a
          device itself, and the terminal and Android debugging work either way.
        </p>
      </Show>
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
      <summary class="card-title">What a Silicon can do here.</summary>
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
      <h2 class="card-title">Activity.</h2>
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
                    <span class="actor">
                      {/* Extend itself (id "extend") acts for no member, e.g. when a pair expires: no Carbon tag. */}
                      <Show when={a.actor.id !== "extend"} fallback={<span data-testid="actor-extend">Silicon Extend</span>}>
                        <MemberTag type={a.actor.type} />
                        <span>{a.actor.id}</span>
                      </Show>
                      <Show when={a.team}>
                        <span class="team-chip" data-testid="activity-team" title="The Silicon's Team">
                          {a.team}
                        </span>
                      </Show>
                    </span>
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

/**
 * Requests for this device, from the Carbon's side:
 * - sent by their Silicons through this pair, to the Silicon using it (same Carbon and Team: it is
 *   named) or else to the Carbon who gave that Silicon access (never named to the asker);
 * - sent to the Carbon: requests routed to them because a Silicon they gave access to is using it.
 *   Carbon decision 2: they see which Silicon asked and why (a service that hides the asker says
 *   "a Silicon another Carbon gave access to");
 * - wake requests from their Silicons.
 */
function Requests(props: { device: DeviceDetail }) {
  const s = session();
  const [items, setItems] = createSignal<ExtendRequest[] | null>(null);
  const [next, setNext] = createSignal<string | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [wakes, setWakes] = createSignal<WakeRequest[] | null>(null);
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
  async function loadWakes() {
    try {
      setWakes((await s.client().listWakeRequests(props.device.device_id, { state: "all", limit: 20 })).items);
    } catch {
      // A service without wake requests (1.0) has nothing to show here.
      setWakes(null);
    }
  }
  onMount(() => {
    load();
    loadWakes();
  });
  // Re-read when the wake requests on the device change (asked, answered, expired).
  const wakesKey = createMemo(() => (props.device.wake_requests ?? []).map((r) => `${r.wake_id}:${r.asks}`).join(","));
  createEffect(on(wakesKey, () => loadWakes(), { defer: true }));

  const me = () => s.member()?.id ?? "";
  const received = (r: ExtendRequest) => r.routed_to === "carbon" && !r.to_hidden && r.to === me();
  const sent = () => (items() ?? []).filter((r) => !received(r));
  const toYou = () => (items() ?? []).filter(received);

  return (
    <div class="card" data-testid="requests">
      <h2 class="card-title">Requests.</h2>
      <p class="fine">
        When a Silicon wants a device another Silicon is using, it asks with a reason. When it needs a device that isn't awake, it asks you to wake it. Extend delivers both through Ting.
      </p>
      <ErrorNote error={error()} compact />
      <Show when={items()} fallback={<Show when={!error()}><Spinner inline label="Loading requests…" /></Show>}>
        <Show when={toYou().length}>
          <section class="request-group" data-testid="requests-received">
            <h3 class="request-group-title">Sent to you</h3>
            <ul class="request-list">
              <For each={toYou()}>
                {(r) => (
                  <li class="request" data-testid="request" data-kind="received">
                    <p class="request-who">
                      <Show when={!r.from_hidden} fallback={<span data-testid="request-from-hidden">{r.from}</span>}>
                        <MemberTag type={memberType(r.from)} />
                        <strong data-testid="request-from">{r.from}</strong>
                      </Show>
                      <Show when={r.team}>
                        <span class="team-chip">{r.team}</span>
                      </Show>{" "}
                      asked you for {props.device.name} <span class="muted">· {relativeTime(r.created_at)}</span>
                      <span class={`badge ${r.delivery === "failed" ? "warn" : "muted"}`}>{r.delivery}</span>
                    </p>
                    <blockquote>{r.reason}</blockquote>
                    <p class="fine">A Silicon you gave access to was using {props.device.name}, so the request came to you. Stop it above to free the device.</p>
                    <Show when={r.last_error}>
                      <p class="fine warn-text">{r.last_error}</p>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </section>
        </Show>
        <section class="request-group" data-testid="requests-sent">
          <Show when={toYou().length || wakes()?.length}>
            <h3 class="request-group-title">Sent by your Silicons</h3>
          </Show>
          <Show when={sent().length} fallback={<p class="muted">{toYou().length || wakes()?.length ? "None." : "No Silicon has asked for this device."}</p>}>
            <ul class="request-list">
              <For each={sent()}>
                {(r) => (
                  <li class="request" data-testid="request" data-kind="sent">
                    <p class="request-who">
                      <MemberTag type={memberType(r.from)} />
                      <strong>{r.from}</strong>
                      <Show when={r.team}>
                        <span class="team-chip">{r.team}</span>
                      </Show>{" "}
                      asked <Show when={!r.to_hidden} fallback={<span data-testid="request-to-hidden">{r.to}</span>}>
                        <strong>{r.to}</strong>
                      </Show>{" "}
                      <span class="muted">· {relativeTime(r.created_at)}</span>
                      <span class={`badge ${r.delivery === "failed" ? "warn" : "muted"}`}>{r.delivery}</span>
                    </p>
                    <blockquote>{r.reason}</blockquote>
                    <Show when={r.last_error}>
                      <p class="fine warn-text">{r.last_error}</p>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </section>
        <Show when={next()}>
          <Button small onClick={() => load(next())}>
            Load older
          </Button>
        </Show>
      </Show>
      <Show when={wakes()?.length}>
        <section class="request-group" data-testid="requests-wake">
          <h3 class="request-group-title">Asked to wake it</h3>
          <ul class="request-list">
            <For each={wakes()!}>
              {(w) => (
                <li class="request" data-testid="wake-history" data-state={w.state}>
                  <p class="request-who">
                    <MemberTag type="silicon" />
                    <strong>{w.from}</strong>
                    <span class="team-chip">{w.team}</span> asked you to wake it <span class="muted">· {relativeTime(w.last_asked_at)}</span>
                    <span class={`badge ${w.state === "open" ? "live" : "muted"}`}>{w.state}</span>
                  </p>
                  <blockquote>{w.reason}</blockquote>
                  <Show when={w.state !== "open" && w.end_reason}>
                    <p class="fine">
                      Ended {relativeTime(w.ended_at)}: {wakeEnd(w.end_reason)}.
                    </p>
                  </Show>
                  <Show when={w.answer_ting_last_error}>
                    <p class="fine warn-text">{w.answer_ting_last_error}</p>
                  </Show>
                </li>
              )}
            </For>
          </ul>
        </section>
      </Show>
    </div>
  );
}

/**
 * Remove device. The copy says exactly what DELETE /devices/{id} does (domain::unpair): the running
 * session ends, every Silicon's access goes, devices paired through it are removed with it, a device
 * with its own Extend app is told to unpair (at once, or when it next connects), a device paired
 * through a computer is dropped by that computer, and the activity log stays readable under Removed.
 */
function DangerZone(props: { device: DeviceDetail; etag: string | null }) {
  const s = session();
  const [open, setOpen] = createSignal(false);
  const [typed, setTyped] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [children, setChildren] = createSignal<Device[]>([]);
  const [hostName, setHostName] = createSignal<string | null>(null);
  const matches = () => typed().trim() === props.device.name.trim();
  const d = () => props.device;

  async function openDialog() {
    setTyped("");
    setError(null);
    setOpen(true);
    try {
      const page = await s.client().listDevices({ scope: "mine", limit: 100 });
      setChildren(page.items.filter((x) => x.host_device_id === d().device_id));
      setHostName(page.items.find((x) => x.device_id === d().host_device_id)?.name ?? null);
    } catch {
      setChildren([]);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      await s.client().removeDevice(d().device_id, ifMatchValue(props.etag, d().version));
      setOpen(false);
      toast(`Removed ${d().name}. Its activity log is under Removed.`);
      devicesChanged();
      navigate("/devices", { replace: true });
    } catch (e) {
      setError(toApiError(e));
    } finally {
      setBusy(false);
    }
  }

  const access = () => d().access_count ?? 0;

  return (
    <div class="card danger" data-testid="danger-zone">
      <h2 class="card-title">Remove device.</h2>
      <p class="fine">
        Ends any session, takes access away from every Silicon and unpairs the device, and any device paired through it. Its activity log stays readable under Removed. To use it
        again, pair it again.
      </p>
      <Button variant="danger" class="quiet" onClick={openDialog} data-testid="remove-device">
        <Trash2 size={16} aria-hidden="true" /> Remove {d().name}
      </Button>
      <Modal open={open()} title={`Remove ${d().name}?`} onClose={() => setOpen(false)} testid="remove-dialog">
        <ul class="consequences" data-testid="remove-consequences">
          <Show when={d().in_use}>
            {(u) => (
              <li>
                <strong>{u().silicon_id}</strong> {u().paused ? "handed it to you and is waiting" : "is using it now"}; its session ends immediately.
              </li>
            )}
          </Show>
          <Show when={d().paired_by_others}>
            <li data-testid="remove-others">Only your pair ends. The other Carbons who paired it keep theirs, with their own Silicons.</li>
          </Show>
          <Show when={access() > 0}>
            <li data-testid="remove-access">{access() === 1 ? "1 Silicon loses access." : `${access()} Silicons lose access.`}</li>
          </Show>
          <Show
            when={d().host_device_id}
            fallback={
              <li data-testid="remove-unpair">
                {d().online
                  ? "The Extend app on it unpairs now and shows a new pairing code."
                  : "It's offline, so the Extend app on it unpairs the next time it connects, then shows a new pairing code."}
              </li>
            }
          >
            <li data-testid="remove-unpair">Extend stops reaching it through {hostName() ?? d().host_device_id}.</li>
          </Show>
          <Show when={children().length}>
            <li data-testid="remove-children">
              Also removed, because they pair through it: <strong>{children().map((c) => c.name).join(", ")}</strong>.
            </li>
          </Show>
          <li>Its activity log stays readable: find it under Removed in your device list. To use the device again, pair it again.</li>
        </ul>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (matches()) remove();
          }}
        >
          <label for="confirm-name">
            Type <strong class="mono">{d().name}</strong> to confirm
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
