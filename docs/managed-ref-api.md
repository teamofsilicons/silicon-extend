# Managed ref selection (API v1, service 1.2.0)

`POST /api/v1/sessions/{session_id}/ref-selection` selects one operation and observed element
reference through Silicon-managed Jev. It does not execute an action. Use the normal Extend
authorization, client version and team/test selection headers.

```json
{
  "type": "ref_selection",
  "data": {
    "instruction": "Click Save",
    "snapshot": {"nodes": [{"ref": "@e1", "role": "button", "name": "Save"}]},
    "has_text": false,
    "threshold": 0.7
  }
}
```

`snapshot` is the structured output of `snapshot -i --force-full`; preserve its context and
nodes. `has_text` indicates the caller already holds exact fill text; do not send that literal
text in this field. `threshold` must be finite and between 0 and 1. Requests are limited to
256 KiB before parsing. Selection further limits eligible refs to 254, instruction size to
8 KiB and generated model context to 128 KiB.

The caller must be the Silicon that owns the active session, retain access, and have an online
device. The backend obtains supported commands from that device and constructs the candidate
set itself. Per-principal/world rate limiting and a concurrent-inference limit apply. Managed
inference has a 30-second timeout. A test world requires the configured test key, even when a
production key exists.

Success uses the `ref_selection` envelope and the SDK's `ref_actions::Decision` fields:
`provider`, `model`, `accepted`, `operation`, `target`, `confidence`, `reason`, `model_ms`, and
`usage`. An abstention is a successful response with `accepted: false`. Service/provider failures
use the ordinary error envelope without provider response bodies or credentials.

Before executing an accepted decision, the caller must capture and compare a fresh snapshot,
validate the operation/ref against its candidates, and invoke the ordinary authorized command
once. `extend act` implements this sequence. On selection errors or abstentions, callers can
continue with fresh snapshots and ordinary ref commands.
