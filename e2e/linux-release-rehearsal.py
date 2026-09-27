#!/usr/bin/env python3
"""Verify an existing Linux arm64 release .deb on an owned Docker X11 desktop.

Uses an already-built silicon-extend-linux-e2e image, an exact owned PostgreSQL database,
and a fresh local service with synthetic IAM. No build, installed host app, existing service,
shared output, or other container is changed. Screenshots/input affect only this container.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import uuid


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--agent-bin", type=Path, help="test a newly built native agent with the package's unchanged engine; reported separately")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    package = args.package.resolve(strict=True)
    agent_override = args.agent_bin.resolve(strict=True) if args.agent_bin else None
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    out.chmod(0o777)  # Writable by the disposable container's carbon user.
    image = "silicon-extend-linux-e2e"
    harness = root / "apps/desktop/linux-e2e"
    container = "extend-linux-release-" + uuid.uuid4().hex[:12]
    database = "extend_linux_release_" + uuid.uuid4().hex
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    api = f"http://127.0.0.1:{port}"
    env = {k: v for k, v in os.environ.items() if not k.startswith("EXTEND_") and k != "SILICON_HOME"}
    env["EXTEND_API_URL"] = api
    service = out / "service"
    cli_binary = out / "extend"
    shutil.copy2(root / "target/debug/extend-service", service)
    shutil.copy2(root / "target/debug/extend", cli_binary)
    report = {"passed": [], "container": container, "database": database, "api": api,
              "package": str(package), "package_sha256": hashlib.sha256(package.read_bytes()).hexdigest(),
              "source_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
              "image": subprocess.check_output(["docker", "image", "inspect", image, "--format", "{{.Id}}"], text=True).strip(),
              "limitations": ["Debian trixie arm64 Docker X11 desktop, synthetic local IAM/files/Ting",
                              "Existing desktop image contains development libraries; this run alone does not establish pristine package dependency sufficiency",
                              "No physical Linux, Wayland, production provider, or native Windows proof"]}
    if agent_override:
        report["agent_override"] = {"path": str(agent_override), "sha256": hashlib.sha256(agent_override.read_bytes()).hexdigest()}
    server = None
    created = False
    container_started = False
    started = time.monotonic()

    def run(argv, **kwargs):
        return subprocess.run(argv, capture_output=True, text=True, check=True, **kwargs).stdout.strip()

    def sql(statement):
        return run(["docker", "exec", "silicon-extend-postgres", "psql", "-U", "extend", "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-Atc", statement])

    def passed(label):
        report["passed"].append(label)
        print("PASS " + label, flush=True)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    def wait(label, fn, timeout=60):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = fn()
            if value:
                return value
            time.sleep(.2)
        raise AssertionError("Timed out: " + label)

    def cli(who, *words, json_output=True):
        home = out / ("cli-" + who)
        home.mkdir(exist_ok=True, mode=0o700)
        command = [str(cli_binary), "--timeout", "90000", *words]
        if json_output:
            command.append("--json")
        result = run(command, env=dict(env, SILICON_HOME=str(home)), timeout=100)
        return json.loads(result) if json_output else result

    def remote(*words):
        value = cli("chef", *words)
        assert value["ok"], value
        return value

    def inside(*words, user="carbon"):
        return run(["docker", "exec", "--user", user, "-e", "DISPLAY=:99", container, *words], timeout=30)

    def status():
        path = out / "agent-home/.extend-agent/status.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def geometry():
        result = subprocess.run(["docker", "exec", "--user", "carbon", "-e", "DISPLAY=:99", container,
                                 "xdotool", "search", "--onlyvisible", "--name", "^Silicon Extend: in use$"], capture_output=True, text=True)
        if not result.stdout.strip():
            return None
        window = result.stdout.splitlines()[-1]
        values = dict(line.split("=", 1) for line in inside("xdotool", "getwindowgeometry", "--shell", window).splitlines())
        return {k: int(values[k]) for k in ("WINDOW", "X", "Y", "WIDTH", "HEIGHT")}

    def capture(name):
        inside("scrot", "/tmp/out/" + name + ".png")

    # All desktop/session startup happens inside the named disposable container.
    (out / "desktop.sh").write_text("""#!/bin/bash
set -euo pipefail
export DISPLAY=:99 XDG_SESSION_TYPE=x11 GTK_A11Y=atspi NO_AT_BRIDGE=0
unset WAYLAND_DISPLAY
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >/tmp/out/xvfb.log 2>&1 &
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep .1; done
openbox >/tmp/out/openbox.log 2>&1 &
/usr/libexec/at-spi-bus-launcher --launch-immediately >/tmp/out/atspi.log 2>&1 &
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 WEBKIT_DISABLE_COMPOSITING_MODE=1
exec extend-agent run --no-autostart >/tmp/out/agent.log 2>&1
""")
    (out / "container.sh").write_text("""#!/bin/bash
