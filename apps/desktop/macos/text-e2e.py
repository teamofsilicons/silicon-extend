#!/usr/bin/env python3
"""Exercise native text entry against an isolated AppKit fixture on this Mac.

Build the signed app first and grant it Accessibility in System Settings. The test
opens a temporary window; it never enters text into any existing user document.
"""
import argparse
import json
import os
import pathlib
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--helper", type=pathlib.Path, default=ROOT / "target/desktop/macos/Silicon Bridge.app/Contents/MacOS/agent-device-macos-helper")
parser.add_argument("--bridge", type=pathlib.Path, help="Also exercise the packaged Bridge driver and selector dispatch")
parser.add_argument("--record", action="store_true", help="Also exercise native and packaged screen recording against this fixture")
parser.add_argument("--record-only", action="store_true", help="Exercise recording of an animated fixture without keyboard or mouse input")
parser.add_argument("--record-duration-limit", action="store_true", help="Run the real public recording command until its default 30-minute cap")
parser.add_argument("--record-stress", action="store_true", help="Also verify a reduced file cap and abrupt recorder-owner exit through the GUI app")
parser.add_argument("--device", help="Run capture through a GUI Bridge app paired to the local test service")
parser.add_argument("--artifacts", type=pathlib.Path, help="Keep the downloaded recording and a decoded frame for visual verification")
parser.add_argument("--service-url", default="http://127.0.0.1:8480")
args = parser.parse_args()
if args.record_stress or args.record_duration_limit:
    if not args.device:
        parser.error("recording stress checks require --device for GUI-owned permissions")
    args.record_only = True
if args.record_only:
    args.record = True
