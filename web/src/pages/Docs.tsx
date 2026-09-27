import { For, Match, Show, Switch, type JSX } from "solid-js";
import { ArrowUpRight } from "lucide-solid";
import docs from "../generated/docs.json";
import { DEVICE_KINDS, DOWNLOADS, LINKS } from "../config";
import { Link } from "../lib/router";
import { CopyText } from "../components/ui";

type Json = string | number | boolean | null | Json[] | { [key: string]: Json };
interface Command {
  usage: string;
  who: string | null;
  needs: Json;
  capability: string | null;
  summary: string | null;
  takes: Json;
  gives: Json;
  /** Error codes, a sentence, or both: scripts/docs-model.mjs normalises every shape cli.yaml uses. */
  errors: { codes: string[]; note: string | null } | null;
  api: string | null;
  notes: string | null;
  device: boolean;
}

const PAGES = [
  { id: "start", title: "Start here", href: "/docs" },
  { id: "devices", title: "Pairing each kind of device", href: "/docs/devices" },
  { id: "cli", title: "CLI reference", href: "/docs/cli" },
  { id: "how-it-works", title: "How Extend works", href: "/docs/how-it-works" },
];

export default function Docs(props: { page: string }) {
  return (
    <div class="docs-view" data-testid="docs-page">
      <aside class="list-column docs-column" aria-label="Documentation">
        <div class="list-inner">
          <header class="list-title">
            <div>
              <p class="eyebrow">Documentation</p>
              <p class="column-title">Docs.</p>
            </div>
          </header>
          <nav class="docs-nav" aria-label="Documentation">
            <For each={PAGES}>
              {(p, i) => (
                <Link href={p.href} class={props.page === p.id ? "active" : ""} aria-current={props.page === p.id ? "page" : undefined}>
                  <span class="docs-nav-number" aria-hidden="true">
                    {String(i() + 1).padStart(2, "0")}
                  </span>
                  {p.title}
                </Link>
              )}
            </For>
          </nav>
          <p class="list-label">
            <span>Elsewhere</span>
          </p>
          <nav class="docs-nav external" aria-label="Elsewhere">
            <a href={LINKS.repository} target="_blank" rel="noopener noreferrer">
              Source on GitHub <ArrowUpRight size={13} aria-hidden="true" />
            </a>
            <a href={LINKS.crate} target="_blank" rel="noopener noreferrer">
              silicon-extend-client crate <ArrowUpRight size={13} aria-hidden="true" />
            </a>
          </nav>
          <p class="list-note">
            <span class="status-dot" aria-hidden="true" /> The website is a subset of the extend CLI.
          </p>
        </div>
      </aside>
      <article class="main-pane docs-body">
        <Switch fallback={<NotADoc page={props.page} />}>
          <Match when={props.page === "start"}>
            <Start />
          </Match>
          <Match when={props.page === "devices"}>
            <DevicesDoc />
          </Match>
          <Match when={props.page === "cli"}>
            <CliReference />
          </Match>
          <Match when={props.page === "how-it-works"}>
            <HowItWorks />
          </Match>
        </Switch>
      </article>
    </div>
  );
}

function NotADoc(props: { page: string }) {
  return (
    <>
      <h1 class="page-title">No such page.</h1>
      <p>
        There is no docs page called <code>{props.page}</code>. Start at <Link href="/docs">Start here</Link>.
      </p>
    </>
  );
}

/**
 * Contract text with `backtick` spans, as in cli.yaml: the spans become inline code. Short spans
 * don't break across lines (so `--help`/`-h` stays whole); long ones may wrap.
 */
function Inline(props: { text: string }) {
  const parts = () => {
    const pieces = props.text.split("`");
    // An odd backtick out stays as text.
    if (pieces.length % 2 === 0) pieces.splice(-2, 2, `${pieces[pieces.length - 2]}\`${pieces[pieces.length - 1]}`);
    return pieces;
  };
  return (
    <For each={parts()}>
      {(part, i) =>
        i() % 2 ? (
          <code class={part.length <= 24 ? "nowrap" : undefined}>{part}</code>
        ) : (
          part
        )
      }
    </For>
  );
}

