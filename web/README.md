# Silicon Extend website

The configuration website at `extend.teamofsilicons.com`, where a Carbon signs in with Silicon IAM,
pairs devices and decides which Silicons can use them. It is a subset of the `extend` CLI and talks
only to the public Extend API (`../understanding/api.yaml`).

Vite + SolidJS + TypeScript. It is a sibling of Silicon Interface: the same tokens and type (IBM
Plex Sans, Source Serif 4 titles that end in a period, IBM Plex Mono eyebrows and badges) and the
same shell: a narrow icon rail with the Extend mark, a breadcrumb bar (`extend / <team> ⌄`, ⌘K
search), a list column on `--surface` and a main pane on `--paper` with Interface's paper grain.
Devices work like Interface's conversations: the list stays on the left and the open device fills
the right; at phone width the rail becomes a bottom bar and one pane shows at a time.

Accents come from the Carbon's "Silicon" board: `src/components/Shader.tsx` ports Interface's
grainy colour study and prints it through an ordered dither (four blues and a warm horizon, with
edges that thin out through the same screen) for empty states, the sign-in page, the overview and
the pairing code, which is set as a printed ticket in poster-size mono. Poster credits sit on their
own hairline rules, clear of the print. Device status is a 5×5 pixel dot, always with its word
(Online, In use with a shimmering rim, Offline as a 50% checker), and Stop and danger use a
risograph orange-red in both themes. Colours are tokens in `src/styles.css`, redefined for dark mode; motion stops
under `prefers-reduced-motion`.

## Run

```sh
pnpm install
pnpm dev:mock        # the website on http://localhost:5190 against the in-memory mock (web/mock)
pnpm dev             # the website on http://localhost:5190 against the Rust service on 127.0.0.1:8480
```

`pnpm dev` proxies `/api` to `EXTEND_API_PROXY` (default `http://127.0.0.1:8480`), so the site is
same-origin in development. With the mock:

- Sign in with the SLT `oac_saket` (Carbon `c:saket`, teams `acme` and `labs`), or through the mock
  consent screen ("Continue with Silicon IAM").
- A live pairing code `4F9C2A` exists at start. More: `curl -XPOST localhost:8490/__mock/enroll -d '{"os":"android"}'`.
- Test environment secret: printed when the mock starts (`ask_checkoutE2Etestenvironment…`). In it,
  sign in with a member id: `c:alice`, `c:saket`, `si:chef`, `si:scout`.
- `POST /__mock/reset` puts the fixtures back.

## Test

```sh
pnpm test            # vitest: API client, pairing codes, wizard state machine, IAM callback
pnpm test:e2e        # Playwright against the mock (starts its own mock on 8491 and site on 5191)
pnpm test:e2e:real   # against a running Rust service (EXTEND_REAL_URL, default :8480)
```

The e2e suites write full-page screenshots at 1280 px and 390 px to `test-results/screenshots/`
(mock) and `test-results/screenshots-real/` (real service), and fail if a page scrolls sideways at
phone width. `e2e/restyle.spec.ts` is a visual tour at 1440×900 and 390×844, light and dark, into
`test-results/restyle/`; it also checks that the dithered print drew with WebGL and that nothing
animates under reduced motion, and that the wizard's step strip keeps the current step in view
(a narrow desktop) or becomes a one-line pixel progress bar (a phone). It also shoots the Removed tab
and a removed device, the Remove dialog, the column centred at 1920 px, and ⌘K on a touch screen, and
checks what those screenshots can't prove on their own (Cancel instead of `esc` on touch, the prints'
paper matching the card in dark mode, "Take away" on its grant's first line at phone width, the tally
staying smaller than poster type). If another process holds the
default ports, move them with
`E2E_MOCK_PORT` / `E2E_WEB_PORT` (for example `E2E_MOCK_PORT=8496 E2E_WEB_PORT=5196 pnpm test:e2e`).

