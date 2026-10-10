// Test identities at a LOCAL Silicon Accounts stack, for Extend's end-to-end scenarios (scripts/e2e_accounts.py).
// Runs with the Silicon Accounts testkit's tsx and library:
//
//   SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts EXTEND_TEST_STACK=/path/to/test-stack.json \
//     $SILICON_ACCOUNTS_DIR/testkit/node_modules/.bin/tsx scripts/mint-accounts.mts <command> ...
//
//   carbon --email ada@example.test
//       -> {uuid, id, kind, access_token, refresh_token}: a first-party session (audience silicon-accounts);
//          creates the Carbon if it is new (one or two email codes)
//   app-signin --app extend --email ada@example.test --redirect http://127.0.0.1:4220/auth/callback [--exchange]
//              [--existing] [--scope "email timezone"]
//       -> drives the hosted sign-in pages over HTTP with an email code from the stack's mock sender and prints
//          {code, code_verifier, state, redirect_uri}; with --exchange the app's server exchanges the code with the
//          app secret and it prints {tokens: {access_token, refresh_token, account}}. --existing skips making sure
//          the Carbon exists first (saves one email code).
//   slt --silicon si:scout --stk stk_... --app extend
//       -> signs the Silicon in with its STK and prints a short-lived token for the app: {slt, app_id, expires_at}
//
// Every command prints one JSON object on stdout. The stack limits email codes (10 per address and 30 per network
// in 10 minutes), so sign each Carbon in once per run and reuse its tokens.
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const accountsDir = process.env.SILICON_ACCOUNTS_DIR;
const stackFile = process.env.EXTEND_TEST_STACK;
if (!accountsDir || !stackFile) {
  console.error('mint: set SILICON_ACCOUNTS_DIR (a silicon-accounts checkout) and EXTEND_TEST_STACK (the stack file)');
  process.exit(2);
}
const testkit = await import(pathToFileURL(`${accountsDir}/testkit/lib/index.ts`).href);
const { AccountsClient, MockMessagingClient, signInWithCode, signUpCarbon } = testkit;

const STACK = JSON.parse(readFileSync(stackFile, 'utf8')) as {
  accounts_api_url: string;
  mock_messaging_url: string;
  apps: Record<string, { app_secret: string }>;
};
for (const url of [STACK.accounts_api_url, STACK.mock_messaging_url]) {
  const host = new URL(url).hostname;
  if (!['localhost', '127.0.0.1', '::1'].includes(host)) {
    console.error(`mint: ${url} is not on this machine; this helper only talks to a local stack`);
    process.exit(2);
  }
}
const accounts = new AccountsClient(STACK.accounts_api_url);
const messaging = new MockMessagingClient(STACK.mock_messaging_url);

function arg(name: string, required = true): string {
  const i = process.argv.indexOf(`--${name}`);
  const v = i >= 0 ? process.argv[i + 1] : undefined;
  if (required && !v) {
    console.error(`mint: --${name} is required`);
    process.exit(2);
  }
  return v ?? '';
}
const flag = (name: string) => process.argv.includes(`--${name}`);
const out = (v: unknown) => console.log(JSON.stringify(v));

function secretOf(app: string): string {
  const s = STACK.apps[app]?.app_secret;
  if (!s) {
    console.error(`mint: no app secret for '${app}' in the stack file (apps: ${Object.keys(STACK.apps).join(', ')})`);
    process.exit(2);
  }
  return s;
}

async function carbonSession(email: string) {
  try {
    const { tokens, session } = await accounts.cliLogin(messaging, { email });
    return { tokens, me: await session.me() };
  } catch {
    const created = await signUpCarbon({ accounts, messaging, email });
    const { tokens } = await accounts.cliLogin(messaging, { email });
    return { tokens, me: created.me };
  }
}

const cmd = process.argv[2];
if (cmd === 'carbon') {
  const { tokens, me } = await carbonSession(arg('email'));
  out({ uuid: me.uuid, id: me.id, kind: 'carbon', access_token: tokens.access_token, refresh_token: tokens.refresh_token });
} else if (cmd === 'app-signin') {
  const app = arg('app');
  const email = arg('email');
  if (!flag('existing')) await carbonSession(email);
  const scope = process.argv.includes('--scope') ? arg('scope') : undefined;
  const r = await signInWithCode({ accounts, messaging, appId: app, email, redirectUri: arg('redirect'), ...(scope ? { scope } : {}) });
  const base = { code: r.code, code_verifier: r.codeVerifier, state: r.state, redirect_uri: r.redirectUri };
  if (flag('exchange')) {
    if (!r.code) throw new Error(`sign-in ended without a code: ${r.error}`);
    const tokens = await accounts.app(app, secretOf(app)).exchangeCode(r.code, r.redirectUri, r.codeVerifier);
    out({ ...base, code: '(used)', code_verifier: '(used)', tokens });
  } else {
    out(base);
  }
} else if (cmd === 'slt') {
  const tokens = await accounts.siliconLogin(arg('silicon'), arg('stk'), 'extend end-to-end tests');
  const res = await fetch(`${accounts.url}/v1/me/short-lived-tokens`, {
    method: 'POST',
    headers: { authorization: `Bearer ${tokens.access_token}`, 'content-type': 'application/json' },
    body: JSON.stringify({ app_id: arg('app') }),
  });
  const body = await res.json();
  if (!res.ok) throw new Error(`short-lived token refused: ${res.status} ${JSON.stringify(body)}`);
  out(body);
} else {
  console.error('usage: mint.mts carbon|app-signin|slt (see the header of this file)');
  process.exit(2);
}
