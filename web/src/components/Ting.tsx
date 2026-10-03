import { For, Show } from "solid-js";
import { BellOff } from "lucide-solid";
import type { TingRegistration } from "../lib/types";
import { registerCommand, tingType } from "../lib/ting";
import { Link } from "../lib/router";
import { CopyText } from "./ui";

/** App types Ting reported missing on sends to this Team, with owning-Team manager guidance. */
export function MissingTingTypes(props: { registration: TingRegistration; compact?: boolean; inSettings?: boolean }) {
  const r = () => props.registration;
  return (
    <div class="ting-missing" data-testid="ting-missing" data-team={r().team}>
      <p>
        Ting reported {r().missing_types.length === 1 ? "a missing app notification type" : `${r().missing_types.length} missing app notification types`} while sending to{" "}
        <strong>{r().team}</strong>. A Ting manager in the Team that owns Extend registers {r().missing_types.length === 1 ? "it" : "them"} once for every delivery Team.
        Replace <code>&lt;owning-team&gt;</code> below with that Team. Turning your notifications on does not register app types.
      </p>
      <ul class="ting-commands">
        <For each={r().missing_types}>
          {(name) => (
            <li>
              <Show when={!props.compact}>
                <span class="fine">{tingType(name)?.description ?? name}</span>
              </Show>
              <CopyText text={registerCommand(name)} label={`Copy the command that registers ${name} in Extend's owning Team`} />
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}

/**
 * Missing notification types for this device in the selected organization.
 */
export function TingBanner(props: { registrations: TingRegistration[] }) {
  return (
    <Show when={props.registrations.length}>
      <div class="card ting-banner" data-testid="ting-banner">
        <p class="eyebrow ting-eyebrow">
          <BellOff size={13} aria-hidden="true" /> Ting
        </p>
        <For each={props.registrations}>{(r) => <MissingTingTypes registration={r} compact />}</For>
        <p class="fine">
          Until then, wake requests and requests for this device still show here. Manage this organization in <Link href="/settings">Settings</Link>.
        </p>
      </div>
    </Show>
  );
}
