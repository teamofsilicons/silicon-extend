#!/usr/bin/env python3
"""Round-trip a native TV screenshot through real Briefcase and display it by Extend file ID.

Opt-in only: the named emulator must already run a debug Extend APK with accessibility enabled.
This replaces its pairing with a disposable real-IAM/Briefcase fixture. It never targets a physical
Android device, installs an APK, changes permissions, or stops another emulator/service. The fixture
is removed even on failure; the selected disposable app is left unpaired at the end.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import time
import uuid
import xml.etree.ElementTree as ET
import zlib

import realiam as fx

PKG = "com.teamofsilicons.extend"
COLORS = [(220, 32, 32), (32, 190, 70), (32, 70, 220), (230, 205, 32)]


def test_card(path):
    width, height = 640, 360
    raw = b"".join(b"\x00" + b"".join(bytes(COLORS[(y >= height // 2) * 2 + (x >= width // 2)])
                                      for x in range(width)) for y in range(height))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff)

    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True, help="explicit dedicated emulator-N serial")
    parser.add_argument("--avd", required=True, help="expected AVD name, checked before any app mutation")
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--adb", default=os.environ.get("ADB", "adb"))
    args = parser.parse_args()
    if not re.fullmatch(r"emulator-[0-9]+", args.serial):
        parser.error("Select an emulator serial; physical devices are refused")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)

    def adb(*words, binary=False, check=True):
        result = subprocess.run([args.adb, "-s", args.serial, *words], capture_output=True,
                                timeout=45, check=check)
        return result.stdout if binary else result.stdout.decode(errors="replace").strip()

    if adb("shell", "getprop", "ro.kernel.qemu") != "1":
        parser.error("The selected serial is not an emulator")
    avd = adb("shell", "getprop", "ro.boot.qemu.avd_name") or adb("shell", "getprop", "ro.kernel.qemu.avd_name")
    if avd != args.avd:
        parser.error(f"The selected emulator is {avd!r}, not the explicitly authorized AVD {args.avd!r}")
    if not adb("shell", "run-as", PKG, "pwd", check=False).startswith("/data/"):
        parser.error("Install the debug APK on the dedicated emulator first")
    if PKG + "/" + PKG + ".a11y.ExtendAccessibilityService" not in adb("shell", "settings", "get", "secure", "enabled_accessibility_services"):
        parser.error("Enable accessibility on the dedicated emulator first")
    if (fx.STATE_DIR / "state.json").exists():
        parser.error("A real-IAM fixture already exists; finish it before this isolated lane")
    if fx.run(["docker", "network", "inspect", fx.NAME], check=False).returncode == 0:
        parser.error("The fixture network already exists; refusing to take ownership")
    if fx.psql(f"SELECT datname FROM pg_database WHERE datname={fx.q(fx.EXTEND_DB)}", container=fx.EXTEND_DB_CONTAINER,
               user="extend", database="extend").strip():
        parser.error("The fixture database already exists; refusing to replace it")

    original_url = None
    config = adb("shell", "run-as", PKG, "cat", "shared_prefs/extend_config.xml", check=False)
    if config:
        original_url = next((e.text for e in ET.fromstring(config) if e.get("name") == "service_url"), None)
    started = time.monotonic()
    session = None
    c = None
    report = {"serial": args.serial, "avd": avd, "passed": [], "failed": [], "native": True}

    def passed(name, **details):
        report["passed"].append({"check": name, **details})
        print(f"PASS {name}", flush=True)

    def pixels(label, expect_card=True):
        # Android screencap's raw RGBA output avoids a host imaging dependency. Persist the PNG too.
        raw = adb("exec-out", "screencap", binary=True)
        width, height, fmt = struct.unpack_from("<III", raw)
        assert 0 < width <= 4096 and 0 < height <= 4096 and fmt in (1, 2), (width, height, fmt)
        offset = len(raw) - width * height * 4
        assert offset in (12, 16), f"Unexpected Android raw screenshot layout: {len(raw)} bytes, offset {offset}"
        observed = []
        matches = []
        for index, color in enumerate(COLORS):
            x, y = width * (1 if index % 2 == 0 else 3) // 4, height * (1 if index < 2 else 3) // 4
            rgb = tuple(raw[offset + 4 * (y * width + x):offset + 4 * (y * width + x) + 3])
            observed.append(rgb)
            matches.append(max(abs(a - b) for a, b in zip(rgb, color)) <= 3)
        assert all(matches) == expect_card, f"{label}: quadrants {observed}; expected test card={expect_card}"
        (out / f"{label}.png").write_bytes(adb("exec-out", "screencap", "-p", binary=True))
        passed(label, width=width, height=height, quadrants=observed, test_card=expect_card)

    try:
        fx.up(argparse.Namespace(briefcase=True, ting=False))
        state = fx.load()
        c = fx.Checks(state)
        for who in ("alice", "chef", "sous"):
            c.extend(who, "login", c.slt(who))
        # Project the Carbon through the real Briefcase login before requesting its read/update grant.
        token = fx.briefcase_token(c, state, "alice")
        fx.http("GET", state["briefcase"]["url"] + "/api/v1/entries", token=token,
                headers={"X-Org-ID": "acme"}, expected=(200,))
        service = f"http://10.0.2.2:{fx.EXTEND_PORT}"
        adb("shell", "am", "start", "-n", PKG + "/.ui.MainActivity", "--es", "service_url", service,
            "--ez", "forget_pair", "true", "--ez", "force_tv", "true")
        deadline = time.monotonic() + 30
        code = None
        while time.monotonic() < deadline:
            adb("shell", "uiautomator", "dump", "/sdcard/extend-native-display-ui.xml", check=False)
            xml = adb("shell", "cat", "/sdcard/extend-native-display-ui.xml", check=False)
            match = re.search(r'text="([0-9A-F]{3}) ([0-9A-F]{3})"', xml)
            if match:
                code = "".join(match.groups())
                break
            time.sleep(0.5)
        assert code, "The native pairing screen never showed a pairing code"
        _, text, _ = c.extend("alice", "--json", "device", "pair", code, "--name", "Native Briefcase TV")
        device = json.loads(text)["device_id"]
        report["device_id"] = device
        for who in ("chef", "sous"):
            c.extend("alice", "device", "access", "grant", device, f"si:{who}")
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            view = fx.extend_api(c, "alice", "GET", f"/api/v1/devices/{device}")
            if view.get("online") and "display" in view.get("capabilities", []):
                break
            time.sleep(0.2)
        else:
            raise AssertionError(f"Native TV did not become display-capable: {view}")
        c.extend("alice", "device", "banner", device, "off")
        _, text, _ = c.extend("chef", "session", "new", device, "--connect")
        session = text.strip()
        report["session_id"] = session
        card = out / "test-card.png"
        test_card(card)
        _, text, _ = c.extend("chef", "--json", "display", "show", "--image", str(card))
        report["initial_display"] = json.loads(text)
        pixels("initial-native-display")
        f = c.screenshot_file("chef")
        report["stored_file"] = f
        entry = fx.bc_get(c, state, "alice", f"/api/v1/entries/{f['file_id']}")[1]
        assert entry.get("owner", {}).get("id") == "si:chef" and entry.get("origin_app_id") == "extend", entry
        assert f.get("shared_with") == "c:alice", f
        status, content, _ = fx.raw_get(state["briefcase"]["url"] + f"/api/v1/entries/{f['file_id']}/content",
                                        token, {"X-Org-ID": "acme"})
        assert status == 200 and content.startswith(b"\x89PNG") and len(content) == f["size_bytes"]
        (out / "briefcase-screenshot.png").write_bytes(content)
        passed("native screenshot stored in real Briefcase as si:chef", file_id=f["file_id"],
               bytes=len(content), sha256=hashlib.sha256(content).hexdigest())
        # Exercise every public stored-file form; each crosses Briefcase OBO -> service attachment -> TV decode.
        for label, reference in (("file-id", "file:" + f["file_id"]), ("bare-id", f["file_id"]), ("private-url", f["url"])):
            c.extend("chef", "display", "clear")
            c.extend("chef", "display", "show", "--text", "Before Briefcase replay: " + label)
            pixels("before-" + label, expect_card=False)
            _, text, _ = c.extend("chef", "--json", "display", "show", "--image", reference)
            report[label + "_result"] = json.loads(text)
            pixels("stored-" + label)
        missing = "file:" + str(uuid.uuid4())
        rc, text, err = c.extend("chef", "--json", "display", "show", "--image", missing, check=False)
        assert rc != 0 and "file_not_found" in text + err, (rc, text, err)
        passed("missing stored file returns file_not_found", exit_code=rc)
        damaged = out / "damaged.png"
        damaged.write_bytes(b"This is not a PNG image.")
        rc, text, err = c.extend("chef", "--json", "display", "show", "--image", str(damaged), check=False)
        assert rc != 0 and "action_failed" in text + err and "damaged" in (text + err).lower(), (rc, text, err)
        report["damaged_result"] = {"exit_code": rc, "stdout": text, "stderr": err}
        passed("damaged image returns native action_failed", exit_code=rc)
        # A failure must not poison the next image command.
        c.extend("chef", "display", "show", "--image", "file:" + f["file_id"])
        pixels("recovered-after-damaged")
        c.extend("chef", "session", "end", session)
        session = None
        # File ownership must still apply to a different Silicon on the same native device.
        _, text, _ = c.extend("sous", "session", "new", device, "--connect")
        session = text.strip()
        rc, text, err = c.extend("sous", "--json", "display", "show", "--image", "file:" + f["file_id"], check=False)
        assert rc != 0 and "file_not_found" in text + err, (rc, text, err)
        passed("another Silicon cannot display si:chef's private Briefcase file", exit_code=rc)
        c.extend("sous", "session", "end", session)
        session = None
    except BaseException as error:
        report["failed"].append({"error": type(error).__name__, "detail": str(error)})
        raise
    finally:
        report["seconds"] = round(time.monotonic() - started, 2)
        (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        try:
            if c is not None:
                for who in ("chef", "sous"):
                    if session:
                        c.extend(who, "session", "end", session, check=False)
                (out / "logcat.txt").write_text(adb("logcat", "-d", "-s", "SiliconExtend", "ActivityManager", check=False))
                (out / "memory.txt").write_text(adb("shell", "dumpsys", "meminfo", PKG, check=False))
                # This AVD is explicitly disposable; forget fixture credentials before its backend disappears.
                adb("shell", "am", "start", "-n", PKG + "/.ui.MainActivity", "--ez", "forget_pair", "true",
                    *(["--es", "service_url", original_url] if original_url else []), check=False)
        finally:
            if (fx.STATE_DIR / "state.json").exists():
                state = fx.load()
                if (fx.STATE_DIR / "extend.log").exists():
                    (out / "extend.log").write_bytes((fx.STATE_DIR / "extend.log").read_bytes())
                if f"{fx.NAME}-briefcase" in state.get("containers", []):
                    logs = fx.run(["docker", "logs", f"{fx.NAME}-briefcase"], check=False)
                    (out / "briefcase.log").write_bytes(logs.stdout + logs.stderr)
                fx.down()
    print(f"{len(report['passed'])} passed; report {out / 'report.json'}", flush=True)


if __name__ == "__main__":
    main()
