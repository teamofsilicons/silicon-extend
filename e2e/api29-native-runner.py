#!/usr/bin/env python3
"""Manual GitHub Linux job only: exact-source API29 phone or actual TV native tests.

No production credentials, signing key, real service, physical device or release APK.
The existing fake service and instrumentation exercise the unchanged product. This
adapter owns lifecycle and evidence; it never edits the source or relaxes a test.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
import urllib.request
import xml.etree.ElementTree as ET

SOURCE = "2a1dbad43f1997e6f8c7dd38df92e38c6f075a33"
RUNTIME_REFERENCE = "cf43b539c5d9ae91e363172436bb8d1d677d2441"
MULTI_FIXTURE_FILES = tuple("apps/android/tools/fake-service/" + name for name in
                            ("run-emulator-test.sh", "fake_extend.py", "make_test_png.py"))
PKG = "com.teamofsilicons.extend"
ACCESSIBILITY_COMPONENT = PKG + "/" + PKG + ".a11y.ExtendAccessibilityService"
TEST_RUNNER = PKG + ".test/androidx.test.runner.AndroidJUnitRunner"
CASES = (("phone", "default", "x86_64", 8, 5560, 8490, 42),
         ("tv", "android-tv", "x86", 3, 5562, 8491, 35))
TLS_SKIPS = ("wirelessPairing", "wirelessReconnect", "discoveryTriesEveryCandidate")
DISPLAY = ("damagedImageFailsInsteadOfReportingShown", "validImageIsDecodedAndShown",
           "corruptImageReturnsACommandFailure", "aSlowHttpFailureCannotReportSuccessBeforeLoading")
LOCAL = ("anImpostorDaemonIsRefused", "connectionStateNeverBlocks", "realLocalDaemon",
         "outputLargerThanTheHeapIsStreamedNotQueued")
RECORDING = ("nativeDurationLimit", "stillScreenSegmentsKeepTheirWallClockLength",
             "burstThenStillKeepsLaterSegmentsInPlace", "badSegmentsAreLeftOutInsteadOfFailingEveryRetry",
             "rotationContinuesInASecondFile", "anUndeliveredRecordingIsSentAgain", "lowSpaceKeepsTheRecording",
             "aCaptureDiscardedWhileOfflineIsExplained", "aSessionTheDeviceGaveUpOnExplainsItsLostRecording",
             "ownershipChecksFinishWhenTheProcessExitsDuringThem", "recoveryStateLivesOutsideTheCache",
             "beyondNativeLimit")


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def redacted(text):
    text = re.sub(r"\b(?:edc|ees)_[A-Za-z0-9_-]+\b", "[redacted-token]", text)
    text = re.sub(r"(?i)(Extend-Device|Extend-Enrollment|Bearer) [^\s\"',}]+", r"\1 [redacted]", text)
    text = re.sub(r'''(?i)([\"'](?:device_credential|credential|access_token|refresh_token|secret)[\"']\s*:\s*)[\"'][^\"'\n]*[\"']''', r'\1"[redacted]"', text)
    text = re.sub(r"(?i)((?:Received public key|Received connected key message|Logging key):? )[A-Za-z0-9+/=]{80,}", r"\1[redacted-public-key]", text)
    return text


def tv_emulator_lifecycle(text):
    # Emulator debug tags can print raw ADB packets. Export only lifecycle lines;
    # keep packet data, public keys and APK payloads in the disposable runner.
    lifecycle = re.compile(r"Adb (?:connected|closed)|reset connection|host connection|guest connection|"
                           r"connection (?:terminated|closed|reset|accepted)|socket (?:closed|opened|accepted)|"
                           r"Android emulator version|qemu\.dalvik\.vm\.heapsize|Boot completed")
    return redacted("\n".join(line for line in text.splitlines() if lifecycle.search(line)
                    and not re.search(r"(?i)payload|packet|(?:public |logging )key|data[=:]", line)) + "\n")


def sdk_properties(path):
    return dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)


def verify_images(sdk, out, profile):
    evidence = []
    for name, tag, abi, revision, *_ in CASES:
        if name != profile:
            continue
        directory = sdk / "system-images/android-29" / tag / abi
        props = sdk_properties(directory / "source.properties")
        assert props["AndroidVersion.ApiLevel"] == "29", props
        assert props["SystemImage.Abi"] == abi, props
        assert props["SystemImage.TagId"] == tag, props
        assert props["Pkg.Revision"] == str(revision), props
        evidence.append({"case": name, "package": f"system-images;android-29;{tag};{abi}",
                         "properties": props, "system_img_sha256": digest(directory / "system.img")})
    write_json(out / "verified-images.json", evidence)


def owned_environment(out):
    assert os.environ.get("GITHUB_ACTIONS") == "true", "Only a disposable GitHub runner is supported"
    assert platform.system() == "Linux" and platform.machine() == "x86_64"
    run_id = os.environ["GITHUB_RUN_ID"]
    assert run_id.isdigit()
    temp = Path(os.environ["RUNNER_TEMP"]).resolve()
    assert out == temp / "extend-api29-evidence", "Use the fixed owned job evidence directory"
    avds = Path(os.environ["ANDROID_AVD_HOME"]).resolve()
    assert avds == temp / "extend-api29-avds", "Use the fixed owned job AVD directory"
    return run_id, avds


def process_start(pid):
    try:
        # Fields after the final ')' start with field3; starttime is field22.
        return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
    except FileNotFoundError:
        return None


def cleanup_owned(out, run_id, avds):
    path = out / "ownership.json"
    if not path.exists():
        return {"ownership_file_absent": True}
    state = json.loads(path.read_text())
    assert state["run_id"] == run_id and Path(state["avd_home"]) == avds
    results = []
    for child in reversed(state["processes"]):
        pid = child["pid"]
        current = process_start(pid)
        if current is not None and current == child["start_ticks"]:
            assert os.getpgid(pid) == pid, "Owned process must lead its own process group"
            os.killpg(pid, signal.SIGTERM)
            limit = time.monotonic() + 5
            while process_start(pid) == current and time.monotonic() < limit:
                time.sleep(.1)
            if process_start(pid) == current:
                os.killpg(pid, signal.SIGKILL)
                limit = time.monotonic() + 5
                while process_start(pid) == current and time.monotonic() < limit:
                    try:
                        os.waitpid(pid, os.WNOHANG)
                    except ChildProcessError:
                        pass
                    time.sleep(.1)
        results.append({"pid": pid, "original_process_absent": process_start(pid) != child["start_ticks"]})
    # Never remove an AVD outside this job-created directory, and never prune the SDK.
    if state.get("avd_home_created") and avds.exists():
        shutil.rmtree(avds)
    result = {"processes": results, "owned_avd_home_absent": not avds.exists()}
    write_json(out / "cleanup.json", result)
    assert all(item["original_process_absent"] for item in results), "Owned process remains after cleanup"
    assert result["owned_avd_home_absent"], "Owned AVD home remains after cleanup"
    return result


def parse_instrumentation(text, class_name, method):
    # am instrument can exit0 when a test fails. Require the exact test's terminal success,
    # plus the runner's one-test summary; assumptions, crashes and missing tests must fail.
    records, current = [], {}
    for line in text.replace("\r", "").splitlines():
        if line.startswith("INSTRUMENTATION_STATUS: "):
            key, _, value = line.removeprefix("INSTRUMENTATION_STATUS: ").partition("=")
            current[key] = value
        elif line.startswith("INSTRUMENTATION_STATUS_CODE: "):
            records.append((dict(current), int(line.split(":", 1)[1])))
            current = {}
    completed = [(record, code) for record, code in records if code != 1]
    assert len(completed) == 1, completed
    record, code = completed[0]
    assert code == 0 and record.get("class") == class_name and record.get("test") == method, completed
    assert re.search(r"\bOK \(1 test\)", text), text[-2000:]
    assert "INSTRUMENTATION_FAILED" not in text and "FAILURES!!!" not in text


def tcp_listeners(text):
    assert "local_address" in text, "Cannot prove whether guest adbd listens; no speculative bridge"
    listeners = []
    for line in text.splitlines():
        fields = line.split()
        if len(fields) > 3 and re.fullmatch(r"[0-9A-Fa-f]+:[0-9A-Fa-f]{4}", fields[1]):
            if fields[3] == "0A":
                listeners.append(int(fields[1].split(":")[1], 16))
    return listeners


def accessibility_unbound(text, component=ACCESSIBILITY_COMPONENT):
    """Read API29's framework state, not just the asynchronous secure setting."""
    assert "ACCESSIBILITY MANAGER" in text, "Missing accessibility framework dump"
    users = list(re.finditer(r"User state\[attributes:\{id=(\d+), currentUser=(true|false)\b", text))
    current = [(index, match) for index, match in enumerate(users) if match[2] == "true"]
    assert len(current) == 1 and current[0][1][1] == "0", "Expected the owned AVD's current user0"
    index, start = current[0]
    section = text[start.end():users[index+1].start() if index+1 < len(users) else len(text)]
    wanted_package, wanted_class = component.split("/", 1)
    wanted_class = wanted_package + wanted_class if wanted_class.startswith(".") else wanted_class
    present = False
    for label in ("Bound", "Enabled", "Binding"):
        markers = list(re.finditer(r"^\s*" + label + r" services:\{", section, re.M))
        assert len(markers) == 1, f"Missing or ambiguous {label} services in framework dump"
        begin = markers[0].end()
        depth, end = 1, begin
        while end < len(section) and depth:
            depth += (section[end] == "{") - (section[end] == "}")
            end += 1
        assert depth == 0, f"Truncated {label} services in framework dump"
        for package, class_name in re.findall(r"([A-Za-z0-9_.$]+)/([A-Za-z0-9_.$]+)", section[begin:end-1]):
            class_name = package + class_name if class_name.startswith(".") else class_name
            present |= (package, class_name) == (wanted_package, wanted_class)
    return not present


