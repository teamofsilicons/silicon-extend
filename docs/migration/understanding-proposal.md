# UNDERSTANDING.md: proposed changes for Extend 4

`understanding/UNDERSTANDING.md` is the Carbons' file and is unchanged. These are the smallest
product-level edits that would make it describe Extend 4 (Silicon Accounts and Silicon Apps, no
orgs, no test environments), for a Carbon to accept, change or refuse. Each quotes the current text
and proposes a replacement. The service already behaves as proposed; the CLI and website stages may
add their own wording for the CLI and website parts.

## 1. Glossary: `Org`

Current: "`Org` - This is our organisation, this is where all the silicons and carbons would stay
for a single organisation and defines the scope."

Proposed: remove the entry, and add:

> `Custodian` - The Carbon who looks after a Silicon, set in Silicon Accounts. Every Silicon has one.

Why: Silicon Accounts has personal accounts only; nothing in Extend is scoped by an org any more.

## 2. Login

Current: the whole `# Login` section (Silicon IAm, the IAM client crate, the `/webhook/` endpoint,
`extend --test <test_id>`).

Proposed:

> Signing in and signing up are handled by Silicon Accounts (accounts.teamofsilicons.com, docs at
> developers.teamofsilicons.com). Extend is the app `extend` in Silicon Apps; its app secret stays
> on the server. Every Carbon and Silicon has a personal account with a permanent id that never
> changes and a public id (`c:ada`, `si:scout`) that can. Use the official, latest Silicon Accounts
> client everywhere.
>
> Silicon Accounts tells Extend at `backend.extend.teamofsilicons.com/webhooks/accounts` whenever
> someone signs out of Extend, removes Extend's access, changes their id, gets a new custodian, or
> is deleted, and Extend ends their access at once.

Why: IAM, its webhook and test environments are gone in Extend 4.

## 3. The configuration website and Access: orgs

Current (The configuration website): "Devices belong to the Carbon, not to an org: a Carbon sees
every device they have paired, whichever org they picked, and nobody else sees them." and "see which
Silicons have access, give access to more (from any org the Carbon is a member of), or take it away".

Proposed: "Devices belong to the Carbon who paired them: a Carbon sees every device they have paired,
and nobody else sees them." and "see which Silicons have access, give access to more (any Silicon,
by its id; the Silicons the Carbon looks after are offered first), or take it away".

Current (Access): "…and can pick Silicons from any org they are a member of. … or the Silicon or the
Carbon leaving the Silicon's org ends it immediately…" and the paragraph "A Silicon uses the device
as a member of its own org, and what it does stays in that org. The Carbon sees everything their own
Silicons do on their devices. Silicons from different orgs don't see each other…".

Proposed: "…and can give access to any Silicon. … the Silicon or the Carbon signing out of Extend
or removing Extend's access, or the Silicon getting a new custodian (for the access its previous
custodian gave), ends it immediately…" and:

> A Silicon's custodian sees everything that Silicon does in Extend (its access, sessions, files and
> requests) and can stop it, but never acts as it. The Carbon sees everything any Silicon does on
> their devices. Silicons that don't share a custodian don't see each other: if another Silicon is
> using the device, the others only see that the device is in use.

## 4. One Silicon at a time: who a request goes to

Current: "Only one Silicon can use a device at a time, whichever org it is from and whichever Carbon
gave it access. … If the Silicon using the device is in the same org and was given access by the
same Carbon, the requesting Silicon sees which Silicon it is, and Extend delivers the request to that
Silicon…"

Proposed: "Only one Silicon can use a device at a time, whichever Carbon gave it access. … If the
Silicon using the device has the same custodian as the requesting Silicon and was given access by
the same Carbon, the requesting Silicon sees which Silicon it is, and Extend delivers the request to
that Silicon…"

## 5. Files

Current: "Extend stores it on the Silicon's behalf through Briefcase's OBO endpoint, and
automatically shares it with the Carbon who gave the Silicon access to the device, with create,
read and update access (not delete)…"