The real-service suite needs the service in `EXTEND_IAM_MODE=local`; it plays an Extend
app over the real device socket (`e2e-real/service.ts`) and creates a test environment through the
Honeycomb lifecycle endpoint with the token in `../e2e/dev.env`. The service allows 60 new
enrollments per hour from one address, counted in memory; if a shared dev service answers
`rate_limited`, run a second instance with the same `dev.env` on another port and its own database,
and point `EXTEND_REAL_URL` at it. The removed-device test skips itself, saying why, against a service
that predates `include_removed` (one that ignores `include_removed=probe` instead of refusing it).
Fake devices for a test environment enroll with its secret, since the service refuses a claim across
worlds.

## Build and deploy

```sh
pnpm build           # regenerates docs from ../understanding, type-checks, builds to dist/
```

Deploy like the sibling apps: a Vercel project with root directory `web`, framework Vite,
`pnpm build`, output `dist` (see `vercel.json`: SPA fallback, security headers, and an optional
`/api` rewrite to the service). Environment:

| Variable | Default | Meaning |
|---|---|---|
| `VITE_EXTEND_API_URL` | `https://backend.extend.teamofsilicons.com` (dev: `same-origin`) | Extend API base. `same-origin` uses `/api` on the website's origin, which `vercel.json` rewrites to the service, so no CORS is needed. |
| `VITE_IAM_LOGIN_URL` | unset | IAM's sign-in origin, used only if `GET /api/v1/iam` gives no `iam_login_url`. |

Calling the service cross-origin (the default) needs CORS on it for this origin, allowing
`Authorization, Content-Type, X-Org-ID, X-Testing-Application-Secret, Idempotency-Key, If-Match,
X-Extend-Telemetry, Silicon-Extend-API-Version, Silicon-Extend-Supported-API-Versions` and exposing
`ETag, X-Request-ID, Silicon-Extend-API-Version`. The local Rust service does this.