def heap_bytes(value):
    match = re.fullmatch(r"([1-9][0-9]*)([kKmMgG]?)", value)
    assert match, f"Cannot establish runtime heap limit from {value!r}"
    return int(match[1]) * {"": 1, "k": 1024, "m": 1024**2, "g": 1024**3}[match[2].lower()]


def avd_memory(profile):
    assert profile in {"phone", "tv"}, "Unknown owned AVD profile"
    # The official API29 TV image is a non-debuggable user build. The supported
    # -lowram option removes the emulator's normal RAM floor; 768 MiB gives a
    # RAM/4 heap of 192 MiB. Runtime properties and the native test still verify it.
    return ({"hw.ramSize": "768" if profile == "tv" else "2048", "vm.heapSize": "192"},
            ["-lowram"] if profile == "tv" else [])


def inapplicable_test(profile, cls, method):
    if profile == "phone" and cls == "DisplayTest" and method == "corruptImageReturnsACommandFailure":
        return "The display command is unsupported on phones; its command-path decoding failure is tested on actual TV"
    if profile == "tv" and cls == "RecordingTest" and method == "aSessionTheDeviceGaveUpOnExplainsItsLostRecording":
        return "The public record command is unsupported on TV; its session-loss explanation is tested on phone"
    if profile == "tv" and cls == "RecordingTest" and method == "rotationContinuesInASecondFile":
        return "Android TV has a fixed landscape display; phone orientation transition tested in phone case"
    return None


def tv_tcp_trace_command(console_port):
    assert console_port == next(case[4] for case in CASES if case[0] == "tv")
    # IPv4 loopback Ethernet/IP/TCP base headers total54 bytes. Never retain
    # packet payload, TCP options beyond that cap, a PCAP, or promiscuous traffic.
    control = (f"ip and host 127.0.0.1 and tcp port {console_port + 1} and "
               "(tcp[tcpflags] & (tcp-syn|tcp-fin|tcp-rst) != 0)")
    return ["/usr/bin/tcpdump", "-i", "lo", "-p", "-nn", "-tt", "-l", "-s", "54", "-c", "256", control]


