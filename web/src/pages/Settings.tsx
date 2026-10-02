import { createEffect, createSignal, For, on, Show } from "solid-js";
import { Bell, FlaskConical } from "lucide-solid";
import { session } from "../lib/session";
import { apiBaseUrl, LINKS } from "../config";
import { navigate } from "../lib/router";
import { toApiError, type ApiError } from "../lib/api";
import type { TingRegistration } from "../lib/types";
import { Button, ErrorNote, MemberTag, Spinner, toast } from "../components/ui";
import { TestingSecretForm } from "../components/TestingSecretForm";
import { PermissionSettings } from "../components/PermissionSettings";
import { SignOutButton, signOutEffect } from "../components/SignOut";
import { MissingTingTypes } from "../components/Ting";
import { applyTheme, currentTheme, type Theme } from "../lib/theme";

export default function Settings() {
  const s = session();
  const [theme, setTheme] = createSignal<Theme>(currentTheme());
  const [leaving, setLeaving] = createSignal(false);
  const world = () => s.world();

  return (
    <section class="page-main narrow" data-testid="settings-page">
      <header class="page-heading">
        <div>
          <p class="eyebrow">This browser · your account</p>
          <h1 class="page-title">Settings.</h1>
          <p class="lead">Your account, feature access, Ting notifications, test environments and appearance.</p>
        </div>
      </header>

      <div class="card">
        <h2 class="card-title">Account.</h2>
        <Show when={s.member()} fallback={<p class="muted">Not signed in{world().kind === "testing" ? " in this test environment" : ""}.</p>}>
          {(m) => (
            <>
              <p class="account-line">
                <MemberTag type={m().type} />
                Signed in as <strong>{m().id}</strong>
                {m().display_name ? ` (${m().display_name})` : ""}, a {m().type === "carbon" ? "Carbon" : "Silicon"}
                {world().kind === "testing" ? " in the test environment" : ""}.
              </p>
              <p class="fine">
                Teams: {s.teams().join(", ")}.{" "}
                {m().type === "silicon"
                  ? "The Team menu at the top chooses the Team you use devices in."
                  : "Your device list shows every device you paired, whichever Team is selected. The Team menu at the top is your default Team when you give Silicons access."}
              </p>
              <SignOutButton />
              <p class="fine sign-out-note" data-testid="settings-sign-out-note">
                {signOutEffect(m().type)}
              </p>
            </>
          )}
        </Show>
      </div>

      <Show when={s.member()}>
        <PermissionSettings />
        <TingSettings />
      </Show>

      <div class="card" data-testid="settings-testing">
        <h2 class="card-title">
          <FlaskConical size={17} aria-hidden="true" /> Test environment.
        </h2>
        <Show
          when={world().kind === "testing" ? (world() as Extract<ReturnType<typeof world>, { kind: "testing" }>) : null}
          fallback={
            <>
              <p class="fine">
                You are using production. To try pairing, access and sessions without touching real data, enter a test application's secret. Test environments are created and managed in Honeycomb.
              </p>
              <TestingSecretForm />
            </>
          }
        >
          {(w) => (
            <>
              <p>
                In <strong>{w().environment.name}</strong> ({w().environment.state})
                <Show when={w().environment.paired_devices !== undefined}>
                  , {w().environment.paired_devices} of {w().environment.device_limit ?? 5} devices paired
                </Show>
                .
              </p>
              <p class="fine">Environment id {w().environment.environment_id}</p>
              <Button
                busy={leaving()}
                onClick={async () => {
                  setLeaving(true);
                  await s.exitTesting();
                  setLeaving(false);
                  navigate(s.pair() ? "/devices" : "/", { replace: true });
                }}
              >
                Exit testing
              </Button>
            </>
          )}
        </Show>
      </div>

      <div class="card">
        <h2 class="card-title">Telemetry.</h2>
        <label class="switch">
          <input type="checkbox" checked={!s.telemetryOff()} onChange={(e) => s.setTelemetry(e.currentTarget.checked)} data-testid="telemetry-toggle" />
          <span>Send usage events to help fix problems</span>
        </label>
        <p class="fine">
          Events say which step ran, whether it worked, how long it took and the error code. Never names, codes, tokens or anything on your screens. When off, the website sends none and tells Extend with{" "}
          <code>X-Extend-Telemetry: off</code> on every request. Saved in this browser.
        </p>
      </div>

      <div class="card">
        <h2 class="card-title">Appearance.</h2>
        <div class="segmented" role="radiogroup" aria-label="Theme">
          {(["auto", "light", "dark"] as Theme[]).map((t) => (
            <button
              role="radio"
              aria-checked={theme() === t}
              class={theme() === t ? "selected" : ""}
              onClick={() => {
                applyTheme(t);
                setTheme(t);
              }}
            >
              {t === "auto" ? "Match system" : t === "light" ? "Light" : "Dark"}
            </button>
          ))}
        </div>
      </div>

      <div class="card">
        <h2 class="card-title">About.</h2>
        <dl class="about">
          <dt>Extend service</dt>
          <dd class="mono">{apiBaseUrl() || `${location.origin} (same origin)`}</dd>
          <dt>Website version</dt>
          <dd class="mono">{__APP_VERSION__}</dd>
          <dt>Source</dt>
          <dd>
            <a href={LINKS.repository} target="_blank" rel="noopener noreferrer">
              {LINKS.repository.replace("https://", "")}
            </a>
          </dd>
        </dl>
      </div>
    </section>
  );
}

