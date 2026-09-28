#!/usr/bin/env python3
"""Verify automatic debugging recovery on an explicitly selected, dedicated emulator.

Install the debug app/test APK, pair it with fake-service/fake_extend.py (no scenario),
enable tcpip 5555, and run LocalAdbTest#connectLocalForService with service_test=true.
Approve Android's debugging prompt once, then reopen the app after instrumentation.
This lane never opens an activity or presses Connect during recovery. It kills only
the app's PID and restarts only the selected emulator's adbd. Never use a shared AVD.
"""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request


PKG = "com.teamofsilicons.extend"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--port", type=int, required=True, help="localhost fake service port")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--adb", default=os.environ.get("ADB", "adb"))
    parser.add_argument("--legacy-emulator-bridge", action="store_true",
                        help="API 26/28 emulator pipe-only adbd: restore its TCP reverse after restart")
    args = parser.parse_args()
    if (not args.serial.startswith("emulator-") or not args.serial.removeprefix("emulator-").isdigit()
            or not 1 <= args.port <= 65535):
        parser.error("Select a dedicated emulator and a valid local fake-service port")
    args.out.mkdir(parents=True, exist_ok=True)
    observations = []

    def adb(*command, check=True):
        result = subprocess.run([args.adb, "-s", args.serial, *command],
                                capture_output=True, text=True, timeout=30, check=check)
        return result.stdout.strip()

    def request(path, data=None):
        body = None if data is None else json.dumps(data).encode()
        req = urllib.request.Request(f"http://127.0.0.1:{args.port}/_test/{path}", data=body,
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=10) as response:
            return json.load(response)

    def command(words):
        return request("command", {"command": "adb", "args": ["shell", *words],
                                   "session_id": "a3f", "timeout_ms": 3000})

    def memory(label):
        (args.out / f"memory-{label}.txt").write_text(adb("shell", "dumpsys", "meminfo", PKG))

    def recovered(label, started, old_pid=None):
        last = None
        while time.monotonic() - started < 120:
            pid = adb("shell", "pidof", PKG, check=False)
            if pid.isdigit() and (old_pid is None or pid != old_pid):
                try:
                    last = command(["id", "-u"])
                    if last.get("ok") and last.get("output", {}).get("stdout", "").strip() == "2000":
                        observation = {"case": label, "seconds": round(time.monotonic() - started, 3),
                                       "old_pid": old_pid, "pid": pid, "result": last}
                        observations.append(observation)
                        print(f"PASS {label}: shell uid 2000 in {observation['seconds']}s; PID {pid}", flush=True)
                        memory(label)
                        return
                except (OSError, urllib.error.URLError, TimeoutError) as error:
                    last = str(error)
            time.sleep(2)
        raise AssertionError(f"{label} did not recover without opening the app: {last}")

    assert adb("shell", "getprop", "ro.kernel.qemu") == "1", "Emulators only"
    if args.legacy_emulator_bridge:
        assert int(adb("shell", "getprop", "ro.build.version.sdk")) <= 28, "Pipe bridge is only for old emulator images"
    state = request("state")
    assert len(state["pairs"]) == 1 and state["pairs"][0]["connected"], "Use an isolated paired fake service"
    # Establish a real command session; the fake service restores it after socket reconnect.
    request("frame", {"type": "session_started", "target": None, "session_id": "a3f",
                      "silicon_id": "si:chef", "since": datetime.now(timezone.utc).isoformat(), "side": "side-alice"})
    try:
        # Ensure the service is connected to the selected AVD before any process mutation.
        avd_property = "ro.boot.qemu.avd_name"
        avd = adb("shell", "getprop", avd_property)
        if not avd:
            avd_property = "ro.kernel.qemu.avd_name"
            avd = adb("shell", "getprop", avd_property)
        assert avd, "The selected emulator must expose its AVD name"
        remote = command(["getprop", avd_property])
        assert remote.get("ok") and remote["output"]["stdout"].strip() == avd, "Fake service targets a different emulator"
        recovered("baseline", time.monotonic())
        adb("shell", "input", "keyevent", "KEYCODE_HOME")
        time.sleep(3)
        memory("background")
        pid = adb("shell", "pidof", PKG)
        assert pid.isdigit(), f"Expected one app process, got {pid!r}"
        started = time.monotonic()
        adb("shell", "run-as", PKG, "kill", "-9", pid)
        recovered("process-death", started, pid)
        # Dropping a live transport must also recover without a manual Connect.
        started = time.monotonic()
        adb("tcpip", "5555")
        time.sleep(2)
        if args.legacy_emulator_bridge:
            host_adb_port = int(args.serial.removeprefix("emulator-")) + 1
            adb("reverse", "tcp:5555", f"tcp:{host_adb_port}")
        recovered("adbd-restart", started)
    finally:
        (args.out / "results.json").write_text(json.dumps(observations, indent=2) + "\n")
        (args.out / "logcat.txt").write_text(adb("logcat", "-d", "-s", "SiliconExtend", "ActivityManager", check=False))
        request("frame", {"type": "session_ended", "session_id": "a3f", "reason": "released"})


if __name__ == "__main__":
    main()
