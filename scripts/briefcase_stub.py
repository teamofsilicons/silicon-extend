#!/usr/bin/env python3
"""A local stand-in for Briefcase's delegated routes, for end-to-end tests of Extend.

    BRIEFCASE_STUB_APP_SECRET=… python3 -I scripts/briefcase_stub.py --port 4222 \\
        --accounts-api http://127.0.0.1:9589 --log .mig/dev-accounts/briefcase-calls.jsonl

It answers the routes Extend calls at Briefcase (`/api/v1/obo/uploads/reserve`, the capability
transfer `PUT /api/v1/obo/uploads/{id}/content`, `uploads/commit`, `uploads/status`,
`uploads/cancel`, `invitations`, `files/read`, `entries/trash`) with the shapes of Briefcase 4.0's
openapi.yaml, and verifies every `Authorization: Proof sap_…` the way Briefcase does: at Silicon
Accounts (`POST /v1/proofs/verify`) with Briefcase's own app credentials, on every call. A call is
refused unless the proof is `valid`, its receiving app is `briefcase`, its issuing app is one this
stand-in accepts (`--issuers`, default `extend`) and its scopes include the route's scope; it then
acts as the proof's `user.uuid`. Files live in memory, owned by that account and readable by it and
the accounts it shared them with.

Every call is appended to the log as one JSON line: the route, the answer, and what the proof said
(its id, kind, issuing and receiving app, user, scopes, expiry, and the first 12 hex characters of
the token's SHA-256). Tokens, refresh tokens, capabilities and file bytes are never logged.

Test helpers (bound to 127.0.0.1 only, like the whole stand-in):
  GET  /ready            200 when listening
  GET  /stub/entries     every stored entry: id, owner, issuing app, name, size, shares, trashed
  POST /stub/reverify    verifies again every proof token this process has seen and answers
                         [{sha, valid, proof_id, user}], so a test can check that a proof Extend sent
                         was revoked later (Extend revokes what it holds when an account signs out)
"""
import argparse
import base64
import hashlib
import json
import os
import re
import secrets
import sys
import threading
import time
import uuid
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.request import ProxyHandler, Request, build_opener

OPENER = build_opener(ProxyHandler({}))
APP_ID = "briefcase"
SCOPES = {
    "uploads/reserve": "briefcase.uploads.reserve",
    "uploads/commit": "briefcase.uploads.commit",
    "uploads/status": "briefcase.uploads.status",
    "uploads/cancel": "briefcase.uploads.cancel",
    "invitations": "briefcase.invitations.create",
    "files/read": "briefcase.files.read",
    "entries/trash": "briefcase.entries.trash",
}
CONTENT = re.compile(r"^/api/v1/obo/uploads/([0-9a-fA-F-]{36})/content$")


def sha(token):
    return hashlib.sha256(token.encode()).hexdigest()[:12]


