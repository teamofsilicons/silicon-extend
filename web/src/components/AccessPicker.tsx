import { createEffect, createResource, createSignal, For, on, Show } from "solid-js";
import { Plus } from "lucide-solid";
import { session } from "../lib/session";
import { ApiError, toApiError } from "../lib/api";
import { parseSiliconIds } from "../lib/pairing";
import type { TeamSilicon } from "../lib/types";
import { Button, ErrorNote } from "./ui";

/** One Team's Silicons, or why they couldn't be listed. */
interface TeamRoster {
  /** "team": the Team's Silicons from Extend. "known": the fallback, Silicons already on the Carbon's devices. */
  source: "team" | "known";
  items: TeamSilicon[];
  error: ApiError | null;
}

/**
 * Who can be given access, per Team. One read of GET /team/silicons?team=any lists every Team the
 * Carbon's login reaches (1.1), each tagged, with the Teams that couldn't be read and why. A service
 * without team=any answers only the selected Team; then each Team is read on its own (X-Org-ID set to
 * it, which the login reaches). If that fails too, Silicons already using the Carbon's other devices
 * are suggested. Typing an id always works; Extend checks it when access is given.
 */
async function loadRosters(teams: string[]): Promise<Map<string, TeamRoster>> {
  const client = session().client();
  const rosters = new Map<string, TeamRoster>();
  try {
    const all = await client.listAllTeamSilicons();
    if (all.across) {
      for (const team of teams) rosters.set(team, { source: "team", items: [], error: null });
      for (const m of all.items) {
        if (!m.team) continue;
        const roster = rosters.get(m.team) ?? { source: "team", items: [], error: null };
        roster.items.push(m);
        rosters.set(m.team, roster);
      }
      for (const reach of all.teams)
        if (!reach.ok) rosters.set(reach.team, { source: "team", items: [], error: reach.error ? new ApiError(0, reach.error) : null });
      return rosters;
    }
  } catch {
    /* read each Team on its own below */
  }
  await Promise.all(
    teams.map(async (team) => {
      try {
        rosters.set(team, { source: "team", items: await client.listTeamSilicons(team), error: null });
      } catch (e) {
        rosters.set(team, { source: "known", items: [], error: toApiError(e) });
      }
    }),
  );
  if ([...rosters.values()].some((r) => r.source === "known")) {
    const known = await knownSilicons().catch(() => []);
    for (const r of rosters.values()) if (r.source === "known") r.items = known;
  }
  return rosters;
}

async function knownSilicons(): Promise<TeamSilicon[]> {
  const client = session().client();
  const page = await client.listDevices({ scope: "mine", limit: 50 });
  const ids = new Set<string>();
  for (const d of page.items) if (d.in_use) ids.add(d.in_use.silicon_id);
  const lists = await Promise.allSettled(page.items.slice(0, 12).map((d) => client.listAccess(d.device_id)));
  for (const r of lists) if (r.status === "fulfilled") for (const g of r.value) ids.add(g.silicon_id);
  return [...ids].sort().map((id) => ({ id }));
}

