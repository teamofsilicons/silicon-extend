import { createEffect, createSignal, Match, on, onMount, Show, Switch, type JSX } from "solid-js";
import { BookOpen, FlaskConical, LogOut, Plus, Settings as SettingsIcon, Smartphone } from "lucide-solid";
import { Link, match, navigate, useLocation } from "./lib/router";
import { session } from "./lib/session";
import { toApiError, type ApiError } from "./lib/api";
import { ErrorNote, Toasts } from "./components/ui";
import { ExtendMark } from "./components/ExtendMark";
import SignIn from "./pages/SignIn";
import Callback from "./pages/Callback";
import Devices from "./pages/Devices";
import AddDevice from "./pages/AddDevice";
import DevicePage from "./pages/DevicePage";
import Settings from "./pages/Settings";
import Docs from "./pages/Docs";
import Download from "./pages/Download";
import { write } from "./lib/storage";

export default function App() {
  const s = session();
  const loc = useLocation();
  const [serviceError, setServiceError] = createSignal<ApiError | null>(null);
  const [environmentError, setEnvironmentError] = createSignal<ApiError | null>(null);

  // Agree the API major once. Only an explicit "no common version" blocks the site.
  onMount(async () => {
    try {
      await s.client().negotiateVersion();
    } catch (error) {
      const e = toApiError(error);
      if (e.code === "api_version_unsupported" || e.code === "api_version_sunset") setServiceError(e);
    }
  });

  // Keep the team list live (IAM is the authority) whenever the signed-in member or world changes.
  createEffect(
    on(
      () => [s.pair()?.member.id, s.world()] as const,
      async ([memberId]) => {
        if (!memberId) return;
        try {
          const me = await s.client().me();
          s.updateTeams(me.teams);
        } catch {
          /* a 401 has already signed the tab out; anything else shows on the page that needs it */
        }
      },
    ),
  );

  // In a test environment, re-read its name and state for the banner.
  createEffect(
    on(s.world, async (world) => {
      setEnvironmentError(null);
      if (world.kind !== "testing") return;
      try {
        s.updateEnvironment(await s.client().testingEnvironment(world.secret));
      } catch (error) {
        setEnvironmentError(toApiError(error));
      }
    }),
  );

  const path = () => loc().pathname.replace(/\/+$/, "") || "/";
  const signedIn = () => !!s.pair();

  /** Pages that need a login show sign-in in place, then come back here. */
  const guarded = (page: () => JSX.Element) => (
    <Show
      when={signedIn()}
      fallback={
        <SignIn
          reason={s.signedOutReason()}
          onSignedIn={() => {
            /* stay on this URL; the page renders now */
          }}
          next={path()}
        />
      }
    >
      {page()}
    </Show>
  );

  const deviceParams = () => match("/devices/:id", path());
  const docsParams = () => match("/docs/:page", path());
  const downloadParams = () => match("/download/:platform", path());

  return (
    <div class="frame">
      <TestingBanner error={environmentError()} />
      <header class="topbar">
        <div class="topbar-inner">
          <Link href={signedIn() ? "/devices" : "/"} class="brand" aria-label="Silicon Extend home">
            <ExtendMark />
            <span>
              Extend
              <small>Silicon Extend</small>
            </span>
          </Link>
          <nav class="nav" aria-label="Main">
            <Show when={signedIn()}>
              <Link href="/devices" class={path() === "/devices" || (path().startsWith("/devices/") && path() !== "/devices/new") ? "active" : ""}>
                <Smartphone size={16} aria-hidden="true" /> Devices
              </Link>
              <Show when={s.member()?.type !== "silicon"}>
                <Link href="/devices/new" class={path() === "/devices/new" ? "active" : ""}>
                  <Plus size={16} aria-hidden="true" /> Add a device
                </Link>
              </Show>
            </Show>
            <Link href="/docs" class={path().startsWith("/docs") ? "active" : ""}>
              <BookOpen size={16} aria-hidden="true" /> Docs
            </Link>
            <Link href="/settings" class={path() === "/settings" ? "active" : ""}>
              <SettingsIcon size={16} aria-hidden="true" /> Settings
            </Link>
          </nav>
          <Show when={signedIn()}>
            <div class="account">
              <TeamPicker />
              <span class="member-id" title={s.member()?.display_name ?? undefined} data-testid="member-id">
                {s.member()?.id}
              </span>
              <button
                class="icon-button"
                aria-label="Sign out"
                title="Sign out"
                data-testid="sign-out"
                onClick={async () => {
                  // Go to "/" first so the sign-in form appears once, not twice.
                  navigate("/", { replace: true });
                  await s.signOut().catch(() => undefined);
                }}
              >
                <LogOut size={17} />
              </button>
            </div>
          </Show>
        </div>
      </header>
      <main class="main">
        <ErrorNote error={serviceError()} />
        <Switch fallback={<NotFound />}>
          <Match when={path() === "/"}>
            <Show when={signedIn()} fallback={<SignIn reason={s.signedOutReason()} next="/devices" onSignedIn={() => navigate("/devices", { replace: true })} />}>
              <Devices />
            </Show>
          </Match>
          <Match when={path() === "/sign-in"}>
            <SignIn reason={s.signedOutReason()} next="/devices" onSignedIn={() => navigate("/devices", { replace: true })} />
          </Match>
          <Match when={path() === "/auth/callback"}>
            <Callback />
          </Match>
          <Match when={path() === "/devices"}>{guarded(() => <Devices />)}</Match>
          <Match when={path() === "/devices/new"}>{guarded(() => <AddDevice />)}</Match>
          <Match when={deviceParams()}>{(params) => guarded(() => <DevicePage id={params().id} />)}</Match>
          <Match when={path() === "/settings"}>
            <Settings />
          </Match>
          <Match when={path() === "/docs"}>
            <Docs page="start" />
          </Match>
          <Match when={docsParams()}>{(params) => <Docs page={params().page} />}</Match>
          <Match when={downloadParams()}>{(params) => <Download platform={params().platform} />}</Match>
        </Switch>
      </main>
      <footer class="footer">
        <span>Silicon Extend</span>
        <a href="https://github.com/teamofsilicons/silicon-extend" target="_blank" rel="noopener noreferrer">
          Source
        </a>
        <Link href="/docs">Docs</Link>
        <Show when={s.telemetryOff()}>
          <span>Telemetry off</span>
        </Show>
      </footer>
      <Toasts />
    </div>
  );
}

