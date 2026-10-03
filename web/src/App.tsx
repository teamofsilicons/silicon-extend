import { createEffect, createSignal, For, Match, on, onMount, Show, Switch, type JSX } from "solid-js";
import { BookOpen, ChevronDown, FlaskConical, LogIn, Plus, Search, Settings as SettingsIcon, Smartphone } from "lucide-solid";
import { Link, match, navigate, useLocation } from "./lib/router";
import { contextId, session } from "./lib/session";
import { toApiError, type ApiError } from "./lib/api";
import { ErrorNote, MemberTag, Toasts } from "./components/ui";
import { ExtendMark } from "./components/ExtendMark";
import { CommandMenu, openCommandMenu } from "./components/CommandMenu";
import { SignOutButton } from "./components/SignOut";
import SignIn from "./pages/SignIn";
import Callback from "./pages/Callback";
import Devices from "./pages/Devices";
import AddDevice from "./pages/AddDevice";
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

  // Verify the selected organization without letting an older request affect a newly selected login.
  createEffect(
    on(
      () => [s.contextKey(), s.world()] as const,
      async ([contextKey]) => {
        if (!contextKey) return;
        const client = s.client();
        try {
          const me = await client.me();
          if (s.contextKey() === contextKey) s.updateTeams(me.teams);
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
  const isCarbon = () => s.member()?.type !== "silicon";

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
      <Show when={s.contextKey()} keyed>{(_context) => page()}</Show>
    </Show>
  );

  const deviceParams = () => (path() === "/devices/new" ? null : match("/devices/:id", path()));
  const docsParams = () => match("/docs/:page", path());
  const downloadParams = () => match("/download/:platform", path());
  /** The device list stays mounted while moving between it and a device, like Interface's conversations. */
  const devicesRoute = () => path() === "/devices" || !!deviceParams() || (path() === "/" && signedIn());

  const onDevices = () => path() === "/devices" || !!deviceParams() || (path() === "/" && signedIn());
  const crumb = () =>
    path() === "/devices/new"
      ? "Add a device"
      : path().startsWith("/docs")
        ? "Docs"
        : path() === "/settings"
          ? "Settings"
          : path().startsWith("/download")
            ? "Download"
            : null;

  return (
    <div class={`application ${s.world().kind === "testing" ? "testing" : ""}`}>
      <a
        class="skip-link"
        href="#main-content"
        onClick={(e) => {
          e.preventDefault();
          document.getElementById("main-content")?.focus();
        }}
      >
        Skip to content
      </a>
      <aside class="app-rail" aria-label="Main navigation">
        <div class="rail-inner">
          <Link href={signedIn() ? "/devices" : "/"} class="rail-brand" aria-label="Silicon Extend home">
            <ExtendMark size={26} />
          </Link>
          <span class="rail-divider" aria-hidden="true" />
          <nav aria-label="Main">
            <Show when={signedIn()}>
              <RailLink href="/devices" label="Devices" short="Devices" active={onDevices()}>
                <Smartphone size={21} stroke-width={1.6} />
              </RailLink>
              <Show when={isCarbon()}>
                <RailLink href="/devices/new" label="Add a device" short="Add" active={path() === "/devices/new"}>
                  <Plus size={22} stroke-width={1.6} />
                </RailLink>
              </Show>
            </Show>
            <Show when={!signedIn()}>
              <RailLink href="/" label="Sign in" short="Sign in" active={path() === "/" || path() === "/sign-in"}>
                <LogIn size={20} stroke-width={1.6} />
              </RailLink>
            </Show>
            <RailLink href="/docs" label="Docs" short="Docs" active={path().startsWith("/docs")}>
              <BookOpen size={20} stroke-width={1.6} />
            </RailLink>
            <button class="rail-button rail-search" aria-label="Search devices and pages" onClick={openCommandMenu}>
              <Search size={19} stroke-width={1.6} />
              <span class="rail-tooltip" aria-hidden="true">
                Search · ⌘ K
              </span>
            </button>
            <RailLink href="/settings" label="Settings" short="Settings" active={path() === "/settings"} class="rail-settings">
              <SettingsIcon size={20} stroke-width={1.6} />
            </RailLink>
          </nav>
        </div>
      </aside>

      <div class="workspace-body">
        <TestingBanner error={environmentError()} />
        <header class="workspace-topbar">
          <div class="workspace-breadcrumb">
            <Link href={signedIn() ? "/devices" : "/"} class="topbar-mark" aria-label="Silicon Extend home">
              <ExtendMark size={18} />
            </Link>
            <span class="breadcrumb-root">extend</span>
            <Show when={signedIn() && s.teams().length > 0}>
              <span class="breadcrumb-slash" aria-hidden="true">
                /
              </span>
              <TeamPicker />
            </Show>
            <Show when={crumb()}>
              <span class="breadcrumb-page">
                <span class="breadcrumb-slash" aria-hidden="true">
                  /
                </span>
                {crumb()}
              </span>
            </Show>
          </div>
          <div class="topbar-right">
            <Show when={s.telemetryOff()}>
              <span class="demo-label" title="Settings › Telemetry">
                Telemetry off
              </span>
            </Show>
            <Show when={signedIn()}>
              <span class="member" title={s.member()?.display_name ?? undefined}>
                <MemberTag type={s.member()?.type} />
                <span class="member-id" data-testid="member-id">
                  {s.member()?.id}
                </span>
              </span>
            </Show>
            <button class="topbar-command" aria-label="Search devices and pages" onClick={openCommandMenu} data-testid="open-search">
              <Search size={14} aria-hidden="true" />
              <span class="topbar-command-label">Search</span>
              <kbd>⌘ K</kbd>
            </button>
            <Show when={signedIn()}>
              <SignOutButton icon />
            </Show>
          </div>
        </header>
        <Show when={serviceError()}>
          <div class="service-error">
            <ErrorNote error={serviceError()} />
          </div>
        </Show>
        <main class="workspace-content" id="main-content" tabindex="-1">
          <Switch fallback={<NotFound />}>
            <Match when={(path() === "/" && !signedIn()) || path() === "/sign-in"}>
              <SignIn reason={s.signedOutReason()} next="/devices" onSignedIn={() => navigate("/devices", { replace: true })} />
            </Match>
            <Match when={path() === "/auth/callback"}>
              <Callback />
            </Match>
            <Match when={path() === "/devices/new"}>
              {/* A new ?kind= starts the wizard over, even when it is already open. */}
              {guarded(() => <For each={[loc().search]}>{() => <AddDevice />}</For>)}
            </Match>
            <Match when={devicesRoute()}>{guarded(() => <Devices selected={deviceParams()?.id ?? null} />)}</Match>
            <Match when={path() === "/settings"}>
              <Show when={s.contextKey()} keyed>{(_context) => <Settings />}</Show>
            </Match>
            <Match when={path() === "/docs"}>
              <Docs page="start" />
            </Match>
            <Match when={docsParams()}>{(params) => <Docs page={params().page} />}</Match>
            <Match when={downloadParams()}>{(params) => <Download platform={params().platform} />}</Match>
          </Switch>
        </main>
      </div>
      <CommandMenu />
      <Toasts />
    </div>
  );
}

function RailLink(props: { href: string; label: string; short: string; active: boolean; class?: string; children: JSX.Element }) {
  return (
    <Link href={props.href} class={`rail-button ${props.active ? "active" : ""} ${props.class ?? ""}`} aria-label={props.label} aria-current={props.active ? "page" : undefined}>
      {props.children}
      <span class="rail-label" aria-hidden="true">
        {props.short}
      </span>
      <span class="rail-tooltip" aria-hidden="true">
        {props.label}
      </span>
    </Link>
  );
}

/** Every choice restores its own account and organization credentials. */
function TeamPicker() {
  const s = session();
  return <div class="team-picker" title="Account and organization">
    <label><span class="visually-hidden">Account and organization</span>
      <select value={s.pair()?(contextId(s.pair()!) ?? ""):""} data-testid="team-picker" onChange={e=>{s.selectContext(e.currentTarget.value);navigate("/devices");}}>
        <For each={s.contexts()}>{p=><option value={contextId(p)!}>{p.member.display_name || p.member.id} · {p.teams[0]}</option>}</For>
      </select>
    </label>
    <Link href="/sign-in" class="icon-button" aria-label="Add account or organization" title="Add account or organization"><Plus size={14}/></Link>
  </div>;
}

function TestingBanner(props: { error: ApiError | null }) {
  const s = session();
  const [leaving, setLeaving] = createSignal(false);
  return (
    <Show when={s.world().kind === "testing" ? (s.world() as Extract<ReturnType<typeof s.world>, { kind: "testing" }>) : null}>
      {(world) => (
        <div class="testing-banner" role="status" data-testid="testing-banner">
          <div class="testing-banner-label">
            <FlaskConical size={14} aria-hidden="true" />
            <span class="testing-banner-name">
              Test environment: <strong data-testid="testing-name">{world().environment.name}</strong>
              <Show when={world().environment.state !== "ready"}> ({world().environment.state})</Show>
            </span>
            <span class="testing-banner-who">{s.member() ? <>signed in as <strong>{s.member()!.id}</strong></> : "not signed in"}</span>
            <Show when={props.error}>
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
    <section class="page-main narrow">
      <header class="page-heading">
        <div>
          <p class="eyebrow">Not found</p>
          <h1 class="page-title">Nothing here.</h1>
          <p class="lead">
            There is no page at <code>{loc().pathname}</code>. Go to <Link href="/devices">your devices</Link> or the <Link href="/docs">docs</Link>.
          </p>
        </div>
      </header>
    </section>
  );
}
