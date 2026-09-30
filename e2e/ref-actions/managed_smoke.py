#!/usr/bin/env python3
"""Smoke managed Jev through the real CLI and a task-owned fake device.

The private --state JSON has api_url, carbon_id, silicon_id, team and, for remote
services, environment_id plus testing_application_secret. Remote production
device access is refused. Loopback services may use the local IAM stand-in.
No TypeSafe key is accepted or passed to the CLI: configure the service instead.

Build first:
  cargo build --locked -p silicon-extend-cli -p extend-service --example fake_device
Run with a chmod-600 state file, writing only non-secret evidence to --out:
  python3 e2e/ref-actions/managed_smoke.py --state /tmp/smoke/state.json --out /tmp/smoke/result.json

A loopback forwarding proxy records only selection request structure and checks
that the literal synthetic fill text never enters the managed-selection request.
The fake device, sessions and CLI login state are removed when the run ends.
The caller owns provisioning and retiring the isolated ecosystem environment.
"""

import argparse
import http.server
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


class SmokeError(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def private_json(path, value):
    with open(path, "w", opener=lambda p, flags: os.open(p, flags, 0o600)) as out:
        json.dump(value, out, indent=2)
        out.write("\n")


def run(args):
    state_path = Path(args.state).resolve()
    if state_path.stat().st_mode & 0o077:
        raise SmokeError("The state file must be private (chmod 600).")
    state = json.loads(state_path.read_text())
    api = state["api_url"].rstrip("/")
    parsed = urllib.parse.urlsplit(api)
    local = parsed.hostname in {"localhost", "127.0.0.1", "::1"}
    if parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path:
        raise SmokeError("api_url must be an origin without credentials, query or path.")
    if parsed.scheme != "https" and not (local and parsed.scheme == "http"):
        raise SmokeError("Remote services require HTTPS.")
    test_id = state.get("environment_id")
    test_secret = state.get("testing_application_secret")
    if bool(test_id) != bool(test_secret) or (not local and not test_id):
        raise SmokeError("Remote smoke requires an explicit isolated environment and test app secret.")
    if test_id:
        uuid.UUID(test_id)
    root = Path(__file__).resolve().parents[2]
    cli = Path(args.cli or root / "target/debug/extend").resolve()
    fake_binary = Path(args.fake_device or root / "target/debug/examples/fake_device").resolve()
    if not cli.is_file() or not fake_binary.is_file():
        raise SmokeError("Build extend and the fake_device example before running the smoke.")
    literal = "managed-smoke-literal-" + uuid.uuid4().hex
    observed = []
    opener = urllib.request.build_opener(NoRedirect())

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass

        def forward(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            if self.path.endswith("/ref-selection"):
                data = json.loads(body)["data"]
                observed.append({
                    "has_text": data.get("has_text"),
                    "fields": sorted(data),
                    "literal_absent": literal.encode() not in body,
                    "test_header_present": bool(self.headers.get("X-Testing-Application-Secret")),
                })
            headers = {k: v for k, v in self.headers.items()
                       if k.lower() not in {"host", "connection", "content-length", "accept-encoding"}}
            request = urllib.request.Request(api + self.path, data=body or None,
                                             headers=headers, method=self.command)
            try:
                upstream = opener.open(request, timeout=45)
            except urllib.error.HTTPError as error:
                upstream = error
            except (OSError, urllib.error.URLError):
                self.send_error(502, "Smoke upstream unavailable")
                return
            with upstream:
                response = upstream.read()
                self.send_response(upstream.status)
                for key, value in upstream.headers.items():
                    if key.lower() not in {"transfer-encoding", "connection", "content-length"}:
                        self.send_header(key, value)
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

        do_GET = do_POST = do_PATCH = do_DELETE = do_PUT = forward

    proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    proxy_api = "http://127.0.0.1:" + str(proxy.server_port)
    report = {"api_url": api, "environment_id": test_id, "fixture": "synthetic fake device",
              "byok_present": False, "runs": [], "cleanup": {}}
    process = None
    device = session = None
    with tempfile.TemporaryDirectory(prefix="extend-managed-smoke-") as work:
        work = Path(work)
        # Clear inherited provider/context configuration: this tests the actual no-BYOK default.
        base_env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("EXTEND_", "TYPESAFE_", "FAKE_")) and k != "SILICON_HOME"}
        base_env.update(EXTEND_API_URL=proxy_api, EXTEND_TELEMETRY="off")
        if test_secret:
            base_env["EXTEND_TEST_SECRET"] = test_secret

        def command(actor, *parts, check=True):
            env = {**base_env, "SILICON_HOME": str(work / actor)}
            (work / actor).mkdir(exist_ok=True)
            argv = [str(cli), "--json", "--team", state["team"]]
            if test_id:
                argv.extend(["--test", test_id])
            result = subprocess.run(argv + list(parts), env=env, capture_output=True, text=True, timeout=90)
            if result.returncode and check:
                # Provider/credential-bearing bodies must never become smoke output.
                try:
                    code = json.loads(result.stderr).get("error", {}).get("code", "unknown")
                except json.JSONDecodeError:
                    code = "non_json_error"
                raise SmokeError(f"CLI {parts[0]} failed ({code}); no response body was logged.")
            if not check:
                return result.returncode == 0
            return json.loads(result.stdout) if result.stdout.strip() else None

        try:
            command("carbon", "login", state["carbon_id"])
            command("silicon", "login", state["silicon_id"])
            snapshot = {"appName": "Managed Jev smoke", "nodes": [
                {"ref": "@e1", "role": "button", "name": "Save", "enabled": True},
                {"ref": "@e2", "role": "textbox", "name": "Search", "editable": True, "enabled": True},
            ]}
            fixture = work / "snapshot.json"
            private_json(fixture, snapshot)
            fake_env = {**base_env, "FAKE_APP_VERSION": "1.1.0", "FAKE_REF_SNAPSHOT": str(fixture)}
            if test_secret:
                fake_env["FAKE_TEST_SECRET"] = test_secret
            log_path = work / "fake.log"
            with open(log_path, "w", opener=lambda p, flags: os.open(p, flags, 0o600)) as log:
                process = subprocess.Popen([str(fake_binary), api, "linux"], env=fake_env,
                                           stdout=log, stderr=subprocess.STDOUT)
            pairing = None
            for _ in range(150):
                match = re.search(r"^PAIRING_CODE (\S+)", log_path.read_text(), re.MULTILINE)
                if match:
                    pairing = match.group(1)
                    break
                if process.poll() is not None:
                    raise SmokeError("Fake device exited before enrollment.")
                time.sleep(0.2)
            if not pairing:
                raise SmokeError("Fake device enrollment timed out.")
            paired = command("carbon", "device", "pair", pairing, "--name", "Managed Jev synthetic smoke",
                             "--access", state["silicon_id"])
            device = paired["device_id"]
            for _ in range(100):
                info = command("carbon", "device", "show", device)
                if info.get("online"):
                    break
                time.sleep(0.2)
            else:
                raise SmokeError("Fake device did not become online.")
            started = command("silicon", "session", "new", device, "--connect")
            session = started["session_id"]
            report.update(device_id=device, session_id=session)
            for index in range(args.repeats):
                result = command("silicon", "act", "Click Save", "--dry-run")
                decision = result["decision"]
                if result["status"] != "selected" or decision["operation"] != "click" or decision["target"] != "@e1":
                    raise SmokeError("Managed dry-run did not select the expected Save button.")
                report["runs"].append({"case": "click_dry_run", "index": index, "status": result["status"],
                                       "decision": decision, "timings": result["timings"]})
            result = command("silicon", "act", "Fill the Search field with the supplied text", "--text", literal)
            decision = result["decision"]
            if result["status"] != "executed" or decision["operation"] != "fill" or decision["target"] != "@e2":
                raise SmokeError("Managed fill did not execute the expected Search field action.")
            report["runs"].append({"case": "fill_execute", "status": result["status"],
                                   "decision": decision, "timings": result["timings"]})
            commands = [line for line in log_path.read_text().splitlines() if line.startswith("COMMAND ")]
            effects = [line for line in commands if not line.startswith("COMMAND snapshot ")]
            if effects != ["COMMAND fill @e2 " + literal]:
                raise SmokeError("Fake device received an unexpected action count or literal fill payload.")
            if len(observed) != args.repeats + 1 or not all(x["literal_absent"] for x in observed):
                raise SmokeError("Selection request count or literal text privacy check failed.")
            if observed[-1]["has_text"] is not True or any("text" in x["fields"] for x in observed):
                raise SmokeError("Selection must carry has_text, never literal text.")
            if any(x["test_header_present"] != bool(test_id) for x in observed):
                raise SmokeError("The isolated testing context was not preserved.")
            report.update(selection_requests=observed, action_count=len(effects),
                          exact_fill_text_delivered=True, literal_absent_from_selection=True,
                          normal_snapshot_after_act=bool(command("silicon", "snapshot", "-i")))
        finally:
            if session:
                report["cleanup"]["session_ended"] = command("silicon", "session", "end", check=False)
            if device:
                report["cleanup"]["device_unpaired"] = command("carbon", "device", "rm", device, "--yes", check=False)
            for actor in ("carbon", "silicon"):
                report["cleanup"][actor + "_logged_out"] = command(actor, "logout", check=False)
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            proxy.shutdown()
            proxy.server_close()
            private_json(args.out, report)
    if not all(report["cleanup"].values()):
        raise SmokeError("Smoke actions passed, but cleanup was incomplete; inspect the private report.")
    report["model_ms_median"] = statistics.median(x["decision"]["model_ms"] for x in report["runs"])
    private_json(args.out, report)
    print(json.dumps({"ok": True, "environment_id": test_id, "cases": len(report["runs"]),
                      "fake_actions": report["action_count"], "model_ms_median": report["model_ms_median"],
                      "literal_absent_from_selection": True, "cleanup_complete": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--state", required=True, help="Private JSON config; no provider keys")
    parser.add_argument("--out", required=True, help="Non-secret evidence JSON")
    parser.add_argument("--cli", help="CLI executable; defaults to target/debug/extend")
    parser.add_argument("--fake-device", help="fake_device executable")
    parser.add_argument("--repeats", type=int, choices=range(1, 11), default=3)
    try:
        run(parser.parse_args())
    except (SmokeError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        raise SystemExit(str(error))