/**
 * Ting notifications, one row per Team: whether Extend's Tings reach you there (with Turn on), and
 * which of Extend's Ting types Ting doesn't know there yet, with the exact command for that Team's
 * Ting manager. A Team your login doesn't reach says "Sign in to Extend for <team>".
 */
function TingSettings() {
  const s = session();
  const [rows, setRows] = createSignal<TingRegistration[] | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [rowErrors, setRowErrors] = createSignal<Record<string, ApiError>>({});
  async function load() {
    try {
      const list = await s.client().getTingRegistrations("any");
      const order = s.teams();
      setRows(list.sort((a, b) => (order.includes(a.team) ? order.indexOf(a.team) : 1e6) - (order.includes(b.team) ? order.indexOf(b.team) : 1e6) || a.team.localeCompare(b.team)));
      setError(null);
    } catch (e) {
      setError(toApiError(e));
    }
  }
  createEffect(on([() => s.member()?.id, s.world], () => load()));

  async function turnOn(team: string) {
    setBusy(team);
    setRowErrors(({ [team]: _gone, ...rest }) => rest);
    try {
      const r = await s.client().turnOnTing(team);
      toast(
        r.status !== "on"
          ? `Asked Ting again for ${team}`
          : r.missing_types?.length
            ? `Notifications are on for ${team}; a Ting manager in Extend's owning Team still has to register ${r.missing_types.length === 1 ? "one app type" : `${r.missing_types.length} app types`}`
            : `Extend's Tings reach you in ${team}`,
      );
      await load();
    } catch (e) {
      setRowErrors((current) => ({ ...current, [team]: toApiError(e) }));
    } finally {
      setBusy(null);
    }
  }
  const STATUS: Record<string, string> = { on: "On", off: "Off", pending: "Not set up yet" };

  return (
    <div class="card" data-testid="settings-ting">
      <h2 class="card-title">
        <Bell size={17} aria-hidden="true" /> Ting notifications.
      </h2>
      <p class="fine">
        Extend tells you through Ting when a Silicon asks you to wake a device or asks for one another Silicon is using, and tells your Silicons when you answer. Ting keeps this per
        Team.
      </p>
      <ErrorNote error={error()} compact />
      <Show when={rows()} fallback={<Show when={!error()}><Spinner inline label="Asking Extend…" /></Show>}>
        {(list) => (
          <Show when={list().length} fallback={<p class="muted">No Team to show.</p>}>
            <div>
              <For each={list()}>
                {(r) => {
                  const reached = () => s.teams().includes(r.team);
                  return (
                    <div class="ting-row" data-testid="ting-row" data-team={r.team} data-status={r.status}>
                      <p class="ting-row-head">
                        <span class="team-chip">{r.team}</span>
                        <span class={`badge ${r.status === "on" ? "live" : r.status === "off" ? "warn" : "muted"}`} data-testid="ting-status">
                          {STATUS[r.status] ?? r.status}
                        </span>
                        <Show when={r.status !== "on" && reached()}>
                          <Button small busy={busy() === r.team} onClick={() => turnOn(r.team)} data-testid="ting-turn-on">
                            Turn on
                          </Button>
                        </Show>
                      </p>
                      <Show when={!reached()}>
                        <p class="fine sign-in-marker">Sign in to Extend for {r.team} to get Tings there.</p>
                      </Show>
                      <Show when={r.status === "off" && reached()}>
                        <p class="fine">You turned Extend off in Ting for {r.team}. Turn on asks Ting again.</p>
                      </Show>
                      <Show when={r.last_error && reached()}>
                        <p class="fine warn-text">{r.last_error}</p>
                      </Show>
                      <Show when={r.missing_types?.length}>
                        <MissingTingTypes registration={r} inSettings />
                      </Show>
                      <ErrorNote error={rowErrors()[r.team]} compact />
                    </div>
                  );
                }}
              </For>
            </div>
          </Show>
        )}
      </Show>
    </div>
  );
}

declare const __APP_VERSION__: string;
