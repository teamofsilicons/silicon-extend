#!/usr/bin/env python3
"""Exercise a released 1.0 CLI, then upgrade it in place, against an isolated current service.

Uses the local development PostgreSQL container and a scripted 1.0 device. No installed app,
saved user login, or existing service is changed. The exact owned database/processes are removed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
import urllib.request
import uuid


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-cli", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--full-cli-lane", action="store_true", help="also run the current complete CLI suite on this owned service")
    args = parser.parse_args()
    old = args.old_cli.resolve(strict=True)
    new = root / "target/debug/extend"
    service = root / "target/debug/extend-service"
    fake = root / "target/debug/examples/fake_device"
    for binary in (new, service, fake):
        if not binary.is_file():
            parser.error(f"Build the current CLI/service/fake_device first: {binary}")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    db = "extend_released_cli_" + uuid.uuid4().hex
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    api = f"http://127.0.0.1:{port}"
    env = dict(os.environ)
    for key in list(env):
        if key.startswith("EXTEND_") or key in ("SILICON_HOME", "NO_COLOR"):
            env.pop(key)
    env.update(EXTEND_API_URL=api, EXTEND_TELEMETRY="off")
    work = out / db
    work.mkdir()
    report = {"passed": [], "database": db, "api": api,
              "old_cli_sha256": hashlib.sha256(old.read_bytes()).hexdigest()}
    processes = []
    logs = []
    created = False

    def sql(statement):
        return subprocess.run(["docker", "exec", "silicon-extend-postgres", "psql", "-U", "extend",
                               "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-Atc", statement],
                              check=True, capture_output=True, text=True).stdout.strip()

    def passed(name):
        report["passed"].append(name)
        print("PASS " + name, flush=True)

    def cli(binary, who, *words, code=0, data=False):
        home = work / who
        home.mkdir(exist_ok=True)
        result = subprocess.run([str(binary), *words], env=dict(env, SILICON_HOME=str(home)),
                                capture_output=True, text=True, timeout=30)
        assert result.returncode == code, (who, words, result.returncode, result.stdout, result.stderr)
        return json.loads(result.stdout) if data else result.stdout

    def start(command, path, process_env):
        log = path.open("w")
        logs.append(log)
        process = subprocess.Popen(command, cwd=root, env=process_env, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        return process

    try:
        sql(f"CREATE DATABASE {db}")
        created = True
        service_env = dict(env, EXTEND_ENVIRONMENT="development", EXTEND_IAM_MODE="local",
                           EXTEND_FILES_MODE="local", EXTEND_TING_MODE="local",
                           EXTEND_LOCAL_MEMBERS="c:alice@acme,c:bob@acme,si:chef@acme,si:sous@acme",
                           EXTEND_DATABASE_URL=f"postgres://extend:extend@127.0.0.1:5440/{db}",
                           EXTEND_BIND=f"127.0.0.1:{port}", EXTEND_PUBLIC_URL=api,
                           EXTEND_HONEYCOMB_SERVICE_TOKEN="hck_local_dev_token",
                           EXTEND_DATA_DIR=str(work / "data"))
        proc = start([str(service)], out / "service.log", service_env)
        for _ in range(100):
            try:
                with urllib.request.urlopen(api + "/ready", timeout=1) as response:
                    if response.status == 204:
                        break
            except OSError:
                assert proc.poll() is None, "Service stopped; inspect service.log"
                time.sleep(0.1)
        else:
            raise AssertionError("Service never became ready")
        version = cli(old, "nobody", "version")
        assert "extend 1.0.0" in version and "service 1.1.0" in version, version
        passed("released CLI 1.0.0 negotiates service 1.1.0")
        for who in ("alice", "chef"):
            cli(old, who, "login", ("c:" if who == "alice" else "si:") + who)
        fake_log = out / "fake-device.log"
        start([str(fake), api, "linux"], fake_log, dict(env, FAKE_APP_VERSION="1.0.0"))
        for _ in range(100):
            match = re.search(r"PAIRING_CODE\s+(\w+)", fake_log.read_text())
            if match:
                break
            time.sleep(0.1)
        else:
            raise AssertionError("Fake 1.0 device never enrolled")
        device = cli(old, "alice", "--json", "device", "pair", match[1], "--name", "Released CLI box",
                     "--access", "si:chef", data=True)["device_id"]
        for _ in range(60):
            view = cli(old, "chef", "--json", "device", "show", device, data=True)
            if view.get("online"):
                break
            time.sleep(0.1)
        else:
            raise AssertionError("Legacy device never became online")
        passed("old CLI login, pairing, grants and device decoding")
        session = cli(old, "chef", "session", "new", device, "--connect").strip()
        assert re.fullmatch("[0-9a-f]{3,}", session), session
        cli(old, "chef", "snapshot", "-i")
        image = work / "old-cli.png"
        capture = cli(old, "chef", "--json", "screenshot", "--out", str(image), data=True)
        assert capture["ok"] and image.read_bytes().startswith(b"\x89PNG")
        passed("old CLI session, device command and file download")
        # A binary upgrade must read the existing login/session, without another login.
        assert cli(new, "chef", "--json", "login", "status", data=True)["authenticated"]
        cli(new, "alice", "device", "banner", device, "off")
        assert cli(new, "alice", "--json", "device", "show", device, data=True)["in_use_indicator"] == "hidden"
        cli(old, "alice", "--json", "device", "show", device, data=True)
        cli(old, "chef", "--json", "session", "status", data=True)
        passed("1.1 reads saved 1.0 login and session; 1.0 reads additive banner responses")
        cli(new, "chef", "snapshot", "-i")
        cli(old, "chef", "takeover", "--reason", "Compatibility rehearsal")
        cli(new, "chef", "snapshot", code=8)
        cli(new, "chef", "takeover", "release")
        cli(old, "chef", "snapshot")
        passed("mixed-version takeover and release preserve the active session")
        cli(new, "alice", "device", "stop", device)
        cli(old, "chef", "snapshot", code=6)
        passed("new CLI Stop reaches the old CLI with session-ended status")
        second = cli(new, "chef", "session", "new", device, "--connect").strip()
        assert second != session and re.fullmatch("[0-9a-f]{3,}", second), second
        cli(old, "chef", "snapshot")
        cli(old, "chef", "session", "end", second)
        cli(old, "alice", "device", "rm", device, "--yes")
        passed("old CLI resumes a session saved by 1.1, ends it and removes its device")
        if args.full_cli_lane:
            with (out / "full-cli.log").open("w") as log:
                subprocess.run(["bash", str(root / "e2e/cli-e2e.sh"), api], cwd=root, env=env,
                               stdout=log, stderr=subprocess.STDOUT, timeout=600, check=True)
            passed("complete current CLI suite against the isolated 1.1 service")
    except BaseException as error:
        report["failure"] = {"type": type(error).__name__, "detail": str(error)}
        raise
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log in logs:
            log.close()
        if created:
            sql(f"DROP DATABASE {db} WITH (FORCE)")
            report["owned_database_removed"] = sql(f"SELECT count(*) FROM pg_database WHERE datname='{db}'") == "0"
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
