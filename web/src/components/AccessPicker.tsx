import { createResource, createSignal, For, Show } from "solid-js";
import { Plus } from "lucide-solid";
import { session } from "../lib/session";
import { ApiError, toApiError } from "../lib/api";
import { parseSiliconIds } from "../lib/pairing";
import type { TeamSilicon } from "../lib/types";
import { Button, ErrorNote } from "./ui";

interface Roster {
  /** "team": every Silicon in the team, from GET /api/v1/team/silicons. "known": the fallback. */
  source: "team" | "known";
  items: TeamSilicon[];
}

/**
 * Who can be given access: the team's Silicons from Extend. If that endpoint isn't available (an
 * older service), Silicons already using the Carbon's other devices are suggested instead. Typing an
 * id always works; Extend checks it when access is given.
 */
async function loadRoster(team: string | null): Promise<Roster> {
  if (!team) return { source: "known", items: [] };
  const client = session().client();
  try {
    return { source: "team", items: await client.listTeamSilicons() };
  } catch {
    const page = await client.listDevices({ scope: "mine", limit: 50 });
    const ids = new Set<string>();
    for (const d of page.items) if (d.in_use) ids.add(d.in_use.silicon_id);
    const lists = await Promise.allSettled(page.items.slice(0, 12).map((d) => client.listAccess(d.device_id)));
    for (const r of lists) if (r.status === "fulfilled") for (const g of r.value) ids.add(g.silicon_id);
    return { source: "known", items: [...ids].sort().map((id) => ({ id })) };
  }
}

export function AccessPicker(props: {
  deviceId: string;
  /** Silicons that already have access; hidden from suggestions. */
  existing?: string[];
  onGranted: (ids: string[]) => void;
  submitLabel?: string;
}) {
  const s = session();
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [errors, setErrors] = createSignal<ApiError[]>([]);
  const [roster] = createResource(() => s.team(), (team) => loadRoster(team).catch((): Roster => ({ source: "known", items: [] })));

  const parsed = () => parseSiliconIds(input());
  const offer = () => (roster()?.items ?? []).filter((m) => !(props.existing ?? []).includes(m.id) && !parsed().ids.includes(m.id));

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
    setBusy(true);
    const results = await Promise.allSettled(ids.map((id) => s.client().grantAccess(props.deviceId, id)));
    setBusy(false);
    const granted: string[] = [];
    const failed: ApiError[] = [];
    results.forEach((r, i) => {
      if (r.status === "fulfilled") granted.push(ids[i]);
      else failed.push(toApiError(r.reason));
    });
    setErrors(failed);
    setInput(ids.filter((id) => !granted.includes(id)).join(", "));
    if (granted.length) props.onGranted(granted);
  }

  return (
    <form class="access-picker" onSubmit={submit} data-testid="access-picker">
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
      <Show when={offer().length}>
        <div class="chips" aria-label={roster()?.source === "team" ? `Silicons in ${s.team()}` : "Silicons that use your other devices"} data-roster={roster()?.source}>
          <span class="fine">{roster()?.source === "team" ? `Silicons in ${s.team()}:` : "Using your other devices:"}</span>
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
        <p class="fine">Every Silicon in {s.team()} already has access.</p>
      </Show>
      <p class="fine">They must be Silicons in this team. Separate several ids with commas or spaces; a bare handle like <code>chef</code> means <code>si:chef</code>.</p>
      <For each={errors()}>{(e) => <ErrorNote error={e} compact />}</For>
    </form>
  );
}