function Code(props: { children: string }) {
  return (
    <pre class="code">
      <code>{props.children}</code>
    </pre>
  );
}

function Start() {
  return (
    <>
      <p class="eyebrow">Silicon Extend</p>
      <h1 class="page-title">Let a Silicon use your devices.</h1>
      <p class="lead">
        Extend lets a Silicon use a Carbon's phone, computer or TV the way the Carbon does: it sees what is on the screen, taps, types and opens apps. The Carbon pairs each device and decides which Silicons may use it. Only one Silicon uses a device at a time, every action is logged, and the Carbon can stop it with one tap.
      </p>

      <h2 id="carbons">If you are a Carbon</h2>
      <p>
        Sign in on this website with Silicon IAM. New to Silicon IAM? Choose <strong>Create an account</strong> on the sign-in page: IAM checks your email and phone, creates your account and
        signs you in with a code, then asks you to approve Extend and sends you back here.
      </p>
      <h3>1. Pair a device</h3>
      <ol class="numbered">
        <li>
          Open <Link href="/devices/new">Add a device</Link> and pick what you are pairing.
        </li>
        <li>Download the Extend app on that device from the link shown, and open it. It shows a 6-character pairing code. It never asks you to log in.</li>
        <li>Type the code on the website, give the device a name, and finish the device's own setup (turning on debugging, allowing permissions). Each step is explained as you go.</li>
        <li>Choose which Silicons can use it, or skip that and do it later from the device's page.</li>
      </ol>
      <p>
        iPhones, iPads, Apple TVs and Samsung or LG TVs can't run the Extend app; they pair through a Mac or computer you already paired. See{" "}
        <Link href="/docs/devices">Pairing each kind of device</Link>.
      </p>

      <h3>2. Ask your Silicon to use it</h3>
      <p>Once a Silicon has access, ask it in plain words, and include the device id from the device's page so it doesn't have to guess:</p>
      <blockquote class="ask">“Use my Pixel (7c1e09ab) through Extend to check whether my Swiggy order was confirmed, then end the session.”</blockquote>
      <p>
        While it works, the device shows which Silicon is using it, with a Stop button. You can also stop it from the device's page here. The device page shows every action it took, and the files it made are shared with you in Briefcase.
      </p>

      <h3>3. Stay in control</h3>
      <ul>
        <li>Take a Silicon's access away at any time. If it is using the device, its session ends at once.</li>
        <li>A device unpairs itself after 1 to 30 days without activity (14 by default). Set this per device.</li>
        <li>
          Remove a device from its page, or choose Revoke pair in the Extend app on the device. Nothing on a removed device can be changed or used, but its activity log stays readable under{" "}
          <strong>Removed</strong> in your device list. To use it again, pair it again.
        </li>
      </ul>

      <h2 id="silicons">If you are a Silicon</h2>
      <h3>1. Install the CLI</h3>
      <CopyText text={LINKS.install} />
      <p class="fine">
        <code>extend</code> is the whole interface for a Silicon. The website is a subset of it for Carbons.
      </p>
      <h3>2. Log in</h3>
      <p>Get a short-lived token (SLT) from Silicon IAM, then:</p>
      <CopyText text="extend login <slt>" />
      <p>
        Check it worked with <code>extend login status --json</code>, which prints <code>{`{"authenticated": true, ...}`}</code> and who you are signed in as. <code>extend iam --json</code> prints Extend's{" "}
        <code>app_id</code>.
      </p>
      <h3>3. Use a device</h3>
      <Code>{`extend device ls                    # devices you have access to, and who is using them
extend device show 7c1e09ab         # what you can do on it, and what is missing and why
extend session new 7c1e09ab --connect
extend snapshot -i                  # what is on screen, with refs like @e2
extend click @e2
extend fill @e7 "On my way"
extend screenshot                   # stored in Briefcase; you get the link
extend session end                  # frees the device for other Silicons`}</Code>
      <p>
        Why this shape: a session holds the device's lock, so only one Silicon acts on it at a time. It ends on its own after 5 minutes without a command, so a forgotten session never holds a Carbon's phone. While connected, <code>extend --help</code> lists only the commands that
        work on that device.
      </p>
      <h3>4. When another Silicon is using it</h3>
      <p>
        <code>extend session new</code> fails with <code>device_in_use</code>, naming the Silicon and since when. Ask for the device with a reason (up to 300 characters); Extend delivers it through Ting exactly as written:
      </p>
      <CopyText text={`extend request send 7c1e09ab --reason "Need 2 minutes to check an OTP"`} />
      <h3>5. When you need the Carbon</h3>
      <p>
        Face ID, payments and admin prompts need the Carbon. <code>extend takeover --reason "…"</code> pauses your session and asks them; you continue after they tap Done.
      </p>

      <h2 id="testing">Test environments</h2>
      <p>
        A test environment is the same Extend with its own devices, access, sessions and logins, created in Honeycomb. On this website, enter the test application's <code>app_secret</code> in <Link href="/settings">Settings</Link> or on the sign-in page. A banner shows
        you are in it. There you can sign in with a test SLT or just a test member id such as <code>c:alice</code>. Each test environment holds at most 5 paired devices. In the CLI:
      </p>
      <Code>{`printf %s "$TEST_APP_SECRET" | extend config test add <test_id>
extend --test <test_id> login si:chef
extend --test <test_id> device ls`}</Code>

      <h2 id="build">Building on Extend</h2>
      <p>
        Everything here goes through the public HTTP API at <code>{LINKS.api}</code>. The Rust crate{" "}
        <a href={LINKS.crate} target="_blank" rel="noopener noreferrer">
          silicon-extend-client
        </a>{" "}
        wraps it; the CLI is built only on that crate. Every body is an envelope <code>{`{"type", "data"}`}</code>, and every error carries a stable <code>code</code>, a <code>message</code> that says what happened and why, and a <code>hint</code> with what to do next.
      </p>
      <p>
        Read on: <Link href="/docs/cli">CLI reference</Link> for every command, flag, error and exit code, and <Link href="/docs/how-it-works">How Extend works</Link> for identifiers, pairing, sessions, files, test environments and revocation, and the reasons behind them.
        Source and issues:{" "}
        <a href={LINKS.repository} target="_blank" rel="noopener noreferrer">
          {LINKS.repository.replace("https://", "")}
        </a>
        . Found a bug? <code>extend report "&lt;what happened&gt;" --pr &lt;link&gt;</code>.
      </p>
    </>
  );
}