Proposed: "Extend stores it on the Silicon's behalf in its own Briefcase, with the Silicon's
permission through Silicon Accounts, and automatically shares it with the Carbon who gave the
Silicon access to the device, with read and update access (not delete)…" (Briefcase allows create
only on folders, as built since 1.0.)

## 6. Testing Environment

Current: the whole `# Testing Environment` section (Honeycomb environments, test app secrets, the
5-device and 10-environment limits, test webhooks).

Proposed: remove it, and add one line under `# Versioning`:

> There are no test environments. To try Extend without touching anything real, run it locally
> against a local Silicon Accounts.

Why: Silicon Apps has no test environments; Extend 4 refuses the old test header instead of
guessing.

## 7. Docs page

Current: "On the docs page, show `honeycomb install 'extend'` to install the CLI, followed by how
to log in."

Proposed: "On the docs page, show `silicon-apps install extend` to install the CLI, followed by how
to sign in."

## 8. Rust Package & CLI: signing in (CLI stage, 2026-10-10)

The CLI and client are built as below (client 4.0.0, CLI 4.0.0); these edits would make the
`# Rust Package & CLI` section say so.

Current: "Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys,
api keys, read, write, patch, delete, everything)."

Proposed: "Everyone should be able to use the CLI/Rust Package (Carbons, Silicons, read, write,
patch, delete, everything)."

Current (--- logging in via cli ---): "For logging in via the cli or the package for any
carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just
request for their short lived token. … For CLI login there should be this exact command: `extend
login <slt>`."

Proposed:

> The CLI never asks for a password. A Carbon runs `extend login`: it shows a short code and a link
> to Silicon Accounts, where the Carbon approves Extend on any device. A Silicon never sees a page:
> it gets a short-lived token for Extend from the Silicon Accounts CLI and hands it over, `silicon-
> accounts login --app extend -q | extend login --slt-stdin` (`extend login <slt>` works too). The
> CLI exchanges the token itself, with no secret in the CLI. The sign-in is kept in the CLI's home
> and refreshed on its own; `extend logout` ends it.

Current (the list after it): "2) `iam --json` the user should be able to run `extend iam --json`
which returns `app_id` alongside other information." and "every app cli must support the following
commands: `app iam --json` gives {app_id: "...", ...}; `app login "..."` takes in a short lived auth
token generated by silicon interpretter; `app login status --json` tells if its {authenticated:
true, ...}".

Proposed: "2) `accounts --json`: `extend accounts --json` returns `app_id`, the Silicon Accounts
Extend signs in at, and how to sign in, with no network and no sign-in." and "every app CLI must
support: `app accounts --json` gives {app_id: "...", ...}; `app login` signs a Carbon in with a code
to approve, and `app login --slt-stdin` (or `app login "<token>"`) takes a short-lived token from
`silicon-accounts login --app <app_id> -q`; `app login status --json` says {authenticated: true,
uuid, id, kind, ...} or {authenticated: false}, and always exits 0."

Current: "CLI is the primary way to interact with IAM Apps." and "it should just ask for short lived
tokens that the user can generate from the official iam cli, or from the web where the the user is
sent to auth concent screen."

Proposed: "CLI is the primary way to interact with Silicon Apps." and "it should never ask for a
password: Carbons approve a code on the Silicon Accounts site, Silicons hand over a short-lived token
from the Silicon Accounts CLI."

Current: "Testing in the test enviorment should also be possible via both cli, and the package." and
"Testing enviorment in cli, … `extend --test <test_id> <command>` …"

Proposed: remove both (see 6).

Current: "All apps are suggested to make a rust library which is stateless. then 2 things that uses
the rust library: always running daemon, and a cli interface that talks to the daemon."

Proposed: keep, and add: "Silicon Apps installs and updates the CLI; no app runs its own updater."

Not proposed: the Silicon runtime still runs `extend iam --json`; the CLI keeps it as a hidden alias
of `extend accounts --json` for one minor release, so it needs no mention here.
