#!/usr/bin/env python3
"""A fake Extend service for testing the Android app against docs/device-protocol.md (1.1).

Standard library only (HTTP/1.1 and a minimal RFC 6455 WebSocket on asyncio). It implements the
device-facing half of the protocol, for several Carbons' pairs of one device:

    POST   /api/v1/enrollments                 enrollment + first pairing code
    POST   /api/v1/device/enrollments          "Pair with another Carbon" (Extend-Device auth)
    GET    /api/v1/enrollments/{id}            poll (Extend-Enrollment auth)
    GET    /api/v1/enrollments/{id}/connect    enrollment WebSocket: code rotations, then paired
    GET    /api/v1/device/connect              device WebSocket, one per pair (Extend-Device auth)
    GET    /api/v1/device                      device_self of the credential's pair
    DELETE /api/v1/device                      revoke that pair
    POST   /api/v1/device/stop                 stop
    PUT    /api/v1/device/artifacts/{upload}   uploads, checked against X-Content-SHA256

plus test controls:

    GET  /_test/state    {"pairing_code", "paired", "pairs": [...], "hellos", ...}
    POST /_test/claim    pairs the waiting enrollment (what a Carbon entering the code does)
    POST /_test/command  {"command", "args", "uploads", "pair"?} -> the result frame
    POST /_test/frame    any service frame; "_pair" picks the pair's socket (default: the first)
    POST /_test/close    {"code", "pair"?}

Each Carbon's pair has its own device id, credential and socket; the first pair belongs to
c:alice, the next to c:bob, then c:carol. With --scenario, once the device says hello it runs a
list of commands and checks each result, printing PASS/FAIL lines, and exits non-zero if anything
failed. Scenarios: phone, tv, smoke, and multi (several Carbons, waking, sides, Stop, revoke).

Every adb call it makes (the scenarios look at the emulator from outside) goes to $ANDROID_SERIAL
only: it never talks to another device.
"""

import argparse
import asyncio
import base64
import hashlib
import json
import os
import re
import secrets
import struct
import sys
import time
import uuid
from datetime import datetime, timedelta, timezone

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
OWNERS = [("c:alice", "Alice"), ("c:bob", None), ("c:carol", None), ("c:dave", None), ("c:erin", None), ("c:frank", None), ("c:grace", None), ("c:heidi", None)]
MAX_PAIRS = len(OWNERS)


def now_iso(offset_s=0):
    t = datetime.now(timezone.utc) + timedelta(seconds=offset_s)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