BUNDLE = f"com.teamofsilicons.bridge.textfixture.{uuid.uuid4().hex}"


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
    fixture = subprocess.Popen([str(binary), str(state), *( ["--animate"] if args.record_only else []), *( ["--noise"] if args.record_stress else [])], stdout=subprocess.DEVNULL)
    peer = None
    peer_app = None
    remote_session = None
    remote_env = {**os.environ, "BRIDGE_API_URL": args.service_url, "SILICON_HOME": str(work / "cli-home"), "BRIDGE_TELEMETRY": "off"}

    def remote_cli(*command):
        result = subprocess.run([str(ROOT / "target/debug/bridge"), *command], env=remote_env, capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, (command, result.stdout, result.stderr)
        return result.stdout

    def remote(*command):
        response = json.loads(remote_cli("--timeout", "90000", *command, "--json"))["data"]["result"]
        assert response["ok"], response
        return response

    def values():
        return json.loads(state.read_text())

    def request(text, replace=True, field=0, **extra):
        item = values()[field]
        return {"text": text, "replace": replace, "bundleId": BUNDLE, "x": item["x"], "y": item["y"], **extra}

    def send(payload, success=True):
        result = subprocess.run([str(args.helper), "text"], input=json.dumps(payload), text=True, capture_output=True, timeout=15)
        response = json.loads(result.stdout)
        assert (result.returncode == 0 and response.get("ok")) == success, (response, payload, values())
        return response

    try:
        until(state.exists)
        if not args.record_only:
            until(lambda: values()[0]["value"] == "first value")
        if not args.record_only:
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

        def video_info(path):
            result = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height:format=duration", "-of", "json", str(path)], capture_output=True, text=True, check=True)
            info = json.loads(result.stdout)
            assert info["streams"][0]["codec_name"] == "h264", info
            assert float(info["format"]["duration"]) >= .5, info
            subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "null", "-"], check=True, capture_output=True)
            return info

        if args.record:
            if args.device:
                remote_cli("login", "si:chef")
                remote_session = remote_cli("session", "new", args.device, "--connect").strip()
            for mode in ["manual", "duration-limit", *( ["size-limit", "owner-exited"] if args.record_stress else [])]:
                assert fixture.poll() is None, "fixture exited before recording"
                if not args.record_only:
                    send(request("native recording " + mode))
                output, status = work / f"{mode}.mp4", work / f"{mode}.json"
                command = [str(args.helper), "record", "--out", str(output), "--status", str(status), "--bundle-id", BUNDLE, "--fps", "12"]
                if mode == "duration-limit":
                    command += ["--max-duration-ms", "1500"]
                if mode == "size-limit":
                    command += ["--max-bytes", str(16 * 1024 * 1024), "--max-duration-ms", "60000"]
                if mode == "owner-exited":
                    command += ["--max-duration-ms", "10000"]
                if args.device:
                    if mode == "owner-exited":
                        owner = work / "recorder-owner.py"
                        owner.write_text("""import json, os, pathlib, subprocess, sys, time
status = pathlib.Path(sys.argv[1])
child = subprocess.Popen(sys.argv[2:])
deadline = time.monotonic() + 8
while time.monotonic() < deadline:
    if child.poll() is not None:
        raise RuntimeError('recorder exited before readiness')
    if status.exists() and json.loads(status.read_text()).get('state') == 'recording':
        time.sleep(1)
        os._exit(0)  # abrupt owner loss: no wait, signal or graceful cleanup
    time.sleep(.05)
child.terminate()
child.wait(timeout=3)
raise RuntimeError('recorder never became ready')
""")
                        command = [sys.executable, str(owner), str(status), *command]
                    script = shlex.join(command)
                    if mode == "manual":
                        script += ' & recorder_pid=$!; sleep 2; kill -TERM "$recorder_pid"; wait "$recorder_pid"'
                    remote("terminal", "run", script)
                    until(lambda: status.exists() and json.loads(status.read_text()).get("state") in ["completed", "failed"], timeout=12)
                    complete = json.loads(status.read_text())
                    assert complete["state"] == "completed", complete
                    assert complete["reason"] == ("stopped" if mode == "manual" else mode), complete
                    info = video_info(output)
                    if mode == "size-limit":
                        assert output.stat().st_size <= 16 * 1024 * 1024, output.stat().st_size
                    if mode == "owner-exited":
                        assert float(info["format"]["duration"]) < 8, info
                    print(f"PASS GUI-owned native recording: {mode}, valid H.264 MP4, {output.stat().st_size} bytes, {info['format']['duration']} seconds", flush=True)
                    continue
                recorder = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                try:
                    def ready():
                        if recorder.poll() is not None:
                            raise AssertionError(recorder.communicate())
                        return status.exists() and json.loads(status.read_text())["state"] == "recording"
                    until(ready, timeout=15)
                    send(request("screen recording fixture"))
                    if mode == "manual":
                        time.sleep(1)
                        recorder.terminate()
                    stdout, stderr = recorder.communicate(timeout=10)
                    assert recorder.returncode == 0, (stdout, stderr)
                    complete = json.loads(status.read_text())
                    assert complete["state"] == "completed", complete
                    assert complete["reason"] == ("stopped" if mode == "manual" else mode), complete
                    video_info(output)
                    print(f"PASS native app-only recording: {mode}, valid H.264 MP4")
                finally:
                    if recorder.poll() is None:
                        recorder.kill()
                        recorder.wait()

            if args.device:
                opened = remote("open", str(app.parent))
                observed = remote("snapshot", "-i")
                print("Service fixture open:", opened.get("output"), "snapshot header:", (observed.get("text") or "").splitlines()[:2], flush=True)
                assert opened["output"].get("appBundleId") == BUNDLE, opened
                # Bring another owned app over the target, then capture without reopening it.
                peer_app = work / "Bridge Focus Peer.app"
                shutil.copytree(app.parent, peer_app)
                peer_bundle = BUNDLE + ".peer"
                peer_plist = peer_app / "Contents/Info.plist"
                metadata = plistlib.loads(peer_plist.read_bytes())
                metadata["CFBundleIdentifier"] = peer_bundle
                metadata["CFBundleName"] = "Bridge Focus Peer"
                peer_plist.write_bytes(plistlib.dumps(metadata))
                subprocess.run([register, "-f", str(peer_app)], check=True)
                peer_state = work / "peer.json"
                peer = subprocess.Popen([str(peer_app / "Contents/MacOS/fixture"), str(peer_state), "--peer"], stdout=subprocess.DEVNULL)
                until(peer_state.exists)
                def frontmost_bundle():
                    status = remote("terminal", "run", shlex.join([str(args.helper), "app", "frontmost"]))
                    return peer_bundle in json.dumps(status)
                until(frontmost_bundle)
                bound = remote("snapshot", "-i")
                serialized = json.dumps(bound)
                assert BUNDLE in serialized and "bridge-field-0" in serialized, bound
                assert "peer-field-0" not in serialized, bound
                screenshot = work / "bound-app.png"
                remote("screenshot", "--out", str(screenshot))
                assert screenshot.is_file(), "bound app screenshot did not download"
                # A backing window can yield a valid, entirely black PNG. Verify actual content.
                pixels = subprocess.run(["ffmpeg", "-v", "error", "-i", str(screenshot), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], capture_output=True, check=True).stdout
                assert pixels and sum(value > 24 for value in pixels) > len(pixels) * .05, "bound screenshot is blank"

                if args.artifacts:
                    args.artifacts.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(screenshot, args.artifacts / "bound-app.png")
                print("PASS app binding after focus switches to an overlapping owned app", flush=True)

                started = remote("record", "start", "fixture", "--scope", "app", "--fps", "12", "--hide-touches")
                print("Service recording start:", started.get("output"), flush=True)
                if not args.record_only:
                    remote("fill", "id=bridge-field-0", "recording through the service")
                time.sleep(1)
                output = work / "service-recording.mp4"
                response = remote("record", "stop", "--out", str(output))
                assert len(response["files"]) == 1, response
                video_info(output)
                if args.record_only:
                    hashes = subprocess.run(["ffmpeg", "-v", "error", "-i", str(output), "-f", "framemd5", "-"], capture_output=True, text=True, check=True).stdout
                    unique_frames = {line.split(",")[-1].strip() for line in hashes.splitlines() if line and not line.startswith("#")}
                    assert len(unique_frames) > 1, "animated fixture recording contains no frame changes"
                if args.artifacts:
                    args.artifacts.mkdir(parents=True, exist_ok=True)
                    saved = args.artifacts / "service-recording.mp4"
                    shutil.copy2(output, saved)
                    subprocess.run(["ffmpeg", "-v", "error", "-y", "-ss", "0.5", "-i", str(saved), "-frames:v", "1", "-update", "1", str(args.artifacts / "recording-frame.png")], check=True)
                print("PASS real-service recording: GUI app, command relay, upload and CLI download")
                if args.record_duration_limit:
                    started = remote("record", "start", "duration-cap", "--scope", "app", "--fps", "1", "--hide-touches")
                    source = pathlib.Path(started["output"]["outPath"])
                    source = source.with_name(source.stem + ".native" + source.suffix)
                    statuses = list(source.parent.glob(source.name + ".*.status.json"))
                    assert len(statuses) == 1, statuses
                    recording_status = statuses[0]
                    identity = json.loads(recording_status.read_text())
                    beginning = time.monotonic()
                    print(f"START full 30-minute cap check: recorder pid={identity['pid']}, session={remote_session}", flush=True)
                    while recording_status.exists() and time.monotonic() - beginning < 1815:
                        time.sleep(30)
                        if not recording_status.exists():
                            break
                        # Keep the session's ordinary idle timeout from ending this cap check.
                        remote("snapshot", "-i")
                        print(f"RUNNING full duration check: {time.monotonic() - beginning:.0f}s, {source.stat().st_size} bytes", flush=True)
                    output = work / "duration-cap.mp4"
                    response = remote("record", "stop", "--out", str(output))
                    assert len(response["files"]) == 1, response
                    info = video_info(output)
                    duration = float(info["format"]["duration"])
                    assert 1799 <= duration <= 1801, info
                    assert output.stat().st_size <= 1024 ** 3, output.stat().st_size
                    if args.artifacts:
                        shutil.copy2(output, args.artifacts / "duration-cap.mp4")
                    print(f"PASS public 30-minute recording cap: {duration}s, {output.stat().st_size} bytes, full decode and download", flush=True)


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
                if args.record:
                    bridge("record", "start", "fixture", "--scope", "app", "--fps", "12", "--hide-touches")
                    bridge("fill", "id=bridge-field-0", "recording through Bridge")
                    time.sleep(1)
                    recorded = bridge("record", "stop")
                    videos = [item for item in recorded["files"] if item["kind"] == "recording"]
                    assert len(videos) == 1, recorded
                    video_info(videos[0]["path"])
                    print("PASS packaged Bridge recording start/stop and playable artifact")
            finally:
                try:
                    if opened:
                        bridge("--end-session", "close")
                finally:
                    contents = args.bridge.parent.parent
                    subprocess.run([str(contents / "Resources/node/bin/node"), str(contents / "Resources/agent-device/bin/agent-device.mjs"), "daemon", "stop", "--state-dir", str(home / ".bridge-agent/agent-device"), "--clean"], check=True, timeout=30)
    finally:
        try:
            if remote_session:
                remote_cli("session", "end", remote_session)
        finally:
            if peer is not None:
                peer.terminate()
                try:
                    peer.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    peer.kill()
                    peer.wait()
            if peer_app is not None:
                subprocess.run([register, "-u", str(peer_app)], check=True)
            fixture.terminate()
            try:
                fixture.wait(timeout=3)
            except subprocess.TimeoutExpired:
                fixture.kill()
                fixture.wait()
            subprocess.run([register, "-u", str(app.parent)], check=True)