class Lane:
    def __init__(self, args, run_id, avds):
        self.root, self.sdk, self.out = args.root.resolve(), args.sdk.resolve(), args.out.resolve()
        self.harness_root = Path(__file__).resolve().parents[1]
        self.run_id, self.avds, self.profile = run_id, avds, args.profile
        self.env = dict(os.environ, ANDROID_SDK_ROOT=str(self.sdk), ANDROID_HOME=str(self.sdk))
        self.env["PATH"] = str(self.sdk / "platform-tools") + os.pathsep + self.env["PATH"]
        self.adb_bin = str(self.sdk / "platform-tools/adb")
        self.avdmanager = str(self.sdk / "cmdline-tools/latest/bin/avdmanager")
        assert not avds.exists(), "Refuse reuse of an existing AVD directory"
        avds.mkdir(mode=0o700)
        self.state = {"run_id": run_id, "avd_home": str(avds), "avd_home_created": True, "processes": []}
        self.handles, self.logs = [], []
        self.report = {"source_head": SOURCE, "runtime_reference": RUNTIME_REFERENCE,
                       "adapter_commit": os.environ.get("GITHUB_SHA"), "cases": [],
                       "qualification": "Native debug APKs on API29 phone and actual Android TV emulators; no signed-upgrade or physical-device claim"}
        self.flush()

    def flush(self):
        write_json(self.out / "ownership.json", self.state)
        write_json(self.out / "report.json", self.report)

    def guard_disk(self, reserve=4):
        free = shutil.disk_usage(self.out).free
        assert free >= reserve * 1024**3, f"Disk floor reached: {free} bytes, need {reserve} GiB"
        return free

    def cmd(self, command, timeout=45, check=True, input=None):
        return subprocess.run([str(x) for x in command], input=input, capture_output=True, text=True,
                              timeout=timeout, check=check, env=self.env, cwd=self.root)

    def adb(self, *command, timeout=45, check=True):
        def record_failure(returncode, stderr):
            if getattr(self, "case", {}).get("name") != "tv":
                return
            if isinstance(stderr, bytes):
                stderr = stderr.decode(errors="replace")
            entry = {"at_unix_s": time.time(), "command": list(command), "returncode": returncode,
                     "stderr": (stderr or "")[-4000:]}
            errors = self.case.setdefault("adb_command_failures", [])
            errors.append(json.loads(redacted(json.dumps(entry))))
            del errors[:-20]
            self.flush()
        try:
            result = self.cmd([self.adb_bin, "-s", self.serial, *command], timeout, check)
        except subprocess.CalledProcessError as error:
            record_failure(error.returncode, error.stderr)
            raise
        except subprocess.TimeoutExpired as error:
            record_failure("timeout", error.stderr)
            raise
        if result.returncode:
            record_failure(result.returncode, result.stderr)
        return result.stdout.strip()

    def tv_adb_key_metadata(self, label):
        if self.case["name"] != "tv":
            return
        entry = {"label": label, "at_unix_s": time.time(), "command": ["shell", "ls", "-ldnZ",
                 "/data/misc/adb", "/data/misc/adb/adb_keys"], "contents_read": False}
        try:
            result = self.cmd([self.adb_bin, "-s", self.serial, *entry["command"]], timeout=5, check=False)
            entry.update(returncode=result.returncode, stdout=result.stdout[-4000:], stderr=result.stderr[-4000:])
        except (OSError, subprocess.SubprocessError) as error:
            entry["diagnostic_error"] = str(error)
        self.case.setdefault("adb_key_metadata", []).append(json.loads(redacted(json.dumps(entry))))
        self.flush()

    def tv_emulator_debug_flags(self):
        if self.case["name"] != "tv":
            return []
        help_text = self.cmd([self.sdk / "emulator/emulator", "-help-debug-tags"]).stdout
        tags = ("adb", "adbserver", "init", "time")
        assert all(re.search(r"(?m)^\s+" + tag + r"\s+", help_text) for tag in tags), "Emulator lacks required diagnostic tags"
        (self.caseout / "emulator-debug-tags.txt").write_text(help_text)
        self.case["emulator_debug"] = {"tags": list(tags), "supported_by_runtime_help": True,
                                       "public_log": "Lifecycle lines only; raw packet payloads excluded"}
        self.flush()
        return ["-debug", ",".join(tags)]

    def start_tv_tcp_trace(self):
        if self.case["name"] != "tv":
            return None
        evidence = {"started_at_unix_s": time.time(), "scope": "Owned IPv4 loopback emulator ADB port only; SYN/FIN/RST headers",
                    "payload_or_pcap": False, "snaplen": 54, "packet_limit": 256}
        self.case["tcp_control_trace"] = evidence
        process = None
        try:
            assert os.environ.get("GITHUB_ACTIONS") == "true" and platform.system() == "Linux"
            assert os.geteuid() != 0, "Capture must run as the normal owned runner process"
            executable = Path("/usr/bin/tcpdump")
            metadata = executable.stat()
            assert not executable.is_symlink() and stat.S_ISREG(metadata.st_mode)
            assert metadata.st_uid == 0 and not metadata.st_mode & 0o022
            capability = self.cmd(["getcap", executable], timeout=5).stdout.strip()
            assert capability == "/usr/bin/tcpdump cap_net_raw=ep", capability
            version = self.cmd([executable, "--version"], timeout=5)
            evidence.update(executable=str(executable), sha256=digest(executable), capability=capability,
                            version=(version.stdout + version.stderr).strip(), command=tv_tcp_trace_command(self.console_port))
            assert re.search(r"tcpdump version \d+\.\d+", evidence["version"])
            log = self.caseout / "tcp-control.log"
            process = self.spawn(evidence["command"], log)
            def listening():
                text = log.read_text(errors="replace")
                return process.poll() is None and "listening on lo" in text and "EN10MB" in text
            self.until("owned passive IPv4 loopback trace readiness", listening, 5)
            evidence["ready"] = True
            return process
        except (AssertionError, OSError, subprocess.SubprocessError, TimeoutError) as error:
            evidence["diagnostic_error"] = str(error)
            self.stop_tv_tcp_trace(process)
            return None
        finally:
            self.flush()

    def stop_tv_tcp_trace(self, process):
        if process is None:
            return
        evidence = self.case["tcp_control_trace"]
        try:
            self.stop(process)
            text = (self.caseout / "tcp-control.log").read_text(errors="replace")
            evidence.update(stopped_at_unix_s=time.time(), stopped=process.poll() is not None, returncode=process.returncode,
                            counters=[line for line in text.splitlines() if re.fullmatch(r"\d+ packets? (?:captured|received by filter|dropped by kernel)", line)],
                            packet_limit_reached=bool(re.search(r"(?m)^256 packets captured$", text)))
        except (OSError, subprocess.SubprocessError) as error:
            evidence["stop_diagnostic_error"] = str(error)
        finally:
            self.flush()

    def spawn(self, command, logfile, extra_env=None):
        log = logfile.open("w"); self.logs.append(log)
        env = dict(self.env, **(extra_env or {}))
        p = subprocess.Popen([str(x) for x in command], cwd=self.root, env=env, stdin=subprocess.DEVNULL,
                             stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        self.handles.append(p)
        self.state["processes"].append({"pid": p.pid, "start_ticks": process_start(p.pid), "command": [str(x) for x in command]})
        self.flush()
        return p

    def stop(self, process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL); process.wait(timeout=5)

    def wait(self, process, timeout, authorize=False):
        end = time.monotonic() + timeout
        next_prompt = 0
        while process.poll() is None:
            self.guard_disk()
            if time.monotonic() >= end:
                self.stop(process)
                raise TimeoutError(f"Owned child exceeded {timeout}s")
            if authorize and time.monotonic() >= next_prompt:
                self.allow_owned_adb_dialog()
                next_prompt = time.monotonic() + 2
            time.sleep(.25)
        assert process.returncode == 0, f"Owned child failed with exit{process.returncode}"

    def until(self, label, predicate, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.guard_disk()
            try:
                value = predicate()
                if value:
                    return value
            except (OSError, subprocess.SubprocessError, ValueError):
                pass
            time.sleep(.5)
        raise TimeoutError(label)

    def allow_owned_adb_dialog(self):
        # Only during the explicit initial local-debugging setup, never concurrently with
        # DisplayTest's UiAutomation. Exact dialog text/package guards precede every tap.
        result = self.cmd([self.adb_bin, "-s", self.serial, "shell", "uiautomator", "dump", "/sdcard/api29-adb-dialog.xml"],
                          timeout=8, check=False)
        if result.returncode:
            return
        raw = self.adb("shell", "cat", "/sdcard/api29-adb-dialog.xml", check=False)
        try:
            nodes = list(ET.fromstring(raw).iter("node"))
        except ET.ParseError:
            return
        titles = [n for n in nodes if n.get("text") == "Allow USB debugging?"]
        if not titles or any(n.get("package") not in {"com.android.systemui", "com.android.settings", "android"} for n in titles):
            return
        def tap(node):
            bounds = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", node.get("bounds", ""))
            assert bounds
            x1, y1, x2, y2 = map(int, bounds.groups())
            self.adb("shell", "input", "tap", str((x1+x2)//2), str((y1+y2)//2))
        for node in nodes:
            if node.get("text") == "Always allow from this computer" and node.get("checked") == "false":
                tap(node)
        buttons = [n for n in nodes if n.get("resource-id") == "android:id/button1" and n.get("text", "").casefold() in {"allow", "ok"}]
        assert len(buttons) == 1, "Do not guess which dialog button authorizes the owned emulator"
        (self.caseout / "owned-adb-authorization.xml").write_text(raw)
        tap(buttons[0])
        self.case["owned_adb_prompt_approved"] = True
        self.flush()

    def instrument(self, cls, method, timeout=150, authorize=False):
        self.reset_instrumentation_accessibility(cls + "-" + method)
        if method == "outputLargerThanTheHeapIsStreamedNotQueued":
            self.verify_heap()
        full = PKG + "." + cls
        logfile = self.caseout / f"{cls}-{method}.log"
        argv = [self.adb_bin, "-s", self.serial, "shell", "am", "instrument", "-w", "-r",
                "-e", "class", full + "#" + method, "-e", "local_daemon", "true", "-e", "service_test", "true",
                "-e", "long_recording", "true", TEST_RUNNER]
        process = self.spawn(argv, logfile)
        self.wait(process, timeout, authorize)
        parse_instrumentation(logfile.read_text(), full, method)
        self.case["instrumentation"].append({"class": cls, "method": method, "result": "passed"})
        self.flush()

    def reset_instrumentation_accessibility(self, label):
        # Android10 instrumentation kills the process without the full package-restarted
        # cleanup. A formerly bound service can stay in the framework's Binding list,
        # where quick null->enabled settings writes cannot rebind it. Explicit force-stop
        # sends that cleanup broadcast; await completion before unchanged test setup.
        # This helper is never called during the separate reconnect recovery checks.
        self.assert_avd()
        prefix = self.caseout / ("accessibility-" + label)
        prefix.with_suffix(".before.txt").write_text(self.adb("shell", "dumpsys", "accessibility", timeout=5))
        self.adb("shell", "am", "force-stop", "--user", "0", PKG)
        def cleared():
            snapshot = self.adb("shell", "dumpsys", "accessibility", timeout=5)
            prefix.with_suffix(".after.txt").write_text(snapshot)
            return accessibility_unbound(snapshot)
        self.until("owned accessibility Bound/Enabled/Binding cleanup before " + label, cleared, 30)

    def verify_heap(self):
        properties = {name: self.adb("shell", "getprop", name) for name in
                      ("dalvik.vm.heapsize", "dalvik.vm.heapgrowthlimit")}
        limits = {"dalvik.vm.heapsize": heap_bytes(properties["dalvik.vm.heapsize"])}
        if properties["dalvik.vm.heapgrowthlimit"]:
            limits["dalvik.vm.heapgrowthlimit"] = heap_bytes(properties["dalvik.vm.heapgrowthlimit"])
        self.case["heap_prerequisite"] = {"properties": properties,
            "max_heap_bytes": limits["dalvik.vm.heapsize"], "limits_bytes": limits,
            "runtime_assertion": "Unchanged native test also requires Runtime.maxMemory <250000000 before transfers"}
        self.flush()
        assert all(value < 250_000_000 for value in limits.values()), "Guest heap would invalidate the native streaming regression"

    def verify_owned_tv_heap(self):
        self.assert_avd()
        evidence = {"strategy": "TV-only supported -lowram with 768 MiB AVD RAM and 192 MiB configured heap; no root or zygote changes",
                    "avd_memory": avd_memory("tv")[0], "emulator_flags": avd_memory("tv")[1]}
        try:
            evidence["image_properties"] = {name: self.adb("shell", "getprop", name) for name in
                ("ro.debuggable", "ro.build.type", "ro.config.low_ram", "ro.kernel.qemu.dalvik.vm.heapsize")}
            evidence["shell_uid"] = self.adb("shell", "id", "-u")
            assert evidence["shell_uid"] == "2000", "Owned TV must use shell UID2000 before APK installation"
            self.verify_heap()
            evidence["properties"] = self.case["heap_prerequisite"]["properties"]
            evidence["verified"] = True
        except BaseException as error:
            evidence["setup_error"] = str(error)
            raise
        finally:
            self.case["heap_setup"] = evidence
            write_json(self.caseout / "heap-setup.json", evidence)
            self.flush()

    def configure_owned_heap(self):
        # Current emulator versions raise vm.heapSize to at least RAM/4. On this
        # disposable debug image, set volatile ART properties while zygote is
        # stopped, then restore shell-UID ADB before installing or exercising the app.
        self.assert_avd()
        evidence = {"debuggable": self.adb("shell", "getprop", "ro.debuggable"),
                    "build_type": self.adb("shell", "getprop", "ro.build.type"),
                    "heap_before": self.adb("shell", "getprop", "dalvik.vm.heapsize")}
        assert evidence["debuggable"] == "1" and evidence["build_type"] in {"userdebug", "eng"}, "Owned image must support debug-only heap setup"
        old_pids = self.adb("shell", "pidof", "zygote64", "zygote", check=False).split()
        assert old_pids and all(pid.isdigit() for pid in old_pids), "Cannot identify running owned zygotes"
        evidence["zygote_pids_before"] = old_pids
        try:
            self.adb("root")
            self.until("owned debug adbd UID0", lambda: self.adb("shell", "id", "-u", timeout=5) == "0", 30)
            self.adb("shell", "stop")
            try:
                self.until("owned zygotes stopped", lambda: not self.adb("shell", "pidof", "zygote64", "zygote", check=False, timeout=5), 30)
                for prop in ("dalvik.vm.heapsize", "dalvik.vm.heapgrowthlimit"):
                    self.adb("shell", "setprop", prop, "192m")
                    assert self.adb("shell", "getprop", prop) == "192m", f"Owned image rejected {prop}"
            finally:
                self.adb("shell", "start")
            def restarted():
                pids = self.adb("shell", "pidof", "zygote64", "zygote", check=False, timeout=5).split()
                if not pids or not all(pid.isdigit() for pid in pids) or set(pids) & set(old_pids):
                    return None
                if self.adb("shell", "am", "get-current-user", timeout=5) != "0":
                    return None
                return pids
            evidence["zygote_pids_after"] = self.until("new owned zygotes and framework user0", restarted, 90)
            self.verify_heap()
            evidence["properties"] = self.case["heap_prerequisite"]["properties"]
            evidence["configured"] = True
        except BaseException as error:
            evidence["setup_error"] = str(error)
            raise
        finally:
            try:
                self.adb("unroot")
                self.until("owned adbd restored to shell UID2000", lambda: self.adb("shell", "id", "-u", timeout=5) == "2000", 30)
                evidence["unrooted_uid"] = "2000"
            except BaseException as error:
                evidence["unroot_error"] = str(error)
                raise
            finally:
                self.case["heap_setup"] = evidence
                write_json(self.caseout / "heap-setup.json", evidence)
                self.flush()

    def assert_avd(self):
        assert self.adb("shell", "getprop", "ro.kernel.qemu") == "1"
        assert self.adb("shell", "getprop", "ro.build.version.sdk") == "29"
        avd = self.adb("emu", "avd", "name").splitlines()[0]
        assert avd == self.avd, (avd, self.avd)

    def transport(self, label):
        """A bridge is permitted only after runtime proof of the API29 pipe-only mechanism.

        No version-gate bypass: the existing reconnect script remains unchanged. This
        adapter repeats the same lifecycle checks but revalidates this exact transport
        after adbd restarts, and labels the result emulator transport rather than real TCP.
        """
        self.assert_avd()
        raw = self.adb("shell", "cat", "/proc/net/tcp", "/proc/net/tcp6")
        (self.caseout / f"{label}-guest-tcp.txt").write_text(raw)
        listeners = tcp_listeners(raw)
        observation = {"label": label, "guest_listeners": listeners, "serial": self.serial, "avd": self.avd}
        previous_bridge = any(item["mode"] == "verified-emulator-pipe-bridge" for item in self.case.get("transport", []))
        reverse_before = self.adb("reverse", "--list")
        own_mapping = ["tcp:5555", f"tcp:{self.console_port + 1}"]
        mapped = any(line.split()[-2:] == own_mapping for line in reverse_before.splitlines())
        if mapped:
            assert previous_bridge, "Unexpected existing port5555 reverse; refuse reuse"
            observation.update(mode="verified-emulator-pipe-bridge", retained_after_restart=True, reverse=reverse_before)
        elif 5555 in listeners:
            observation["mode"] = "guest-tcp-listener"
        else:
            # This is the owned emulator's documented console+1 ADB transport. Require a
            # real ADB protocol response, then the native app must prove shell UID/AVD too.
            port = self.console_port + 1
            payload = b"host::features=shell_v2,cmd\0"
            cnxn = int.from_bytes(b"CNXN", "little")
            packet = struct.pack("<6I", cnxn, 0x01000001, 1024*1024, len(payload), sum(payload), cnxn ^ 0xffffffff) + payload
            with socket.create_connection(("127.0.0.1", port), timeout=4) as stream:
                stream.sendall(packet)
                header = b""
                while len(header) < 24:
                    chunk = stream.recv(24-len(header))
                    assert chunk, "Owned emulator ADB endpoint closed before a protocol response"
                    header += chunk
            command, _, _, length, _, magic = struct.unpack("<6I", header)
            assert command in {int.from_bytes(b"AUTH", "little"), cnxn} and magic == command ^ 0xffffffff and length < 1024*1024
            self.adb("reverse", "tcp:5555", f"tcp:{port}")
            reverse = self.adb("reverse", "--list")
            assert any(line.split()[-2:] == ["tcp:5555", f"tcp:{port}"] for line in reverse.splitlines())
            observation.update(mode="verified-emulator-pipe-bridge", host_adb_port=port,
                               adb_reply=header[:4].decode("ascii"), reverse=reverse)
        self.case.setdefault("transport", []).append(observation)
        self.flush()

    def request(self, path, data=None):
        payload = None if data is None else json.dumps(data).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}/_test/{path}", data=payload,
                                         headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=12) as response:
            return json.load(response)

    def remote(self, args):
        return self.request("command", {"command": "adb", "args": ["shell", *args], "session_id": "a3f", "timeout_ms": 3000})

    def reconnect(self):
        # Same three checks/120s bounds as tools/adb-reconnect-lane.py, with only the
        # evidence-gated transport restoration above in place of its API<=28 flag.
        fake = self.spawn([sys.executable, self.root / "apps/android/tools/fake-service/fake_extend.py",
                           "--port", str(self.port), "--auto-claim-after", "1", "--out", self.caseout / "reconnect-uploads"],
                          self.caseout / "reconnect-fake.log", {"ANDROID_SERIAL": self.serial})
        self.until("fake service ready", lambda: self.request("state"))
        self.adb("shell", "am", "start", "-n", PKG + "/.ui.MainActivity", "--es", "service_url",
                 f"http://10.0.2.2:{self.port}", "--ez", "forget_pair", "true", "--ez", "force_tv", "false")
        self.until("exactly one pair connected", lambda: len((s := self.request("state"))["pairs"]) == 1 and s["pairs"][0]["connected"])
        self.request("frame", {"type": "session_started", "target": None, "session_id": "a3f",
                              "silicon_id": "si:chef", "since": "2026-09-28T00:00:00Z", "side": "side-alice"})
        observations = []
        def recovered(label, started, old_pid=None):
            def check():
                pid = self.adb("shell", "pidof", PKG, check=False)
                if not pid.isdigit() or pid == old_pid:
                    return None
                result = self.remote(["id", "-u"])
                if not result.get("ok") or result.get("output", {}).get("stdout", "").strip() != "2000":
                    return None
                return pid
            pid = self.until(label + " recovery without app launch or Connect", check, 120)
            observations.append({"case": label, "seconds": round(time.monotonic()-started, 3), "pid": pid, "old_pid": old_pid})
            (self.caseout / f"memory-{label}.txt").write_text(self.adb("shell", "dumpsys", "meminfo", PKG))
        try:
            prop = "ro.boot.qemu.avd_name"
            if not self.adb("shell", "getprop", prop):
                prop = "ro.kernel.qemu.avd_name"
            def matching_avd():
                remote = self.remote(["getprop", prop])
                return remote.get("ok") and remote.get("output", {}).get("stdout", "").strip() == self.avd
            self.until("native command matches selected AVD", matching_avd)
            recovered("baseline", time.monotonic())
            self.adb("shell", "input", "keyevent", "KEYCODE_HOME")
            time.sleep(3)
            pid = self.adb("shell", "pidof", PKG); assert pid.isdigit()
            started = time.monotonic()
            self.adb("shell", "run-as", PKG, "kill", "-9", pid)
            recovered("process-death", started, pid)
            started = time.monotonic()
            self.adb("tcpip", "5555")
            self.until("adbd control transport", lambda: self.adb("shell", "id", "-u") == "2000")
            self.transport("after-adbd-restart")
            recovered("adbd-restart", started)
        finally:
            write_json(self.caseout / "reconnect-results.json", observations)
            try:
                self.request("frame", {"type": "session_ended", "session_id": "a3f", "reason": "released"})
            finally:
                self.stop(fake)
        assert len(observations) == 3
        self.case["reconnect"] = observations
        self.flush()

    def media(self):
        names = ("duration-recording-proof.mp4", "still-recording-proof.mp4", "burst-recording-proof.mp4", "long-recording-proof.mp4")
        evidence = []
        for name in names:
            file = self.caseout / name
            self.adb("pull", f"/sdcard/Android/data/{PKG}/files/{name}", str(file), timeout=60)
            streams = json.loads(self.cmd(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_streams", "-of", "json", file], timeout=30).stdout)["streams"]
            time_base = streams[0]["time_base"]
            assert re.fullmatch(r"[1-9][0-9]*/[1-9][0-9]*", time_base), time_base
            # Explicit source timebase works with Ubuntu24.04 FFmpeg6.1 as well as newer
            # builds. Do not depend on the newer symbolic `demux` option.
            self.cmd(["ffmpeg", "-v", "error", "-xerror", "-i", file, "-map", "0:v:0",
                      "-enc_time_base:v", time_base.replace("/", ":"), "-fps_mode", "passthrough", "-f", "null", "-"], timeout=120)
            packets = json.loads(self.cmd(["ffprobe", "-v", "error", "-select_streams", "v", "-show_packets", "-of", "json", file], timeout=30).stdout)["packets"]
            assert packets and all(int(a["dts"]) < int(b["dts"]) for a,b in zip(packets, packets[1:]))
            if name.startswith("long-"):
                assert float(packets[-1]["pts_time"]) > 181
            frames = json.loads(self.cmd(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_frames",
                                         "-show_entries", "frame=best_effort_timestamp_time", "-of", "json", file], timeout=120).stdout)["frames"]
            assert frames, "No native video frames to inspect"
            previews = []
            for label, index in (("first", 0), ("middle", len(frames)//2), ("last", len(frames)-1)):
                timestamp = float(frames[index]["best_effort_timestamp_time"])
                preview = self.caseout / (file.stem + "-" + label + ".png")
                filters = f"select=eq(n\\,{index}),scale=480:480:force_original_aspect_ratio=decrease"
                self.cmd(["ffmpeg", "-v", "error", "-xerror", "-i", file, "-map", "0:v:0", "-vf", filters,
                          "-fps_mode", "passthrough", "-frames:v", "1", "-update", "1", "-y", preview], timeout=120)
                header = preview.read_bytes()[:24]
                assert header[:8] == b"\x89PNG\r\n\x1a\n" and len(header) == 24
                width, height = struct.unpack(">II", header[16:24])
                assert 0 < width <= 480 and 0 < height <= 480
                previews.append({"name": preview.name, "frame_index": index, "timestamp_s": timestamp,
                                 "width": width, "height": height, "sha256": digest(preview)})
            evidence.append({"name": name, "bytes": file.stat().st_size, "sha256": digest(file), "full_decode": True,
                             "ordered_dts": True, "representative_frames": previews})
        self.case["recording_media"] = evidence
        self.flush()

    def run_case(self, name, tag, abi, revision, console_port, port, multi_checks):
        self.guard_disk(12)
        self.serial, self.console_port, self.port = f"emulator-{console_port}", console_port, port
        self.avd = f"ExtendApi29_{name}_{self.run_id}"
        self.caseout = self.out / name; self.caseout.mkdir()
        self.case = {"name": name, "avd": self.avd, "serial": self.serial, "package": f"system-images;android-29;{tag};{abi}",
                     "instrumentation": [], "skips": [{"class": "LocalAdbTest", "method": m,
                     "reason": "API29 uses legacy network debugging; wireless pairing/TLS begins at API30"} for m in TLS_SKIPS]}
        self.report["cases"].append(self.case); self.flush()
        assert self.serial not in self.cmd([self.adb_bin, "devices"]).stdout
        for number in (console_port, console_port+1, port):
            with socket.socket() as check:
                check.bind(("127.0.0.1", number))
        self.cmd([self.avdmanager, "create", "avd", "--name", self.avd, "--package", self.case["package"]], timeout=60, input="no\n")
        config = self.avds / (self.avd + ".avd/config.ini")
        properties = sdk_properties(config)
        memory, emulator_flags = avd_memory(name)
        emulator_flags += self.tv_emulator_debug_flags()
        properties.update({**memory, "disk.dataPartition.size": "6G",
                           "hw.lcd.width": "720" if name == "phone" else "1280",
                           "hw.lcd.height": "1280" if name == "phone" else "720",
                           "hw.lcd.density": "320" if name == "phone" else "213", "showDeviceFrame": "no"})
        config.write_text("".join(f"{key}={value}\n" for key,value in properties.items()))
        shutil.copy2(config, self.caseout / "avd-config.ini")
        emulator = self.spawn([self.sdk / "emulator/emulator", "-avd", self.avd, "-port", str(console_port), "-no-window",
                               "-no-audio", "-no-boot-anim", "-no-snapshot", "-gpu", "swiftshader_indirect", "-accel", "on",
                               *emulator_flags], self.caseout / "emulator.log")
        case_error = None
        try:
            self.until("owned API29 emulator boot", lambda: emulator.poll() is None and self.adb("shell", "getprop", "sys.boot_completed", check=False) == "1", 360)
            self.assert_avd()
            features = self.adb("shell", "pm", "list", "features")
            tv = "feature:android.software.leanback" in features or "feature:android.hardware.type.television" in features
            assert tv == (name == "tv"), "Actual Android TV features must match the official selected image"
            self.case["actual_tv_features"] = tv
            self.case["android_properties"] = self.adb("shell", "getprop")
            self.case["features"] = features
            self.case["display_size"] = self.adb("shell", "wm", "size")
            self.adb("shell", "input", "keyevent", "KEYCODE_WAKEUP")
            self.adb("shell", "wm", "dismiss-keyguard")
            self.adb("shell", "settings", "put", "system", "screen_off_timeout", "1800000")
            if name == "tv":
                self.verify_owned_tv_heap()
            else:
                self.configure_owned_heap()
            for apk in (self.apk, self.test_apk):
                self.adb("install", "-r", "-g", str(apk), timeout=120)
            self.adb("tcpip", "5555")
            self.until("legacy adb control", lambda: self.adb("shell", "id", "-u") == "2000")
            tcp_trace = self.start_tv_tcp_trace()
            try:
                self.transport("initial")
                self.tv_adb_key_metadata("before-initial-authorization")
                try:
                    self.instrument("LocalAdbTest", "connectLocalForService", timeout=90, authorize=True)
                finally:
                    self.tv_adb_key_metadata("after-initial-authorization")
            finally:
                self.stop_tv_tcp_trace(tcp_trace)
            # Existing multi harness grants required access and checks the actual product frames.
            multi_out = self.caseout / "multi"
            multi = self.spawn(["sh", self.harness_root / MULTI_FIXTURE_FILES[0], "multi"], self.caseout / "multi.log",
                               {"SERIAL": self.serial, "ANDROID_SERIAL": self.serial, "PORT": str(port), "OUT": str(multi_out),
                                "APK": str(self.apk), "ADB": self.adb_bin, "FORCE_TV": "0"})
            self.wait(multi, 480)
            text = (self.caseout / "multi.log").read_text()
            assert f"SCENARIO DONE: {multi_checks} passed, 0 failed" in text, text[-4000:]
            assert "FAIL" not in text, text[-4000:]
            self.case["multi"] = {"passed": multi_checks, "failed": 0}
            self.flush()
            for cls, methods in (("DisplayTest", DISPLAY), ("LocalAdbTest", LOCAL), ("RecordingTest", RECORDING)):
                for method in methods:
                    reason = inapplicable_test(name, cls, method)
                    if reason:
                        self.case["skips"].append({"class": cls, "method": method, "reason": reason})
                        self.flush()
                        continue
                    timeout = 660 if method == "outputLargerThanTheHeapIsStreamedNotQueued" else 260 if method == "beyondNativeLimit" else 180
                    self.instrument(cls, method, timeout)
            self.media()
            # Native tests change the process; reset only its a11y binding, then start the
            # separately paired reconnect fixture. Never launch or reconnect during recovery.
            self.adb("shell", "settings", "put", "secure", "enabled_accessibility_services", "null")
            self.adb("shell", "settings", "put", "secure", "enabled_accessibility_services", f"{PKG}/{PKG}.a11y.ExtendAccessibilityService")
            self.adb("shell", "settings", "put", "secure", "accessibility_enabled", "1")
            self.reconnect()
            self.case["result"] = "passed"
        except BaseException as error:
            case_error = error
            self.case["result"] = "failed"
            self.case["failure"] = {"type": type(error).__name__, "message": str(error)}
        finally:
            try:
                (self.caseout / "logcat.txt").write_text(self.adb("logcat", "-d", check=False))
                with (self.caseout / "native-screen.png").open("wb") as stream:
                    subprocess.run([self.adb_bin, "-s", self.serial, "exec-out", "screencap", "-p"], stdout=stream, stderr=subprocess.DEVNULL, timeout=20, env=self.env, check=True)
                screenshot = self.caseout / "native-screen.png"
                header = screenshot.read_bytes()[:24]
                assert header[:8] == b"\x89PNG\r\n\x1a\n" and len(header) == 24
                width, height = struct.unpack(">II", header[16:24])
                assert width > 0 and height > 0 and (name != "tv" or width > height)
                self.case["native_screenshot"] = {"width": width, "height": height, "sha256": digest(screenshot)}
            except Exception as error:
                self.case["diagnostics_error"] = str(error)
                if case_error is None:
                    case_error = error
                    self.case["result"] = "failed"
                    self.case["failure"] = {"type": type(error).__name__, "message": str(error)}
            for child in reversed(self.handles):
                self.stop(child)
            self.cmd([self.avdmanager, "delete", "avd", "--name", self.avd], timeout=30)
            self.case["cleanup"] = {"owned_emulator_stopped": emulator.poll() is not None,
                                    "avd_removed": not (self.avds / (self.avd + ".avd")).exists(),
                                    "free_bytes": shutil.disk_usage(self.out).free}
            self.flush()
        if case_error:
            raise case_error

    def run(self):
        assert self.cmd(["git", "rev-parse", "HEAD"]).stdout.strip() == SOURCE
        assert not self.cmd(["git", "status", "--porcelain", "--untracked-files=no"]).stdout
        fixture_commit = self.cmd(["git", "-C", self.harness_root, "rev-parse", "HEAD"]).stdout.strip()
        assert fixture_commit == os.environ["GITHUB_SHA"], "Multi fixture must come from the recorded workflow commit"
        self.report["multi_fixture"] = {"commit": fixture_commit,
            "hashes": {name: digest(self.harness_root / name) for name in MULTI_FIXTURE_FILES},
            "apk_source": SOURCE}
        self.apk = self.root / "apps/android/app/build/outputs/apk/debug/app-debug.apk"
        self.test_apk = self.root / "apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
        self.report["apks"] = [{"name": p.name, "sha256": digest(p)} for p in (self.apk, self.test_apk)]
        self.report["android_source_tree"] = self.cmd(["git", "rev-parse", "HEAD:apps/android"]).stdout.strip()
        self.report["existing_reconnect_harness_sha256"] = digest(self.root / "apps/android/tools/adb-reconnect-lane.py")
        errors = []
        for case in CASES:
            if case[0] != self.profile:
                continue
            try:
                self.run_case(*case)
            except Exception as error:
                errors.append({"case": case[0], "error": str(error)})
        self.report["result"] = "failed" if errors else "passed"
        self.report["failures"] = errors
        self.flush()
        assert not errors, errors



def export_evidence(out):
    """Upload only allowlisted diagnostics/results/screenshots, no APK/media/state/HTTP uploads."""
    public = out / "public"
    public.mkdir(exist_ok=True)
    top = {"report.json", "cleanup.json", "verified-images.json", "runner-image.txt", "disk-before.txt",
           "sdk-installed.txt", "sdk-license-inventory.json", "emulator-version.txt", "acceleration.txt", "adb-version.txt",
           "provenance.json", "source-before.sha256", "source-after.sha256", "source-before.json", "source-after.json", "adapter.sha256", "ffmpeg-version.txt"}
    native = {"native-screen.png", "emulator.log", "multi.log", "logcat.txt", "reconnect-results.json", "avd-config.ini", "heap-setup.json", "emulator-debug-tags.txt", "tcp-control.log"}
    copied = []
    for path in sorted(out.rglob("*")):
        if not path.is_file() or public in path.parents:
            continue
        relative = path.relative_to(out)
        allow = len(relative.parts) == 1 and path.name in top
        allow = allow or (len(relative.parts) == 2 and relative.parts[0] in {"phone", "tv"} and (
            path.name in native or re.fullmatch(r"(?:DisplayTest|LocalAdbTest|RecordingTest)-[A-Za-z]+\.log", path.name)
            or re.fullmatch(r"accessibility-(?:DisplayTest|LocalAdbTest|RecordingTest)-[A-Za-z]+\.(?:before|after)\.txt", path.name)
            or re.fullmatch(r"(?:initial|after-adbd-restart)-guest-tcp\.txt", path.name)
            or re.fullmatch(r"memory-[a-z-]+\.txt", path.name)
            or re.fullmatch(r"(?:duration|still|burst|long)-recording-proof-(?:first|middle|last)\.png", path.name)))
        if not allow:
            continue
        target = public / relative; target.parent.mkdir(parents=True, exist_ok=True)
        if path.suffix == ".png":
            shutil.copy2(path, target)
        elif relative == Path("tv/emulator.log"):
            target.write_text(tv_emulator_lifecycle(path.read_text(errors="replace")))
        else:
            target.write_text(redacted(path.read_text(errors="replace")))
        copied.append({"path": str(relative), "sha256": digest(target)})
    write_json(public / "evidence-manifest.json", {"files": copied, "excluded": [
        "APKs, signing material, private ownership state, raw fake-service HTTP logs/uploads",
        "MP4 bytes remain job-private; full decode, timestamp and hash evidence is in report.json"],
        "qualification": "Fresh owned emulator content only; no physical or signed release-upgrade claim"})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--profile", required=True, choices=("phone", "tv"))
    parser.add_argument("--sdk", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--verify-images", action="store_true")
    parser.add_argument("--cleanup", action="store_true")
    parser.add_argument("--export-evidence", action="store_true")
    args = parser.parse_args()
    args.out = args.out.resolve(); args.out.mkdir(parents=True, exist_ok=True)
    run_id, avds = owned_environment(args.out)
    if args.export_evidence:
        export_evidence(args.out)
        return
    if args.cleanup:
        cleanup_owned(args.out, run_id, avds)
        return
    if args.verify_images:
        verify_images(args.sdk.resolve(), args.out, args.profile)
        return
    assert args.root
    verify_images(args.sdk.resolve(), args.out, args.profile)
    lane = Lane(args, run_id, avds)
    def interrupted(signum, frame):
        raise InterruptedError(f"Received signal{signum}; cleaning exact owned fixture")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        lane.run()
    finally:
        for child in reversed(lane.handles):
            lane.stop(child)
        for log in lane.logs:
            log.close()
        cleanup_owned(args.out, run_id, avds)


if __name__ == "__main__":
    main()
