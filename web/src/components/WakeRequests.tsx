import { createSignal, For, Show } from "solid-js";
import { AlarmClock, Check, X } from "lucide-solid";
import { session } from "../lib/session";
import { toApiError, type ApiError } from "../lib/api";
import type { DeviceDetail, WakeRequest } from "../lib/types";
import { awakeLabel, clock, relativeTime } from "../lib/format";
import { Button, ErrorNote, MemberTag, toast } from "./ui";

/** Whether the device itself showed the request, in words. */
export function deviceNotice(r: Pick<WakeRequest, "device_notice" | "device_notice_note">): string {
  switch (r.device_notice) {
    case "shown":
      return "The device showed it";
    case "sent":
      return "Sent to the device";
    case "not_shown":
      return `The device couldn't show it${r.device_notice_note ? `: ${r.device_notice_note}` : ""}`;
    case "offline":
      return "The device was offline, so it couldn't show it";
    case "unsupported":
      return "This device can't show it itself";
    default:
      return r.device_notice_note ?? String(r.device_notice);
  }
}

/** Where the Ting to the Carbon is. Null when no Ting goes for it. */
export function tingToCarbon(r: Pick<WakeRequest, "ting">): string | null {
  switch (r.ting) {
    case "delivered":
      return "You were told through Ting";
    case "pending":
      return "Telling you through Ting";
    case "deferred":
      return "Its Ting waits: you already got 6 wake Tings in the last hour, so it goes when that frees up";
    case "covered":
      return "No new Ting: you were told about this device in the last 15 minutes";
    case "failed":
      return "The Ting to you couldn't be delivered";
    case null:
    case undefined:
      return null;
    default:
      return `Ting: ${r.ting}`;
  }
}

/**
 * The wake banner on a device's page: the open requests from the Carbon's Silicons to wake it, with
 * who asked (and in which Team), why, until when, whether the device showed it, and the Ting.
 *
 * "It's awake" is a fact about the device: it answers every open request to wake it, including those
 * that came through other Carbons' pairs, and each asking Silicon is told in its own Team. "Decline"
 * answers only this Carbon's own side. Extend itself never wakes a device.
 */
export function WakeBanner(props: { device: DeviceDetail; requests: WakeRequest[]; onChanged: () => void }) {
  const s = session();
  const client = s.client();
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const d = () => props.device;
  const open = () => props.requests.filter((r) => r.state === "open");
  const hosts = () => {
    const seen = new Map<string, { name: string; online: boolean }>();
    for (const r of open()) if (r.host) seen.set(r.host.device_id, r.host);
    return [...seen.values()];
  };
  const detectable = () => open().every((r) => r.wake_detectable) && d().wake_detectable !== false;
  const state = () => awakeLabel(d());

  async function act(key: string, run: () => Promise<string>) {
    setBusy(key);
    setError(null);
    try {
      toast(await run());
      props.onChanged();
    } catch (e) {
      setError(toApiError(e));
      props.onChanged();
    } finally {
      setBusy(null);
    }
  }

  const woken = () =>
    act("woken", async () => {
      await client.answerWake(d().device_id, "woken");
      return `Told every Silicon that asked: ${d().name} is awake`;
    });
  const decline = (ids?: string[]) =>
    act(ids?.length ? `decline:${ids[0]}` : "decline", async () => {
      const answered = await client.answerWake(d().device_id, "declined", ids);
      const who = [...new Set(answered.ended.map((r) => r.from))];
      return who.length ? `Declined: ${who.join(", ")} ${who.length === 1 ? "was" : "were"} told` : "Declined";
    });
  const muteDevice = () =>
    act("mute", async () => {
      await client.setWakeSettings(d().device_id, { muted: true });
      return `Wake requests for ${d().name} are off. Turn them back on under Pairing.`;
    });
  const muteSilicon = (r: WakeRequest) =>
    act(`mute:${r.wake_id}`, async () => {
      await client.setWakeSettings(d().device_id, { muted: true, silicon_id: r.from, team: r.team });
      return `${r.from} can't ask you to wake ${d().name} any more (in ${r.team})`;
    });

  return (
    <Show when={open().length}>
      <div class="card wake-banner" data-testid="wake-banner">
        <p class="eyebrow wake-eyebrow">
          <AlarmClock size={13} aria-hidden="true" /> Asked to wake it
        </p>
        <h2 class="card-title">
          {open().length === 1 ? `${open()[0].from} asks you to wake ${d().name}.` : `${open().length} Silicons ask you to wake ${d().name}.`}
        </h2>
        <p class="fine">
          <Show when={state()}>{(st) => <>Right now: {st().text.toLowerCase()}. </>}</Show>
          Extend never wakes a device itself: turn it on or unlock it, and every Silicon that asked is told.
          <Show when={!detectable()}> Extend can't tell when this {d().kind === "tv" ? "TV" : d().kind} wakes, so choose It's awake once it is.</Show>
        </p>
        <Show when={hosts().length}>
          <p class="notice wake-host" data-testid="wake-host">
            {d().name} pairs through {hosts().map((h) => h.name).join(" and ")}: wake {hosts().length === 1 ? "that computer" : "those computers"} too
            {hosts().some((h) => !h.online) ? " (it's offline now)" : ""}.
          </p>
        </Show>
        <ul class="wake-list">
          <For each={open()}>
            {(r) => (
              <li class="wake-request" data-testid="wake-request" data-silicon={r.from}>
                <p class="request-who">
                  <MemberTag type="silicon" />
                  <strong>{r.from}</strong>
                  <span class="team-chip" title="The Silicon's Team">
                    {r.team}
                  </span>
                  <span class="muted">
                    {" "}
                    · asked {relativeTime(r.last_asked_at)}
                    {r.asks > 1 ? ` (${r.asks} times)` : ""} · ends at {clock(r.expires_at)} if nobody wakes it
                  </span>
                </p>
                <blockquote>{r.reason}</blockquote>
                <p class="fine" data-testid="wake-delivery">
                  {deviceNotice(r)}
                  <Show when={tingToCarbon(r)}>{(t) => <>. {t()}</>}</Show>.
                </p>
                <Show when={r.ting_last_error}>
                  <p class="fine warn-text">{r.ting_last_error}</p>
                </Show>
                <div class="wake-row-actions">
                  <Button small variant="ghost" busy={busy() === `decline:${r.wake_id}`} onClick={() => decline([r.wake_id])} data-testid="wake-decline-one">
                    Decline
                  </Button>
                  <Button small variant="ghost" busy={busy() === `mute:${r.wake_id}`} onClick={() => muteSilicon(r)} data-testid="wake-mute-silicon">
                    Turn off for {r.from}
                  </Button>
                </div>
              </li>
            )}
          </For>
        </ul>
        <div class="wake-actions">
          <Button variant="primary" busy={busy() === "woken"} onClick={woken} data-testid="wake-awake">
            <Check size={16} aria-hidden="true" /> It's awake
          </Button>
          <Button busy={busy() === "decline"} onClick={() => decline()} data-testid="wake-decline">
            <X size={16} aria-hidden="true" /> Decline {open().length > 1 ? "all" : ""}
          </Button>
          <Button variant="ghost" busy={busy() === "mute"} onClick={muteDevice} data-testid="wake-mute">
            Turn off wake requests
          </Button>
        </div>
        <p class="fine" data-testid="wake-awake-note">
          It's awake answers every open request to wake {d().name}, including ones that came through other Carbons who paired it. Decline answers only yours.
        </p>
        <ErrorNote error={error()} compact testid="wake-error" />
      </div>
    </Show>
  );
}
