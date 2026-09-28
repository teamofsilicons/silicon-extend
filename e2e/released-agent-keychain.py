#!/usr/bin/env python3
"""Rehearse signed Mac 1.0 -> 1.1 Keychain migration in a fresh local-service namespace.

Requires explicit --allow-native-keychain and exact old/new signed executables. Uses only
three preflighted Keychain accounts at a random loopback port, a private SILICON_HOME,
an owned database and child processes. No secret is read by security(1), UI/engine is
started, or installed app/autostart/Keychain ACL is changed. Any Keychain block aborts.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import re
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
KEYRING_SERVICE = "Silicon Extend"


class KeychainBlocked(RuntimeError):
    """User interaction would be needed; the harness never grants it."""


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-agent", required=True, type=Path)
    parser.add_argument("--new-agent", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--allow-native-keychain", action="store_true")
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64" or not args.allow_native_keychain:
        parser.error("Requires macOS arm64 and explicit --allow-native-keychain")
    old, new = args.old_agent.resolve(strict=True), args.new_agent.resolve(strict=True)
    if digest(old) != OLD_AGENT_SHA256:
        parser.error("Old executable does not match the verified published 1.0.0 release")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False, mode=0o700)
    home, work = out / "private-home", out / "terminal-work"
    home.mkdir(mode=0o700)
    work.mkdir(mode=0o700)
    state = home / ".extend-agent"
    credfile, statusfile = state / "credential.json", state / "status.json"
    absent_engine = out / "disabled-engine-does-not-exist"
    service = out / "current-service"
    shutil.copy2(root / "target/debug/extend-service", service)
    database = "extend_keychain_upgrade_" + uuid.uuid4().hex
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    api = f"http://127.0.0.1:{port}"
    legacy, index = f"device-credential@127.0.0.1:{port}", f"{api}/#pairs"
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

    def security_agents():
        result = subprocess.run(["pgrep", "-x", "SecurityAgent"], capture_output=True, text=True, timeout=3)
        if result.returncode not in (0, 1):
            raise RuntimeError("Cannot monitor whether Keychain requests user interaction")
        return set(result.stdout.split())

    prompt_baseline = security_agents()
    report = {"passed": [], "api": api, "database": database,
              "source_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
              "release_url": "https://github.com/teamofsilicons/silicon-extend/releases/tag/v1.0.0",
              "original_release_archive_sha256": RELEASE_SHA256,
              "old_agent": str(old), "old_agent_sha256": digest(old),
              "new_agent": str(new), "new_agent_sha256": digest(new),
              "service_sha256": digest(service), "autostart_before": autostart_snapshot(),
              "keychain_service": KEYRING_SERVICE, "preflighted_accounts": [],
              "limitations": ["Headless native OS Keychain migration against synthetic local service only",
                              "No installed GUI replacement, TCC, screen engine, GUI pairing or physical interaction proof",
                              "Exact-account metadata queries only; credential equality inferred from successful authenticated reconnect and unchanged server digest",
                              "New SecurityAgent process or blocked operation aborts; no prompt approval, keychain unlock or ACL change"]}
    processes, logs, owned_accounts = [], [], []
    database_created = False
    started = time.monotonic()

    def flush_report():
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    def passed(name):
        report["passed"].append(name)
        print("PASS " + name, flush=True)
        flush_report()

    def sql(statement, db="postgres"):
        return subprocess.check_output(["docker", "exec", "silicon-extend-postgres", "psql", "-U", "extend",
                                        "-d", db, "-v", "ON_ERROR_STOP=1", "-Atc", statement], text=True, timeout=10).strip()

    def keychain(action, account):
        assert account in (legacy, index) or re.fullmatch(re.escape(api + "/#") + r"[0-9a-f]{8}", account)
        # No -w or -g: only account metadata is queried. Never enumerate other accounts.
        try:
            result = subprocess.run(["security", action, "-s", KEYRING_SERVICE, "-a", account],
                                    capture_output=True, text=True, timeout=4)
        except subprocess.TimeoutExpired as error:
            raise KeychainBlocked("Exact-account Keychain operation blocked; no prompt was approved") from error
        if result.returncode == 0:
            return True
        if result.returncode == 44 and "could not be found" in result.stderr:
            return False
        raise KeychainBlocked(f"Exact-account Keychain operation refused (exit {result.returncode}); user interaction may be required")

    def preflight(account):
        if keychain("find-generic-password", account):
            raise RuntimeError("Test Keychain account already exists; refusing to use or delete it: " + account)
        owned_accounts.append(account)
        report["preflighted_accounts"].append(account)
        flush_report()

    def signing(binary, label, version):
        subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True, capture_output=True, text=True, timeout=15)
        result = subprocess.run(["codesign", "-d", "-r-", "--verbose=4", str(binary)],
                                check=True, capture_output=True, text=True, timeout=15)
        details = result.stdout + result.stderr
        (out / (label + "-codesign.txt")).write_text(details)
        assert "Identifier=com.teamofsilicons.extend\n" in details
        assert "TeamIdentifier=LTBSK59BJ2\n" in details and "flags=0x10000(runtime)" in details
        assert "Authority=Developer ID Application: Shubham Gupta (LTBSK59BJ2)\n" in details
        requirement = next(line.removeprefix("designated => ") for line in details.splitlines() if line.startswith("designated => "))
        assert version in subprocess.check_output([str(binary), "--version"], env=env, text=True, timeout=5)
        return requirement

    def stop(child):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)

    def spawn(command, name, process_env=env):
        logfile = (out / (name + ".log")).open("w")
        logs.append(logfile)
        child = subprocess.Popen(command, cwd=work, env=process_env, stdin=subprocess.DEVNULL,
                                 stdout=logfile, stderr=subprocess.STDOUT)
        processes.append(child)
        return child

    def await_value(label, fn, seconds=8, keychain_sensitive=False):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if keychain_sensitive and security_agents() - prompt_baseline:
                raise KeychainBlocked("SecurityAgent appeared during owned Keychain access; stopped without approving a prompt")
            value = fn()
            if value:
                return value
            time.sleep(.15)
        if keychain_sensitive:
            raise KeychainBlocked("Owned agent did not complete " + label + "; Keychain may require user interaction")
        raise AssertionError("Timed out: " + label)

    def call(method, path, data=None, token=None):
        headers = {"Content-Type": "application/json", "Idempotency-Key": str(uuid.uuid4())}
        if token:
            headers.update(Authorization=f"Bearer {token}", **{"X-Org-ID": "acme"})
        body = None if data is None else json.dumps({"type": "request", "data": data}).encode()
        request = urllib.request.Request(api + path, data=body, method=method, headers=headers)
        with urllib.request.urlopen(request, timeout=45) as response:
            value = json.loads(response.read())
        return value.get("data", value)

    def read_status():
        try:
            return json.loads(statusfile.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def agent_argv(binary, *words):
        return [str(binary), "--service-url", api, "--home", str(home), "--credential-store", "keyring", *words]

    def start_agent(binary, name):
        return spawn(agent_argv(binary, "run", "--headless"), name)

    def online(child, version, device, owner):
        def check():
            assert child.poll() is None, "Owned agent exited; inspect its log"
            value = read_status()
            if value.get("pid") != child.pid or value.get("app_version") != version or value.get("phase") != "online":
                return False
            if version == "1.1.0" and (len(value.get("pairs", [])) != 1 or value["pairs"][0]["device_id"] != device):
                return False
            return value if call("GET", "/api/v1/devices/" + device, token=owner)["online"] else False
        value = await_value("online " + version, check, keychain_sensitive=True)
        assert "terminal" in value["capabilities"]
        assert not any(c.startswith(("screen.", "input.")) for c in value["capabilities"])
        assert not credfile.exists(), "Explicit keyring mode must never fall back to credential.json"
        return value

    def server_credential_digest(device):
        assert re.fullmatch(r"[0-9a-f]{8}", device)
        result = sql("SELECT credential_digest FROM extend.devices WHERE device_id='" + device + "'", database)
        assert re.fullmatch(r"[0-9a-f]{64}", result)
        return result

    def terminal(session, token, command):
        return call("POST", f"/api/v1/sessions/{session}/commands",
                    {"command": "terminal", "args": ["run", "--cwd", str(work), command], "timeout_ms": 40000}, token)

    def terminal_ok(session, token, label):
        result = terminal(session, token, "printf '%s\\n' '" + label + "'; pwd")
        assert result["ok"] and result["output"]["stdout"] == label + "\n" + str(work) + "\n"
        (out / (label + ".json")).write_text(json.dumps(result, indent=2) + "\n")

    try:
        old_requirement = signing(old, "old", "1.0.0")
        new_requirement = signing(new, "new", "1.1.0")
        assert old_requirement == new_requirement, "Signing identity requirements changed; do not override Keychain ACL"
        report["matching_designated_requirement"] = old_requirement
        passed("published 1.0 and candidate 1.1 verify as the same Developer ID designated requirement")
        preflight(legacy)
        preflight(index)
        passed("exact legacy and 1.1 index Keychain accounts absent before test")
        sql(f"CREATE DATABASE {database}")
        database_created = True
        service_env = dict(env, EXTEND_ENVIRONMENT="development", EXTEND_IAM_MODE="local", EXTEND_FILES_MODE="local",
                           EXTEND_TING_MODE="local", EXTEND_LOCAL_MEMBERS="c:alice@acme,si:chef@acme",
                           EXTEND_DATABASE_URL=f"postgres://extend:extend@127.0.0.1:5440/{database}", EXTEND_BIND=f"127.0.0.1:{port}",
                           EXTEND_PUBLIC_URL=api, EXTEND_DATA_DIR=str(out / "data"), EXTEND_HONEYCOMB_SERVICE_TOKEN="hck_local_dev_token")
        server = spawn([str(service)], "service", service_env)
        def ready():
            assert server.poll() is None, "Owned service exited"
            try:
                with urllib.request.urlopen(api + "/ready", timeout=1) as response:
                    return response.status == 204
            except OSError:
                return False
        await_value("owned service readiness", ready, seconds=30)
        alice = call("POST", "/api/v1/auth/login", {"slt": "c:alice"})["access_token"]
        chef = call("POST", "/api/v1/auth/login", {"slt": "si:chef"})["access_token"]
        child = start_agent(old, "old-first")
        code = await_value("released agent pairing code", lambda: (read_status().get("pairing") or {}).get("code"), keychain_sensitive=True)
        device = call("POST", "/api/v1/pairings", {"pairing_code": code, "name": "Owned Keychain migration",
                                                      "silicon_ids": ["si:chef"]}, alice)["device_id"]
        report["device_id"] = device
        preflight(f"{api}/#{device}")
        first = online(child, "1.0.0", device, alice)
        assert keychain("find-generic-password", legacy) and not keychain("find-generic-password", index)
        before = server_credential_digest(device)
        report["server_credential_digest_before"] = before
        (out / "old-status.json").write_text(json.dumps(first, indent=2) + "\n")
        session = call("POST", "/api/v1/sessions", {"device_id": device}, chef)["session_id"]
        report["session_id"] = session
        terminal_ok(session, chef, "old-terminal")
        passed("signed released 1.0 pairs in native Keychain and executes owned terminal without file fallback")
        stop(child)
        child = start_agent(old, "old-reconnect")
        online(child, "1.0.0", device, alice)
        assert server_credential_digest(device) == before
        terminal_ok(session, chef, "old-reconnected-terminal")
        passed("signed 1.0 reconnect loads its Keychain item and preserves device, digest and session")
        stop(child)
        child = start_agent(new, "new-migrate")
        current = online(child, "1.1.0", device, alice)
        assert not keychain("find-generic-password", legacy)
        assert keychain("find-generic-password", index) and keychain("find-generic-password", f"{api}/#{device}")
        assert server_credential_digest(device) == before
        assert current["pairs"][0]["first_pair"] is True
        (out / "new-status.json").write_text(json.dumps(current, indent=2) + "\n")
        terminal_ok(session, chef, "new-migrated-terminal")
        passed("signed 1.1 migrates legacy account to index+pair, deletes legacy and preserves same live session/credential")
        stop(child)
        child = start_agent(new, "new-reconnect")
        online(child, "1.1.0", device, alice)
        report["server_credential_digest_after"] = server_credential_digest(device)
        assert report["server_credential_digest_after"] == before
        terminal_ok(session, chef, "new-reconnected-terminal")
        passed("signed 1.1 restarts from only its migrated Keychain index+pair with no credential.json")
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(terminal, session, chef, "printf started > stop-started; /bin/sleep 30; printf escaped > stop-escaped")
            await_value("owned terminal started", lambda: (work / "stop-started").exists())
            stopped_at = time.monotonic()
            result = subprocess.run(agent_argv(new, "stop"), env=env, cwd=work, capture_output=True, text=True, timeout=10)
            assert result.returncode == 0 and "Stopped" in result.stdout
            outcome = pending.result(timeout=12)
            report["stop_elapsed_s"] = round(time.monotonic() - stopped_at, 3)
            assert outcome["ok"] is False and not (work / "stop-escaped").exists()
            assert call("GET", f"/api/v1/sessions/{session}", token=chef)["state"] == "ended"
            (out / "stop-result.json").write_text(json.dumps(outcome, indent=2) + "\n")
        passed("signed 1.1 native Stop ends carried-over session and cancels its owned terminal")
        assert not credfile.exists() and not (state / "start-at-login.json").exists() and not absent_engine.exists()
        assert autostart_snapshot() == report["autostart_before"]
        passed("global autostart content/mtime unchanged; no remembered autostart choice or file credential fallback")
    except BaseException as error:
        report["failure"] = {"type": type(error).__name__, "detail": str(error)}
        raise
    finally:
        cleanup_errors = []
        for process in reversed(processes):
            try:
                stop(process)
            except Exception as error:
                cleanup_errors.append({"process_pid": process.pid, "error": str(error)})
        for logfile in logs:
            logfile.close()
        report["keychain_cleanup"] = []
        for account in reversed(owned_accounts):
            try:
                if keychain("find-generic-password", account):
                    keychain("delete-generic-password", account)
                absent = not keychain("find-generic-password", account)
                report["keychain_cleanup"].append({"account": account, "absent": absent})
                assert absent
            except Exception as error:
                cleanup_errors.append({"account": account, "error": str(error)})
        if database_created:
            try:
                sql(f"DROP DATABASE {database} WITH (FORCE)")
                report["owned_database_removed"] = sql(f"SELECT count(*) FROM pg_database WHERE datname='{database}'") == "0"
            except Exception as error:
                cleanup_errors.append({"database": database, "error": str(error)})
        report["autostart_after"] = autostart_snapshot()
        report["owned_processes_stopped"] = all(p.poll() is not None for p in processes)
        report["credential_file_absent"] = not credfile.exists()
        report["cleanup_errors"] = cleanup_errors
        report["duration_s"] = round(time.monotonic() - started, 3)
        flush_report()
        if cleanup_errors:
            raise RuntimeError("Owned cleanup incomplete; see report (do not modify other Keychain accounts)")


if __name__ == "__main__":
    main()
