#!/usr/bin/env python3
"""Verify the production macOS banner in an isolated fake-agent app.

No screen capture, Accessibility/TCC change, service, driver, or real session is used.
The probe runs inside the fixture and sees only its own AppKit windows. Button
tests traverse the production WKWebView -> IPC -> native window/action path.
Physical mouse dragging is a separate manual/CUA check, never inferred from a
programmatic AppKit move. --interactive leaves the fixture available for that check.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent


def write(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


def wait(check, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.025)
    raise AssertionError("fixture did not reach expected state")


class Fixture:
    def __init__(self, work):
        self.work = work
        self.evidence = []
        self.sequence = 0

    def command(self, op="snapshot", **fields):
        identity = uuid.uuid4().hex
        write(self.work / "probe-command.json", {"id": identity, "op": op, **fields})

        def response():
            try:
                data = json.loads((self.work / "probe-response.json").read_text())
                return data if data["id"] == identity else None
            except (FileNotFoundError, json.JSONDecodeError):
                return None

        return wait(response)

    def status(self, *, environment=False, attached=False, takeover=False, online=True, in_use=True):
        self.sequence += 1
        now = datetime.datetime.now(datetime.timezone.utc).isoformat()
        use = {"silicon_id": "si:banner-fixture", "session_id": f"fixture-{self.sequence}", "since": now}
        value = {"pid": self.process.pid, "app_version": "1.1.0", "service_url": "https://example.invalid",
                 "phase": "online" if online else "reconnecting", "credential_store": "fixture-none",
                 "updated_at": now, "in_use": use if in_use else None}
        if environment:
            value["environment"] = {"environment_id": "fixture", "name": "Banner fixture", "state": "active"}
        if takeover:
            value["takeover"] = {"session_id": use["session_id"], "reason": "Owned fixture takeover", "expires_at": now}
        if attached:
            value["attached"] = [{"device_id": "ba000001", "name": "Fixture TV", "os": "android_tv", "online": True, "in_use": use}]
        write(self.work / "status.json", value)
        return value

    def snapshot(self, label, predicate=lambda _: True):
        def matching():
            result = self.command()
            return result if result.get("dom") and predicate(result) else None
        result = wait(matching)
        self.evidence.append({"check": label, **result})
        write(self.work / "native-results.json", self.evidence)
        print(f"PASS {label}: {result['frame']}", flush=True)
        return result

    def actions(self):
        path = self.work / "actions.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def checks(self):
        self.status()
        initial = self.snapshot("native expanded", lambda s: s["visible"] and s["frame"]["width"] == 420 and "si:banner-fixture" in s["dom"]["text"])
        assert initial["frame"]["height"] == 52 and initial["level"] > 0 and not initial["key"]
        self.command("move", x=160, y=220)
        placed = self.snapshot("controlled AppKit position", lambda s: s["frame"]["x"] == 160 and s["frame"]["y"] == 220)
        self.command("click", selector="#banner-minimize")
        small = self.snapshot("native collapsed", lambda s: s["frame"]["width"] == 250 and s["dom"]["collapsed"])
        assert small["frame"] == {**placed["frame"], "width":250, "height":44}
        self.command("click", selector="#banner-restore")
        restored = self.snapshot("native restored with position retained", lambda s: s["frame"]["width"] == 420 and not s["dom"]["collapsed"])
        assert restored["frame"] == placed["frame"]
        self.command("click", selector="#banner-button")
        wait(lambda: self.actions() == [{"action":"stop", "target":None}])
        self.command("click", selector="#banner-minimize")
        self.snapshot("native collapsed Stop remains reachable", lambda s: s["dom"]["collapsed"])
        self.command("click", selector="#banner-stop")
        wait(lambda: self.actions() == [{"action":"stop", "target":None}] * 2)
        self.status(environment=True, attached=True)
        self.snapshot("new carried session expands native window", lambda s: s["frame"]["height"] == 90 and not s["dom"]["collapsed"])
        self.command("click", selector="#banner-minimize")
        self.snapshot("native environment collapse", lambda s: s["frame"]["width"] == 360 and s["frame"]["height"] == 44)
        self.command("click", selector="#banner-restore")
        self.snapshot("native environment restore", lambda s: s["frame"]["width"] == 420 and s["frame"]["height"] == 90)
        self.command("click", selector='#banner-more [data-action="stop"][data-target="ba000001"]')
        wait(lambda: len(self.actions()) == 3 and self.actions()[-1] == {"action":"stop", "target":"ba000001"})
        self.command("click", selector="#banner-minimize")
        small = self.snapshot("native before edge expansion", lambda s: s["frame"]["width"] == 360)
        screen = small["screen"]
        self.command("move", x=screen["x"] + screen["width"] - 360, y=screen["y"] + screen["height"] - 44)
        self.command("click", selector="#banner-restore")
        edge = self.snapshot("native edge expansion clamped on screen", lambda s: s["frame"]["width"] == 420)
        frame = edge["frame"]
        assert frame["x"] >= screen["x"] and frame["y"] >= screen["y"]
        assert frame["x"] + frame["width"] <= screen["x"] + screen["width"]
        assert frame["y"] + frame["height"] <= screen["y"] + screen["height"]
        self.status(in_use=False)
        self.snapshot("native hides after session end", lambda s: not s["visible"])
        focus = self.command("focus_sentinel")
        assert focus["key_title"] == "Owned banner focus sentinel"
        self.status()
        shown = self.snapshot("native reappears for a new session", lambda s: s["visible"])
        assert not shown["key"] and shown["key_title"] == "Owned banner focus sentinel"
        time.sleep(10.2)
        self.snapshot("native hides after ten-second deadline", lambda s: not s["visible"])
        print("PASS expanded/collapsed/carried Stop: fake action sink received exactly three expected targets", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interactive", action="store_true")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--work", type=Path, default=ROOT / "target/desktop/banner-native-e2e")
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    project = work / "cargo"
    project.mkdir(exist_ok=True)
    manifest = f'''[package]
name = "extend-banner-fixture"
version = "0.0.0"
edition = "2024"
[workspace]
[[bin]]
name = "extend-banner-fixture"
path = {json.dumps(str(HERE / "banner-harness.rs"))}
[dependencies]
extend-agent = {{ path = {json.dumps(str(ROOT / "crates/extend-agent"))} }}
tokio = {{ version = "1.53.1", features = ["full"] }}
tokio-util = "0.7.16"
serde_json = "1.0.145"
'''
    (project / "Cargo.toml").write_text(manifest)
    app = work / "Extend Banner Fixture.app/Contents"
    (app / "MacOS").mkdir(parents=True, exist_ok=True)
    binary = app / "MacOS/extend-banner-fixture"
    library = work / "banner-probe.dylib"
    if not args.skip_build:
        shutil.copyfile(ROOT / "Cargo.lock", project / "Cargo.lock")
        with (work / "build.log").open("w") as log:
            subprocess.run(["cargo", "build", "--manifest-path", str(project / "Cargo.toml"), "--offline", "--target-dir", str(ROOT / "target")], stdout=log, stderr=subprocess.STDOUT, check=True)
            shutil.copy2(ROOT / "target/debug/extend-banner-fixture", binary)
            constructor = work / "probe-init.c"
            constructor.write_text("extern void start_banner_probe(void);\n__attribute__((constructor)) static void initialize(void) { start_banner_probe(); }\n")
            subprocess.run(["xcrun", "clang", "-c", str(constructor), "-o", str(work / "probe-init.o")], stdout=log, stderr=subprocess.STDOUT, check=True)
            subprocess.run(["xcrun", "swiftc", "-emit-library", str(HERE / "banner-probe.swift"), str(work / "probe-init.o"), "-o", str(library)], stdout=log, stderr=subprocess.STDOUT, check=True)
    with (app / "Info.plist").open("wb") as output:
        plistlib.dump({"CFBundleIdentifier":"com.teamofsilicons.extend.bannerfixture", "CFBundleExecutable":binary.name, "CFBundleName":"Extend Banner Fixture", "CFBundlePackageType":"APPL", "CFBundleVersion":"1", "LSUIElement":True}, output)
    for name in ["quit", "status.json", "actions.jsonl", "probe-command.json", "probe-response.json"]:
        (work / name).unlink(missing_ok=True)
    fixture = Fixture(work)
    with (work / "app.log").open("w") as log:
        fixture.process = subprocess.Popen([str(binary)], env={**os.environ, "EXTEND_BANNER_FIXTURE":str(work), "DYLD_INSERT_LIBRARIES":str(library)}, stdout=log, stderr=subprocess.STDOUT)
        write(work / "process.json", {"pid":fixture.process.pid, "app":str(app.parent)})
        try:
            fixture.checks()
            if args.interactive:
                fixture.command("hide_sentinel")
                fixture.status(takeover=True)
                fixture.command("move", x=160, y=220)
                print(f"Interactive owned fixture: {app.parent}; control directory: {work}", flush=True)
                while fixture.process.poll() is None:
                    time.sleep(.25)
        finally:
            (work / "quit").touch()
            try:
                fixture.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                fixture.process.terminate()
                fixture.process.wait(timeout=5)


if __name__ == "__main__":
    main()