The docs pages are generated at build time by `scripts/gen-docs.mjs` from `understanding/cli.yaml`
and `understanding/TECHNICAL.md` into `src/generated/docs.json`. Vercel includes files outside the
root directory by default; if a build ever lacks `../understanding`, the script keeps the last
generated file. `scripts/docs-model.mjs` gives every command one shape whatever cli.yaml uses (a
command's `errors` may be a list of codes or a sentence), and stops the build, naming the command
and field, on a shape the CLI reference page can't show.

## Layout

```
src/config.ts               API URL, download files (stable GitHub release asset names), device kinds and guides
src/lib/api.ts              the Extend client: envelopes, errors, headers, serialised token refresh
src/lib/session.ts          worlds (production / test environment), tokens, team, telemetry
src/lib/auth.ts             IAM consent redirect and callback
src/lib/pairing.ts          pairing-code and Silicon-id parsing
src/lib/wizard.ts           "Add a device" state machine
src/pages/*                 sign-in, callback, devices (list + device), add a device, device page, settings, docs, download
src/components/Shader.tsx   the dithered colour study (WebGL, with a CSS fallback)
src/components/CommandMenu.tsx  ⌘K: find a device or a page
mock/                       in-memory mock of the service + stand-in IAM consent screen
e2e/, e2e-real/             Playwright suites (mock, real service)
tests/unit/                 vitest
```

## Decisions

- **Sign-in** follows IAM's client docs: `<iam_login_url>?app_id=<app_id>&redirect_uri=<origin>/auth/callback?state=<random>`,
  IAM returns `?slt=…` on that callback, and the website posts it to `POST /api/v1/auth/login`
  (Extend does the secret-authenticated exchange). `state` lives in sessionStorage and binds the
  callback to the tab that started it (IAM's SLT exchange has no PKCE). The SLT is removed from the
  address bar immediately. `app_id` and `iam_login_url` come from `GET /api/v1/iam`. Only if
  `iam_login_url` is missing does the site fall back to `VITE_IAM_LOGIN_URL`, then to `iam_base_url` with
  `backend.X` → `auth.X` (the Silicon Interface's `auth.iam.teamofsilicons.com`), plus `/login`.
- **Sign-up** goes through IAM too: "Create an account" sends a new Carbon to IAM's sign-up page with
  the same `app_id` and state-bound callback, so after creating the account IAM shows the consent
  screen and returns here signed in (the attempt is honoured for 30 minutes instead of 10, since IAM
  checks an email and a phone first). The page is `iam_signup_url` if `GET /api/v1/iam` ever names
  one, else `/signup` beside IAM's `<auth origin>/login`. For a login page laid out any other way (the
  local stand-in, the mock) the website doesn't guess: it opens the sign-in page and says so. Test
  environments don't offer sign-up; their identities come from the test environment.
- **Removed devices:** the Removed tab lists the Carbon's removed devices
  (`GET /api/v1/devices?scope=mine&include_removed=true`, every page, newest removal first). A removed
  device's page is read-only: when and why it was removed, its activity log and its requests; nothing
  that changes a device is offered, and it stops polling. The Remove dialog says exactly what
  `DELETE /devices/{id}` does: the session ends, each Silicon loses access (counted), the Extend app
  unpairs now or when it next connects (or, for a device paired through a computer, Extend stops
  reaching it through that computer), devices paired through it go too, and the log stays readable.
- **Tokens:** production's pair is kept in memory and localStorage so tabs share one login; a test
  environment's secret and pair live in sessionStorage, so a test world never leaks into production
  tabs. Refresh is serialised per tab (one shared promise) and across tabs (Web Locks), and a waiting
  tab re-reads storage before refreshing, so a rotated refresh token is never reused. The new pair is
  written in one `setItem`. The access token is refreshed 30 s before expiry and once after a 401.
- **Test environments:** the secret is validated with `GET /api/v1/testing-environment` before
  anything switches. Every request then carries `X-Testing-Application-Secret`. Exiting signs the test
  identity out (best effort), forgets the secret and its tokens, and returns to the production login
  if there is one, else to sign-in.
- **Team** comes from the login's team list, kept live with `/auth/me`; the choice is remembered per world.
- **Polling:** the device list and device page refresh every 5 s while the tab is visible (never
  overlapping); setup status every 2 s until complete.
- **Pairing codes** accept any case and separators (`4f9-c2a`); O reads as 0 and I/L as 1, since none is
  hexadecimal.
- **Silicons** to give access to are offered from `GET /api/v1/team/silicons`. Typing `si:` ids still
  works (a bare handle means `si:<handle>`), and if that endpoint isn't available the site suggests the
  Silicons already using the Carbon's other devices. Extend checks each id when access is given.
- **If-Match** uses the `ETag` when the browser can read it, else the device's `version` field.
- **Setup codes:** a setup step with `input: "code"` (an Apple TV's PIN) shows a code field. Only when
  the service sends no `input` on any step does the site infer it from a tvOS step waiting on the Carbon.
- **Takeovers:** when `in_use.paused` is set, the device page reads the takeover
  (`GET /api/v1/sessions/{id}/takeover`), shows the Silicon's reason and when the session would end, and
  **Done** releases it (`DELETE` on the same path).
- **Activity** entries that aren't commands are summarised from their `details` ("Renamed from … to …",
  "Stays paired 9 days without activity", "Asked si:chef for the device: “…”"); unknown actions and
  details still show.
- **Stop** is one tap (as UNDERSTANDING.md says). Taking access from the Silicon using the device asks
  first, since it ends that session. Removing a device requires typing its name.
- **Wide screens:** above 1600 px the page column (880 px, or 720 px for settings and the wizard) and
  the device pane's content sit in the middle of their pane, as Interface centres its reading column.
- **Touch screens** get a Cancel button in ⌘K search instead of keyboard hints (`esc`, `⌘ K`, arrows).
- **Telemetry** is on by default; off is remembered in localStorage, sends no events and adds
  `X-Extend-Telemetry: off` to every request. Events carry step, outcome, duration and error code only.
- **Errors** always show the service's `message` and `hint`, the `code`, the request id and the docs
  link. Failures without an envelope (a proxy's 502, a network or CORS failure) get their own message
  naming what failed.