async def adb(*args):
    """Runs adb against $ANDROID_SERIAL only (the test's own emulator); returns stdout bytes."""
    serial = os.environ.get("ANDROID_SERIAL")
    if not serial:
        raise RuntimeError("set ANDROID_SERIAL to the emulator's serial: the scenarios never pick a device themselves")
    exe = os.environ.get("ADB", os.path.expanduser("~/Library/Android/sdk/platform-tools/adb"))
    p = await asyncio.create_subprocess_exec(exe, "-s", serial, *args, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    out, _ = await p.communicate()
    return out


# ───────────────────────────── WebSocket ─────────────────────────────

class WebSocket:
    def __init__(self, reader, writer):
        self.reader = reader
        self.writer = writer
        self.closed = False
        self.close_code = None

    async def _read_frame(self):
        h = await self.reader.readexactly(2)
        fin = h[0] & 0x80
        op = h[0] & 0x0F
        masked = h[1] & 0x80
        n = h[1] & 0x7F
        if n == 126:
            n = struct.unpack(">H", await self.reader.readexactly(2))[0]
        elif n == 127:
            n = struct.unpack(">Q", await self.reader.readexactly(8))[0]
        mask = await self.reader.readexactly(4) if masked else None
        data = bytearray(await self.reader.readexactly(n))
        if mask:
            for i in range(n):
                data[i] ^= mask[i % 4]
        return fin, op, bytes(data)

    async def _send(self, op, payload: bytes):
        if self.closed and op != 0x8:
            return
        header = bytearray([0x80 | op])
        n = len(payload)
        if n < 126:
            header.append(n)
        elif n < 65536:
            header.append(126)
            header += struct.pack(">H", n)
        else:
            header.append(127)
            header += struct.pack(">Q", n)
        self.writer.write(bytes(header) + payload)
        await self.writer.drain()

    async def send(self, obj):
        text = obj if isinstance(obj, str) else json.dumps(obj)
        await self._send(0x1, text.encode())

    async def recv(self):
        """The next text message, or None once the peer closed."""
        buf = b""
        while True:
            try:
                fin, op, data = await self._read_frame()
            except (asyncio.IncompleteReadError, ConnectionError):
                self.closed = True
                return None
            if op == 0x9:  # ping
                await self._send(0xA, data)
                continue
            if op == 0xA:
                continue
            if op == 0x8:
                self.close_code = struct.unpack(">H", data[:2])[0] if len(data) >= 2 else 1005
                if not self.closed:
                    self.closed = True
                    try:
                        await self._send(0x8, data[:2])
                    except Exception:
                        pass
                return None
            buf += data
            if fin:
                return buf.decode()

    async def close(self, code=1000, reason=""):
        if self.closed:
            return
        self.closed = True
        try:
            await self._send(0x8, struct.pack(">H", code) + reason.encode())
            await asyncio.sleep(0.2)
            self.writer.close()
        except Exception:
            pass


# ───────────────────────────── State ─────────────────────────────

class Pair:
    """One Carbon's pair of the device."""

    def __init__(self, device_id, credential, owner, display, name, first):
        self.device_id = device_id
        self.credential = credential
        self.owner = owner
        self.display = display
        self.name = name
        self.first = first
        self.ws = None
        self.revoked = False
        self.session = None
        self.takeover = None
        self.hellos = []
        self.awakes = []
        self.connections = 0
        self.refuse = False

    def view(self):
        return {"device_id": self.device_id, "owner": self.owner, "name": self.name, "first_pair": self.first,
                "connected": self.ws is not None and not self.ws.closed, "hellos": len(self.hellos),
                "awakes": len(self.awakes), "revoked": self.revoked}


class State:
    def __init__(self, args):
        self.args = args
        self.enrollments = {}  # id -> dict
        self.pairs = {}  # device_id -> Pair, in pairing order
        self.instance_id = str(uuid.uuid4())
        self.events = asyncio.Queue()  # frames from the device other than results, tagged "_pair"
        self.results = {}  # id -> future
        self.uploads = {}  # upload_id -> {name, content_type, bytes, pair}
        self.http_log = []
        self.environment = None
        self.failures = 0
        self.passes = 0
        self.scenario_started = False

    @property
    def live(self):
        return [p for p in self.pairs.values() if not p.revoked]

    @property
    def primary(self):
        live = self.live
        return live[0] if live else None

    def by_credential(self, auth):
        for p in self.pairs.values():
            if not p.revoked and auth == "Extend-Device " + p.credential:
                return p
        return None

    def waiting_enrollment(self):
        """The newest enrollment still waiting (an app shows only its latest code)."""
        for e in reversed(list(self.enrollments.values())):
            if not e["paired"]:
                return e
        return None


def new_code():
    return secrets.token_hex(3).upper()


# ───────────────────────────── HTTP ─────────────────────────────

async def read_request(reader):
    line = await reader.readline()
    if not line:
        return None
    method, path, _ = line.decode().rstrip("\r\n").split(" ", 2)
    headers = {}
    while True:
        h = await reader.readline()
        if h in (b"\r\n", b"\n", b""):
            break
        k, v = h.decode().split(":", 1)
        headers[k.strip().lower()] = v.strip()
    body = b""
    if "content-length" in headers:
        body = await reader.readexactly(int(headers["content-length"]))
    return method, path, headers, body


async def respond(writer, status, obj=None, raw=None, ctype="application/json"):
    reasons = {200: "OK", 201: "Created", 204: "No Content", 409: "Conflict", 400: "Bad Request", 401: "Unauthorized", 404: "Not Found", 422: "Unprocessable Entity", 503: "Service Unavailable"}
    body = raw if raw is not None else (json.dumps(obj).encode() if obj is not None else b"")
    head = f"HTTP/1.1 {status} {reasons.get(status, 'OK')}\r\nContent-Length: {len(body)}\r\nConnection: close\r\n"
    if body:
        head += f"Content-Type: {ctype}\r\n"
    writer.write(head.encode() + b"\r\n" + body)
    await writer.drain()
    writer.close()


def error(code, message):
    return {"type": "error", "data": {"code": code, "message": message}}


def device_self(st, p):
    return {
        "type": "device_self",
        "data": {
            "device_id": p.device_id,
            "name": p.name,
            "owner": {"type": "carbon", "id": p.owner, **({"display_name": p.display} if p.display else {})},
            "team": "acme",
            "os": p.hellos[-1]["os"] if p.hellos else "android",
            "in_use": p.session,
            "takeover": p.takeover,
            "setup": p.hellos[-1]["setup"] if p.hellos else {"state": "in_progress", "steps": []},
            "environment": st.environment,
            "instance_id": st.instance_id,
            "first_pair": p.first,
        },
    }


def new_enrollment(st, kind, data, from_pair=None):
    eid = str(uuid.uuid4())
    e = {
        "id": eid, "secret": "ees_" + secrets.token_urlsafe(32)[:43], "code": new_code(), "expires": now_iso(st.args.rotate_s),
        "paired": False, "request": data, "sockets": [], "kind": kind, "from": from_pair.device_id if from_pair else None, "pair": None,
    }
    st.enrollments[eid] = e
    log("enrollment", kind, eid, "code", e["code"], "from", from_pair.device_id if from_pair else data)
    return {"type": "enrollment", "data": {
        "enrollment_id": eid, "enrollment_secret": e["secret"], "pairing_code": e["code"],
        "code_expires_at": e["expires"], "rotates_every_s": st.args.rotate_s}}


async def handle(st: State, reader, writer):
    req = await read_request(reader)
    if req is None:
        writer.close()
        return
    method, path, headers, body = req
    st.http_log.append((method, path))
    auth = headers.get("authorization", "")
    if headers.get("upgrade", "").lower() == "websocket":
        key = headers.get("sec-websocket-key", "")
        m = re.fullmatch(r"/api/v1/enrollments/([^/]+)/connect", path)
        pair = None
        ok = False
        if m and m.group(1) in st.enrollments and auth == "Extend-Enrollment " + st.enrollments[m.group(1)]["secret"]:
            ok = True
        if path == "/api/v1/device/connect":
            pair = st.by_credential(auth)
            if pair is not None and pair.refuse:
                log("WS refused on purpose for", pair.device_id)
                await respond(writer, 503, error("unavailable", "try again"))
                return
            ok = pair is not None
        if not ok:
            log("WS refused", path, "auth=", auth[:24])
            await respond(writer, 401, error("unauthorized", "bad credentials"))
            return
        accept = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
        writer.write(
            ("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
             f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode()
        )
        await writer.drain()
        ws = WebSocket(reader, writer)
        if m:
            await enrollment_socket(st, st.enrollments[m.group(1)], ws)
        else:
            await device_socket(st, pair, ws)
        return

    log("HTTP", method, path)
    if method == "POST" and path == "/api/v1/enrollments":
        payload = json.loads(body or b"{}")
        await respond(writer, 201, new_enrollment(st, "first", payload.get("data", {})))
        return
    if method == "POST" and path == "/api/v1/device/enrollments":
        p = st.by_credential(auth)
        if p is None:
            await respond(writer, 401, error("unauthorized", "device credential invalid"))
            return
        if st.args.old_service:
            await respond(writer, 404, error("not_found", f"No such endpoint: {method} {path}"))
            return
        if len(st.live) >= MAX_PAIRS:
            await respond(writer, 409, error("conflict", f"This device is paired to {MAX_PAIRS} Carbons, the most Extend allows."))
            return
        if body:
            log("FAIL-NOTE: POST /device/enrollments had a body:", body[:200])
        await st.events.put({"type": "_http_pair_enrollment", "_pair": p.device_id})
        await respond(writer, 201, new_enrollment(st, "pair", {}, from_pair=p))
        return
    m = re.fullmatch(r"/api/v1/enrollments/([^/]+)", path)
    if m:
        e = st.enrollments.get(m.group(1))
        if not e or auth != "Extend-Enrollment " + e["secret"]:
            await respond(writer, 404 if not e else 401, error("not_found", "no such enrollment"))
            return
        if method == "DELETE":
            del st.enrollments[e["id"]]
            await respond(writer, 204)
            return
        if e["paired"]:
            p = e["pair"]
            del st.enrollments[e["id"]]
            await respond(writer, 200, {"type": "enrollment", "data": {"state": "paired", "device_id": p.device_id, "device_credential": p.credential, "environment": None}})
        else:
            await respond(writer, 200, {"type": "enrollment", "data": {"state": "waiting", "pairing_code": e["code"], "code_expires_at": e["expires"]}})
        return
    if path.startswith("/api/v1/device"):
        p = st.by_credential(auth)
        if p is None:
            await respond(writer, 401, error("unauthorized", "device credential invalid"))
            return
        if method == "GET" and path == "/api/v1/device":
            await respond(writer, 200, device_self(st, p))
            return
        if method == "DELETE" and path == "/api/v1/device":
            p.revoked = True
            await st.events.put({"type": "_http_revoke", "_pair": p.device_id})
            await respond(writer, 204)
            if p.ws:
                await p.ws.close(4401, "pair revoked")
            return
        if method == "POST" and path == "/api/v1/device/stop":
            await st.events.put({"type": "_http_stop", "_pair": p.device_id})
            await respond(writer, 204)
            return
        m = re.fullmatch(r"/api/v1/device/artifacts/([^/]+)", path)
        if method == "PUT" and m:
            digest = hashlib.sha256(body).hexdigest()
            if headers.get("x-content-sha256") != digest:
                await respond(writer, 422, error("digest_mismatch", "X-Content-SHA256 does not match"))
                return
            st.uploads[m.group(1)] = {"name": headers.get("x-file-name"), "content_type": headers.get("content-type"), "bytes": body, "pair": p.device_id}
            os.makedirs(st.args.out, exist_ok=True)
            with open(os.path.join(st.args.out, headers.get("x-file-name") or m.group(1)), "wb") as f:
                f.write(body)
            log("upload", m.group(1), headers.get("x-file-name"), len(body), "bytes, pair", p.device_id)
            await respond(writer, 201)
            return
    if path == "/_test/state":
        e = st.waiting_enrollment()
        p = st.primary
        await respond(writer, 200, {
            "pairing_code": e["code"] if e else None, "enrollment_kind": e["kind"] if e else None,
            "enrollment_request": e["request"] if e else None,
            "paired": p is not None, "device_id": p.device_id if p else None,
            "hellos": sum(len(x.hellos) for x in st.pairs.values()), "last_hello": p.hellos[-1] if p and p.hellos else None,
            "pairs": [x.view() for x in st.pairs.values()],
            "passes": st.passes, "failures": st.failures,
            "enrollments_created": len([x for x in st.http_log if x == ("POST", "/api/v1/enrollments")]),
        })
        return
    if path == "/_test/command" and method == "POST":
        # {"command": "snapshot", "args": ["-i"], "uploads": 0, "attachments": [...], "pair": id?} -> the result frame
        req = json.loads(body or b"{}")
        sc = Scenario(st)
        sc.pair = st.pairs.get(req.get("pair")) or st.primary
        if sc.pair is None or sc.pair.ws is None:
            await respond(writer, 409, error("offline", "device isn't connected"))
            return
        sc.session = req.get("session_id", "a3f")
        res = await sc.cmd(req["command"], req.get("args", []), uploads=req.get("uploads", 0),
                           attachments=req.get("attachments", []), timeout_ms=req.get("timeout_ms", 30000))
        await respond(writer, 200, res)
        return
    if path == "/_test/frame" and method == "POST":
        # Send any service frame to the device, e.g. {"type":"session_started",...}; "_pair" picks the socket.
        frame = json.loads(body or b"{}")
        p = st.pairs.get(frame.pop("_pair", None)) or st.primary
        track(p, frame, st)
        if p and p.ws:
            await p.ws.send(frame)
        await respond(writer, 200, {"sent": frame.get("type"), "pair": p.device_id if p else None})
        return
    if path == "/_test/close" and method == "POST":
        # Close a pair's device socket with a code, e.g. {"code": 4409}
        req = json.loads(body or b"{}")
        p = st.pairs.get(req.get("pair")) or st.primary
        code = req.get("code", 1000)
        if p and p.ws:
            await p.ws.close(code, "test")
        await respond(writer, 200, {"closed": code})
        return
    if path == "/_test/claim" and method == "POST":
        e = st.waiting_enrollment()
        if not e:
            await respond(writer, 404, error("no_enrollment", "nothing is waiting"))
            return
        p = await claim(st, e)
        await respond(writer, 200, {"device_id": p.device_id, "owner": p.owner})
        return
    await respond(writer, 404, error("unknown_command", f"No such endpoint: {method} {path}"))


async def claim(st, e):
    """What a Carbon entering the code does: a new pair for the next Carbon."""
    n = len(st.pairs)
    owner, display = OWNERS[n % len(OWNERS)]
    names = ["Test Pixel", "Family phone", "Kitchen screen", "Shared device"]
    first = e["kind"] == "first"
    if first:
        # A device that lost every pair starts again as a new device.
        st.instance_id = str(uuid.uuid4()) if not st.live else st.instance_id
    p = Pair(secrets.token_hex(4), "edc_" + secrets.token_urlsafe(32)[:43], owner, display, st.args.device_name if n == 0 else names[n % len(names)], first)
    st.pairs[p.device_id] = p
    e["paired"] = True
    e["pair"] = p
    frame = {"type": "paired", "device_id": p.device_id, "device_credential": p.credential, "environment": None}
    log("claimed", e["kind"], e["code"], "-> device", p.device_id, "for", owner)
    if st.args.scenario and not st.scenario_started:
        async def watchdog():
            await asyncio.sleep(60)
            if not p.hellos:
                log("FAIL no hello within 60 s of pairing")
                os._exit(2)
        asyncio.create_task(watchdog())
    for ws in list(e["sockets"]):
        await ws.send(frame)
        await ws.close(1000)
    return p


async def enrollment_socket(st, e, ws):
    e["sockets"].append(ws)
    log("enrollment socket open", e["id"])
    # A rotation straight away, then every rotate_s seconds.
    e["code"], e["expires"] = new_code(), now_iso(st.args.rotate_s)
    await ws.send({"type": "code", "pairing_code": e["code"], "code_expires_at": e["expires"]})

    async def rotate():
        nonce = 0
        while not ws.closed:
            await asyncio.sleep(min(st.args.rotate_s, 10))
            if ws.closed or e["paired"]:
                return
            nonce += 1
            await ws.send({"type": "ping", "nonce": nonce})

    task = asyncio.create_task(rotate())
    try:
        while True:
            msg = await ws.recv()
            if msg is None:
                break
            log("enrollment <-", msg)
    finally:
        task.cancel()
        if ws in e["sockets"]:
            e["sockets"].remove(ws)


async def device_socket(st, p, ws):
    if p.ws and not p.ws.closed:
        old = p.ws
        await old.send({"type": "superseded"})
        await old.close(4409, "superseded")
    p.ws = ws
    p.connections += 1
    log("device socket open: pair %s (%s) #%d" % (p.device_id, p.owner, p.connections))
    try:
        while True:
            msg = await ws.recv()
            if msg is None:
                log("device socket of %s closed by device, code" % p.device_id, ws.close_code)
                break
            frame = json.loads(msg)
            t = frame.get("type")
            if t == "result":
                fut = st.results.get(frame.get("id"))
                if fut and not fut.done():
                    frame["_pair"] = p.device_id
                    fut.set_result(frame)
                else:
                    log("unexpected result", frame.get("id"))
                continue
            if t == "hello":
                p.hellos.append(frame)
                log("hello", p.device_id, json.dumps({k: frame.get(k) for k in ("os", "os_version", "model", "app_version", "capabilities", "features")}))
                if not st.scenario_started and st.args.scenario:
                    st.scenario_started = True
                    asyncio.create_task(run_scenario(st))
            else:
                log("device %s ->" % p.device_id, msg[:300])
                if t == "takeover_done":
                    for x in st.pairs.values():
                        x.takeover = None
                if t == "awake":
                    p.awakes.append(frame)
            frame["_pair"] = p.device_id
            await st.events.put(frame)
    finally:
        if p.ws is ws:
            p.ws = None


def track(p, frame, st=None):
    """Keep GET /api/v1/device consistent with the frames sent (like the real service)."""
    if p is None:
        return
    t = frame.get("type")
    if t in ("session_ended", "takeover_ended") and st is not None:
        # The session may run through another pair than the socket this frame went on.
        for x in st.pairs.values():
            if x.session and x.session.get("session_id") == frame.get("session_id"):
                if t == "session_ended":
                    x.session = None
                x.takeover = None
    if t == "session_started":
        p.session = {"silicon_id": frame["silicon_id"], "session_id": frame["session_id"], "since": frame["since"], "paused": False}
        p.takeover = None
    elif t == "session_ended":
        p.session = None
        p.takeover = None
    elif t == "takeover":
        p.takeover = {"takeover_id": str(uuid.uuid4()), "session_id": frame["session_id"], "reason": frame["reason"], "started_at": now_iso(), "expires_at": frame["expires_at"]}
    elif t == "takeover_ended":
        p.takeover = None


# ───────────────────────────── Scenarios ─────────────────────────────

class Scenario:
    def __init__(self, st: State):
        self.st = st
        self.session = "a3f"
        # The pair whose socket carries this scenario's frames and commands (default: the first).
        self.pair = st.primary

    def check(self, name, cond, detail=""):
        if cond:
            self.st.passes += 1
            log("PASS", name)
        else:
            self.st.failures += 1
            log("FAIL", name, "--", str(detail)[:1500])
        return cond

    async def send(self, frame, pair=None):
        p = pair or self.pair or self.st.primary
        if p is None or p.ws is None:
            raise RuntimeError("device isn't connected")
        track(p, frame, self.st)
        await p.ws.send(frame)

    async def cmd(self, command, args=(), uploads=0, attachments=(), timeout_ms=30000, pair=None):
        cid = str(uuid.uuid4())
        fut = asyncio.get_running_loop().create_future()
        self.st.results[cid] = fut
        frame = {
            "type": "command", "id": cid, "session_id": self.session, "target": None, "command": command,
            "args": list(args), "attachments": list(attachments), "timeout_ms": timeout_ms,
            "upload_ids": [str(uuid.uuid4()) for _ in range(uploads)],
        }
        await self.send(frame, pair)
        try:
            res = await asyncio.wait_for(fut, timeout_ms / 1000 + 5)
        except asyncio.TimeoutError:
            res = {"ok": False, "error": {"code": "_no_result", "message": "no result within the timeout"}}
        res["_frame"] = frame
        text = (res.get("text") or "")
        log(f"  {command} {' '.join(args)[:80]} -> ok={res.get('ok')} {('' if res.get('ok') else res.get('error'))}")
        if text:
            for line in text.splitlines()[:40]:
                log("    |", line)
        return res

    async def expect_event(self, type_, timeout=10.0, pred=lambda f: True):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                f = await asyncio.wait_for(self.st.events.get(), max(0.1, deadline - time.time()))
            except asyncio.TimeoutError:
                break
            if f.get("type") == type_ and pred(f):
                return f
        return None

    async def drain_events(self):
        while not self.st.events.empty():
            self.st.events.get_nowait()

    def ref_for(self, snap_res, *labels):
        """The ref of the first node whose label matches one of labels (case-insensitive, contains)."""
        text = snap_res.get("text") or ""
        for want in labels:
            for line in text.splitlines():
                m = re.match(r"\s*(@e\d+) \[[^\]]+\] \"(.*?)\"", line)
                if m and want.lower() in m.group(2).lower():
                    return m.group(1)
        return None


async def scenario_common_start(sc: Scenario, want_os):
    st = sc.st
    hello = st.primary.hellos[-1]
    sc.check("hello.os is %s" % want_os, hello.get("os") == want_os, hello.get("os"))
    sc.check("hello has app_version/os_version/model", all(hello.get(k) for k in ("app_version", "os_version", "model")), hello)
    sc.check("hello.setup has steps", len(hello.get("setup", {}).get("steps", [])) >= 3, hello.get("setup"))
    missing = {m["capability"] for m in hello.get("missing", [])}
    sc.check("hello reports adb/apps.install/logs as missing with reasons", {"adb", "apps.install", "logs"} <= missing, hello.get("missing"))
    await sc.send({"type": "ping", "nonce": 42})
    pong = await sc.expect_event("pong", 5, lambda f: f.get("nonce") == 42)
    sc.check("ping 42 -> pong 42", pong is not None)
    await sc.send({"type": "session_started", "target": None, "session_id": sc.session, "silicon_id": "si:chef", "since": now_iso(), "side": "side-alice"})
    return hello


async def phone_scenario(sc: Scenario):
    st = sc.st
    hello = await scenario_common_start(sc, "android")
    caps = set(hello["capabilities"])
    for c in ["screen.read", "screen.capture", "input.touch", "input.text", "nav.system", "apps.launch", "apps.list", "takeover", "links", "alerts"]:
        sc.check(f"capability {c}", c in caps, sorted(caps))
    sc.check("no TV-only capabilities on a phone", not ({"input.remote", "display"} & caps), sorted(caps))
    sc.check("screen.record requires Android debugging setup",
             any(m["capability"] == "screen.record" and "Android debugging" in m["reason"] for m in hello["missing"]))

    r = await sc.cmd("home")
    sc.check("home", r["ok"], r)
    r = await sc.cmd("open", ["com.android.settings"])
    sc.check("open com.android.settings", r["ok"] and r["output"].get("foreground") == "com.android.settings", r)
    await sc.cmd("wait", ["1500"])
    snap = await sc.cmd("snapshot", ["-i"])
    sc.check("snapshot -i ok with nodes and @e1", snap["ok"] and len(snap["output"]["nodes"]) > 0 and "@e1 [" in snap["text"], snap)
    sc.check("snapshot json has documented node fields", snap["ok"] and all(
        k in snap["output"]["nodes"][0] for k in ("ref", "role", "label", "text", "value", "rect", "enabled", "focused", "selected", "editable", "children")))
    full = await sc.cmd("snapshot")
    sc.check("snapshot (full) ok", full["ok"] and len(full["output"]["refs"]) > 0 and full["output"]["total_nodes"] >= len(full["output"]["refs"]), full.get("error"))
    # Refs belong to the latest snapshot in the session (the full one just taken).
    ref = sc.ref_for(full, "Network", "Connected devices", "Apps")
    snap = full
    sc.check("found a Settings row ref", ref is not None, snap.get("text"))
    if ref:
        before = snap["text"]
        r = await sc.cmd("get", ["text", ref])
        sc.check("get text @ref", r["ok"] and len(r["text"]) > 0, r)
        r = await sc.cmd("click", [ref])
        sc.check(f"click {ref}", r["ok"], r)
        await sc.cmd("wait", ["1500"])
        after = await sc.cmd("snapshot", ["-i"])
        sc.check("screen changed after click", after["ok"] and after["text"] != before, after.get("text"))
        r = await sc.cmd("back")
        sc.check("back", r["ok"], r)
        r = await sc.cmd("wait", ["text", "Apps", "5000"])
        sc.check("wait text Apps after back", r["ok"], r)
    r = await sc.cmd("click", ["@e999"])
    sc.check("click @e999 -> stale_ref", not r["ok"] and r["error"]["code"] == "stale_ref", r)
    r = await sc.cmd("is", ["visible", 'text="Apps"'])
    sc.check("is visible text=Apps", r["ok"], r)
    r = await sc.cmd("is", ["absent", 'text="No such row anywhere"'])
    sc.check("is absent", r["ok"], r)
    r = await sc.cmd("is", ["visible", 'text="No such row anywhere"'])
    sc.check("is visible (false) -> assertion_failed", not r["ok"] and r["error"]["code"] == "assertion_failed", r)
    r = await sc.cmd("find", ["Apps", "list"])
    sc.check("find Apps list", r["ok"] and r["output"]["matches"] >= 1, r)
    r = await sc.cmd("screenshot", ["settings.png"], uploads=1)
    up = r["_frame"]["upload_ids"][0]
    stored = st.uploads.get(up)
    sc.check("screenshot uploaded as PNG under the command's upload id", r["ok"] and stored is not None and stored["bytes"][:8] == b"\x89PNG\r\n\x1a\n"
             and r["files"] and r["files"][0]["upload_id"] == up and r["files"][0]["kind"] == "screenshot"
             and r["files"][0]["size_bytes"] == len(stored["bytes"]) and stored["name"] == "settings.png", r)
    r = await sc.cmd("screenshot", ["--scale", "0.25", "--overlay-refs"], uploads=1)
    sc.check("screenshot --scale 0.25 --overlay-refs", r["ok"] and r["output"]["width"] < 400, r)
    r = await sc.cmd("screenshot")
    sc.check("screenshot without upload ids -> upload_failed", not r["ok"] and r["error"]["code"] == "upload_failed", r)
    r = await sc.cmd("scroll", ["down"])
    sc.check("scroll down", r["ok"], r)
    r = await sc.cmd("scroll", ["up", "--pixels", "300"])
    sc.check("scroll up --pixels 300", r["ok"], r)
    r = await sc.cmd("swipe", ["540", "1500", "540", "900"])
    sc.check("swipe", r["ok"], r)
    r = await sc.cmd("appstate")
    sc.check("appstate is Settings", r["ok"] and r["output"]["package"] == "com.android.settings", r)
    r = await sc.cmd("apps")
    sc.check("apps", r["ok"] and "apps" in r["output"], r.get("error"))
    r = await sc.cmd("apps", ["--all"])
    sc.check("apps --all includes com.android.settings", r["ok"] and any(a["package"] == "com.android.settings" for a in r["output"]["apps"]), r.get("error"))
    r = await sc.cmd("alert", ["get"])
    sc.check("alert get (none)", r["ok"] and r["output"]["present"] is False, r)

    # Text entry through Settings search.
    r = await sc.cmd("click", ['id="com.android.settings:id/search_action_bar"'])
    sc.check("click the search bar by selector", r["ok"], r)
    await sc.cmd("wait", ["2000"])
    r = await sc.cmd("type", ["wifi"])
    sc.check("type into the focused search field", r["ok"], r)
    r = await sc.cmd("keyboard", ["status"])
    sc.check("keyboard status", r["ok"], r)
    r = await sc.cmd("fill", ["editable", "bluetooth"])
    sc.check("fill editable (selector) bluetooth", r["ok"], r)
    r = await sc.cmd("get", ["text", "editable"])
    sc.check("get text reads back the filled text", r["ok"] and r["output"]["text"] == "bluetooth", r)
    r = await sc.cmd("keyboard", ["dismiss"])
    sc.check("keyboard dismiss", r["ok"], r)
    await sc.cmd("close", ["com.google.android.settings.intelligence"])
    await sc.cmd("home")

    r = await sc.cmd("clipboard", ["write", "hello-extend"])
    sc.check("clipboard write", r["ok"], r)
    r = await sc.cmd("clipboard", ["read"])
    sc.check("clipboard read returns what was written", r["ok"] and r["output"].get("text") == "hello-extend", r)
    if "notifications" in caps:
        r = await sc.cmd("notifications")
        sc.check("notifications lists items", r["ok"] and isinstance(r["output"]["items"], list), r)
    r = await sc.cmd("open", ["https://example.com"])
    sc.check("open a link", r["ok"], r)
    await sc.cmd("wait", ["1500"])
    r = await sc.cmd("close")
    sc.check("close (foreground app)", r["ok"], r)
    r = await sc.cmd("app-switcher")
    sc.check("app-switcher", r["ok"], r)
    await sc.cmd("home")
    r = await sc.cmd("longpress", ["540", "1200", "600"])
    sc.check("longpress x y ms", r["ok"], r)
    await sc.cmd("home")

    steps = json.dumps([{"command": "home"}, {"command": "open", "args": ["Settings"]}, {"command": "appstate"}])
    r = await sc.cmd("batch", ["--steps", steps])
    sc.check("batch home/open/appstate", r["ok"] and len(r["output"]["steps"]) == 3, r)
    script = "# replay\nhome\nopen com.android.settings\nwait 1000\nsnapshot -i\n"
    att = {"name": "flow.ad", "content_type": "text/plain", "content_base64": base64.b64encode(script.encode()).decode()}
    r = await sc.cmd("replay", ["attachment:flow.ad"], attachments=[att])
    sc.check("replay attachment:flow.ad (attachment written to a local path)", r["ok"] and len(r["output"]["steps"]) == 4, r)
    r = await sc.cmd("replay", ["attachment:missing.ad"], attachments=[att])
    sc.check("attachment:<unknown> -> invalid_args", not r["ok"] and r["error"]["code"] == "invalid_args", r)

    for command, args, code in [
        ("record", ["start"], "unsupported_on_device"),
        ("adb", ["shell", "ls"], "unsupported_on_device"),
        ("install", ["com.x", "x.apk"], "unsupported_on_device"),
        ("display", ["show", "--text", "hi"], "unsupported_on_device"),
        ("tv-remote", ["press", "down"], "unsupported_on_device"),
        ("teleport", [], "unsupported_on_device"),
        ("click", [], "invalid_args"),
        ("scroll", ["sideways"], "invalid_args"),
    ]:
        r = await sc.cmd(command, args)
        sc.check(f"{command} {' '.join(args)} -> {code}", not r["ok"] and r["error"]["code"] == code and len(r["error"]["message"]) > 10, r)

    # Cancel: a long wait, cancelled; the next command still runs.
    cid_task = asyncio.create_task(sc.cmd("wait", ["20000"], timeout_ms=25000))
    await asyncio.sleep(1.0)
    cancel_id = [k for k, f in st.results.items() if not f.done()]
    if cancel_id:
        await sc.send({"type": "cancel", "id": cancel_id[-1]})
    await asyncio.sleep(0.5)
    r = await sc.cmd("appstate")
    sc.check("a command runs right after a cancelled one", r["ok"], r)
    cid_task.cancel()

    await ui_checks(sc, tv=False)


async def ui_checks(sc: Scenario, tv: bool):
    """Drive the Extend app itself: environment banner, takeover Done, Stop, refresh, reconnect, revoke."""
    st = sc.st
    st.environment = {"environment_id": str(uuid.uuid4()), "name": "checkout-e2e", "state": "ready", "paired_devices": 1, "device_limit": 5}
    await sc.send({"type": "environment", "environment": st.environment})
    await sc.send({"type": "refresh"})
    await asyncio.sleep(1.0)
    sc.check("refresh -> GET /api/v1/device", ("GET", "/api/v1/device") in st.http_log[-10:], st.http_log[-10:])
    await sc.send({"type": "takeover", "target": None, "session_id": sc.session, "reason": "Please approve the payment", "expires_at": now_iso(1800)})
    r = await sc.cmd("open", ["com.teamofsilicons.extend"])
    sc.check("open the Extend app", r["ok"], r)
    await sc.cmd("wait", ["1500"])
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("app shows the test-environment banner", "Test environment: checkout-e2e" in t, t)
    sc.check("app shows which Silicon is using it", "si:chef is using this" in t, t)
    sc.check("app shows the takeover reason", "Please approve the payment" in t, t)
    await sc.drain_events()
    r = await sc.cmd("find", ["Done", "click"])
    sc.check("tap Done", r["ok"], r)
    done = await sc.expect_event("takeover_done", 5)
    sc.check("Done -> takeover_done frame", done is not None)
    await sc.send({"type": "takeover_ended", "target": None, "session_id": sc.session})
    await sc.cmd("wait", ["800"])
    await sc.drain_events()
    # Stop cancels every command in this session, including this synthetic tap. A result
    # from the cancelled command is not required; the Stop frame is the observable action.
    click_stop = asyncio.create_task(sc.cmd("find", ["Stop", "click"]))
    stop = await sc.expect_event("stop", 5)
    sc.check("tap Stop", stop is not None)
    sc.check("Stop -> stop frame", stop is not None)
    click_stop.cancel()
    await sc.send({"type": "session_ended", "target": None, "session_id": sc.session, "reason": "stopped_by_carbon"})
    refused = await sc.cmd("snapshot")
    sc.check("ended session cannot issue another command", refused.get("error", {}).get("code") == "session_ended", refused)
    # Observe the now-idle app through the emulator harness, outside the ended Silicon session.
    await asyncio.sleep(0.8)
    await adb("shell", "uiautomator", "dump", "/sdcard/extend-stopped.xml")
    xml = await adb("shell", "cat", "/sdcard/extend-stopped.xml")
    sc.check("indicator cleared after session_ended", b"No Silicon is using this" in xml, xml.decode()[:1000])

    # Reconnect after the service drops the socket.
    p = st.primary
    hellos = len(p.hellos)
    await p.ws.close(1011, "restarting")
    t0 = time.time()
    while time.time() - t0 < 8 and len(p.hellos) == hellos:
        await asyncio.sleep(0.2)
    sc.check("reconnects and says hello again after a drop (%.1f s)" % (time.time() - t0), len(p.hellos) > hellos)

    # Revoke pair from the app, with its confirmation dialog.
    sc.session = "b71"
    await sc.send({"type": "session_started", "target": None, "session_id": sc.session, "silicon_id": "si:chef", "since": now_iso(), "side": "side-alice"})
    await sc.cmd("open", ["com.teamofsilicons.extend"])
    await sc.cmd("wait", ["1000"])
    await reach(sc, tv, "Revoke pair")
    r = await sc.cmd("find", ["Revoke pair", "click"])
    sc.check("tap Revoke pair", r["ok"], r)
    await sc.cmd("wait", ["800"])
    snap = await sc.cmd("snapshot")
    sc.check("revoke asks for confirmation", "Revoke pair?" in (snap.get("text") or ""), snap.get("text"))
    await sc.drain_events()
    enrollments_before = len([p for p in st.http_log if p == ("POST", "/api/v1/enrollments")])
    confirm = asyncio.create_task(sc.cmd("find", ["Revoke pair", "click", "--last"], timeout_ms=8000))
    ev = await sc.expect_event("_http_revoke", 10)
    sc.check("confirm -> DELETE /api/v1/device", ev is not None)
    t0 = time.time()
    while time.time() - t0 < 10:
        if len([p for p in st.http_log if p == ("POST", "/api/v1/enrollments")]) > enrollments_before:
            break
        await asyncio.sleep(0.2)
    sc.check("after revoke the app asks for a new pairing code", len([p for p in st.http_log if p == ("POST", "/api/v1/enrollments")]) > enrollments_before)
    confirm.cancel()


async def tv_scenario(sc: Scenario):
    st = sc.st
    hello = await scenario_common_start(sc, "android_tv")
    caps = set(hello["capabilities"])
    for c in ["screen.read", "screen.capture", "input.text", "input.remote", "nav.system", "apps.launch", "display", "links"]:
        sc.check(f"TV capability {c}", c in caps, sorted(caps))
    sc.check("TV has no touch/keyboard/clipboard/notifications", not ({"input.touch", "input.keyboard", "clipboard", "notifications"} & caps), sorted(caps))
    await sc.cmd("home")
    r = await sc.cmd("display", ["show", "--text", "Dinner is ready"])
    sc.check("display show --text", r["ok"], r)
    snap = await sc.cmd("snapshot")
    sc.check("the display shows the text", "Dinner is ready" in (snap.get("text") or ""), snap.get("text"))
    r = await sc.cmd("screenshot", ["tv-display.png"], uploads=1)
    sc.check("screenshot of the display (badge visible in the corner)", r["ok"], r)
    r = await sc.cmd("tv-remote", ["press", "back"])
    sc.check("tv-remote press back", r["ok"], r)
    await sc.cmd("wait", ["800"])
    snap = await sc.cmd("snapshot")
    sc.check("back cleared the display", "Dinner is ready" not in (snap.get("text") or ""), snap.get("text"))
    png = base64.b64encode(open(st.args.image, "rb").read()).decode() if st.args.image else None
    if png:
        att = {"name": "cat.png", "content_type": "image/png", "content_base64": png}
        r = await sc.cmd("display", ["show", "--image", "attachment:cat.png"], attachments=[att])
        sc.check("display show --image attachment:cat.png", r["ok"], r)
        snap = await sc.cmd("snapshot")
        sc.check("the display shows an image", "Silicon Extend display: image" in (snap.get("text") or ""), snap.get("text"))
        r = await sc.cmd("screenshot", ["tv-image.png"], uploads=1)
        sc.check("screenshot of the image on the display", r["ok"], r)
        r = await sc.cmd("display", ["clear"])
        sc.check("display clear", r["ok"] and r["output"]["cleared"] is True, r)
    r = await sc.cmd("display", ["show", "--url", "https://example.com"])
    sc.check("display show --url", r["ok"], r)
    await sc.cmd("wait", ["2500"])
    r = await sc.cmd("display", ["clear"])
    sc.check("display clear (url)", r["ok"], r)
    for b in ["down", "up", "right", "left", "select", "home", "play-pause", "volume-up"]:
        r = await sc.cmd("tv-remote", ["press", b])
        sc.check(f"tv-remote press {b}", r["ok"], r)
    r = await sc.cmd("tv-remote", ["press", "menu"])
    sc.check("tv-remote press menu -> unsupported with reason", not r["ok"] and r["error"]["code"] == "unsupported_on_device", r)
    r = await sc.cmd("click", ["540", "1200"])
    sc.check("click on a TV -> unsupported_on_device (no input.touch)", not r["ok"] and r["error"]["code"] == "unsupported_on_device", r)
    r = await sc.cmd("snapshot", ["-i"])
    sc.check("snapshot -i on the TV home", r["ok"], r)
    await sc.send({"type": "session_ended", "target": None, "session_id": sc.session, "reason": "ended_by_silicon"})



async def wait_for(cond, timeout, step=0.2):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return True
        await asyncio.sleep(step)
    return cond()


def sleeping_after(frames, baseline, sleep_state):
    """Require a new final sleep state; Android may report locked before screen-off."""
    return (len(frames) > baseline and frames[-1].get("awake") is False
            and frames[-1].get("sleep_state") == sleep_state)


async def dumpsys_notifications():
    return (await adb("shell", "dumpsys", "notification", "--noredact")).decode(errors="replace")


def around(text, *needles, width=3):
    """The lines around each needle, for a failure's detail."""
    lines = text.splitlines()
    out = []
    for i, line in enumerate(lines):
        if any(n in line for n in needles):
            # The record and the section it is in.
            record = next((lines[j].strip()[:200] for j in range(i, -1, -1) if "Record(" in lines[j] or lines[j].startswith("  ") and not lines[j].startswith("   ")), "")
            section = next((lines[j].strip() for j in range(i, -1, -1) if lines[j].startswith("  ") and not lines[j].startswith("   ")), "")
            out.append(f"[{section}] {record} :: " + " | ".join(x.strip() for x in lines[max(0, i - width):i + width + 1]))
    return "\n".join(out[:8])


async def keep_on_windows():
    """What `dumpsys window` says about windows keeping the screen on."""
    out = (await adb("shell", "dumpsys", "window", "windows")).decode(errors="replace")
    keeper = "Silicon Extend keeps the screen on" in out
    badge_on = False
    for block in out.split("Window #"):
        if "Silicon Extend in-use badge" in block and "KEEP_SCREEN_ON" in block:
            badge_on = True
    return keeper, badge_on


async def reach(sc: Scenario, tv: bool, text: str, tries: int = 25):
    """Scrolls the Extend app's page down (the D-pad on a TV) until [text] is on screen."""
    for _ in range(tries):
        r = await sc.cmd("find", [text, "exists"])
        if r.get("ok"):
            return True
        if tv:
            for _ in range(2):
                await sc.cmd("tv-remote", ["press", "down"])
        else:
            await sc.cmd("scroll", ["down", "--pixels", "500"])
        await sc.cmd("wait", ["300"])
    return False


async def page_end(sc: Scenario, tv: bool, bottom: bool):
    """Moves the Extend app's page to its end: a scroll on a phone, the remote's D-pad on a TV."""
    if tv:
        for _ in range(30):
            await sc.cmd("tv-remote", ["press", "down" if bottom else "up"])
    else:
        await sc.cmd("scroll", ["bottom" if bottom else "top"])
    await sc.cmd("wait", ["600"])


async def multi_scenario(sc: Scenario):
    """Several Carbons on one device (1.1): pairs, sides, waking, Stop and revoke per Carbon."""
    st = sc.st
    a = st.primary
    hello = a.hellos[-1]
    tv = hello.get("os") == "android_tv"
    sc.check("hello.features has setup_retry", "setup_retry" in (hello.get("features") or []), hello.get("features"))
    sc.check("hello sends no engine version (this app runs no engine)", "engine_version" not in hello and "agent_device_version" not in hello, sorted(hello))
    sc.check("hello.app_version is 1.1.0", hello.get("app_version") == "1.1.0", hello.get("app_version"))
    await wait_for(lambda: a.awakes, 5)
    aw = a.awakes[0] if a.awakes else None
    sc.check("awake right after hello, with run and seq", aw is not None and aw.get("run") and isinstance(aw.get("seq"), int), aw)
    sc.check("the device is awake for this test (screen on, unlocked)", aw is not None and aw.get("awake") is True, aw)

    # A session through Alice's pair drives the app's own screen.
    sc.pair, sc.session = a, "a01"
    await sc.send({"type": "session_started", "target": None, "session_id": "a01", "silicon_id": "si:chef", "since": now_iso(), "side": "side-alice"})
    r = await sc.cmd("open", ["com.teamofsilicons.extend"])
    await sc.cmd("wait", ["1500"])
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("the paired screen names Alice", "Alice (c:alice)" in t or "c:alice" in t, t)
    sc.check("the in-use card says through which Carbon", "through c:alice" in t, t)
    await reach(sc, tv, "Pair with another Carbon")
    r = await sc.cmd("find", ["Pair with another Carbon", "click"])
    sc.check("tap Pair with another Carbon", r["ok"], r)
    await sc.cmd("wait", ["1000"])
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("the shared-device note comes first", "Silicons any Carbon gives access to can use this whole device, including what others leave on it" in t, t)
    await sc.drain_events()
    r = await sc.cmd("find", ["Show a pairing code", "click"])
    sc.check("tap Show a pairing code", r["ok"], r)
    ev = await sc.expect_event("_http_pair_enrollment", 10)
    sc.check("the code comes from POST /api/v1/device/enrollments with a pair's credential", ev is not None and ev["_pair"] == a.device_id, ev)
    await sc.cmd("wait", ["2500"])
    e = st.waiting_enrollment()
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    shown = e and (e["code"][:3] + " " + e["code"][3:]) in t
    sc.check("the other Carbon's code is on screen", shown, (e and e["code"], t[:800]))
    if not e or e["kind"] != "pair":
        sc.check("a pair enrollment is waiting (the rest needs a second pair)", False, e)
        return
    await claim(st, e)
    b = list(st.pairs.values())[-1]
    await wait_for(lambda: b.hellos, 20)
    sc.check("a second connection says hello with the second credential", bool(b.hellos), b.view())
    sc.check("the first pair's socket stayed up", a.ws is not None and not a.ws.closed)
    await wait_for(lambda: b.awakes, 5)
    sc.check("the second connection says awake too, same run", bool(b.awakes) and b.awakes[0].get("run") == aw.get("run"), (b.awakes[:1], aw))
    await sc.cmd("wait", ["2500"])
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("the app lists both Carbons with their names for the device", "c:alice" in t and "c:bob" in t and "Family phone" in t, t)
    await sc.send({"type": "session_ended", "target": None, "session_id": "a01", "reason": "ended_by_silicon"})

    # A wake request through Alice's pair.
    wid = str(uuid.uuid4())
    wake = {"type": "wake_request", "target": None, "wake_id": wid, "silicon_id": "si:waker", "reason": "Please unlock to check the oven timer",
            "side": "side-alice", "alert": True, "created_at": now_iso(), "expires_at": now_iso(1800)}
    await sc.drain_events()
    await sc.send(wake, a)
    shown = await sc.expect_event("wake_request_shown", 5, lambda f: f.get("wake_id") == wid)
    if tv:
        sc.check("a TV answers shown:false with why", shown is not None and shown.get("shown") is False and "can't show notifications" in (shown.get("note") or ""), shown)
    else:
        sc.check("a phone shows it: wake_request_shown true", shown is not None and shown.get("shown") is True, shown)
        await asyncio.sleep(1.0)
        n = await dumpsys_notifications()
        sc.check("the wake notification names the Silicon and the reason", "si:waker" in n and "oven timer" in n, n[-3000:])
        sc.check("it is private on the lock screen, with a public version", "vis=PRIVATE" in n and "publicVersion" in n, "")
    sc.check("wake_request_shown came on the pair the request came through", shown is not None and shown["_pair"] == a.device_id, shown)
    await sc.send(dict(wake, alert=False), a)
    again = await sc.expect_event("wake_request_shown", 2, lambda f: f.get("wake_id") == wid)
    sc.check("wake_request_shown is sent once per wake_id", again is None, again)

    # Another side's session starts, and its Silicon reads notifications straight away.
    sc.pair, sc.session = b, "b01"
    await sc.send({"type": "session_started", "target": None, "session_id": "b01", "silicon_id": "si:bob-helper", "since": now_iso(), "side": "side-bob"})
    if not tv:
        r = await sc.cmd("notifications")
        body = json.dumps(r)
        sc.check("notifications at once: no other side's Silicon or reason", r["ok"] and "si:waker" not in body and "oven" not in body, body[:1500])
        n = await dumpsys_notifications()
        sc.check("dumpsys --noredact shows no other side's Silicon or reason", "si:waker" not in n and "oven timer" not in n, around(n, "si:waker", "oven timer"))
        sc.check("the notification says a Silicon asked, naming none", "A Silicon asked to use this" in n, n[-3000:])
    r = await sc.cmd("open", ["com.teamofsilicons.extend"])
    await sc.cmd("wait", ["1500"])
    await page_end(sc, tv, bottom=False)
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("the app's own screen hides the other side's request too", "si:waker" not in t and "oven" not in t, t)
    sc.check("the in-use card: si:bob-helper through c:bob", "si:bob-helper" in t and "through c:bob" in t, t)
    await asyncio.sleep(1.0)
    keeper, badge_on = await keep_on_windows()
    if tv:
        sc.check("TV: the badge window keeps the screen on during the session", badge_on, "")
    else:
        sc.check("phone: the keep-screen-on window is up during the session", keeper, "")

    # Stop while the session's own pair (Bob's) is down: the Stop goes out on Alice's socket.
    b.refuse = True
    await b.ws.close(1011, "test: Bob's connection drops")
    await asyncio.sleep(0.5)
    await sc.drain_events()
    # --first: a heads-up of the in-use notification can show its own STOP.
    stop_task = asyncio.create_task(sc.cmd("find", ["Stop", "click", "--first"], pair=a))
    stop = await sc.expect_event("stop", 8)
    sc.check("Stop with the holder's socket down goes out on another pair's socket", stop is not None and stop["_pair"] == a.device_id, stop)
    stop_task.cancel()
    b.refuse = False
    await sc.send({"type": "session_ended", "target": None, "session_id": "b01", "reason": "stopped_by_carbon"}, a)
    await wait_for(lambda: b.ws is not None, 15)
    sc.check("Bob's pair reconnects afterwards", b.ws is not None)
    await asyncio.sleep(1.5)
    keeper, badge_on = await keep_on_windows()
    sc.check("the screen is no longer kept on after the session", not keeper and not badge_on, (keeper, badge_on))

    # The screen goes off and comes back: awake frames in order, on every connection.
    before = (len(a.awakes), len(b.awakes))
    want = "standby" if tv else "screen_off"
    await adb("shell", "input", "keyevent", "KEYCODE_SLEEP")
    await wait_for(lambda: sleeping_after(a.awakes, before[0], want)
                   and sleeping_after(b.awakes, before[1], want), 8)
    off_a = a.awakes[-1] if len(a.awakes) > before[0] else None
    sc.check(f"screen off: awake false ({want}) on both pairs", off_a and off_a["awake"] is False and off_a.get("sleep_state") == want and sleeping_after(b.awakes, before[1], want), (off_a, b.awakes[-1:]))
    await asyncio.sleep(1.0)
    mid = (len(a.awakes), len(b.awakes))
    await adb("shell", "input", "keyevent", "KEYCODE_WAKEUP")
    await asyncio.sleep(1.5)
    await adb("shell", "wm", "dismiss-keyguard")
    await wait_for(lambda: a.awakes and a.awakes[-1].get("awake") is True, 10)
    on_a = a.awakes[-1]
    sc.check("screen on (and unlocked): awake true", on_a.get("awake") is True, a.awakes[mid[0]:])
    if not tv:
        sc.check("the unlock says a person was there: input_seen", any(f.get("input_seen") is True for f in a.awakes[mid[0]:]), a.awakes[mid[0]:])
    frames = sorted(a.awakes + b.awakes, key=lambda f: f["seq"])
    seqs = [f["seq"] for f in frames]
    sc.check("seq grows across both connections, one run", len(set(seqs)) == len(seqs) and len({f["run"] for f in frames}) == 1, seqs)
    if not tv:
        await asyncio.sleep(1.0)
        n = await dumpsys_notifications()
        sc.check("an awake phone drops its wake notification", "A Silicon asked to use this" not in n and "si:waker" not in n, n[-2000:])

    # setup_retry with nothing failed: the app just reports again, and the socket stays up.
    await sc.drain_events()
    await sc.send({"type": "setup_retry", "target": None, "step": None}, a)
    await sc.send({"type": "ping", "nonce": 77}, a)
    pong = await sc.expect_event("pong", 5, lambda f: f.get("nonce") == 77)
    sc.check("setup_retry is understood (the socket stays up)", pong is not None)

    # Revoke Bob's pair on the device: only Bob's pair ends.
    sc.pair, sc.session = a, "a02"
    await sc.send({"type": "session_started", "target": None, "session_id": "a02", "silicon_id": "si:chef", "since": now_iso(), "side": "side-alice"})
    await sc.cmd("open", ["com.teamofsilicons.extend"])
    await sc.cmd("wait", ["1000"])
    await reach(sc, tv, "Pair with another Carbon")
    r = await sc.cmd("find", ["Revoke pair", "click", "--last"])
    sc.check("tap Revoke pair on Bob's row", r["ok"], r)
    await sc.cmd("wait", ["800"])
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("the confirmation names the Carbon", "Revoke pair?" in t and "c:bob's account" in t, t)
    await sc.drain_events()
    confirm = asyncio.create_task(sc.cmd("find", ["Revoke pair", "click", "--last"], timeout_ms=8000))
    ev = await sc.expect_event("_http_revoke", 10)
    sc.check("confirm -> DELETE /api/v1/device with Bob's credential", ev is not None and ev["_pair"] == b.device_id, ev)
    await confirm
    await asyncio.sleep(1.5)
    sc.check("Alice's pair is untouched", a.ws is not None and not a.ws.closed and not a.revoked)
    snap = await sc.cmd("snapshot")
    t = snap.get("text") or ""
    sc.check("the app lists only Alice now", "c:bob" not in t and "c:alice" in t, t)
    await sc.send({"type": "session_ended", "target": None, "session_id": "a02", "reason": "ended_by_silicon"})

    # Extend unpairs the last pair: the app asks for a new pairing code.
    before = len([x for x in st.http_log if x == ("POST", "/api/v1/enrollments")])
    a.revoked = True
    await a.ws.send({"type": "unpaired", "reason": "device_removed"})
    ok = await wait_for(lambda: len([x for x in st.http_log if x == ("POST", "/api/v1/enrollments")]) > before, 15)
    sc.check("after the last pair ends, the app asks for a new pairing code", ok)


async def run_scenario(st: State):
    await asyncio.sleep(1.0)
    sc = Scenario(st)
    try:
        if st.args.scenario == "phone":
            await phone_scenario(sc)
        elif st.args.scenario == "tv":
            await tv_scenario(sc)
        elif st.args.scenario == "multi":
            await multi_scenario(sc)
        elif st.args.scenario == "smoke":
            await scenario_common_start(sc, st.primary.hellos[-1]["os"])
            r = await sc.cmd("snapshot", ["-i"])
            sc.check("snapshot -i", r["ok"], r)
    except Exception as e:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        st.failures += 1
        log("FAIL scenario crashed:", e)
    log(f"SCENARIO DONE: {st.passes} passed, {st.failures} failed")
    await asyncio.sleep(0.5)
    os._exit(1 if st.failures else 0)


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8490)
    ap.add_argument("--rotate-s", type=int, default=300)
    ap.add_argument("--auto-claim-after", type=float, default=None, help="pair the first enrollment after N seconds")
    ap.add_argument("--scenario", choices=["phone", "tv", "smoke", "multi"], default=None)
    ap.add_argument("--device-name", default="Test Pixel")
    ap.add_argument("--image", default=None, help="a PNG to send as an attachment in the TV scenario")
    ap.add_argument("--old-service", action="store_true", help="answer POST /api/v1/device/enrollments with 404, like a 1.0 service")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
    args = ap.parse_args()
    st = State(args)
    server = await asyncio.start_server(lambda r, w: handle(st, r, w), args.host, args.port)
    log(f"fake Extend service on http://{args.host}:{args.port} (emulator: http://10.0.2.2:{args.port})")

    if args.auto_claim_after is not None:
        async def auto():
            await asyncio.sleep(args.auto_claim_after)
            while True:
                e = st.waiting_enrollment()
                if e:
                    await claim(st, e)
                    return
                await asyncio.sleep(0.5)
        asyncio.create_task(auto())
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(130)
