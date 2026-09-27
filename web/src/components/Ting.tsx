import { For, Show } from "solid-js";
import { BellOff } from "lucide-solid";
import type { TingRegistration } from "../lib/types";
import { registerCommand, tingType } from "../lib/ting";
import { Link } from "../lib/router";
import { CopyText } from "./ui";

/**
 * Extend's Ting types a Team is missing, with the exact command its Ting manager runs for each.
 * Ting refuses a notification whose type isn't registered in the Team it is sent in, so until they
 * are, requests and wake requests there reach nobody through Ting (the website still shows them).
 */
export function MissingTingTypes(props: { registration: TingRegistration; compact?: boolean; inSettings?: boolean }) {
  const r = () => props.registration;
  return (
    <div class="ting-missing" data-testid="ting-missing" data-team={r().team}>
      <p>
        Ting doesn't know {r().missing_types.length === 1 ? "one of Extend's notification types" : `${r().missing_types.length} of Extend's notification types`} in{" "}
        <strong>{r().team}</strong>, so Tings of {r().missing_types.length === 1 ? "that type" : "those types"} can't be sent there. If you are a Ting manager of {r().team}, Extend
        registers them with your login when you choose Turn on{props.inSettings ? "" : " in Settings"}. Otherwise a Ting manager of {r().team} runs:
      </p>
      <ul class="ting-commands">
        <For each={r().missing_types}>
          {(name) => (
            <li>
              <Show when={!props.compact}>
                <span class="fine">{tingType(name)?.description ?? name}</span>
              </Show>
              <CopyText text={registerCommand(r().team, name)} label={`Copy the command that registers ${name} in ${r().team}`} />
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}

/**
 * The device page's Ting banner: the Teams of this device's grants where Extend's Ting types are
 * missing. Settings shows every Team, with Turn on.
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
          Until then, wake requests and requests for this device still show here. See every Team in <Link href="/settings">Settings</Link>.
        </p>
      </div>
    </Show>
  );
}
