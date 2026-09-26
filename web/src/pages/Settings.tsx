import { createSignal, Show } from "solid-js";
import { FlaskConical } from "lucide-solid";
import { session } from "../lib/session";
import { apiBaseUrl, LINKS } from "../config";
import { navigate } from "../lib/router";
import { Button } from "../components/ui";
import { TestingSecretForm } from "../components/TestingSecretForm";
import { applyTheme, currentTheme, type Theme } from "../lib/theme";

export default function Settings() {
  const s = session();
  const [theme, setTheme] = createSignal<Theme>(currentTheme());
  const [leaving, setLeaving] = createSignal(false);
  const world = () => s.world();

  return (
    <section class="page narrow" data-testid="settings-page">
      <h1 class="page-title">Settings</h1>

      <div class="card">
        <h2 class="card-title">Account</h2>
        <Show when={s.member()} fallback={<p class="muted">Not signed in{world().kind === "testing" ? " in this test environment" : ""}.</p>}>
          {(m) => (
            <>
              <p>
                Signed in as <strong>{m().id}</strong>
                {m().display_name ? ` (${m().display_name})` : ""}, a {m().type === "carbon" ? "Carbon" : "Silicon"}
                {world().kind === "testing" ? " in the test environment" : ""}.
              </p>
              <p class="fine">Teams: {s.teams().join(", ")}. The team picker at the top chooses which one you are working in.</p>
              <Button
                onClick={async () => {
                  await s.signOut().catch(() => undefined);
                  navigate("/", { replace: true });
                }}
              >
                Sign out
              </Button>
            </>
          )}
        </Show>
      </div>

      <div class="card" data-testid="settings-testing">
        <h2 class="card-title">
          <FlaskConical size={17} aria-hidden="true" /> Test environment
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
        <h2 class="card-title">Telemetry</h2>
        <label class="switch">
          <input type="checkbox" checked={!s.telemetryOff()} onChange={(e) => s.setTelemetry(e.currentTarget.checked)} data-testid="telemetry-toggle" />
          <span>Send usage events to help fix problems</span>
        </label>
        <p class="fine">
          Events say which step ran, whether it worked, how long it took and the error code. Never names, codes, tokens or anything on your screens. When off, the website sends none and tells Bridge with{" "}
          <code>X-Bridge-Telemetry: off</code> on every request. Saved in this browser.
        </p>
      </div>

      <div class="card">
        <h2 class="card-title">Appearance</h2>
        <div class="segmented" role="radiogroup" aria-label="Theme">
          {(["auto", "light", "dark"] as Theme[]).map((t) => (
            <button
              role="radio"
              aria-checked={theme() === t}
              class={theme() === t ? "active" : ""}
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
        <h2 class="card-title">About</h2>
        <dl class="about">
          <dt>Bridge service</dt>
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

declare const __APP_VERSION__: string;