def now_iso(delta=0):
    return (datetime.now(timezone.utc) + timedelta(seconds=delta)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


class Store:
    """Uploads and entries, in memory, behind one lock."""

    def __init__(self):
        self.lock = threading.Lock()
        self.uploads = {}  # operation_id -> upload
        self.by_upload = {}  # upload_id -> operation_id
        self.entries = {}  # entry_id -> entry
        self.invitations = {}  # operation_id -> invitation answer
        self.trashes = set()  # operation ids already applied
        self.tokens = {}  # sha -> proof token (memory only, for /stub/reverify)


class Stub:
    def __init__(self, accounts_api, secret, issuers, log_path):
        self.accounts_api = accounts_api.rstrip("/")
        self.basic = "Basic " + base64.b64encode(f"{APP_ID}:{secret}".encode()).decode()
        self.issuers = set(issuers)
        self.log_path = log_path
        self.log_lock = threading.Lock()
        self.store = Store()

    def log(self, record):
        record = {"at": now_iso(), **record}
        with self.log_lock, open(self.log_path, "a") as handle:
            handle.write(json.dumps(record, sort_keys=True) + "\n")

    def verify(self, token):
        """Silicon Accounts' answer for a proof token, as Briefcase gets it."""
        body = json.dumps({"proof_token": token}).encode()
        request = Request(f"{self.accounts_api}/v1/proofs/verify", data=body, method="POST",
                          headers={"Authorization": self.basic, "Content-Type": "application/json",
                                   "Accept": "application/json"})
        try:
            with OPENER.open(request, timeout=15) as response:
                return json.loads(response.read() or b"{}")
        except HTTPError as error:
            with error:
                return {"valid": False, "http_status": error.code, "error": error.read()[:300].decode(errors="replace")}
        except (URLError, OSError) as error:
            return {"valid": False, "error": f"Silicon Accounts unreachable: {getattr(error, 'reason', error)}"}


def summary(verified, token):
    """What the log keeps of a verification: never the token itself."""
    user = verified.get("user") or {}
    return {
        "sha": sha(token),
        "valid": bool(verified.get("valid")),
        "proof_id": verified.get("proof_id"),
        "kind": verified.get("kind"),
        "issuing_app": (verified.get("issuing_app") or {}).get("app_id"),
        "receiving_app": (verified.get("receiving_app") or {}).get("app_id"),
        "user": {k: user.get(k) for k in ("uuid", "id", "kind")} if user else None,
        "scopes": verified.get("scopes"),
        "expires_at": verified.get("expires_at"),
        "error": verified.get("error"),
    }


def make_handler(stub):
    store = stub.store

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        # --- answers -----------------------------------------------------------------------------------------

        def send_json(self, status, payload):
            raw = b"" if payload is None else json.dumps(payload).encode()
            self.send_response(status)
            if payload is not None:
                self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(raw)))
            self.send_header("cache-control", "no-store")
            self.end_headers()
            self.wfile.write(raw)

        def error(self, status, code, message):
            self.send_json(status, {"error": {"code": code, "message": message,
                                              "request_id": f"stub-{uuid.uuid4().hex[:12]}"}})

        def body_bytes(self):
            length = int(self.headers.get("content-length") or 0)
            return self.rfile.read(length) if length else b""

        # --- routes ------------------------------------------------------------------------------------------

        def do_GET(self):  # noqa: N802 (http.server naming)
            if self.path == "/ready":
                return self.send_json(200, {"ready": True})
            if self.path == "/stub/entries":
                with store.lock:
                    entries = [{k: v for k, v in e.items() if k != "bytes"} | {"size": len(e["bytes"])}
                               for e in store.entries.values()]
                return self.send_json(200, {"entries": entries})
            return self.error(404, "not_found", f"The Briefcase stand-in has no GET {self.path}.")

        def do_PUT(self):  # noqa: N802
            match = CONTENT.match(self.path)
            if not match:
                return self.error(404, "not_found", f"The Briefcase stand-in has no PUT {self.path}.")
            return self.transfer(match.group(1))

        def do_POST(self):  # noqa: N802
            if self.path == "/stub/reverify":
                self.body_bytes()
                with store.lock:
                    seen = list(store.tokens.items())
                out = []
                for digest, token in seen:
                    v = stub.verify(token)
                    out.append({"sha": digest, "valid": bool(v.get("valid")), "proof_id": v.get("proof_id"),
                                "user": (v.get("user") or {}).get("uuid")})
                return self.send_json(200, {"proofs": out})
            route = self.path.removeprefix("/api/v1/obo/")
            if route not in SCOPES or not self.path.startswith("/api/v1/obo/"):
                self.body_bytes()
                return self.error(404, "not_found", f"The Briefcase stand-in has no POST {self.path}.")
            raw = self.body_bytes()
            try:
                body = json.loads(raw or b"{}")
            except ValueError:
                return self.error(400, "invalid_json", "The body is not JSON.")
            record = {"route": route, "method": "POST"}
            auth = self.headers.get("authorization") or ""
            if not auth.startswith("Proof "):
                record.update(status=401, refused="no_proof")
                stub.log(record)
                return self.error(401, "proof_required", "Send Authorization: Proof sap_… (a User verification proof).")
            token = auth[len("Proof "):].strip()
            verified = stub.verify(token)
            proof = summary(verified, token)
            record["proof"] = proof
            with store.lock:
                store.tokens[proof["sha"]] = token
            why = None
            if not proof["valid"]:
                why = (401, "invalid_proof", "Silicon Accounts says this proof is not valid (expired, revoked, or not for Briefcase).")
            elif proof["receiving_app"] != APP_ID:
                why = (401, "invalid_proof", f"The proof is for {proof['receiving_app']}, not Briefcase.")
            elif proof["kind"] != "user_verification" or not proof["user"]:
                why = (403, "user_verification_required", "These routes act for an account: send a User verification proof.")
            elif proof["issuing_app"] not in stub.issuers:
                why = (403, "app_not_allowed", f"Briefcase does not accept {proof['issuing_app']} for {SCOPES[route]}.")
            elif SCOPES[route] not in (proof["scopes"] or []):
                why = (403, "scope_not_granted", f"The proof does not carry {SCOPES[route]}.")
            if why:
                record.update(status=why[0], refused=why[1])
                stub.log(record)
                return self.error(*why)
            account = proof["user"]["uuid"]
            status, payload, extra = getattr(self, "route_" + route.replace("/", "_"))(body, account, proof["issuing_app"])
            record.update(status=status, **extra)
            stub.log(record)
            if route == "files/read" and status == 200:
                return self.send_file(payload)
            return self.send_json(status, payload)

        # --- the delegated routes ----------------------------------------------------------------------------

        def route_uploads_reserve(self, body, account, issuer):
            op = body.get("operation_id")
            with store.lock:
                upload = store.uploads.get(op)
                if upload is None:
                    upload = {"operation_id": op, "upload_id": str(uuid.uuid4()), "state": "reserved",
                              "expires_at": now_iso(3600), "published_entry_id": None, "account": account,
                              "issuer": issuer, "name": body.get("name"), "content_type": body.get("content_type"),
                              "size": body.get("size"), "sha256": body.get("sha256"), "bytes": None,
                              "capability": "cap_" + secrets.token_urlsafe(24)}
                    store.uploads[op] = upload
                    store.by_upload[upload["upload_id"]] = op
                elif upload["account"] != account:
                    return 404, {"error": {"code": "not_found", "message": "No such upload for this account."}}, {}
                answer = status_of(upload) | {"capability": upload["capability"] if upload["state"] == "reserved" else None}
            return 200, answer, {"operation_id": op, "name": body.get("name"), "size": body.get("size"),
                                 "parent_path": body.get("parent_path")}

        def transfer(self, upload_id):
            data = self.body_bytes()
            record = {"route": "uploads/{id}/content", "method": "PUT", "x_org_id": self.headers.get("x-org-id"),
                      "proof_sent": bool(self.headers.get("authorization")), "size": len(data)}
            with store.lock:
                op = store.by_upload.get(upload_id)
                upload = store.uploads.get(op) if op else None
                if upload is None:
                    record.update(status=404)
                    stub.log(record)
                    return self.error(404, "not_found", "No reservation has this upload id.")
                if self.headers.get("x-briefcase-upload-capability") != upload["capability"]:
                    record.update(status=401, refused="capability")
                    stub.log(record)
                    return self.error(401, "invalid_capability", "The upload capability is missing or wrong.")
                org = self.headers.get("x-org-id")
                if org is not None and org != upload["account"]:
                    record.update(status=403, refused="x_org_id")
                    stub.log(record)
                    return self.error(403, "drive_mismatch", "X-Org-ID names another account than the reservation's.")
                if len(data) != upload["size"] or hashlib.sha256(data).hexdigest() != upload["sha256"]:
                    record.update(status=409, refused="digest")
                    stub.log(record)
                    return self.error(409, "content_mismatch", "The bytes do not match the reserved size and SHA-256.")
                upload["bytes"], upload["state"] = data, "staged"
                answer = status_of(upload)
            record.update(status=200, account=upload["account"])
            stub.log(record)
            return self.send_json(200, answer)

        def route_uploads_commit(self, body, account, issuer):
            with store.lock:
                upload = store.uploads.get(body.get("operation_id"))
                if upload is None or upload["account"] != account or upload["upload_id"] != body.get("upload_id"):
                    return 404, {"error": {"code": "not_found", "message": "No such upload for this account."}}, {}
                if upload["state"] == "staged":
                    entry_id = str(uuid.uuid4())
                    store.entries[entry_id] = {"entry_id": entry_id, "owner": account, "created_by_app": issuer,
                                               "name": upload["name"], "content_type": upload["content_type"],
                                               "bytes": upload["bytes"], "shared_with": [], "trashed": False}
                    upload["state"], upload["published_entry_id"] = "committed", entry_id
                answer = status_of(upload)
            return 200, answer, {"operation_id": upload["operation_id"], "entry_id": upload["published_entry_id"]}

        def route_uploads_status(self, body, account, issuer):
            with store.lock:
                upload = store.uploads.get(body.get("operation_id"))
                if upload is None or upload["account"] != account:
                    return 404, {"error": {"code": "not_found", "message": "No such upload for this account."}}, {}
                return 200, status_of(upload), {"operation_id": upload["operation_id"], "state": upload["state"]}

        def route_uploads_cancel(self, body, account, issuer):
            with store.lock:
                upload = store.uploads.get(body.get("operation_id"))
                if upload is None or upload["account"] != account:
                    return 404, {"error": {"code": "not_found", "message": "No such upload for this account."}}, {}
                if upload["state"] in ("reserved", "staged"):
                    upload["state"] = "cancelled"
                return 200, status_of(upload), {"operation_id": upload["operation_id"]}

        def route_invitations(self, body, account, issuer):
            invitation = body.get("invitation") or {}
            principal = invitation.get("principal") or {}
            with store.lock:
                entry = store.entries.get(body.get("entry_id"))
                if entry is None or entry["owner"] != account or entry["trashed"]:
                    return 404, {"error": {"code": "not_found", "message": "No such entry for this account."}}, {}
                answer = store.invitations.get(body.get("operation_id"))
                if answer is None:
                    answer = {"id": str(uuid.uuid4()), "principal": principal,
                              "access": invitation.get("access") or ["read"], "inherit": invitation.get("inherit", True),
                              "expires_at": None}
                    store.invitations[body.get("operation_id")] = answer
                    entry["shared_with"].append({"type": principal.get("type"), "id": principal.get("id"),
                                                 "access": answer["access"]})
            return 200, answer, {"entry_id": body.get("entry_id"), "principal": principal,
                                 "access": invitation.get("access")}

        def route_files_read(self, body, account, issuer):
            with store.lock:
                entry = store.entries.get(body.get("entry_id"))
                readable = entry is not None and not entry["trashed"] and (
                    entry["owner"] == account
                    or any(s["id"] == account and "read" in s["access"] for s in entry["shared_with"]))
                if not readable:
                    return 404, {"error": {"code": "not_found", "message": "The file is absent or unreadable."}}, \
                        {"entry_id": body.get("entry_id"), "reader": account}
                return 200, entry, {"entry_id": entry["entry_id"], "reader": account, "owner": entry["owner"]}

        def route_entries_trash(self, body, account, issuer):
            with store.lock:
                entry = store.entries.get(body.get("entry_id"))
                if entry is None or entry["owner"] != account:
                    return 404, {"error": {"code": "not_found", "message": "Entry is absent or not visible."}}, {}
                if entry["created_by_app"] != issuer:
                    return 403, {"error": {"code": "not_created_by_app",
                                           "message": "The issuing app did not create this entry."}}, {}
                entry["trashed"] = True
                store.trashes.add(body.get("operation_id"))
            return 204, None, {"entry_id": body.get("entry_id"), "operation_id": body.get("operation_id")}

        def send_file(self, entry):
            self.send_response(200)
            self.send_header("content-type", entry["content_type"] or "application/octet-stream")
            self.send_header("content-length", str(len(entry["bytes"])))
            self.send_header("cache-control", "private, no-store")
            self.send_header("x-content-type-options", "nosniff")
            self.end_headers()
            self.wfile.write(entry["bytes"])

    return Handler


def status_of(upload):
    return {k: upload[k] for k in ("operation_id", "upload_id", "state", "expires_at", "published_entry_id")}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--accounts-api", required=True, help="Silicon Accounts' API origin (the local stack)")
    parser.add_argument("--log", required=True, help="JSON lines file every call is appended to")
    parser.add_argument("--issuers", default="extend", help="comma-separated issuing apps accepted (default extend)")
    args = parser.parse_args(argv)
    secret = os.environ.get("BRIEFCASE_STUB_APP_SECRET", "")
    if not secret:
        print("briefcase-stub: set BRIEFCASE_STUB_APP_SECRET to Briefcase's app secret at the local stack", file=sys.stderr)
        return 2
    stub = Stub(args.accounts_api, secret, [i.strip() for i in args.issuers.split(",") if i.strip()], args.log)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), make_handler(stub))
    print(f"briefcase stand-in on http://127.0.0.1:{args.port} (verifying proofs at {args.accounts_api})", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