function TeamPicker() {
  const s = session();
  return (
    <Show when={s.teams().length > 0}>
      <label class="team-picker">
        <span class="visually-hidden">Team</span>
        <select
          value={s.team() ?? ""}
          data-testid="team-picker"
          disabled={s.teams().length < 2}
          onChange={(e) => s.setTeam(e.currentTarget.value)}
        >
          {s.teams().map((t) => (
            <option value={t}>{t}</option>
          ))}
        </select>
      </label>
    </Show>
  );
}

function TestingBanner(props: { error: ApiError | null }) {
  const s = session();
  const [leaving, setLeaving] = createSignal(false);
  return (
    <Show when={s.world().kind === "testing" ? (s.world() as Extract<ReturnType<typeof s.world>, { kind: "testing" }>) : null}>
      {(world) => (
        <div class="testing-banner" role="status" data-testid="testing-banner">
          <div class="testing-banner-label">
            <FlaskConical size={15} aria-hidden="true" />
            <span>
              Test environment: <strong data-testid="testing-name">{world().environment.name}</strong>
              <Show when={world().environment.state !== "ready"}> ({world().environment.state})</Show>
            </span>
            <span class="sep">·</span>
            <span>{s.member() ? <>signed in as <strong>{s.member()!.id}</strong></> : "not signed in"}</span>
            <Show when={props.error}>
              <span class="sep">·</span>
              <span class="banner-error" title={props.error!.hint ?? undefined}>
                {props.error!.message}
              </span>
            </Show>
          </div>
          <button
            class="testing-exit"
            data-testid="exit-testing"
            disabled={leaving()}
            onClick={async () => {
              setLeaving(true);
              await s.exitTesting();
              setLeaving(false);
              write("session", "extend.next", "");
              navigate(s.pair() ? "/devices" : "/", { replace: true });
            }}
          >
            {leaving() ? "Leaving…" : "Exit testing"}
          </button>
        </div>
      )}
    </Show>
  );
}

function NotFound() {
  const loc = useLocation();
  return (
    <section class="page narrow">
      <h1 class="page-title">Nothing here</h1>
      <p class="lead">
        There is no page at <code>{loc().pathname}</code>. Go to <Link href="/devices">your devices</Link> or the <Link href="/docs">docs</Link>.
      </p>
    </section>
  );
}
