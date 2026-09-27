# Request: app-level notification types in Ting

To the maintainers of Ting and Honeycomb, from Silicon Extend. 2026-09-27.

We ask for notification types that an app registers once and that work in every Team and every
test environment. Today a type must be registered in each Team, and again after every test clean.

## The problem

Ting keeps notification types per environment, Team and app. `tings.send` refuses a Ting whose type
isn't registered in the Ting's Team, and only that Team's Ting manager can register it:

```sh
ting --org <team> types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'
```

Extend 1.1 sends four types: `extend.device.requested`, `extend.device.wake_requested`,
`extend.device.woken` and `extend.device.wake_declined`. It sends them in whichever Team the Silicon
and the Carbon involved belong to. A device belongs to the Carbon who paired it, and that Carbon can
give access to Silicons from any of their Teams. So Extend sends in every Team where any Carbon uses
it, and every such Team needs four registrations first.

What that means in practice:

- **A new Team gets no Extend notifications** until one of its Ting managers runs four commands. A
  Carbon who never heard of Ting types finds out when a Silicon's wake request doesn't reach them.
- **Some requests have no other way to arrive.** An iPhone, an iPad, a TV in standby or a sleeping
  computer can't show a notification, so Ting is the only way a Silicon's wake request reaches its
  Carbon.
- **Every test clean removes the types.** Each test environment needs the four types registered
  again, in each Team the tests use, after every clean.
- **Extend can't do it for the Team.** An app's login has no type-manager authority, and should not
  need it: these types are Extend's own, and they mean the same thing in every Team.

Today production has `extend.device.requested` registered in one Team (`tos`) only. Other apps with
their own types (DM's `dm.sync.changed`) have the same problem.

What Extend does meanwhile: it records which types Ting reported missing in which Team, shows them
with the exact command (in Settings, on the device page, in `extend ting status`, and after sending),
retries them every 10 minutes instead of silently, and registers them itself with the login of a
Carbon who is that Team's Ting manager, where Ting allows that.

## What we ask for

1. **Types declared by the app, once.** An app declares its notification types (name and
   description) with the app itself, for example in its Honeycomb application definition, reviewed
   like the rest of the application.
2. **Valid in every Team.** `tings.send` accepts an app-level type in any Team, as long as everything
   else holds as today: the sender and the recipient are members of that Team, the OBO proof is
   bound to the sender, and the recipient registered the app (`subscriptions.register`). Consent
   and grants don't change.
3. **Valid in every test environment.** A test environment gets the app's types when the app joins
   it, and a clean keeps them (or re-creates them from the app's definition).
4. **Teams keep control.** A Team's Ting manager can turn an app-level type off in their Team, and a
   member can still turn an app off for themselves.
5. **Names don't change.** Types stay `{app_id}.{event}`, and IAM's app identity still decides which
   app may send a type.

## Compatibility

- Additive. A type registered in a Team keeps working as today; Ting would look up the Team's own
  registration first, then the app-level one.
- An app that declares no types behaves exactly as today.
- Nothing changes for recipients: registration, consent and turning an app off stay as they are.

## What Extend would change

- Declare its four types, with their descriptions, once in its application definition.
- Drop the per-Team runbook and the re-registration after every test clean (`docs/operations.md`,
  `docs/deployment.md`, and the real-service test seeding).
- Stop registering types with a Ting manager's login.
- Keep detecting a missing type as a safety net. Extend's API, CLI and apps need no change: the
  missing-type lists would simply stay empty.

## Questions for you

- Where should an app declare its types: the Honeycomb application definition, or a Ting call made
  with the app's own credentials?
- Should a Team be able to turn an app-level type off, or only its members?
- Should a test environment take an app's types when the app is imported, and keep them through a
  clean?
