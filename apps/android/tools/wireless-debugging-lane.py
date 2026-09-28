#!/usr/bin/env python3
"""TLS lane for Android debugging on a dedicated emulator (never a physical device).

Pairs the installed debug app with Wireless debugging through Android's own pairing-code dialog,
then checks, in a new app process, that discovery finds the paired device's service, that the
connection uses TLS and that the peer proves it is Android's shell.

Needs: the debug app and its instrumentation APK installed, Wireless debugging switched on
(`adb shell settings put global adb_wifi_enabled 1`), and the emulator's screen unlocked.

    python3 tools/wireless-debugging-lane.py --serial emulator-5580 [--keep-connected]

The serial is required: the lane never picks a device by itself.
"""
import argparse
import os
import re
import subprocess
import sys
import time

parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--serial", required=True, help="the dedicated emulator, for example emulator-5580")
parser.add_argument("--keep-connected", action="store_true", help="leave the app's debugging connection on afterwards")
args = parser.parse_args()
RUNNER = "com.teamofsilicons.extend.test/androidx.test.runner.AndroidJUnitRunner"


def adb(*command):
    return subprocess.run(["adb", "-s", args.serial, *command], capture_output=True, text=True).stdout


if adb("shell", "getprop", "ro.kernel.qemu").strip() != "1":
    sys.exit("This lane pairs Android debugging and runs only on a dedicated emulator.")


def screen():
    adb("shell", "uiautomator", "dump", "/sdcard/extend-lane.xml")
    return adb("shell", "cat", "/sdcard/extend-lane.xml")


def find(xml, pattern):
    for node in re.findall(r"<node [^>]*>", xml):
        text = re.search(r' text="([^"]*)"', node).group(1)
        if re.fullmatch(pattern, text):
            b = list(map(int, re.findall(r"\d+", re.search(r'bounds="([^"]*)"', node).group(1))))
            return text, ((b[0] + b[2]) // 2, (b[1] + b[3]) // 2)
    return None


def tap(point):
    adb("shell", "input", "tap", str(point[0]), str(point[1]))
    time.sleep(1.5)


def instrument(method, *extras):
    command = ["shell", "am", "instrument", "-w", "-e", "class", f"com.teamofsilicons.extend.LocalAdbTest#{method}", *extras, RUNNER]
    out = adb(*command)
    print(out.strip().splitlines()[-1] if out.strip() else "(no output)")
    if "OK (1 test)" not in out:
        sys.exit(f"{method} failed:\n{out}")


adb("shell", "am", "start", "-a", "android.settings.APPLICATION_DEVELOPMENT_SETTINGS")
time.sleep(2)
for _ in range(15):
    entry = find(screen(), "Wireless debugging")
    if entry:
        tap(entry[1])
        break
    adb("shell", "input", "swipe", "540", "1800", "540", "700", "300")
    time.sleep(2)
else:
    sys.exit("Developer options has no Wireless debugging entry.")
entry = find(screen(), "Pair device with pairing code")
if not entry:
    sys.exit("Wireless debugging is off: switch it on, then run this again.")
tap(entry[1])
dialog = screen()
code, address = find(dialog, r"\d{6}"), find(dialog, r"[\d.]+:\d+")
if not code or not address:
    sys.exit("Could not read the pairing code dialog.")
port = address[0].rsplit(":", 1)[1]
print(f"Pairing with port {port}")
instrument("wirelessPairing", "-e", "adb_pairing_port", port, "-e", "adb_pairing_code", code[0], "-e", "keep_connected", "true")
adb("shell", "input", "keyevent", "KEYCODE_BACK")
adb("shell", "am", "force-stop", "com.teamofsilicons.extend")
extras = ["-e", "adb_tls", "true"] + (["-e", "keep_connected", "true"] if args.keep_connected else [])
instrument("wirelessReconnect", *extras)
print("PASS TLS pairing, discovery of the paired service, and the shell proof")
