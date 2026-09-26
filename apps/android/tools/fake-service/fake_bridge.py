#!/usr/bin/env python3
"""A fake Bridge service for testing the Android app against docs/device-protocol.md.

Standard library only (HTTP/1.1 and a minimal RFC 6455 WebSocket on asyncio). It implements the
device-facing half of the protocol:

    POST   /api/v1/enrollments                 enrollment + first pairing code
    GET    /api/v1/enrollments/{id}            poll (Bridge-Enrollment auth)
    GET    /api/v1/enrollments/{id}/connect    enrollment WebSocket: code rotations, then paired
    GET    /api/v1/device/connect              device WebSocket (Bridge-Device auth)
    GET    /api/v1/device                      device_self
    DELETE /api/v1/device                      revoke pair
    POST   /api/v1/device/stop                 stop
    PUT    /api/v1/device/artifacts/{upload}   uploads, checked against X-Content-SHA256

plus test controls:

    GET  /_test/state   {"pairing_code", "paired", "device_id", "hellos", ...}
    POST /_test/claim   pairs the waiting enrollment (what a Carbon entering the code does)

With --scenario, once the device says hello it runs a list of commands and checks each result,
printing PASS/FAIL lines, and exits non-zero if anything failed.
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


def now_iso(offset_s=0):
    t = datetime.now(timezone.utc) + timedelta(seconds=offset_s)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


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

class State:
    def __init__(self, args):
        self.args = args
        self.enrollments = {}  # id -> dict
        self.credential = None
        self.device_id = None
        self.device_ws = None
        self.hellos = []
        self.events = asyncio.Queue()  # frames from the device other than results
        self.results = {}  # id -> future
        self.uploads = {}  # upload_id -> {name, content_type, bytes}
        self.http_log = []
        self.enroll_waiters = []
        self.revoked = False
        self.stops_http = 0
        self.environment = None
        self.session = None
        self.takeover = None
        self.failures = 0
        self.passes = 0
        self.scenario_started = False
        self.device_connections = 0

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
    reasons = {200: "OK", 201: "Created", 204: "No Content", 409: "Conflict", 400: "Bad Request", 401: "Unauthorized", 404: "Not Found", 422: "Unprocessable Entity"}
    body = raw if raw is not None else (json.dumps(obj).encode() if obj is not None else b"")
    head = f"HTTP/1.1 {status} {reasons.get(status, 'OK')}\r\nContent-Length: {len(body)}\r\nConnection: close\r\n"
    if body:
        head += f"Content-Type: {ctype}\r\n"
    writer.write(head.encode() + b"\r\n" + body)
    await writer.drain()
    writer.close()


def error(code, message):
    return {"type": "error", "data": {"code": code, "message": message}}


def device_self(st):
    return {
        "type": "device_self",
        "data": {
            "device_id": st.device_id,
            "name": st.args.device_name,
            "owner": {"type": "carbon", "id": "c:alice", "display_name": "Alice"},
            "team": "acme",
            "os": st.hellos[-1]["os"] if st.hellos else "android",
            "in_use": st.session,
            "takeover": st.takeover,
            "setup": st.hellos[-1]["setup"] if st.hellos else {"state": "in_progress", "steps": []},
            "environment": st.environment,
        },
    }


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
        ok = False
        if m and m.group(1) in st.enrollments and auth == "Bridge-Enrollment " + st.enrollments[m.group(1)]["secret"]:
            ok = True
        if path == "/api/v1/device/connect" and st.credential and auth == "Bridge-Device " + st.credential and not st.revoked:
            ok = True
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
            await device_socket(st, ws)
        return

    log("HTTP", method, path)
    if method == "POST" and path == "/api/v1/enrollments":
        payload = json.loads(body or b"{}")
        data = payload.get("data", {})
        eid = str(uuid.uuid4())
        e = {
            "id": eid, "secret": "bes_" + secrets.token_urlsafe(32)[:43], "code": new_code(), "expires": now_iso(st.args.rotate_s),
            "paired": False, "request": data, "sockets": [],
        }
        st.enrollments[eid] = e
        log("enrollment", eid, "code", e["code"], "from", data)
        await respond(writer, 201, {"type": "enrollment", "data": {
            "enrollment_id": eid, "enrollment_secret": e["secret"], "pairing_code": e["code"],
            "code_expires_at": e["expires"], "rotates_every_s": st.args.rotate_s}})
        return
    m = re.fullmatch(r"/api/v1/enrollments/([^/]+)", path)
    if m:
        e = st.enrollments.get(m.group(1))
        if not e or auth != "Bridge-Enrollment " + e["secret"]:
            await respond(writer, 404 if not e else 401, error("not_found", "no such enrollment"))
            return
        if method == "DELETE":
            del st.enrollments[e["id"]]
            await respond(writer, 204)
            return
        if e["paired"]:
            del st.enrollments[e["id"]]
            await respond(writer, 200, {"type": "enrollment", "data": {"state": "paired", "device_id": st.device_id, "device_credential": st.credential, "environment": None}})
        else:
            await respond(writer, 200, {"type": "enrollment", "data": {"state": "waiting", "pairing_code": e["code"], "code_expires_at": e["expires"]}})
        return
    if path.startswith("/api/v1/device"):
        if not st.credential or auth != "Bridge-Device " + st.credential or st.revoked:
            await respond(writer, 401, error("unauthorized", "device credential invalid"))
            return
        if method == "GET" and path == "/api/v1/device":
            await respond(writer, 200, device_self(st))
            return
        if method == "DELETE" and path == "/api/v1/device":
            st.revoked = True
            await st.events.put({"type": "_http_revoke"})
            await respond(writer, 204)
            if st.device_ws:
                await st.device_ws.close(4401, "pair revoked")
            return
        if method == "POST" and path == "/api/v1/device/stop":
            st.stops_http += 1
            await st.events.put({"type": "_http_stop"})
            await respond(writer, 204)
            return
        m = re.fullmatch(r"/api/v1/device/artifacts/([^/]+)", path)
        if method == "PUT" and m:
            digest = hashlib.sha256(body).hexdigest()
            if headers.get("x-content-sha256") != digest:
                await respond(writer, 422, error("digest_mismatch", "X-Content-SHA256 does not match"))
                return
            st.uploads[m.group(1)] = {"name": headers.get("x-file-name"), "content_type": headers.get("content-type"), "bytes": body}
            os.makedirs(st.args.out, exist_ok=True)
            with open(os.path.join(st.args.out, headers.get("x-file-name") or m.group(1)), "wb") as f:
                f.write(body)
            log("upload", m.group(1), headers.get("x-file-name"), len(body), "bytes")
            await respond(writer, 201)
            return
    if path == "/_test/state":
        e = st.waiting_enrollment()
        await respond(writer, 200, {
            "pairing_code": e["code"] if e else None, "enrollment_request": e["request"] if e else None,
            "paired": st.credential is not None and not st.revoked, "device_id": st.device_id,
            "hellos": len(st.hellos), "last_hello": st.hellos[-1] if st.hellos else None,
            "passes": st.passes, "failures": st.failures, "revoked": st.revoked,
            "enrollments_created": len([p for p in st.http_log if p == ("POST", "/api/v1/enrollments")]),
        })
        return
    if path == "/_test/command" and method == "POST":
        # {"command": "snapshot", "args": ["-i"], "uploads": 0, "attachments": [...]} -> the result frame
        if not st.device_ws:
            await respond(writer, 409, error("offline", "device isn't connected"))
            return
        req = json.loads(body or b"{}")
        sc = Scenario(st)
        sc.session = req.get("session_id", "a3f")
        res = await sc.cmd(req["command"], req.get("args", []), uploads=req.get("uploads", 0),
                           attachments=req.get("attachments", []), timeout_ms=req.get("timeout_ms", 30000))
        await respond(writer, 200, res)
        return
    if path == "/_test/frame" and method == "POST":
        # Send any service frame to the device, e.g. {"type":"session_started",...}
        frame = json.loads(body or b"{}")
        track(st, frame)
        if st.device_ws:
            await st.device_ws.send(frame)
        await respond(writer, 200, {"sent": frame.get("type")})
        return
    if path == "/_test/close" and method == "POST":
        # Close the device socket with a code, e.g. {"code": 4409}
        code = json.loads(body or b"{}").get("code", 1000)
        if st.device_ws:
            await st.device_ws.close(code, "test")
        await respond(writer, 200, {"closed": code})
        return
    if path == "/_test/claim" and method == "POST":
        e = st.waiting_enrollment()
        if not e:
            await respond(writer, 404, error("no_enrollment", "nothing is waiting"))
            return
        await claim(st, e)
        await respond(writer, 200, {"device_id": st.device_id})
        return
    await respond(writer, 404, error("unknown_command", f"No such endpoint: {method} {path}"))


async def claim(st, e):
    st.device_id = secrets.token_hex(4)
    st.credential = "bdc_" + secrets.token_urlsafe(32)[:43]
    st.revoked = False
    e["paired"] = True
    frame = {"type": "paired", "device_id": st.device_id, "device_credential": st.credential, "environment": None}
    log("claimed", e["code"], "-> device", st.device_id)
    if st.args.scenario:
        async def watchdog(hellos=len(st.hellos)):
            await asyncio.sleep(60)
            if len(st.hellos) == hellos:
                log("FAIL no hello within 60 s of pairing")
                os._exit(2)
        asyncio.create_task(watchdog())
    for ws in list(e["sockets"]):
        await ws.send(frame)
        await ws.close(1000)


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


async def device_socket(st, ws):
    if st.device_ws and not st.device_ws.closed:
        old = st.device_ws
        await old.send({"type": "superseded"})
        await old.close(4409, "superseded")
    st.device_ws = ws
    st.device_connections += 1
    log("device socket open (#%d)" % st.device_connections)
    try:
        while True:
            msg = await ws.recv()
            if msg is None:
                log("device socket closed by device, code", ws.close_code)
                break
            frame = json.loads(msg)
            t = frame.get("type")
            if t == "result":
                fut = st.results.get(frame.get("id"))
                if fut and not fut.done():
                    fut.set_result(frame)
                else:
                    log("unexpected result", frame.get("id"))
                continue
            if t == "hello":
                st.hellos.append(frame)
                log("hello", json.dumps({k: frame[k] for k in ("os", "os_version", "model", "app_version", "capabilities")}))
                if not st.scenario_started and st.args.scenario:
                    st.scenario_started = True
                    asyncio.create_task(run_scenario(st))
            else:
                log("device ->", msg[:300])
                if t == "takeover_done":
                    st.takeover = None
            await st.events.put(frame)
    finally:
        if st.device_ws is ws:
            st.device_ws = None


def track(st, frame):
    """Keep GET /api/v1/device consistent with the frames sent (like the real service)."""
    t = frame.get("type")
    if t == "session_started":
        st.session = {"silicon_id": frame["silicon_id"], "session_id": frame["session_id"], "since": frame["since"], "paused": False}
        st.takeover = None
    elif t == "session_ended":
        st.session = None
        st.takeover = None
    elif t == "takeover":
        st.takeover = {"takeover_id": str(uuid.uuid4()), "session_id": frame["session_id"], "reason": frame["reason"], "started_at": now_iso(), "expires_at": frame["expires_at"]}
    elif t == "takeover_ended":
        st.takeover = None


# ───────────────────────────── Scenarios ─────────────────────────────

class Scenario:
    def __init__(self, st: State):
        self.st = st
        self.session = "a3f"

    def check(self, name, cond, detail=""):
        if cond:
            self.st.passes += 1
            log("PASS", name)
        else:
            self.st.failures += 1
            log("FAIL", name, "--", str(detail)[:1500])
        return cond

    async def send(self, frame):
        ws = self.st.device_ws
        if ws is None:
            raise RuntimeError("device isn't connected")
        track(self.st, frame)
        await ws.send(frame)

    async def cmd(self, command, args=(), uploads=0, attachments=(), timeout_ms=30000):
        cid = str(uuid.uuid4())
        fut = asyncio.get_running_loop().create_future()
        self.st.results[cid] = fut
        frame = {
            "type": "command", "id": cid, "session_id": self.session, "target": None, "command": command,
            "args": list(args), "attachments": list(attachments), "timeout_ms": timeout_ms,
            "upload_ids": [str(uuid.uuid4()) for _ in range(uploads)],
        }
        await self.send(frame)
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
    hello = st.hellos[-1]
    sc.check("hello.os is %s" % want_os, hello.get("os") == want_os, hello.get("os"))
    sc.check("hello has app_version/os_version/model", all(hello.get(k) for k in ("app_version", "os_version", "model")), hello)
    sc.check("hello.setup has steps", len(hello.get("setup", {}).get("steps", [])) >= 3, hello.get("setup"))
    missing = {m["capability"] for m in hello.get("missing", [])}
    sc.check("hello reports adb/apps.install/logs as missing with reasons", {"adb", "apps.install", "logs"} <= missing, hello.get("missing"))
    await sc.send({"type": "ping", "nonce": 42})
    pong = await sc.expect_event("pong", 5, lambda f: f.get("nonce") == 42)
    sc.check("ping 42 -> pong 42", pong is not None)
    await sc.send({"type": "session_started", "target": None, "session_id": sc.session, "silicon_id": "si:chef", "since": now_iso()})
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

    r = await sc.cmd("clipboard", ["write", "hello-bridge"])
    sc.check("clipboard write", r["ok"], r)
    r = await sc.cmd("clipboard", ["read"])
    sc.check("clipboard read returns what was written", r["ok"] and r["output"].get("text") == "hello-bridge", r)
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
    """Drive the Bridge app itself: environment banner, takeover Done, Stop, refresh, reconnect, revoke."""
    st = sc.st
    st.environment = {"environment_id": str(uuid.uuid4()), "name": "checkout-e2e", "state": "ready", "paired_devices": 1, "device_limit": 5}
    await sc.send({"type": "environment", "environment": st.environment})
    await sc.send({"type": "refresh"})
    await asyncio.sleep(1.0)
    sc.check("refresh -> GET /api/v1/device", ("GET", "/api/v1/device") in st.http_log[-10:], st.http_log[-10:])
    await sc.send({"type": "takeover", "target": None, "session_id": sc.session, "reason": "Please approve the payment", "expires_at": now_iso(1800)})
    r = await sc.cmd("open", ["com.teamofsilicons.bridge"])
    sc.check("open the Bridge app", r["ok"], r)
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
    adb = os.environ.get("ADB", os.path.expanduser("~/Library/Android/sdk/platform-tools/adb"))
    dump = await asyncio.create_subprocess_exec(adb, "shell", "uiautomator", "dump", "/sdcard/bridge-stopped.xml", stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    await dump.communicate()
    read = await asyncio.create_subprocess_exec(adb, "shell", "cat", "/sdcard/bridge-stopped.xml", stdout=asyncio.subprocess.PIPE)
    xml, _ = await read.communicate()
    sc.check("indicator cleared after session_ended", b"No Silicon is using this" in xml, xml.decode()[:1000])

    # Reconnect after the service drops the socket.
    hellos = len(st.hellos)
    await st.device_ws.close(1011, "restarting")
    t0 = time.time()
    while time.time() - t0 < 8 and len(st.hellos) == hellos:
        await asyncio.sleep(0.2)
    sc.check("reconnects and says hello again after a drop (%.1f s)" % (time.time() - t0), len(st.hellos) > hellos)

    # Revoke pair from the app, with its confirmation dialog.
    sc.session = "b71"
    await sc.send({"type": "session_started", "target": None, "session_id": sc.session, "silicon_id": "si:chef", "since": now_iso()})
    await sc.cmd("open", ["com.teamofsilicons.bridge"])
    await sc.cmd("wait", ["1000"])
    await sc.cmd("scroll", ["bottom"])
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
        sc.check("the display shows an image", "Silicon Bridge display: image" in (snap.get("text") or ""), snap.get("text"))
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


async def run_scenario(st: State):
    await asyncio.sleep(1.0)
    sc = Scenario(st)
    try:
        if st.args.scenario == "phone":
            await phone_scenario(sc)
        elif st.args.scenario == "tv":
            await tv_scenario(sc)
        elif st.args.scenario == "smoke":
            await scenario_common_start(sc, st.hellos[-1]["os"])
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
    ap.add_argument("--scenario", choices=["phone", "tv", "smoke"], default=None)
    ap.add_argument("--device-name", default="Test Pixel")
    ap.add_argument("--image", default=None, help="a PNG to send as an attachment in the TV scenario")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
    args = ap.parse_args()
    st = State(args)
    server = await asyncio.start_server(lambda r, w: handle(st, r, w), args.host, args.port)
    log(f"fake Bridge service on http://{args.host}:{args.port} (emulator: http://10.0.2.2:{args.port})")

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
