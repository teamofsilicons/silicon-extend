# Silicon Extend website

Next.js 16, React 19 and the shared Silicon Silicon UI. The product pages call the real Extend API through a same-origin server proxy. Silicon Accounts tokens stay in a sealed, HttpOnly cookie; the browser never receives an access or refresh token in JavaScript. Sign-in uses PKCE and state, token claims are checked against Accounts JWKS, and writes require a same-origin request.

## Run locally

Use Node 24 or later and pnpm 10.33.0. Copy `.env.example` to `.env.local`, set the app secret and a random session secret, and register `http://127.0.0.1:4220/auth/callback` on the local Accounts app `extend`. Start the real app backend on 4221 and the shared local Accounts stack first.

```sh
pnpm install --frozen-lockfile
pnpm dev
```

## Environment

`APP_ID=extend`, `APP_SECRET`, `ACCOUNTS_URL`, optional `ACCOUNTS_API_URL`, `APP_API_URL`, `SESSION_SECRET` (at least 32 random bytes), and `PUBLIC_URL` are read on the server at request time. Never put these secrets in a `NEXT_PUBLIC_` variable. `EXTRA_IMG_ORIGINS` extends trusted profile image origins; `EXTRA_ORIGINS` is only for another explicitly trusted website origin. Production uses HTTPS and Secure cookies.

## Verification

```sh
pnpm typecheck
pnpm lint
pnpm test
pnpm build
TEST_STACK_JSON=/private/path/test-stack.json pnpm test:e2e
```

Browser tests use the real Accounts hosted sign-in and app API, including the product mutations, refresh, logout, responsive layouts and axe WCAG 2.2 AA checks. `scripts/e2e_accounts.py` in the repository rehearses backend and CLI behavior. Extend browser product tests also need the `fake_device` native protocol example built under `target/mig`. Screenshots are written to ignored `screens/`; credentials and browser state stay under ignored test outputs. See `docs/migration/web-verification.md` for recorded results.

## Deployment

The Vercel project root is `web`, framework Next.js, build command `pnpm build`, install command `pnpm install --frozen-lockfile`; clear the old Vite output directory and all VITE_ settings. `vercel.json` records these choices. Configure the server environment in Vercel before promotion. The device download URLs and versions remain those of the existing device releases; native device applications do not change with this website.

Register the production `PUBLIC_URL/auth/callback` before switching traffic. Keep one website build and secret for a deployment; changing SESSION_SECRET signs browsers out. Sign-in redirects, refreshed sessions, actual product writes and sign-out must be checked on the deployed origin. Production release is coordinated by the migration cutover runbook; these local tests do not deploy it.
