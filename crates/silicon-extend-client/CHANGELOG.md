# Changelog

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
