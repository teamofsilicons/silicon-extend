"""scripts/package_apps.py: the refusals, the discovery checks and a full pack.

    python3 -m unittest discover -s scripts -p 'test_*.py'

The full pack runs the real `silicon-apps` (0.2.x) when it is installed and is skipped otherwise
(the release workflow installs it). Stand-in executables carry only the headers the script reads;
the discovery checks run a small shell script in their place, so they need a Unix shell.
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("package_apps", HERE / "package_apps.py")
package_apps = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_apps)
VERSION = package_apps.cli_version()


def elf(machine, elf_class=2, interpreter=True, glibc=None):
    """A little-endian ELF header with one program header (PT_INTERP or PT_LOAD), plus glibc text."""
    header = bytearray(64 if elf_class == 2 else 52)
    header[:6] = b"\x7fELF" + bytes([elf_class, 1])
    struct.pack_into("<HH", header, 16, 2, machine)
    kind = package_apps.PT_INTERP if interpreter else 1
    if elf_class == 2:
        struct.pack_into("<Q", header, 32, 64)
        struct.pack_into("<HH", header, 54, 56, 1)
        program = struct.pack("<I", kind) + bytes(52)
    else:
        struct.pack_into("<I", header, 28, 52)
        struct.pack_into("<HH", header, 42, 32, 1)
        program = struct.pack("<I", kind) + bytes(28)
    return bytes(header) + program + (f"\0GLIBC_{glibc}\0".encode() if glibc else b"")


def pe(machine):
    header = bytearray(64)
    header[:2] = b"MZ"
    struct.pack_into("<I", header, 60, 64)
    return bytes(header) + b"PE\0\0" + struct.pack("<H", machine)


def macho(cpu):
    header = bytearray(64)
    header[:4] = b"\xcf\xfa\xed\xfe"
    struct.pack_into("<I", header, 4, cpu)
    return bytes(header)


class Manifest(unittest.TestCase):
    def test_one_target_and_the_cli_version(self):
        text = package_apps.render_manifest(package_apps.TEMPLATE.read_text(), "4.0.0", "windows-aarch64")
        self.assertEqual(text, "schema_version: 1\napp_id: extend\nversion: 4.0.0\ncommand: extend\n"
                               "targets:\n  windows-aarch64:\n    binary: bin/extend.exe\n")

    def test_the_version_is_the_cli_crates_not_the_workspaces(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nname = "silicon-extend-cli"\nversion = "9.8.7"\nedition.workspace = true\n'
                                '[dependencies]\nfoo = { version = "1.0.0" }\n')
            self.assertEqual(package_apps.cli_version(manifest), "9.8.7")
            manifest.write_text('[package]\nversion.workspace = true\n')
            with self.assertRaisesRegex(package_apps.PackageError, "keeps its own version"):
                package_apps.cli_version(manifest)

    def test_a_lost_placeholder_is_refused(self):
        with self.assertRaisesRegex(package_apps.PackageError, "@TARGET@"):
            package_apps.render_manifest("app_id: extend\nversion: @VERSION@\n", "4.0.0", "linux-x86_64")


class NativeExecutables(unittest.TestCase):
    def test_each_shipped_target_accepts_its_own_format(self):
        cases = {
            "linux-x86_64": elf(62, glibc="2.39"),
            "linux-aarch64": elf(183, glibc="2.17"),
            "windows-x86_64": pe(0x8664),
            "windows-aarch64": pe(0xAA64),
            "macos-x86_64": macho(0x01000007),
            "macos-aarch64": macho(0x0100000C),
        }
        for target, data in cases.items():
            with self.subTest(target=target):
                package_apps.check_native(data, target)

    def test_the_wrong_system_or_processor_is_refused(self):
        cases = [
            ("linux-x86_64", elf(183), "isn't built for linux-x86_64"),
            ("linux-aarch64", macho(0x0100000C), "needs a little-endian ELF"),
            ("windows-aarch64", pe(0x8664), "isn't built for windows-aarch64"),
            ("macos-x86_64", macho(0x0100000C), "isn't built for macos-x86_64"),
            ("macos-aarch64", pe(0xAA64), "64-bit Mach-O"),
            ("linux-armv7hf", elf(40, elf_class=2), "isn't built for linux-armv7hf"),
        ]
        for target, data, reason in cases:
            with self.subTest(target=target), self.assertRaisesRegex(package_apps.PackageError, reason):
                package_apps.check_native(data, target)

    def test_a_glibc_newer_than_ubuntu_24_04_is_refused(self):
        with self.assertRaisesRegex(package_apps.PackageError, "needs glibc 2.41, newer than 2.39"):
            package_apps.check_native(elf(62, glibc="2.41"), "linux-x86_64")
        self.assertEqual(package_apps.check_native(elf(62, interpreter=False, glibc="2.41"), "linux-x86_64"),
                         "static ELF")

    def test_the_version_must_match_the_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "extend"
            binary.write_bytes(macho(0x0100000C))
            with self.assertRaisesRegex(package_apps.PackageError, "differs from crates/extend-cli/Cargo.toml"):
                package_apps.check("0.0.1", "macos-aarch64", binary, "auto")
            with self.assertRaisesRegex(package_apps.PackageError, "strict x.y.z"):
                package_apps.check(f"{VERSION}-rc.1", "macos-aarch64", binary, "auto")
            with self.assertRaisesRegex(package_apps.PackageError, "unknown target"):
                package_apps.check(VERSION, "linux-riscv64", binary, "auto")


HOST_TARGET = {"linux": "linux-x86_64", "macos": "macos-aarch64"}.get(package_apps.host_system())
GOOD = {
    "--help": 'echo "extend: let a Silicon use the devices a Carbon has paired"',
    "accounts --json": f"echo '{{\"app_id\":\"extend\",\"version\":\"{VERSION}\"}}'",
    "login status --json": "echo '{\"authenticated\":false}'",
}


@unittest.skipUnless(HOST_TARGET and shutil.which("sh"), "the discovery stand-in is a shell script")
class Discovery(unittest.TestCase):
    """discovery() runs whatever it is given; a shell script stands in for the native binary."""

    def run_stand_in(self, **overrides):
        answers = {**GOOD, **{key.replace("_", " "): value for key, value in overrides.items()}}
        body = "\n".join(f'  "{args}") {command} ;;' for args, command in answers.items())
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "extend"
            script.write_text(f'#!/bin/sh\ncase "$*" in\n{body}\n  *) exit 2 ;;\nesac\n')
            return package_apps.discovery(script, HOST_TARGET, VERSION, "require")

    def test_the_three_answers_silicon_apps_requires_pass(self):
        self.assertTrue(self.run_stand_in())

    def test_each_wrong_answer_is_refused_with_why(self):
        cases = [
            ({"--help": "exit 1"}, "--help` must exit 0"),
            ({"accounts --json": "echo '{\"app_id\":\"ring\"}'"}, '"app_id": "extend"'),
            ({"accounts --json": "echo '{\"app_id\":\"extend\",\"version\":\"0.0.1\"}'"}, "reports version '0.0.1'"),
            ({"accounts --json": "echo not json"}, "must print one JSON object"),
            ({"login status --json": "echo '{\"authenticated\":false,\"reason\":\"x\"}'"}, "exactly"),
            ({"login status --json": "echo '{\"authenticated\":false}'; exit 1"}, "must exit 0"),
            ({"login status --json": "mkdir \"$HOME/.extend\"; echo '{\"authenticated\":false}'"}, "must not write"),
        ]
        for overrides, reason in cases:
            with self.subTest(reason=reason), self.assertRaisesRegex(package_apps.PackageError, reason):
                self.run_stand_in(**{key.replace(" ", "_"): value for key, value in overrides.items()})

    def test_the_binary_sees_none_of_this_shells_settings(self):
        os.environ["EXTEND_API_URL"] = "http://should-not-leak.invalid"
        try:
            self.assertTrue(self.run_stand_in(login_status_json=(
                'test -z "$EXTEND_API_URL" && test "$HOME" = "$SILICON_HOME" && echo \'{"authenticated":false}\'')))
        finally:
            del os.environ["EXTEND_API_URL"]

    def test_another_systems_binary_is_noted_or_refused(self):
        other = "windows-x86_64"
        self.assertFalse(package_apps.discovery(Path("unused"), other, VERSION, "auto"))
        with self.assertRaisesRegex(package_apps.PackageError, "can't run on this"):
            package_apps.discovery(Path("unused"), other, VERSION, "require")


def packer_available():
    try:
        command = package_apps.find_packer(None)
    except package_apps.PackageError:
        return False
    version = subprocess.run([command, "--version"], capture_output=True, text=True, check=False).stdout.split()
    return len(version) == 2 and version[1].startswith(package_apps.PACKER_SERIES)


@unittest.skipUnless(packer_available(), "silicon-apps 0.2.x is not installed")
class FullPack(unittest.TestCase):
    """The real packer, with a stand-in for a system this machine can't run (discovery is noted)."""

    def test_one_archive_per_target_with_licences_and_its_sha256(self):
        target, data = (("linux-x86_64", elf(62, glibc="2.17")) if package_apps.host_system() == "windows"
                        else ("windows-x86_64", pe(0x8664)))
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            binary = work / package_apps.binary_name(target)
            binary.write_bytes(data)
            self.assertEqual(package_apps.main([VERSION, target, str(binary), "--check-only"]), 0)
            self.assertEqual(package_apps.main([VERSION, target, str(binary), "--check-only", "--discovery",
                                                "require"]), 1)
            self.assertEqual(package_apps.main([VERSION, target, str(binary), "--output-dir", str(work / "dist")]), 0)
            archive = work / "dist" / f"extend-{VERSION}-{target}.tar.gz"
            digest = package_apps.sha256(archive)
            self.assertEqual(Path(f"{archive}.sha256").read_text(), f"{digest}  {archive.name}\n")
            with tarfile.open(archive, "r:gz") as packed:
                names = {m.name.removeprefix("./") for m in packed.getmembers() if m.isfile()}
                manifest = packed.extractfile("apps.yaml").read().decode()
                self.assertEqual(packed.extractfile(f"bin/{package_apps.binary_name(target)}").read(), data)
            self.assertEqual(names, package_apps.expected_files(target))
            self.assertIn(f"targets:\n  {target}:\n", manifest)
            self.assertEqual(manifest.count("binary:"), 1)
            # Packing the same input again gives the same archive (Silicon Apps packs deterministically).
            self.assertEqual(package_apps.main([VERSION, target, str(binary), "--output-dir", str(work / "again")]), 0)
            self.assertEqual(package_apps.sha256(work / "again" / archive.name), digest)


if __name__ == "__main__":
    unittest.main()
