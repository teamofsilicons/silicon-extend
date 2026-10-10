# Real Briefcase integration — 10 October 2026

Verified Extend's existing native enrollment/WebSocket/file-upload protocol against real Briefcase and Moto storage after the shared Accounts UUID cutover. The device was the scripted native protocol fixture, reporting Android TV capabilities and producing a known 1×1 PNG. This proves the service, native protocol and delegated storage flow; it does not claim a physical device or actual screen capture.

An isolated current Extend service on `127.0.0.1:4223` used database `extend_real_briefcase`, real Briefcase `127.0.0.1:4101`, and its website origin `localhost:4100`. The complete final 211-row retired-subject ledger was applied before the first request. Primary Extend data, native credentials and its Accounts webhook callback remained intact.

The successful flow used fresh canonical Accounts UUIDs and real delegation proofs:

1. A Carbon paired the device and explicitly granted access to its Silicon.
2. The Silicon opened a session and requested a screenshot. The native SDK uploaded the 67-byte PNG; Extend reserved, transferred and committed it through Briefcase.
3. Both the owning Silicon and explicitly shared Carbon downloaded exactly the same bytes. SHA256: `2b1da20a14b97d8f01f0a809d9f7d53eeefc59df6312eaa5a0c8b5c1228d1d7f`.
4. A `display show --image file:<id>` request resolved the stored file through a new delegated read. The native fixture echoed the attachment it actually received, proving byte equality across the return transfer.
5. The Carbon kept the file permanently; its self-destruct timestamp cleared. The Silicon ended the session.

The fixture's durable file ID is `01a1251b-47b4-7ef0-aa15-5534302a9a22`; its Briefcase location is `/org/4eb1de48-df66-4801-ba07-d69f59c81692/apps/extend/screenshot-01a1251b-47aa-7684-84b0-086ccbc90627.png`. The generated website URL uses the real Briefcase origin. The supported legacy location route preserves compatibility with existing Extend file links.

The actual production-build website was then opened at that exact link while signed out. Real hosted Accounts sign-in as the existing explicit Carbon recipient returned to the file, opened the screenshot dialog, decoded the image, and downloaded the identical 67 bytes through the BFF. The clean `/{UUID}/...` location also passed, with no browser page errors. This exposed and fixed Briefcase's anonymous private-link handoff: concealed 404 responses now prompt sign-in and preserve the exact return path. Evidence: Briefcase `.mig/uuid-cutover/extend-link-proof.json`, `extend-link.log`, and `extend-link.png` (visually inspected by the Briefcase task); its complete browser suite passed 28 tests.

The accepted Extend issuer scopes are `uploads.reserve`, `uploads.commit`, `uploads.status`, `uploads.cancel`, `files.read`, `invitations.create` and `entries.trash`. A foreign-drive file read requires an explicit share and is confined to the `apps/extend` entry. Custody alone grants no read; foreign listing or writing remains denied. Briefcase regression `src/api/delegation_tests.rs::confined_file_read_honors_foreign_shares_without_custodial_powers` (commit `a751d67`) covers no-share custodian refusal, whole/range/download success after sharing, wrong scope/outsider/outside-folder refusal, foreign listing/trash refusal, and immediate revocation. The Briefcase service suite passed 236 tests with strict clippy.

Evidence is preserved in `.mig/real-briefcase/`: `result.json`, `integration.log`, `screenshot-command.json`, `display-command.json`, `files.json`, `native-device.log`, `screenshot.png`, and the actual `integration.py` / `setup.py` runners. Private token/identity inputs are separate mode-0600 files. The example rebuilt successfully and formatting checks passed. The earlier Linux fixture correctly refused its unsupported display command; the final successful run used a display-capable Android TV fixture.

After verification, the isolated service was stopped. Its PostgreSQL dump and complete private state were archived as mode-0600 files alongside the primary app snapshots. Commit `.mig/cutover/final-cleanup.json` records paths and hashes; no listeners remain in any of the four owned app port blocks. Briefcase owns the matching database/object archive and has completed the website link check.

No production service, public package or existing original checkout was changed. Native archive candidates remain development-profile macOS ARM64 builds; optimized and other-platform releases are still separate verification gates.
