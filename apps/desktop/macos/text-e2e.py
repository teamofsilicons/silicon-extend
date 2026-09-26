#!/usr/bin/env python3
"""Exercise native text entry against an isolated AppKit fixture on this Mac.

Build the signed app first and grant it Accessibility in System Settings. The test
opens a temporary window; it never enters text into any existing user document.
"""
import argparse
import json
import pathlib
import plistlib
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--helper", type=pathlib.Path, default=ROOT / "target/desktop/macos/Silicon Bridge.app/Contents/MacOS/agent-device-macos-helper")
parser.add_argument("--bridge", type=pathlib.Path, help="Also exercise the packaged Bridge driver and selector dispatch")
args = parser.parse_args()
BUNDLE = "com.teamofsilicons.bridge.textfixture"


def until(check, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError("fixture did not reach the expected state")


fixture_root = ROOT / "target/desktop"
fixture_root.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="bridge-text-e2e-", dir=fixture_root) as directory:
    work = pathlib.Path(directory)
    app = work / "Bridge Text Fixture.app/Contents"
    (app / "MacOS").mkdir(parents=True)
    binary = app / "MacOS/fixture"
    subprocess.run(["xcrun", "swiftc", str(ROOT / "apps/desktop/macos/text-fixture.swift"), "-o", str(binary)], check=True)
    with (app / "Info.plist").open("wb") as file:
        plistlib.dump({"CFBundleIdentifier": BUNDLE, "CFBundleExecutable": "fixture", "CFBundlePackageType": "APPL", "CFBundleName": "Bridge Text Fixture", "CFBundleVersion": "1", "CFBundleShortVersionString": "1.0", "CFBundleInfoDictionaryVersion": "6.0"}, file)
    register = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
    subprocess.run([register, "-f", "-v", str(app.parent)], check=True)
    state = work / "state.json"
    fixture = subprocess.Popen([str(binary), str(state)], stdout=subprocess.DEVNULL)

    def values():
        return json.loads(state.read_text())

    def request(text, replace=True, field=0, **extra):
        item = values()[field]
        return {"text": text, "replace": replace, "bundleId": BUNDLE, "x": item["x"], "y": item["y"], **extra}

    def send(payload, success=True):
        result = subprocess.run([str(args.helper), "text"], input=json.dumps(payload), text=True, capture_output=True, timeout=15)
        response = json.loads(result.stdout)
        assert (result.returncode == 0 and response.get("ok")) == success, (response, values())
        return response

    try:
        until(state.exists)
        until(lambda: values()[0]["value"] == "first value")
        # AppKit may position a new window after activation. Capture settled coordinates.
        previous = None
        stable_since = time.monotonic()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            positions = [(v["x"], v["y"]) for v in values()]
            if positions != previous:
                previous, stable_since = positions, time.monotonic()
            elif time.monotonic() - stable_since > .5:
                break
            time.sleep(.05)
        else:
            raise AssertionError("fixture window did not settle")
        send(request("Bridge 👋 café"))
        until(lambda: values()[0]["value"] == "Bridge 👋 café")
        assert values()[1]["value"] == "untouched"
        send(request(" + appended", replace=False))
        until(lambda: values()[0]["value"] == "Bridge 👋 café + appended")
        send(request(""))
        until(lambda: values()[0]["value"] == "")
        send(request("  whitespace  "))
        until(lambda: values()[0]["value"] == "  whitespace  ")
        for exact in ['"quotes" -- dashes  ', "a" * 19 + "  spaces", "👩🏽‍💻 café नमस्ते " * 8]:
            send(request(exact))
            until(lambda: values()[0]["value"] == exact)
        send(request(""))
        send(request("second field", field=1))
        assert values()[0]["value"] == ""
        send(request("", focusOnly=True))
        send({"text": "focused", "replace": False, "bundleId": BUNDLE})
        until(lambda: values()[0]["value"] == "focused")
        before = [v["value"] for v in values()]
        send(request("must not type", x=1, y=1), success=False)
        assert [v["value"] for v in values()] == before
        send(request("secret-fixture-value", field=2))
        until(lambda: values()[2]["value"] == "secret-fixture-value")
        send(request("replacement", field=2))
        until(lambda: values()[2]["value"] == "replacement")
        print("PASS Unicode fill, append, empty clear, focus, field isolation and secure input")

        slow = subprocess.Popen([str(args.helper), "text"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            slow.stdin.write(json.dumps(request("x" * 500, delayMs=50)))
            slow.stdin.close()
            until(lambda: values()[0]["value"].startswith("x"))
            slow.terminate()
            slow.wait(timeout=3)
            time.sleep(.2)
            stopped = values()[0]["value"]
            time.sleep(.2)
            assert values()[0]["value"] == stopped and len(stopped) < 500
            send(request("after cancellation"))
            print("PASS cancellation stops input and releases keys")
        finally:
            if slow.poll() is None:
                slow.kill()
                slow.wait()

        if args.bridge:
            home = work / "bridge-home"
            prefix = [str(args.bridge), "--home", str(home), "exec", "--session", "abc"]

            def bridge(*command):
                result = subprocess.run([*prefix, *command], capture_output=True, text=True, timeout=40)
                response = json.loads(result.stdout)
                assert result.returncode == 0 and response.get("ok"), response
                return response

            opened = False
            try:
                bridge("open", "--surface", "frontmost-app")
                opened = True
                bridge("snapshot", "-i")
                bridge("fill", "id=bridge-field-0", "Bridge driver 👋  ")
                until(lambda: values()[0]["value"] == "Bridge driver 👋  ")
                bridge("type", "+ appended")
                until(lambda: values()[0]["value"] == "Bridge driver 👋  + appended")
                print("PASS packaged Bridge driver: open, snapshot, selector fill and type")
            finally:
                try:
                    if opened:
                        bridge("--end-session", "close")
                finally:
                    contents = args.bridge.parent.parent
                    subprocess.run([str(contents / "Resources/node/bin/node"), str(contents / "Resources/agent-device/bin/agent-device.mjs"), "daemon", "stop", "--state-dir", str(home / ".bridge-agent/agent-device"), "--clean"], check=True, timeout=30)
    finally:
        fixture.terminate()
        try:
            fixture.wait(timeout=3)
        except subprocess.TimeoutExpired:
            fixture.kill()
            fixture.wait()
        subprocess.run([register, "-u", str(app.parent)], check=True)