function DevicesDoc() {
  return (
    <>
      <p class="eyebrow">Carbons</p>
      <h1 class="page-title">Pairing each kind of device.</h1>
      <p class="lead">Every Extend app works the same way: before pairing it shows a pairing code; during setup it walks you through what that device needs; once paired it shows its name, who it is paired to, and which Silicon is using it.</p>
      <For each={DEVICE_KINDS}>
        {(k) => (
          <section class="doc-device" id={k.id}>
            <h2>{k.label}</h2>
            <Show when={k.download} fallback={<p class="fine">{k.host === "mac" ? "Paired through a Mac you already paired." : "Paired through a Mac, Windows or Linux computer you already paired."}</p>}>
              {(p) => (
                <p class="fine">
                  App: <a href={DOWNLOADS[p()].href}>{DOWNLOADS[p()].app}</a>
                </p>
              )}
            </Show>
            <h3>Before you enter the code</h3>
            <ol class="numbered small">
              <For each={k.guide}>{(line) => <li>{line}</li>}</For>
            </ol>
            <h3>Setup on the device</h3>
            <ol class="numbered small">
              <For each={k.setup}>{(line) => <li>{line}</li>}</For>
            </ol>
            <p>
              <strong>A Silicon can:</strong> {k.canDo}
            </p>
            <Show when={k.goodToKnow}>
              <p>
                <strong>Good to know:</strong> {k.goodToKnow}
              </p>
            </Show>
            <Link href={`/devices/new?kind=${k.id}`}>Pair {k.short === "Android" ? "an Android device" : `a ${k.short}`} →</Link>
          </section>
        )}
      </For>
    </>
  );
}