export function AccessPicker(props: {
  deviceId: string;
  /** Grants already on the device; a Silicon is hidden from its own Team's suggestions. */
  existing?: { silicon_id: string; team?: string | null }[];
  /** The Team a grant without `team` belongs to (a 1.0 service's grants are in the device's Team). */
  fallbackTeam?: string | null;
  onGranted: (ids: string[], team: string) => void;
  submitLabel?: string;
}) {
  const s = session();
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [errors, setErrors] = createSignal<ApiError[]>([]);
  // The Team menu's Team is the default grant Team; the picker can choose any of the Carbon's Teams.
  const [team, setTeam] = createSignal<string>(s.team() ?? s.teams()[0] ?? "");
  createEffect(on(s.team, (t) => t && setTeam(t), { defer: true }));
  const [rosters] = createResource(
    () => ({ key: `${s.world().kind}:${s.member()?.id ?? ""}`, teams: s.teams() }),
    ({ teams }) => loadRosters(teams).catch(() => new Map<string, TeamRoster>()),
  );

  const roster = () => rosters()?.get(team()) ?? null;
  const parsed = () => parseSiliconIds(input());
  const granted = (id: string) => (props.existing ?? []).some((g) => g.silicon_id === id && (g.team ?? props.fallbackTeam ?? team()) === team());
  const offer = () => (roster()?.items ?? []).filter((m) => !granted(m.id) && !parsed().ids.includes(m.id));

  function add(id: string) {
    const current = input().trim();
    setInput(current ? `${current}, ${id}` : id);
  }

  async function submit(event: Event) {
    event.preventDefault();
    const { ids, invalid } = parsed();
    setErrors([]);
    if (invalid.length) {
      setErrors([
        new ApiError(0, {
          code: "invalid_input",
          message: `${invalid.join(", ")} ${invalid.length === 1 ? "is not a Silicon id" : "are not Silicon ids"}. Silicon ids look like si:chef.`,
          hint: "Only Silicons can be given access. Ask the Silicon for its id, or find it in Silicon IAM.",
        }),
      ]);
      return;
    }
    if (!ids.length) {
      setErrors([new ApiError(0, { code: "invalid_input", message: "Type at least one Silicon id, like si:chef.", hint: "Separate several with commas or spaces." })]);
      return;
    }
    const inTeam = team();
    setBusy(true);
    const results = await Promise.allSettled(ids.map((id) => s.client().grantAccess(props.deviceId, id, inTeam || null)));
    setBusy(false);
    const done: string[] = [];
    const failed: ApiError[] = [];
    results.forEach((r, i) => {
      if (r.status === "fulfilled") done.push(ids[i]);
      else failed.push(toApiError(r.reason));
    });
    setErrors(failed);
    setInput(ids.filter((id) => !done.includes(id)).join(", "));
    if (done.length) props.onGranted(done, inTeam);
  }

  return (
    <form class="access-picker" onSubmit={submit} data-testid="access-picker">
      <div class="access-team">
        <label for={`grant-team-${props.deviceId}`}>Team</label>
        <Show when={s.teams().length > 1} fallback={<span class="access-team-only" data-testid="grant-team-only">{team()}</span>}>
          <select id={`grant-team-${props.deviceId}`} value={team()} onChange={(e) => setTeam(e.currentTarget.value)} data-testid="grant-team-select">
            <For each={s.teams()}>{(t) => <option value={t}>{t}</option>}</For>
          </select>
        </Show>
      </div>
      <label for={`grant-${props.deviceId}`}>Silicon ids</label>
      <div class="input-row">
        <input
          id={`grant-${props.deviceId}`}
          placeholder="si:chef, si:scout"
          autocomplete="off"
          spellcheck={false}
          value={input()}
          data-testid="grant-input"
          onInput={(e) => setInput(e.currentTarget.value)}
        />
        <Button type="submit" variant="primary" busy={busy()} data-testid="grant-submit">
          {props.submitLabel ?? "Give access"}
        </Button>
      </div>
      <Show when={roster()?.error && roster()?.source === "team"}>
        <p class="fine warn-text" data-testid="roster-error">
          Couldn't list the Silicons in {team()}: {roster()!.error!.message} You can still type an id.
        </p>
      </Show>
      <Show when={offer().length}>
        <div class="chips" aria-label={roster()?.source === "team" ? `Silicons in ${team()}` : "Silicons that use your other devices"} data-roster={roster()?.source}>
          <span class="fine">{roster()?.source === "team" ? `Silicons in ${team()}:` : "Using your other devices:"}</span>
          <For each={offer()}>
            {(m) => (
              <button type="button" class="chip" onClick={() => add(m.id)} data-testid="grant-suggestion" title={m.display_name ?? undefined}>
                <Plus size={13} aria-hidden="true" /> {m.id}
                <Show when={m.display_name}>
                  <span class="chip-name">{m.display_name}</span>
                </Show>
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show when={roster()?.source === "team" && roster()!.items.length > 0 && offer().length === 0 && !parsed().ids.length}>
        <p class="fine">Every Silicon in {team()} already has access.</p>
      </Show>
      <p class="fine">
        They must be Silicons in {team() || "the Team you pick"}, and they use the device as members of it. Separate several ids with commas or spaces; a bare handle like <code>chef</code> means{" "}
        <code>si:chef</code>. Don't see a Team? Sign in to Extend again and select it in Silicon IAM.
      </p>
      <For each={errors()}>{(e) => <ErrorNote error={e} compact />}</For>
    </form>
  );
}
