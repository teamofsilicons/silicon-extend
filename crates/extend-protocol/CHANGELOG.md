# Changelog

## 1.1.0

Devices belong to the Carbons who paired them, several Carbons can pair one device, Silicons can
ask a Carbon to wake a device, failed setup steps can be retried, the in-use banner can be turned
off per device, and the device engine is called Silicon Extend everywhere. The API stays v1: everything below is additive on the wire, and a 1.0
reader ignores the new fields. No `ErrorCode`, `EndReason`, `Capability`, `Visibility` or
`DeviceOs` value is added, because 1.0 readers refuse values they don't know.

### Update your code

- `DeviceFrame` and `ServiceFrame` have new variants: exhaustive `match`es need an arm for them
  (or `_`).
- New fields on existing structs (below): struct literals outside this crate need them, or
  `..` with a value to copy from.
- `Hello.agent_device_version` and `EnrollmentCreate.agent_device_version` are now
  `engine_version`. They are written as `engine_version`, and `agent_device_version` is still read.
- `ServiceFrame::SessionStarted` has a new field, `side`, and `ServiceFrame::Attach` a new field,
  `in_use_indicator`: patterns naming every field need `..`.
- New structs are `#[non_exhaustive]`: build them with their constructors and setters. New enums
  decode values they don't know as `Other`.

### Frames

- `DeviceFrame::Awake {awake, sleep_state, input_seen, run, seq}`: whether the device is awake.
- `DeviceFrame::WakeRequestShown {wake_id, shown, note}`.
- `DeviceFrame::CredentialSaved`.
- `ServiceFrame::WakeRequest {target, wake_id, silicon_id, reason, side, alert, created_at, expires_at}`
  and `ServiceFrame::WakeRequestEnded {target, wake_id, reason: WakeEnd}`.
- `ServiceFrame::Credential {device_credential}`, for computers several Carbons paired. The
  credential is a `DeviceCredential`, whose `Debug` never shows the secret.
- `ServiceFrame::SetupRetry {target, step}`: run failed setup steps again. Sent only to apps whose
  hello lists `"setup_retry"`.
- `ServiceFrame::SessionStarted.side`: the session's side tag.
- `ServiceFrame::Attach.in_use_indicator`: whether a carried device shows the in-use banner. The
  service sends `attach` again when it changes. Absent means shown.
- `Hello.features` (`feature::SETUP_RETRY`) and `Hello::supports`; `Hello.engine_version`.
- `AttachedStatus.awake`, `.sleep_state` and `.hardware_key`.

### Resources

- `Device`: `engine_version` (and `agent_device_version`, a deprecated duplicate), `awake`,
  `sleep_state`, `last_sleep_state`, `awake_changed_at`, `wake_detectable`, `in_use_by_other`,
  `in_use_by_other_carried`, `open_wake_requests`, `wake_requests`, `wake_muted`,
  `paired_by_others` and `same_device`. `team` is absent in owner views, and `visibility` is
  always `personal`.
- `InUse.team`, `AccessGrant.team` and `.wake_muted`, `RequestInfo.team`, `.routed_to`,
  `.to_hidden` and `.from_hidden`, `ActivityEntry.team`, `Session.team`, `FileInfo.team`,
  `TeamSilicon.team`.
- `DeviceSelf.instance_id`, `.hardware_salt` and `.first_pair`.
- `in_use_indicator` (`InUseIndicator`: `shown` or `hidden`; absent means shown) on `Device` and
  `DeviceSelf`, and optional on the new `DeviceSettingsPatch`. The original `DevicePatch`
  keeps its 1.0 source-compatible fields. One setting per physical device, shared by every
  pair of it. New `DeviceSelfPatch`, the body of `PATCH /api/v1/device`, for the device app to
  change it. `InUseIndicator::shows`, `on_off`, `from_on_off` and `AUTO_HIDE_S` (10 seconds).
- `SetupStep.error` is one or two plain sentences for the Carbon; `Setup::failed` and
  `Setup::step`.
- New: `WakeCreate`, `WakeAnswer`, `WakeAnswered`, `WakeSettings`, `WakeSettingsView`,
  `MutedSilicon`, `WakeRequest`, `HostDevice`, `TingRegistration`, `DeviceStopped`, `TeamReach`,
  `TeamSilicons`, `SetupRetryInput` and `RetryResult`.
- New open enums: `SleepState`, `TingStatus`, `TingDelivery`, `WakeState`, `WakeEndReason`,
  `DeviceNotice`, `WakeAnswerKind`, `RequestRoute`, `InUseIndicator` (`#[non_exhaustive]`,
  default `Shown`), and `frames::WakeEnd`. Each has `ALL`,
  `as_str` and `parse`.

### Other

- `ting`: Extend's Ting types (`device.requested`, `device.wake_requested`, `device.woken`,
  `device.wake_declined`), `register_command`, the idempotency keys, and the `WokenBy` and
  `WakeNow` values.
- Constants: `WAKE_REQUEST_TTL_S`, `WAKE_ASK_AGAIN_AFTER_S`, `WAKE_ALERT_EVERY_S`,
  `WAKE_TING_EVERY_S`, `WAKE_TINGS_PER_CARBON_PER_HOUR`, `WAKE_NOTE_MAX_CHARS`,
  `REQUEST_TO_HIDDEN`, `REQUEST_FROM_HIDDEN`, `SIDE_TAG_LEN`, `SETUP_RETRY_EVERY_S` and
  `TERMINAL_NOT_SHARED_REASON`.
- `ids::DeviceCredential`.
- Documentation says "the device engine" where it said agent-device.

## 1.0.0

First release.