set -euo pipefail
dpkg-deb -f /tmp/extend-package.deb >/tmp/out/package-control.txt
dpkg -i /tmp/extend-package.deb >/tmp/out/package-install.log 2>&1
if test -f /tmp/agent-override; then install -m 755 /tmp/agent-override /usr/bin/extend-agent; fi
ldd /usr/bin/extend-agent >/tmp/out/agent-ldd.txt
! grep -q 'not found' /tmp/out/agent-ldd.txt
/usr/bin/extend-agent --version >/tmp/out/installed-version.txt
exec runuser -u carbon -- env PATH=/tmp/out:/usr/bin:/bin SILICON_HOME=/tmp/out/agent-home EXTEND_AGENT_CREDENTIAL_STORE=file EXTEND_API_URL="$EXTEND_API_URL" EXTEND_TERMINAL_SHELL='/bin/sh -c' dbus-run-session -- bash /tmp/out/desktop.sh
""")
    fixture = out / "extend-recording-fixture"
    fixture.write_text("#!/bin/sh\nexec python3 /harness/record-fixture.py\n")
    fixture.chmod(0o755)
    try:
        sql(f"CREATE DATABASE {database}")
        created = True
        service_env = dict(env, EXTEND_ENVIRONMENT="development", EXTEND_IAM_MODE="local", EXTEND_FILES_MODE="local",
                           EXTEND_TING_MODE="local", EXTEND_LOCAL_MEMBERS="c:alice@acme,si:chef@acme",
                           EXTEND_DATABASE_URL=f"postgres://extend:extend@127.0.0.1:5440/{database}", EXTEND_BIND=f"127.0.0.1:{port}",
                           EXTEND_PUBLIC_URL=api, EXTEND_DATA_DIR=str(out / "service-data"), EXTEND_HONEYCOMB_SERVICE_TOKEN="hck_local_dev_token")
        with (out / "service.log").open("w") as log:
            server = subprocess.Popen([str(service)], env=service_env, cwd=out, stdout=log, stderr=subprocess.STDOUT)
        import urllib.request

        def ready():
            assert server.poll() is None, "Owned service exited"
            try:
                return urllib.request.urlopen(api + "/ready", timeout=1).status == 204
            except OSError:
                return False
        wait("owned service readiness", ready)
        cli("alice", "login", "c:alice")
        cli("chef", "login", "si:chef")
        extra_mounts = ["-v", str(agent_override) + ":/tmp/agent-override:ro"] if agent_override else []
        run(["docker", "run", "-d", "--init", "--name", container,
             "-e", "EXTEND_API_URL=" + f"http://host.docker.internal:{port}",
             "-v", str(package) + ":/tmp/extend-package.deb:ro", "-v", str(harness) + ":/harness:ro",
             "-v", str(out) + ":/tmp/out", *extra_mounts, image, "bash", "/tmp/out/container.sh"])
        container_started = True
        code = wait("native Linux pairing", lambda: (status().get("pairing") or {}).get("code"))
        device = cli("alice", "device", "pair", code, "--name", "Owned Linux release", "--access", "si:chef")["device_id"]
        current = wait("native Linux online", lambda: status() if status().get("phase") == "online" else None)
        assert current["app_version"] == "1.1.0" and "screen.record" in current["capabilities"], current
        report["device_id"] = device
        passed(("CI arm64 package with explicitly recorded native agent override" if agent_override else "CI arm64 package")
               + " installs on Debian trixie; native 1.1 WebKit/X11 app pairs through isolated service")
        session = cli("chef", "session", "new", device, "--connect", json_output=False)
        report["session_id"] = session
        original_banner = wait("native activity banner", geometry)
        time.sleep(1)
        original_banner = geometry()
        report["banner_initial_geometry"] = original_banner
        if (original_banner["WIDTH"], original_banner["HEIGHT"]) != (420, 52):
            report["findings"] = ["Native banner geometry differs from intended 420x52: " + json.dumps(original_banner)]
        capture("banner-expanded")
        remote("open", "extend-recording-fixture")
        for scope in ("app", "device"):
            start = remote("record", "start", "release-" + scope, "--scope", scope, "--fps", "12", "--hide-touches")
            time.sleep(1)
            cli("alice", "device", "banner", device, "off")
            wait("banner hidden", lambda: geometry() is None)
            cli("alice", "device", "rename", device, "Renamed during " + scope + " recording")
            time.sleep(1)
            cli("alice", "device", "banner", device, "on")
            wait("banner restored", geometry)
            time.sleep(1)
            path = out / (scope + ".mp4")
            result = remote("record", "stop", "--out", str(path))
            assert len(result["files"]) == 1 and path.stat().st_size > 0, result
            run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "null", "-"])
            info = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", str(path)]))
            video = next(s for s in info["streams"] if s["codec_type"] == "video")
            assert (video["width"], video["height"]) == ((642, 482) if scope == "app" else (1280, 800)), video
            assert float(info["format"]["duration"]) >= 3, info
            frames = run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "framemd5", "-"])
            assert len({line.split(",")[-1] for line in frames.splitlines() if line and not line.startswith("#")}) > 1
            again = out / (scope + "-again.mp4")
            cli("chef", "file", "get", result["files"][0]["file_id"], "--out", str(again))
            assert hashlib.sha256(path.read_bytes()).digest() == hashlib.sha256(again.read_bytes()).digest()
            (out / (scope + "-recording.json")).write_text(json.dumps({"start": start, "result": result, "ffprobe": info}, indent=2) + "\n")
            passed(scope + " recording survives banner off/name update/banner on, fully decodes and downloads identically")
        # The normal banner expires after ten seconds. Confirm it expires, then a new session
        # gives the native-control checks their own ten-second interval.
        wait("normal banner expires after ten seconds", lambda: geometry() is None, timeout=20)
        assert cli("chef", "session", "status", session)["state"] == "active"
        passed("normal activity banner expires while its session remains active")
        cli("chef", "session", "end", session)
        session = cli("chef", "session", "new", device, "--connect", json_output=False)
        report["native_control_session_id"] = session
        expanded = wait("new session restores native banner", geometry)
        assert (expanded["WIDTH"], expanded["HEIGHT"]) == (420, 52), expanded
        window = str(expanded["WINDOW"])
        inside("xdotool", "windowsize", window, "600", "300")
        constrained = wait("native fixed banner bounds", lambda: g if (g := geometry()) and (g["WIDTH"], g["HEIGHT"]) == (420, 52) else None)
        assert constrained["WIDTH"] == 420 and constrained["HEIGHT"] == 52
        # The actual 420x52 native banner was captured above; CSS places the collapse control in its left grip.
        inside("xdotool", "mousemove", "--window", window, "14", "14", "click", "1")
        collapsed = wait("native collapsed banner", lambda: g if (g := geometry()) and g["WIDTH"] == 250 else None)
        assert collapsed["HEIGHT"] == 44
        capture("banner-collapsed")
        inside("xdotool", "mousemove", "--window", window, "85", "22", "click", "1")
        restored = wait("native restored banner", lambda: g if (g := geometry()) and g["WIDTH"] == 420 else None)
        assert (restored["X"], restored["Y"]) == (expanded["X"], expanded["Y"])
        # Drag the text area, away from the collapse and Stop buttons.
        inside("xdotool", "mousemove", "--window", window, "120", "25", "mousedown", "1", "sleep", ".15",
               "mousemove_relative", "--sync", "100", "50", "sleep", ".15", "mouseup", "1")
        moved = wait("native banner moved", lambda: g if (g := geometry()) and (g["X"], g["Y"]) != (restored["X"], restored["Y"]) else None)
        cli("alice", "device", "rename", device, "Renamed after native banner drag")
        wait("native pair metadata refresh", lambda: any(p.get("name") == "Renamed after native banner drag" for p in status().get("pairs", [])))
        refreshed = geometry()
        assert refreshed == moved, (moved, refreshed)
        capture("banner-moved")
        (out / "banner-geometry.json").write_text(json.dumps({"expanded": expanded, "constrained": constrained, "collapsed": collapsed, "restored": restored, "moved": moved, "refreshed": refreshed}, indent=2) + "\n")
        passed("native WebKit banner respects fixed bounds, collapses, restores, drags, and retains position through metadata refresh")
        remote("terminal", "run", "setsid /bin/sleep 300 >/dev/null 2>&1 & echo $! > /tmp/out/terminal-child.pid", "--cwd", "/tmp/out")
        child_pid = int((out / "terminal-child.pid").read_text().strip())
        wait("detached terminal child running", lambda: inside("sh", "-c", f"tr '\\000' ' ' < /proc/{child_pid}/cmdline") == "/bin/sleep 300")
        inside("xdotool", "mousemove", "--window", window, "14", "14", "click", "1")
        wait("native collapsed Stop", lambda: g if (g := geometry()) and g["WIDTH"] == 250 else None)
        inside("xdotool", "mousemove", "--window", window, "213", "22", "click", "1")
        wait("session ended by native Stop", lambda: cli("chef", "session", "status", session).get("state") == "ended")
        wait("detached terminal child removed", lambda: inside("sh", "-c", f"if test -d /proc/{child_pid}; then echo alive; else echo gone; fi") == "gone")
        passed("collapsed native Stop ends the session and kills its detached setsid terminal descendant")
        capture("after-stop")
    except BaseException as error:
        report["failure"] = {"type": type(error).__name__, "detail": str(error)}
        raise
    finally:
        if container_started:
            result = subprocess.run(["docker", "logs", container], capture_output=True, text=True)
            (out / "container.log").write_text(result.stdout + result.stderr)
            subprocess.run(["docker", "stop", "--time", "10", container], check=True, stdout=subprocess.DEVNULL)
            subprocess.run(["docker", "rm", container], check=True, stdout=subprocess.DEVNULL)
            report["owned_container_removed"] = True
        if server and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        if created:
            sql(f"DROP DATABASE {database} WITH (FORCE)")
            report["owned_database_removed"] = sql(f"SELECT count(*) FROM pg_database WHERE datname='{database}'") == "0"
        report["owned_service_stopped"] = server is None or server.poll() is not None
        report["duration_s"] = round(time.monotonic() - started, 3)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
