# Device protocol

How a Bridge app (Android, Android TV, Mac, Windows, Linux) talks to the Bridge service. The Rust
types are in `crates/bridge-protocol/src/frames.rs` and `model.rs`; this page is the same contract
for apps written in other languages (the Android app is Kotlin).

Service URL: production `https://backend.bridge.teamofsilicons.com`, local development
`http://127.0.0.1:8480` (the Android emulator reaches it as `http://10.0.2.2:8480`). WebSocket URLs
are the same host with `ws://` / `wss://`.

## 1. Before pairing: enrollment

```
POST /api/v1/enrollments
{"type":"enrollment","data":{"os":"android","os_version":"15","model":"Pixel 9","app_version":"1.0.0"}}

201 {"type":"enrollment","data":{
  "enrollment_id":"0192…","enrollment_secret":"bes_…","pairing_code":"4F9C2A",
  "code_expires_at":"2026-09-26T10:05:00.000Z","rotates_every_s":300}}
```

`os` is one of `android`, `android_tv`, `macos`, `windows`, `linux`.

Show `pairing_code` large (uppercase, 6 hexadecimal characters). Then open the enrollment socket:

```
GET /api/v1/enrollments/{enrollment_id}/connect   (WebSocket upgrade)
Authorization: Bridge-Enrollment <enrollment_secret>
```

Frames from the service (JSON text messages):

```json
{"type":"code","pairing_code":"7B21E0","code_expires_at":"2026-09-26T10:10:00.000Z"}
{"type":"paired","device_id":"7c1e09ab","device_credential":"bdc_…","environment":null}
{"type":"ping","nonce":17}
```

Answer every `ping` with `{"type":"pong","nonce":17}`. On `paired`, store `device_credential` in the
OS secret store (Android Keystore-backed storage, macOS Keychain, Windows DPAPI, libsecret), forget the
enrollment, and move to section 2. If the socket drops, reconnect; if the enrollment is gone (401/404),
start a new one. Polling alternative: `GET /api/v1/enrollments/{id}` with the same header returns
`{"state":"waiting",…}` or, exactly once, `{"state":"paired","device_id":…,"device_credential":…}`.

## 2. Paired: the device socket

```
GET /api/v1/device/connect   (WebSocket upgrade)
Authorization: Bridge-Device <device_credential>
```

Keep it open always. Reconnect with exponential backoff from 1 s to 60 s with full jitter. Close
codes: `4401` credential invalid or device unpaired (forget the credential, go back to enrollment),
`4409` superseded by a newer connection (don't reconnect), `4426` app too old.

### First frame: hello

```json
{"type":"hello","app_version":"1.0.0","os":"android","os_version":"15","model":"Pixel 9",
 "agent_device_version":null,
 "capabilities":["screen.read","screen.capture","input.touch","input.text","nav.system","apps.launch","apps.list","takeover","notifications","links"],
 "missing":[{"capability":"adb","reason":"Wireless debugging is off. Turn it on in Developer options."}],
 "setup":{"state":"needs_carbon","steps":[
   {"key":"accessibility","title":"Allow Silicon Bridge to control the screen","status":"done"},
   {"key":"wireless_debugging","title":"Turn on wireless debugging","status":"needs_carbon","help":"Settings › System › Developer options › Wireless debugging"}]}}
```

Send `hello` again whenever capabilities or setup change (or send `{"type":"setup_progress","setup":{…}}`
for setup-only changes). Capability names are in `crates/bridge-protocol/src/capability.rs`.

### Frames the service sends

```json
{"type":"command","id":"<uuid>","session_id":"a3f","target":null,"command":"click","args":["@e2"],
 "attachments":[],"timeout_ms":30000,"upload_ids":["<uuid>","<uuid>"]}
{"type":"cancel","id":"<uuid>"}
{"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":"2026-09-26T10:00:00.000Z"}
{"type":"session_ended","target":null,"session_id":"a3f","reason":"idle_timeout"}
{"type":"takeover","target":null,"session_id":"a3f","reason":"Please approve Face ID","expires_at":"…"}
{"type":"takeover_ended","target":null,"session_id":"a3f"}
{"type":"refresh"}
{"type":"environment","environment":{"environment_id":"…","name":"checkout-e2e","state":"ready","paired_devices":1,"device_limit":5}}
{"type":"unpaired","reason":"device_removed"}
{"type":"superseded"}
{"type":"ping","nonce":42}
```

- `session_started` → show the in-use indicator naming `silicon_id`, with a Stop button.
- `session_ended` → clear it.
- `takeover` → show the reason and a **Done** button; Done sends `{"type":"takeover_done"}`.
- `refresh` → re-read `GET /api/v1/device` (name, owner, environment).
- `environment` non-null → show a permanent test-environment banner with its name.
- `unpaired` → forget the credential, return to the pairing screen.
- `attach` / `setup_code` are only sent to host computers (Mac, Windows, Linux).

### Frames the device sends

```json
{"type":"result","id":"<same uuid>","ok":true,"output":{…},"text":"Tapped @e2 \"Continue\"","error":null,
 "files":[{"upload_id":"<uuid>","name":"screenshot.png","content_type":"image/png","kind":"screenshot","size_bytes":184223}]}
{"type":"stop"}                               (a host adds "target":"<device_id>" to stop a device it carries)
{"type":"takeover_done"}                      (same optional "target")
{"type":"pong","nonce":42}
```

A failed command still answers `result`, with `ok:false` and
`"error":{"code":"…","message":"…"}`. Use `"code":"unsupported_on_device"` for a command this device
can't do, `"code":"invalid_args"` for arguments it can't parse, and a precise message either way.

### Commands

`command` is the top-level name and `args` the remaining CLI tokens, exactly as agent-device's CLI
takes them (see `vendor/agent-device/website/docs/docs/commands.md` and `understanding/cli.yaml`,
`device_commands`). The list of names is `COMMANDS` in `capability.rs`. Refs like `@e2` come from the
latest `snapshot` in the same session.

### Attachments (files the caller sends with a command)

A command may carry `attachments: [{"name","content_type","content_base64"}]` (a replay script, an
APK to install, an image or video to show on a TV). Write each one into the command's scratch
directory, then replace every argument of the form `attachment:<name>` with that file's local path
before running the command. Example: `display show --image attachment:cat.png` with an attachment
named `cat.png`.

### Files

For each file a command produces, upload it before sending the `result`, using the next unused id
from `upload_ids`:

```
PUT /api/v1/device/artifacts/{upload_id}
Authorization: Bridge-Device <device_credential>
Content-Type: image/png
X-Content-SHA256: <64 lowercase hex of the bytes>
X-File-Name: screenshot.png
<bytes>
```

Then list it in `result.files`. Kinds: `screenshot`, `recording`, `log`, `replay_script`, `diff`, `other`.

## 3. Other device endpoints

```
GET    /api/v1/device        → {"type":"device_self","data":{device_id,name,owner:{type,id},team,os,in_use,takeover,setup,environment}}
DELETE /api/v1/device        → 204. "Revoke pair" (confirm with the Carbon first).
POST   /api/v1/device/stop   → 204. Same as the stop frame, for when the socket is down.
```

All with `Authorization: Bridge-Device <device_credential>`.

## 4. What every app shows

1. Unpaired: the pairing code, large, and never a login.
2. During setup: each setup step with where to find it on this device.
3. Paired: device name, the Carbon it's paired to (`owner.id`), whether a Silicon is using it.
4. In use: which Silicon, with Stop.
5. Revoke pair, with a confirmation.
6. A test-environment banner with its name whenever `environment` is set.

It starts when the device starts and keeps the socket open.
