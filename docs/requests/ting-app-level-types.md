# Superseded Ting type request

The 2026-09-27 draft was not sent. Its premise that every delivery Team needs its own registration
was disproved by the real Ting 0.1.9 lane on 2026-09-28; it is not an outstanding maintainer request
or a release dependency.

Ting looks up a type by context and app, across delivery Teams. The app-owning Team's manager
registers it through the supported CLI. In each used context, Extend needs these four types:

- `extend.device.requested`
- `extend.device.wake_requested`
- `extend.device.woken`
- `extend.device.wake_declined`

All four production types were registered and listed in the owning Team `tos` on 2026-09-28,
as described in [operations](../operations.md). Do not register duplicates
in every Team receiving a notification. Test contexts need their own supported setup; this does
not imply registrations survive a context clean.

Ting's published OBO catalog has no `types.register`. Extend no longer attempts that unsupported
operation with a Carbon's delegated login. It retains a visible missing-type error and manager CLI
guidance, while recipient consent and subscription registration remain separate. Missing-type
observations can be tracked by delivery Team without implying a Team-scoped type catalog.

The actual local IAM/Ting lane verified globally registered types across three Teams, real missing
catalog entries, and self-send (HTTP 202). See [verification](../verification.md). The inspected Ting
revision is `6853b4e247f434e358f4bbd05e5d23d5ae7870cd` (0.1.9).

App-manifest declarations and automatic provisioning of types could still be proposed as separate
conveniences. They are not needed to obtain cross-Team type lookup, which already exists. No new
request is being sent to Ting or Honeycomb maintainers.
