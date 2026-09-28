#!/usr/bin/env python3
"""Rehearse the released macOS 1.0 agent's private state upgrade against current Extend.

Run on macOS with the development PostgreSQL container already running on 5440:
  python3 e2e/released-agent-compat.py --archive /path/to/Silicon-Extend-macos-arm64.zip --out /new/output

Only owned processes, database and SILICON_HOME are used. Both engine locators point at a
nonexistent owned path; no UI, TCC prompt, Keychain, autostart write or screen command runs.
Terminal uses /bin/sh -c (no user login scripts) and an owned working directory. The second
Carbon pair is claimed through the real API and added to the private credential file while
the agent is stopped, because adding pairs is otherwise a native UI action. The two native
links then rotate their credentials after Stop and reconnect from the persisted new values.
Pass --old-agent and --new-agent to reuse verified signed release executables without extracting
or modifying either app bundle.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import stat
import subprocess
import time
import urllib.error
import urllib.request
import uuid


RELEASE_SHA256 = "e2e500a4e49dc30cbe7936e4604831ce6b51f82cf2b4a32178bd0b5bfd3aa171"
OLD_AGENT_SHA256 = "0ac5325eecda5dca755a46cfeda7475cb1a5df363394edb29dd9c61bc68ba7fb"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    released = parser.add_mutually_exclusive_group(required=True)
    released.add_argument("--archive", type=Path)
    released.add_argument("--old-agent", type=Path, help="Existing executable from the verified published 1.0 Mac release")
    parser.add_argument("--new-agent", type=Path, help="Existing signed 1.1 candidate; both Developer ID signatures are verified")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("This rehearsal is only for macOS arm64")
    archive = args.archive.resolve(strict=True) if args.archive else None
    if archive and digest(archive) != RELEASE_SHA256:
        parser.error("Archive does not match the verified GitHub v1.0.0 release digest")
    if args.old_agent and digest(args.old_agent.resolve(strict=True)) != OLD_AGENT_SHA256:
        parser.error("Old executable does not match the verified published 1.0.0 release")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False, mode=0o700)
    home = out / "private-home"
    home.mkdir(mode=0o700)
    work = out / "terminal-work"
    work.mkdir(mode=0o700)
    state = home / ".extend-agent"
    credfile = state / "credential.json"
    statusfile = state / "status.json"
    absent_engine = out / "disabled-engine-does-not-exist"
    new = args.new_agent.resolve(strict=True) if args.new_agent else out / "current-agent"
    service = out / "current-service"
    if not args.new_agent:
        shutil.copy2(root / "target/debug/extend-agent", new)
    shutil.copy2(root / "target/debug/extend-service", service)
    # Extracting an app is not launching it. Only its executable is run with explicit headless flags.
    if archive:
        subprocess.run(["ditto", "-x", "-k", str(archive), str(out / "release")], check=True)
        old = out / "release/Silicon Extend.app/Contents/MacOS/extend-agent"
    else:
        old = args.old_agent.resolve(strict=True)
    assert old.is_file() and digest(old) == OLD_AGENT_SHA256
    db = "extend_agent_upgrade_" + uuid.uuid4().hex
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    api = f"http://127.0.0.1:{port}"
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("EXTEND_") and k not in ("SILICON_HOME", "ENV", "BASH_ENV")}
    env.update(SILICON_HOME=str(home), EXTEND_API_URL=api,
               EXTEND_ENGINE=str(absent_engine), EXTEND_AGENT_DEVICE=str(absent_engine),
               EXTEND_TERMINAL_SHELL="/bin/sh -c")
    plist = Path.home() / "Library/LaunchAgents/com.teamofsilicons.extend-agent.plist"

    def autostart_snapshot():
        if not plist.exists():
            return {"exists": False}
        info = plist.stat()
        return {"exists": True, "sha256": digest(plist), "mtime_ns": info.st_mtime_ns,
                "mode": stat.S_IMODE(info.st_mode)}

    report = {"passed": [], "api": api, "database": db,
              "source_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
              "release_url": "https://github.com/teamofsilicons/silicon-extend/releases/tag/v1.0.0",
              "archive_sha256": RELEASE_SHA256, "archive_reverified_this_run": archive is not None,
              "old_agent": str(old), "old_agent_sha256": digest(old), "current_agent": str(new),
              "current_agent_sha256": digest(new), "current_service_sha256": digest(service),
              "autostart_before": autostart_snapshot(),
              "limitations": ["Headless native agent and local service with synthetic IAM only",
                              "No UI, Keychain, TCC interaction, real screen engine, fresh notarization or installed-app replacement is tested",
                              "Terminal uses an explicit non-login shell; second pair is API-claimed then loaded from the owned file store"]}
    processes, logs = [], []
    created = False
    started = time.monotonic()

    def sql(statement, database="postgres"):
        return subprocess.check_output(["docker", "exec", "silicon-extend-postgres", "psql", "-U", "extend",
                                        "-d", database, "-v", "ON_ERROR_STOP=1", "-Atc", statement], text=True, timeout=10).strip()

    def passed(name):
        report["passed"].append(name)
        print("PASS " + name, flush=True)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    def spawn(command, name, process_env=env):
        logfile = (out / (name + ".log")).open("w")
        logs.append(logfile)
        child = subprocess.Popen(command, cwd=work, env=process_env, stdin=subprocess.DEVNULL,
                                 stdout=logfile, stderr=subprocess.STDOUT)
        processes.append(child)
        return child

    def stop(child):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()

    def await_value(label, fn, seconds=35):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = fn()
            if value:
                return value
            time.sleep(0.15)
        raise AssertionError("Timed out: " + label)

    def call(method, path, data=None, token=None, scheme="Bearer", expected=None):
        headers = {"Content-Type": "application/json", "Idempotency-Key": str(uuid.uuid4())}
        if token:
            headers.update(Authorization=f"{scheme} {token}", **{"X-Org-ID": "acme"})
        body = None if data is None else json.dumps({"type": "request", "data": data}).encode()
        request = urllib.request.Request(api + path, data=body, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=45) as response:
                code, raw = response.status, response.read()
        except urllib.error.HTTPError as error:
            code, raw = error.code, error.read()
        value = json.loads(raw) if raw else {}
        if expected is not None:
            assert code in (expected if isinstance(expected, tuple) else (expected,)), (method, path, code, value)
        else:
            assert code < 400, (method, path, code, value)
        return value.get("data", value)

    def read_status():
        try:
            return json.loads(statusfile.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def credentials():
        value = json.loads(credfile.read_text())
        return value if isinstance(value, list) else [value]

    def agent_argv(binary, *words):
        return [str(binary), "--service-url", api, "--home", str(home), "--credential-store", "file", *words]

    def start_agent(binary, name):
        return spawn(agent_argv(binary, "run", "--headless"), name)

    def online(child, version, device, owner, count=1):
        def check():
            assert child.poll() is None, "Agent exited; inspect its owned log"
            value = read_status()
            if value.get("pid") != child.pid or value.get("app_version") != version or value.get("phase") != "online":
                return False
            if version != "1.0.0" and (len(value.get("pairs", [])) != count
                                      or any(p["phase"] != "online" for p in value["pairs"])):
                return False
            return value if call("GET", "/api/v1/devices/" + device, token=owner)["online"] else False
        value = await_value("Agent online " + version, check)
        assert "terminal" in value["capabilities"]
        assert not any(c.startswith(("screen.", "input.")) for c in value["capabilities"]), value["capabilities"]
        return value

    def terminal(session, token, command, expected=None):
        return call("POST", f"/api/v1/sessions/{session}/commands",
                    {"command": "terminal", "args": ["run", "--cwd", str(work), command], "timeout_ms": 40000},
                    token=token, expected=expected)

    def terminal_ok(session, token, label):
        result = terminal(session, token, "printf '%s\\n' '" + label + "'; pwd")
        assert result["ok"] and result["output"]["exit_code"] == 0, result
        assert result["output"]["stdout"] == label + "\n" + str(work) + "\n", result
        (out / (label + ".json")).write_text(json.dumps(result, indent=2) + "\n")

    def signing(binary, label, version):
        subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True, capture_output=True, text=True, timeout=15)
        result = subprocess.run(["codesign", "-d", "-r-", "--verbose=4", str(binary)],
                                check=True, capture_output=True, text=True, timeout=15)
        details = result.stdout + result.stderr
        (out / (label + "-codesign.txt")).write_text(details)
        assert "Identifier=com.teamofsilicons.extend\n" in details
        assert "TeamIdentifier=LTBSK59BJ2\n" in details and "flags=0x10000(runtime)" in details
        assert "Authority=Developer ID Application: Shubham Gupta (LTBSK59BJ2)\n" in details
        assert version in subprocess.check_output([str(binary), "--version"], env=env, text=True, timeout=5)
        return next(line.removeprefix("designated => ") for line in details.splitlines() if line.startswith("designated => "))

    def pair_rows(ids):
        assert all(len(i) == 8 and all(c in "0123456789abcdef" for c in i) for i in ids)
        names = ",".join("'" + i + "'" for i in ids)
        rows = json.loads(sql("SELECT json_agg(json_build_object('device_id',device_id,'instance_id',instance_id,"
                              "'owner_id',owner_id,'first_pair',first_pair,'credential_digest',credential_digest,"
                              "'next_credential_digest',next_credential_digest) ORDER BY device_id) FROM extend.devices "
                              "WHERE device_id IN (" + names + ")", db))
        assert len(rows) == len(ids)
        return {row["device_id"]: row for row in rows}

    def rotated(previous):
        current = {row["device_id"]: row for row in credentials()}
        if current.keys() != previous.keys():
            return False
        rows = pair_rows(current)
        for identity, row in current.items():
            if row["device_credential"] == previous[identity]["device_credential"]:
                return False
            if rows[identity]["credential_digest"] != hashlib.sha256(row["device_credential"].encode()).hexdigest():
                return False
            if rows[identity]["next_credential_digest"] is not None:
                return False
        return current

    def native_stop(binary):
        result = subprocess.run(agent_argv(binary, "stop"), env=env, cwd=work, capture_output=True, text=True, timeout=15)
        assert result.returncode == 0 and "Stopped" in result.stdout, (result.returncode, result.stdout, result.stderr)

    try:
        if args.new_agent:
            before = signing(old, "old-before", "1.0.0")
            assert before == signing(new, "new-before", "1.1.0")
            report["matching_designated_requirement"] = before
            passed("published 1.0 and candidate 1.1 verify with the same Developer ID designated requirement")
        sql(f"CREATE DATABASE {db}")
        created = True
        service_env = dict(env, EXTEND_ENVIRONMENT="development", EXTEND_IAM_MODE="local", EXTEND_FILES_MODE="local",
                           EXTEND_TING_MODE="local", EXTEND_LOCAL_MEMBERS="c:alice@acme,c:bob@acme,si:chef@acme,si:sous@acme",
                           EXTEND_DATABASE_URL=f"postgres://extend:extend@127.0.0.1:5440/{db}", EXTEND_BIND=f"127.0.0.1:{port}",
                           EXTEND_PUBLIC_URL=api, EXTEND_DATA_DIR=str(out / "data"), EXTEND_HONEYCOMB_SERVICE_TOKEN="hck_local_dev_token")
        server = spawn([str(service)], "service", service_env)

        def ready():
            assert server.poll() is None, "Owned service exited"
            try:
                with urllib.request.urlopen(api + "/ready", timeout=1) as response:
                    return response.status == 204
            except OSError:
                return False
        await_value("owned service readiness", ready)
        tokens = {member: call("POST", "/api/v1/auth/login", {"slt": member})["access_token"]
                  for member in ("c:alice", "c:bob", "si:chef", "si:sous")}
        alice, bob, chef, sous = (tokens[m] for m in ("c:alice", "c:bob", "si:chef", "si:sous"))
        child = start_agent(old, "old-first")
        code = await_value("released agent pairing code", lambda: (read_status().get("pairing") or {}).get("code"))
        device = call("POST", "/api/v1/pairings", {"pairing_code": code, "name": "Owned native upgrade fixture",
                                                      "silicon_ids": ["si:chef"]}, alice)["device_id"]
        first = online(child, "1.0.0", device, alice)
        (out / "old-status.json").write_text(json.dumps(first, indent=2) + "\n")
        original = credentials()[0]
        assert isinstance(json.loads(credfile.read_text()), dict), "1.0 must start with its actual single-object file layout"
        report["device_id"] = device
        report["credential_before_sha256"] = hashlib.sha256(original["device_credential"].encode()).hexdigest()
        passed("actual released 1.0 Mac agent pairs and connects to current 1.1 service with screen engine disabled")
        session = call("POST", "/api/v1/sessions", {"device_id": device}, chef)["session_id"]
        terminal_ok(session, chef, "old-terminal")
        passed("released 1.0 terminal executes only the harmless command in the owned working directory")
        stop(child)
        child = start_agent(old, "old-reconnect")
        online(child, "1.0.0", device, alice)
        assert credentials()[0] == original
        terminal_ok(session, chef, "old-reconnected-terminal")
        passed("released 1.0 reconnect preserves saved credential, device identity and active session on service 1.1")
        stop(child)
        # A synthetic marker in the legacy engine-state directory proves the rename without running the engine.
        legacy_state = state / "agent-device"
        legacy_state.mkdir(exist_ok=True)
        (legacy_state / "owned-upgrade-marker.txt").write_text("owned fixture only\n")
        child = start_agent(new, "new-upgrade")
        current = online(child, "1.1.0", device, alice)
        migrated = credentials()
        assert len(migrated) == 1 and migrated[0]["device_id"] == device
        assert migrated[0]["device_credential"] == original["device_credential"]
        assert migrated[0]["first_pair"] is True
        assert isinstance(json.loads(credfile.read_text()), list)
        assert stat.S_IMODE(credfile.stat().st_mode) == 0o600
        assert stat.S_IMODE(state.stat().st_mode) == 0o700
        assert (state / "engine/owned-upgrade-marker.txt").read_text() == "owned fixture only\n"
        assert not legacy_state.exists()
        (out / "upgraded-status.json").write_text(json.dumps(current, indent=2) + "\n")
        report["credential_after_sha256"] = hashlib.sha256(migrated[0]["device_credential"].encode()).hexdigest()
        terminal_ok(session, chef, "upgraded-terminal")
        passed("1.0 to 1.1 same-home upgrade migrates credential layout and engine-state name, preserving identity, secret and live session")
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(terminal, session, chef, "printf '%s' started > stop-started; /bin/sleep 30; printf '%s' escaped > stop-escaped", (200, 409))
            await_value("owned terminal began", lambda: (work / "stop-started").exists())
            stopped_at = time.monotonic()
            native_stop(new)
            outcome = pending.result(timeout=12)
            report["stop_elapsed_s"] = round(time.monotonic() - stopped_at, 3)
            # The native result and service session-ended observer race intentionally. Both
            # must report this specific cancellation; neither an unrelated error nor success passes.
            assert (outcome.get("ok") is False and outcome.get("error", {}).get("code") in ("session_ended", "cancelled")) or (
                outcome.get("code") == "session_ended"
                and outcome.get("details", {}).get("end_reason") == "stopped_by_carbon"
                and outcome.get("details", {}).get("may_have_run") is True), outcome
            assert not (work / "stop-escaped").exists()
            (out / "stop-command-result.json").write_text(json.dumps(outcome, indent=2) + "\n")
        ended = call("GET", f"/api/v1/sessions/{session}", token=chef)
        assert ended["state"] == "ended", ended
        passed("native 1.1 Stop ends the carried-over session and cancels its active owned terminal command")
        enrollment = call("POST", "/api/v1/device/enrollments", {}, credentials()[0]["device_credential"], "Extend-Device")
        second = call("POST", "/api/v1/pairings", {"pairing_code": enrollment["pairing_code"], "name": "Owned second Carbon",
                                                      "silicon_ids": ["si:sous"]}, bob)["device_id"]
        claimed = call("GET", "/api/v1/enrollments/" + enrollment["enrollment_id"], token=enrollment["enrollment_secret"], scheme="Extend-Enrollment")
        stop(child)
        pairs = credentials()
        pairs.append({"device_id": second, "device_credential": claimed["device_credential"], "service_url": api + "/", "first_pair": False})
        credfile.write_text(json.dumps(pairs, indent=2) + "\n")
        credfile.chmod(0o600)
        child = start_agent(new, "new-multicarbon")
        shared = online(child, "1.1.0", device, alice, count=2)
        byid = {p["device_id"]: p for p in shared["pairs"]}
        assert byid[device]["terminal_withheld"] is False
        assert byid[second]["terminal_withheld"] is True
        assert "terminal" not in call("GET", "/api/v1/devices/" + second, token=bob)["capabilities"]
        other_session = call("POST", "/api/v1/sessions", {"device_id": second}, sous)["session_id"]
        refused = terminal(other_session, sous, "printf forbidden > should-not-exist", expected=422)
        assert not (work / "should-not-exist").exists()
        (out / "second-carbon-terminal-refusal.json").write_text(json.dumps(refused, indent=2) + "\n")
        before_rotation = {row["device_id"]: row for row in credentials()}
        identity_before = pair_rows(before_rotation)
        assert len(before_rotation) == 2
        native_stop(new)
        assert call("GET", f"/api/v1/sessions/{other_session}", token=sous)["state"] == "ended"
        after_rotation = await_value("both native links persist and acknowledge rotated credentials", lambda: rotated(before_rotation))
        facts = []
        for identity, row in after_rotation.items():
            # The DB check above establishes acknowledgement before these reads, so a GET with
            # the new credential cannot itself promote an otherwise-unacknowledged rotation.
            call("GET", "/api/v1/device", token=before_rotation[identity]["device_credential"], scheme="Extend-Device", expected=401)
            info = call("GET", "/api/v1/device", token=row["device_credential"], scheme="Extend-Device", expected=200)
            assert info["device_id"] == identity
            facts.append({"device_id": identity,
                          "old_sha256": hashlib.sha256(before_rotation[identity]["device_credential"].encode()).hexdigest(),
                          "new_sha256": hashlib.sha256(row["device_credential"].encode()).hexdigest(),
                          "old_status": 401, "new_status": 200, "server_acknowledged_before_api_read": True})
        report["rotation"] = facts
        passed("ending a shared-computer session rotates both native persisted credentials; acknowledged new values authenticate and old values are refused")
        stop(child)
        child = start_agent(new, "new-rotated-reconnect")
        shared = online(child, "1.1.0", device, alice, count=2)
        def fallbacks_cleared():
            pairs = {row["device_id"]: row for row in credentials()}
            return pairs if pairs.keys() == after_rotation.keys() and all(
                row["device_credential"] == after_rotation[identity]["device_credential"]
                and row.get("previous_credential") is None for identity, row in pairs.items()) else False
        await_value("both persisted fallback credentials cleared after successful reconnect", fallbacks_cleared)
        identity_after = pair_rows(after_rotation)
        for identity in identity_before:
            for key in ("device_id", "instance_id", "owner_id", "first_pair"):
                assert identity_before[identity][key] == identity_after[identity][key], (identity, key)
        assert len({row["instance_id"] for row in identity_after.values()}) == 1
        assert {p["device_id"]: p["terminal_withheld"] for p in shared["pairs"]} == {device: False, second: True}
        assert "terminal" not in call("GET", "/api/v1/devices/" + second, token=bob)["capabilities"]
        report["rotation_identity_before"] = identity_before
        report["rotation_identity_after"] = identity_after
        report["previous_credentials_cleared"] = True
        passed("both native links reconnect from rotated file credentials, clear fallback values and preserve pair identity plus first-pair terminal rules")
        first_session = call("POST", "/api/v1/sessions", {"device_id": device}, chef)["session_id"]
        terminal_ok(first_session, chef, "first-carbon-terminal")
        native_stop(new)
        assert call("GET", f"/api/v1/sessions/{first_session}", token=chef)["state"] == "ended"
        (out / "multicarbon-status.json").write_text(json.dumps(shared, indent=2) + "\n")
        passed("native 1.1 reconnects both Carbon pairs; only first-pair Silicon gets terminal; Stop also ends second-pair session")
        assert not (state / "start-at-login.json").exists()
        assert not absent_engine.exists()
        assert autostart_snapshot() == report["autostart_before"]
        passed("global autostart entry remains byte-for-byte and timestamp unchanged; no remembered autostart choice or screen engine created")
    except BaseException as error:
        report["failure"] = {"type": type(error).__name__, "detail": str(error)}
        raise
    finally:
        for process in reversed(processes):
            stop(process)
        for logfile in logs:
            logfile.close()
        if created:
            sql(f"DROP DATABASE {db} WITH (FORCE)")
            report["owned_database_removed"] = sql(f"SELECT count(*) FROM pg_database WHERE datname='{db}'") == "0"
        report["autostart_after"] = autostart_snapshot()
        report["owned_processes_stopped"] = all(p.poll() is not None for p in processes)
        report["owned_pids"] = [p.pid for p in processes]
        report["agent_hashes_unchanged"] = digest(old) == report["old_agent_sha256"] and digest(new) == report["current_agent_sha256"]
        if args.new_agent:
            report["signatures_valid_after"] = signing(old, "old-after", "1.0.0") == signing(new, "new-after", "1.1.0")
        if credfile.exists():
            report["owned_credential_file_sha256"] = digest(credfile)
            credfile.unlink()
        report["owned_credential_file_removed"] = not credfile.exists()
        report["duration_s"] = round(time.monotonic() - started, 3)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
