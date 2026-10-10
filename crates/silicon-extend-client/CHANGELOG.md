# Changelog

## 4.0.0

Extend 4 signs everyone in with Silicon Accounts and has no Teams and no test environments. This
release speaks API v2 (`/api/v2/…`) for every account call; the device-side calls keep API v1, the
wire the installed Extend apps speak. A 3.x client gets `410 api_version_sunset` on its account
calls from an Extend 4 service.

### New

- `auth::SignIn`: sign in to Extend at Silicon Accounts as a public client (`client_id` = `extend`,
  no secret): the device flow for Carbons (`start_device`, `poll_device`, `wait_for_device`,
  honouring `interval` and `slow_down`), short-lived token exchange for Silicons (`exchange_slt`,
  with every refusal named: `slt_already_used`, `slt_expired`, `slt_wrong_app`, `slt_unknown`,
  `slt_sign_in_ended`, `not_an_slt`), rotating `refresh`, and `revoke`. Typed `AuthError` with
  `code`, `message`, `hint`, `status`, `request_id` and `transient`; tokens are `Secret`s that never
  print. `auth::check_url` allows https, and http only for this machine.
- `Client::accounts()` (`GET /api/v2/accounts`): the Silicon Accounts Extend trusts, its app id and
  links, and whether it delivers notifications through Ting.
- `Authed::me()` answers `AccountMe` (uuid, id, kind, name, photo, a Silicon's custodian);
  `Authed::sign_out(refresh_token)`; `Authed::lookup(id)`.
- Custodian views: `Authed::silicons()`, `silicon(id)`, `silicon_grants(id)`, `renounce(silicon,
  device_id)`, and `ListQuery::silicon` for sessions, files and requests.
- `Error::status()` and `Error::is_code()`.

### Changed

- `Client::authed(token)` takes only the access token (no Team).
- `ting_registration()` and `ting_turn_on()` take no Team: one registration per account.
- The version handshake offers 2; each request pins the major its path names
  (`Silicon-Extend-API-Version: 1` on `/api/v1/…`, 2 on `/api/v2/…`). `SUPPORTED_API_VERSIONS` is
  `[2]`; `DEVICE_API_VERSION` is 1.
- `needs_refresh` is true for `401 token_expired` only.
- Service URLs: https, or http for localhost, `127.0.0.1` and `[::1]` only (no longer `10.0.2.2`).

### Removed

- `Client::login`, `refresh`, `logout` and `iam` (Silicon IAM sign-in through the service): use
  `auth::SignIn` and `Authed::sign_out`.
- `ClientBuilder::testing_secret`, `Client::testing_secret`, `Client::testing_environment`: there are
  no test environments.
- `Authed::team_silicons`, `team_silicons_all`, `revoke_in_team`, `importable_devices`,
  `import_device`, `ting_registrations`, `permissions`, `request_permissions`,
  `complete_permissions`, `stop_device` (use `stop`), and `PermissionEndpoint`,
  `FeaturePermission(s)`, `FeaturePermissionRequest`.

Built on `silicon-extend-protocol` 2.0.0 and `silicon-accounts-client` 0.4.0.

## 2.0.0

Removes the experimental Jev/model-backed ref selection API: `ref_actions`,
`Authed::select_ref`, and the `ref_benchmark` example. Use an ordinary `snapshot` command,
choose a ref in your agent, and send the device action through `Authed::run`.

The remaining HTTP API stays at v1. Existing device, session, snapshot, ref action and Android
debugging commands are unchanged. The entries below describe previous releases.

## 1.3.0

Adds `Authed::select_ref` and `ref_actions::RefSelectionRequest` for authenticated managed Jev
selection through service 1.2.0. The server keeps separate production/test provider keys and
returns a decision without executing an action. The caller still owns fresh-snapshot validation
and execution. Direct provider calls continue to support personal keys.

`ModelClient::choose_with_threshold` applies a per-request confidence threshold while reusing
the existing connection pool. API remains v1; protocol and device applications stay at 1.1.0.

## 1.2.0

Adds the experimental `ref_actions` module for selecting an existing device ref with TypeSafe
Jev or a configured OpenAI-compatible model. `Observation` normalizes nested and flat snapshots,
limits choices to supported operations, and validates selected refs. `ModelClient` returns a
decision; callers retain responsibility for screen revalidation and command execution.

Jev evaluates operation and target questions in one request, with confidence-gated abstention.
Exact fill text stays with the caller. The `ref_benchmark` example measures either one provider
or paired providers on recorded observations. API v1 and protocol 1.1.0 remain unchanged.

## 1.1.0

Devices belong to the Carbons who paired them, several Carbons can pair one device, Silicons can
ask a Carbon to wake a device, and failed setup steps can be retried. Built on
`silicon-extend-protocol` 1.1.0, whose `CHANGELOG.md` lists every new type and field. The API stays
v1, and a 1.0 client keeps working against a 1.1 service.

### Update your code

- The re-exported `protocol` types have new fields and variants. Exhaustive `match`es on
  `DeviceFrame` and `ServiceFrame` need an arm for the new variants (or `_`), and struct literals of
  `Device`, `Session`, `FileInfo`, `RequestInfo`, `AccessGrant`, `InUse`, `ActivityEntry`,
  `TeamSilicon`, `DeviceSelf`, `Hello` or `AttachedStatus` need the new fields (or `..`).
- `EnrollmentCreate.agent_device_version` is now `engine_version` (the old name is still read).
- New structs are `#[non_exhaustive]`: build them with their constructors (`WakeAnswer::declined()`,
  `WakeSettings::new(true).silicon(..)`).
- `DeviceQuery`, `ActivityQuery`, `ListQuery`, `PairingClaim`, `AttachmentCreate` and `DevicePatch`
  are unchanged. `DeviceQuery { scope: Some("team") }` and a `visibility` in a claim or patch are
  accepted and have no effect: a device is only visible to the Carbons who paired it.

### New calls

- `Client::pair_enrollment(credential)`: "Pair with another Carbon", for Extend apps.
- `Authed::stop(device_id) -> StopOutcome`: `Session` when the stopped session ran through your own
  pair, `Other(DeviceStopped)` when the Silicon using the device was given access by another Carbon
  who paired it. `stop_device` stays, and decodes only the first.
- `Authed::team_silicons_all()`: the Silicons of every Team your login reaches, each with its Team,
  and which Teams couldn't be read (`TeamSilicons`).
- `Authed::revoke_in_team(device_id, silicon_id, team)`: take access away in one Team only.
  `revoke` still takes it away in every Team.
- Waking a device: `wake(device_id, reason)` (sent with an `Idempotency-Key`), `cancel_wake`,
  `wake_requests(device_id, ListQuery { state, limit, cursor })`, `answer_wake(device_id,
  &WakeAnswer)` and `set_wake_settings(device_id, &WakeSettings)`.
- Ting: `ting_registration(team)`, `ting_registrations()` (every Team) and `ting_turn_on(team)`.
- `retry_setup(device_id, step)`: run a failed setup step (or every failed step) again now.

### Behaviour

- A Carbon's calls on their own devices (list, show, stop, access, activity, requests, sessions,
  files, wake requests) answer for every Team, so `authed(token, None)` works for them. A grant still
  gives access in the `Authed`'s Team.
- `send_request` answers `to_hidden: true` and `to` = `REQUEST_TO_HIDDEN` when the request went to
  the Carbon who gave the Silicon using the device access.

## 1.0.0

First release.