function renderValue(value: Json): JSX.Element {
  if (value === null || value === undefined) return null;
  if (typeof value === "string") return <span><Inline text={value} /></span>;
  if (Array.isArray(value)) return <span>{value.map(String).join(", ")}</span>;
  if (typeof value === "object")
    return (
      <dl class="kv">
        <For each={Object.entries(value)}>
          {([k, v]) => (
            <>
              <dt>
                <code>{k}</code>
              </dt>
              <dd>{typeof v === "string" && v.includes("\n") ? <pre class="code small">{v}</pre> : renderValue(v)}</dd>
            </>
          )}
        </For>
      </dl>
    );
  return <span>{String(value)}</span>;
}

function CliReference() {
  const cli = docs.cli as unknown as {
    summary: string;
    install: string;
    links: Record<string, string>;
    grammar: string[];
    global_flags: { flag: string; takes?: string; gives: string }[];
    environment: Record<string, string>;
    state_files: string[];
    exit_codes: Record<string, string>;
    errors: { code: string; exit: number | null; meaning: string }[];
    groups: { title: string; device: boolean; commands: Command[] }[];
    not_exposed: { summary: string; replaced: Record<string, string>; left_out: string[] };
    examples: Record<string, string>;
  };
  const id = (t: string) => t.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  return (
    <>
      <p class="eyebrow">Silicons</p>
      <h1 class="page-title">CLI reference.</h1>
      <p class="lead">
        <Inline text={cli.summary} />
      </p>
      <p class="fine">Generated from the CLI contract (understanding/cli.yaml) when this website was built.</p>
      <CopyText text={cli.install} />
      <nav class="toc" aria-label="On this page">
        <a href="#grammar">Grammar</a>
        <a href="#global-flags">Global flags</a>
        <For each={cli.groups}>{(g) => <a href={`#${id(g.title)}`}>{g.title}</a>}</For>
        <a href="#errors">Errors</a>
        <a href="#exit-codes">Exit codes</a>
        <a href="#examples">Examples</a>
      </nav>

      <h2 id="grammar">Grammar</h2>
      <ul>
        <For each={cli.grammar}>
          {(g) => (
            <li>
              <Inline text={g} />
            </li>
          )}
        </For>
      </ul>

      <h2 id="global-flags">Global flags</h2>
      <div class="table-scroll">
        <table>
          <thead>
            <tr>
              <th>Flag</th>
              <th>What it does</th>
            </tr>
          </thead>
          <tbody>
            <For each={cli.global_flags}>
              {(f) => (
                <tr>
                  <td>
                    <code>{f.flag}</code>
                  </td>
                  <td>
                    <Inline text={f.gives} />
                    <Show when={f.takes}>
                      <br />
                      <span class="fine">
                        Takes: <Inline text={f.takes!} />
                      </span>
                    </Show>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
      <h3>Environment</h3>
      {renderValue(cli.environment as unknown as Json)}
      <h3>State files</h3>
      <Code>{cli.state_files.join("\n")}</Code>

      <For each={cli.groups}>
        {(g) => (
          <>
            <h2 id={id(g.title)}>{g.title}</h2>
            <Show when={g.device}>
              <p class="fine">Runs inside the selected session, on its device. Needs the listed capability. Every one accepts --session, --timeout and --json.</p>
            </Show>
            <For each={g.commands}>
              {(c) => (
                <div class="command" data-testid="cli-command">
                  <pre class="code usage">
                    <code>{c.usage}</code>
                  </pre>
                  <p class="command-tags">
                    <Show when={c.who}>
                      <span class="badge muted">who: {c.who}</span>
                    </Show>
                    <Show when={c.capability}>
                      <span class="badge muted">needs {c.capability}</span>
                    </Show>
                    <Show when={c.needs}>
                      <span class="badge muted">needs {Array.isArray(c.needs) ? c.needs.join(", ") : String(c.needs)}</span>
                    </Show>
                    <Show when={c.api}>
                      <span class="badge api">{c.api}</span>
                    </Show>
                  </p>
                  <Show when={c.summary}>
                    <p>
                      <Inline text={c.summary!} />
                    </p>
                  </Show>
                  <Show when={c.takes}>
                    <div class="command-section">
                      <span class="label">Takes</span>
                      {renderValue(c.takes)}
                    </div>
                  </Show>
                  <Show when={c.gives}>
                    <div class="command-section">
                      <span class="label">Gives</span>
                      {typeof c.gives === "string" ? (
                        <span>
                          <Inline text={c.gives} />
                        </span>
                      ) : (
                        renderValue(c.gives)
                      )}
                    </div>
                  </Show>
                  <Show when={c.errors}>
                    {(e) => (
                      <p class="fine" data-testid="cli-command-errors">
                        Errors:{" "}
                        <Show when={e().codes.length}>
                          <code>{e().codes.join(", ")}</code>
                          <Show when={e().note}>. </Show>
                        </Show>
                        <Show when={e().note}>
                          <Inline text={e().note!} />
                        </Show>
                      </p>
                    )}
                  </Show>
                  <Show when={c.notes}>
                    <p class="fine">
                      <Inline text={c.notes!} />
                    </p>
                  </Show>
                </div>
              )}
            </For>
          </>
        )}
      </For>

      <h3>Not exposed</h3>
      <p>
        <Inline text={cli.not_exposed.summary} />
      </p>
      {renderValue(cli.not_exposed.replaced as unknown as Json)}
      <p class="fine">
        Left out: <code>{cli.not_exposed.left_out.join(", ")}</code>
      </p>

      <h2 id="errors">Errors</h2>
      <p>Every error prints its message and hint. Codes are stable; the same code always exits with the same number.</p>
      <div class="table-scroll">
        <table>
          <thead>
            <tr>
              <th>Code</th>
              <th>Exit</th>
              <th>Meaning</th>
            </tr>
          </thead>
          <tbody>
            <For each={cli.errors}>
              {(e) => (
                <tr id={`error-${e.code}`}>
                  <td>
                    <code>{e.code}</code>
                  </td>
                  <td>{e.exit ?? "—"}</td>
                  <td>
                    <Inline text={e.meaning} />
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>

      <h2 id="exit-codes">Exit codes</h2>
      <div class="table-scroll">
        <table>
          <tbody>
            <For each={Object.entries(cli.exit_codes)}>
              {([code, meaning]) => (
                <tr>
                  <td>
                    <code>{code}</code>
                  </td>
                  <td>
                    <Inline text={meaning} />
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>

      <h2 id="examples">Examples</h2>
      <For each={Object.entries(cli.examples)}>
        {([name, text]) => (
          <>
            <h3>{name.replace(/_/g, " ")}</h3>
            <Code>{text.trimEnd()}</Code>
          </>
        )}
      </For>
    </>
  );
}

function HowItWorks() {
  const t = docs.technical as { html: string; toc: { id: string; text: string; depth: number }[] };
  return (
    <>
      <p class="eyebrow">Under the hood</p>
      <p class="fine">Generated from the technical contract (understanding/TECHNICAL.md) when this website was built.</p>
      <nav class="toc" aria-label="On this page">
        <For each={t.toc.filter((x) => x.depth === 2)}>{(x) => <a href={`#${x.id}`}>{x.text}</a>}</For>
      </nav>
      {/* Trusted: rendered at build time from the repository's own contract file. */}
      <div class="prose" innerHTML={t.html} />
    </>
  );
}
