#!/usr/bin/env python3
"""Package one `extend` CLI binary as a Silicon Apps archive.

    scripts/package-apps.sh <version> <target> <binary>
    scripts/package-apps.sh 4.0.0 macos-aarch64 target/release/extend

Renders packaging/apps.yaml.in for exactly one target and stages:

    apps.yaml                            that target only
    bin/extend                           bin/extend.exe on Windows; mode 0755
    licences/LICENSE                     Extend's MIT licence
    licences/THIRD_PARTY_NOTICES.md      what each artifact bundles, and where its licences are
    licences/THIRD_PARTY_LICENSES.txt    every Rust crate's licence text (`cargo about`)

Then it runs `silicon-apps validate` and `silicon-apps pack`, checks that the archive holds exactly
those files, byte for byte, validates it again (packed, and extracted), and writes
dist/apps/extend-<version>-<target>.tar.gz and its .sha256. It never uploads or publishes anything.

Before staging it refuses:
- a version that isn't strict x.y.z, or that differs from crates/extend-cli/Cargo.toml (the CLI has
  its own version; the workspace version in Cargo.toml is the desktop app's);
- a file that isn't a native executable for the target (ELF, Mach-O or PE for the target's
  processor), and a Linux executable that needs a newer glibc than Ubuntu 24.04's 2.39 (the
  release runners, Silicon Apps' Linux validation workers and the Silicons' hosts);
- a binary that answers the three discovery commands wrongly, run signed out in an empty
  HOME/SILICON_HOME with nothing else set: `extend --help` (exit 0, help text),
  `extend accounts --json` (exit 0, "app_id": "extend", and "version" equal to the package's) and
  `extend login status --json` (exit 0, exactly {"authenticated": false}). None of them may write
  to that home.

`extend --version` isn't run: it asks the Extend service which API versions it speaks, and packaging
must not depend on the network. `accounts --json` carries the version offline.

Discovery (--discovery, or PACKAGE_DISCOVERY): `auto` runs the commands when this machine can run
the binary and says so when it can't (Silicon Apps' validation worker for that target runs them at
upload); `require` fails instead. --check-only verifies the binary (header, glibc, discovery)
without packing, so a build runner can check what it built where it built it; it needs no
silicon-apps.
"""

import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "extend"
COMMAND = "extend"
TEMPLATE = ROOT / "packaging" / "apps.yaml.in"
CLI_MANIFEST = ROOT / "crates" / "extend-cli" / "Cargo.toml"
# Shipped beside the executable in every archive (THIRD_PARTY_NOTICES.md says where each artifact
# keeps them). Silicon Apps installs the whole package directory, so they stay with the command.
LICENCES = ("LICENSE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt")
PACKER_SERIES = "0.2."
PACKER_INSTALL = "cargo install --locked silicon-apps-cli --version 0.2.0"
# Highest glibc symbol version a dynamically linked Linux binary may need.
GLIBC_CEILING = (2, 39)

# Every Silicon Apps target and its operating system. Extend ships six of them (see
# .github/workflows/release.yml); the others are accepted here so a new target needs no change.
TARGETS = {
    "linux-x86_64": "linux",
    "linux-i686": "linux",
    "linux-aarch64": "linux",
    "linux-armv7hf": "linux",
    "windows-x86_64": "windows",
    "windows-i686": "windows",
    "windows-aarch64": "windows",
    "macos-x86_64": "macos",
    "macos-aarch64": "macos",
}
ELF_MACHINE = {"x86_64": (2, 62), "i686": (1, 3), "aarch64": (2, 183), "armv7hf": (1, 40)}
MACHO_CPU = {"x86_64": 0x01000007, "aarch64": 0x0100000C}
PE_MACHINE = {"x86_64": 0x8664, "i686": 0x014C, "aarch64": 0xAA64}
PT_INTERP = 3
# Errors that mean "this machine can't execute that processor's code": Exec format error, macOS's
# Bad CPU type (EBADARCH, also what a missing Rosetta gives) and Windows' ERROR_BAD_EXE_FORMAT and
# ERROR_EXE_MACHINE_TYPE_MISMATCH. Anything else is a real failure.
CANNOT_RUN_ERRNOS = {errno.ENOEXEC, getattr(errno, "EBADARCH", 86)}
CANNOT_RUN_WINERRORS = {193, 216}


class PackageError(Exception):
    """A refusal with its exact reason; the script prints it and exits 1."""


def binary_name(target):
    return f"{COMMAND}.exe" if target.startswith("windows-") else COMMAND


def cli_version(manifest=CLI_MANIFEST):
    """The version in crates/extend-cli/Cargo.toml's [package] table: what apps.yaml carries."""
    text = manifest.read_text(encoding="utf-8")
    package = re.search(r"(?ms)^\[package\]\s*$(.*?)(?=^\[|\Z)", text)
    match = package and re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"', package.group(1))
    if not match:
        raise PackageError(f"{manifest} has no [package] version (the CLI keeps its own version)")
    return match.group(1)


def render_manifest(template, version, target):
    """apps.yaml for one target: comment lines dropped, placeholders filled in."""
    text = "".join(line for line in template.splitlines(keepends=True) if not line.lstrip().startswith("#"))
    for placeholder, value in (("@VERSION@", version), ("@TARGET@", target), ("@BINARY@", f"bin/{binary_name(target)}")):
        if placeholder not in text:
            raise PackageError(f"packaging/apps.yaml.in lost its {placeholder} placeholder")
        text = text.replace(placeholder, value)
    if "@" in text:
        raise PackageError("packaging/apps.yaml.in has a placeholder this script doesn't know")
    return text


def elf_interpreter(data, elf_class):
    """True when the ELF executable names a dynamic loader (PT_INTERP)."""
    if elf_class == 2:
        offset = struct.unpack_from("<Q", data, 32)[0]
        size, count = struct.unpack_from("<HH", data, 54)
    else:
        offset = struct.unpack_from("<I", data, 28)[0]
        size, count = struct.unpack_from("<HH", data, 42)
    if count == 0 or size == 0 or offset + size * count > len(data):
        raise PackageError("the ELF executable has no readable program headers")
    return any(struct.unpack_from("<I", data, offset + size * index)[0] == PT_INTERP for index in range(count))


def highest_glibc(data):
    """The highest GLIBC_x.y[.z] symbol version named in the executable, or None."""
    versions = [tuple(int(part) for part in match.groups() if part is not None)
                for match in re.finditer(rb"GLIBC_(\d+)\.(\d+)(?:\.(\d+))?", data)]
    return max(versions) if versions else None


def check_native(data, target):
    """Refuse anything but a native executable for the target. Returns a short description."""
    system = TARGETS[target]
    arch = target.split("-", 1)[1]
    if system == "linux":
        elf_class, machine = ELF_MACHINE[arch]
        if data[:4] != b"\x7fELF" or len(data) < 52 or data[5] != 1:
            raise PackageError(f"{target} needs a little-endian ELF executable; this file isn't one")
        if data[4] != elf_class or struct.unpack_from("<H", data, 18)[0] != machine:
            raise PackageError(f"the ELF executable isn't built for {target}")
        if not elf_interpreter(data, elf_class):
            return "static ELF"
        glibc = highest_glibc(data)
        if glibc is None:
            return "dynamic ELF (no glibc symbol versions)"
        shown = ".".join(str(part) for part in glibc)
        if glibc[:2] > GLIBC_CEILING:
            ceiling = ".".join(str(part) for part in GLIBC_CEILING)
            raise PackageError(
                f"the {target} executable needs glibc {shown}, newer than {ceiling} (Ubuntu 24.04), the newest "
                "the release runners, Silicon Apps' Linux validation workers and the Silicons' hosts have; build "
                "it on ubuntu-24.04 as the release workflow does"
            )
        return f"dynamic ELF, needs glibc {shown}"
    if system == "macos":
        if data[:4] != b"\xcf\xfa\xed\xfe" or len(data) < 8:
            raise PackageError(f"{target} needs a 64-bit Mach-O executable for one processor; this file isn't one")
        if struct.unpack_from("<I", data, 4)[0] != MACHO_CPU[arch]:
            raise PackageError(f"the Mach-O executable isn't built for {target}")
        return "Mach-O"
    if data[:2] != b"MZ" or len(data) < 64:
        raise PackageError(f"{target} needs a Windows PE executable; this file isn't one")
    offset = struct.unpack_from("<I", data, 60)[0]
    if len(data) < offset + 6 or data[offset:offset + 4] != b"PE\0\0":
        raise PackageError(f"{target} needs a Windows PE executable; this file has no PE header")
    if struct.unpack_from("<H", data, offset + 4)[0] != PE_MACHINE[arch]:
        raise PackageError(f"the Windows executable isn't built for {target}")
    return "PE"


def host_system():
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform in ("win32", "cygwin", "msys"):
        return "windows"
    return sys.platform


def clean_environment(home):
    """What a validation worker gives the binary: an empty home and none of this shell's settings."""
    environment = {"HOME": str(home), "SILICON_HOME": str(home), "USERPROFILE": str(home),
                   "TMPDIR": str(home / "tmp"), "PATH": os.pathsep.join(["/usr/bin", "/bin"])}
    if host_system() == "windows":
        system_root = os.environ.get("SYSTEMROOT", r"C:\Windows")
        environment.update({"SYSTEMROOT": system_root, "WINDIR": os.environ.get("WINDIR", system_root),
                            "PATH": os.pathsep.join([os.path.join(system_root, "System32"), system_root])})
        environment["TEMP"] = environment["TMP"] = environment["TMPDIR"]
    return environment


def run_clean(binary, arguments, home):
    """(exit code, stdout, stderr) of the binary in the empty home. OSError when it can't start here."""
    (home / "tmp").mkdir(exist_ok=True)
    result = subprocess.run([str(binary), *arguments], cwd=home, env=clean_environment(home),
                            capture_output=True, timeout=60, check=False)
    return result.returncode, result.stdout.decode("utf-8", "replace"), result.stderr.decode("utf-8", "replace")


def json_object(text, command):
    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        raise PackageError(f"`{command}` must print one JSON object; it printed {text.strip()[:200]!r} ({error})")
    if not isinstance(value, dict):
        raise PackageError(f"`{command}` must print a JSON object; it printed {text.strip()[:200]!r}")
    return value


def cannot_run(error):
    return error.errno in CANNOT_RUN_ERRNOS or getattr(error, "winerror", None) in CANNOT_RUN_WINERRORS


def discovery(original, target, version, mode):
    """Run the commands Silicon Apps runs at upload, signed out, in an empty home. False when it can't run here."""
    if TARGETS[target] != host_system():
        if mode == "require":
            raise PackageError(f"discovery is required, but a {target} binary can't run on this {host_system()} machine")
        print(f"note: a {target} binary can't run here; Silicon Apps' validation worker for {target} runs the "
              "discovery commands at upload")
        return False
    with tempfile.TemporaryDirectory(prefix="extend-discovery-") as directory:
        # Run a private copy named like the installed command and marked executable (downloaded
        # workflow artifacts lose their mode bits), from inside an empty home that holds nothing else.
        work = Path(directory)
        home = work / "home"
        home.mkdir()
        (work / "bin").mkdir()
        binary = work / "bin" / binary_name(target)
        shutil.copyfile(original, binary)
        binary.chmod(0o755)
        try:
            code, out, err = run_clean(binary, ["--help"], home)
        except OSError as error:
            if not cannot_run(error):
                raise PackageError(f"`{COMMAND} --help` couldn't be started: {error}") from error
            if mode == "require":
                raise PackageError(f"discovery is required, but this machine can't run the {target} binary: {error}")
            print(f"note: this machine can't run the {target} binary ({error}); Silicon Apps' validation worker "
                  f"for {target} runs the discovery commands at upload")
            return False
        if code != 0 or not out.strip():
            raise PackageError(f"`{COMMAND} --help` must exit 0 and print help; it exited {code}: {(err or out)[:400]}")
        code, out, err = run_clean(binary, ["accounts", "--json"], home)
        if code != 0:
            raise PackageError(f"`{COMMAND} accounts --json` must exit 0; it exited {code}: {(err or out)[:400]}")
        accounts = json_object(out, f"{COMMAND} accounts --json")
        if accounts.get("app_id") != APP_ID:
            raise PackageError(f'`{COMMAND} accounts --json` must contain "app_id": "{APP_ID}"; it printed '
                               f"{out.strip()[:400]}")
        if accounts.get("version") != version:
            raise PackageError(f"`{COMMAND} accounts --json` reports version {accounts.get('version')!r}, but the "
                               f"package is {version}: package the binary built from this version")
        code, out, err = run_clean(binary, ["login", "status", "--json"], home)
        if code != 0:
            raise PackageError(f"`{COMMAND} login status --json` must exit 0; it exited {code}: {(err or out)[:400]}")
        status = json_object(out, f"{COMMAND} login status --json")
        if status != {"authenticated": False}:
            raise PackageError(f'`{COMMAND} login status --json` must print exactly {{"authenticated":false}} in an '
                               f"empty home; it printed {out.strip()[:400]}")
        leftovers = sorted(str(path.relative_to(home)) for path in home.rglob("*")
                           if path.relative_to(home).parts[0] != "tmp")
        if leftovers:
            raise PackageError(f"the discovery commands must not write to the home; they created {leftovers}")
    print(f"discovery: --help, accounts --json and login status --json answered as Silicon Apps requires ({target})")
    return True


def find_packer(explicit):
    # The Apps installer's own copy comes last; packer() runs it with an empty home and no server,
    # so a sign-in saved on this machine is never used.
    candidates = [explicit, os.environ.get("SILICON_APPS"), shutil.which("silicon-apps"),
                  str(Path.home() / ".cargo" / "bin" / "silicon-apps"),
                  str(Path.home() / ".apps" / "bin" / "silicon-apps")]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return candidate
    raise PackageError(f"silicon-apps isn't installed: run `{PACKER_INSTALL}` or pass --silicon-apps PATH")


def packer(command, arguments, apps_home):
    """Run silicon-apps with an empty home of its own and no server: validate and pack are local operations."""
    environment = {key: value for key, value in os.environ.items()
                   if key not in ("APPS_TOKEN", "APPS_URL", "ACCOUNTS_URL", "SILICON_HOME")}
    environment["SILICON_APPS_NO_DAEMON"] = "1"
    # A loopback address nothing listens on: if a packer ever tried to reach a server during
    # validate or pack, it would fail here instead of calling one.
    nowhere = "http://127.0.0.1:9"
    return subprocess.run([command, *arguments, "--home", str(apps_home), "--server", nowhere,
                           "--accounts-url", nowhere, "--json"],
                          env=environment, capture_output=True, text=True, timeout=300, check=False)


def validate(command, path, apps_home):
    result = packer(command, ["validate", str(path)], apps_home)
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        report = {}
    if result.returncode != 0 or report.get("valid") is not True:
        raise PackageError(f"silicon-apps validate refused {path}: {(result.stdout or result.stderr).strip()[:2000]}")


def expected_files(target):
    return {"apps.yaml", f"bin/{binary_name(target)}", *(f"licences/{name}" for name in LICENCES)}


def verify_archive(command, archive, stage, target, apps_home):
    """The archive holds exactly the staged files, byte for byte, and validates again once extracted."""
    expected = expected_files(target)
    with tarfile.open(archive, "r:gz") as package:
        members = package.getmembers()
        files = {member.name[2:] if member.name.startswith("./") else member.name: member
                 for member in members if member.isfile()}
        others = [member.name for member in members if not (member.isfile() or member.isdir())]
        if set(files) != expected or others:
            raise PackageError(f"the archive must contain exactly {sorted(expected)}; it contains "
                               f"{sorted(set(files) | set(others))}")
        if TARGETS[target] != "windows" and not files[f"bin/{COMMAND}"].mode & 0o111:
            raise PackageError(f"bin/{COMMAND} lost its executable mode in the archive")
        with tempfile.TemporaryDirectory(prefix="extend-extracted-") as directory:
            extracted = Path(directory)
            for name, member in files.items():
                destination = extracted / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(package.extractfile(member).read())
                if destination.read_bytes() != (stage / name).read_bytes():
                    raise PackageError(f"{name} in the archive differs from the staged file")
            validate(command, extracted, apps_home)
    validate(command, archive, apps_home)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def check(version, target, binary, mode):
    """Everything about the binary itself; returns (description, discovery ran)."""
    if target not in TARGETS:
        raise PackageError(f"unknown target {target!r}; Silicon Apps targets are {', '.join(TARGETS)}")
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", version):
        raise PackageError(f"version {version!r} must be strict x.y.z (no prerelease or build suffix)")
    expected = cli_version()
    if version != expected:
        raise PackageError(f"version {version} differs from crates/extend-cli/Cargo.toml ({expected}); apps.yaml "
                           "must match the binary")
    if binary.is_symlink() or not binary.is_file() or binary.stat().st_size == 0:
        raise PackageError(f"{binary} isn't a regular, non-empty file")
    binary = binary.resolve()  # discovery runs it from inside the empty home
    try:
        description = check_native(binary.read_bytes(), target)
    except struct.error as error:
        raise PackageError(f"{binary} is truncated: {error}") from error
    print(f"binary: {binary} is a {target} executable ({description})")
    return description, discovery(binary, target, version, mode)


def package(version, target, binary, output_dir, mode, explicit_packer=None):
    check(version, target, binary, mode)
    for name in LICENCES:
        if not (ROOT / name).is_file():
            raise PackageError(f"{name} is missing from the repository root; every archive ships it in licences/")
    command = find_packer(explicit_packer)
    packer_version = subprocess.run([command, "--version"], capture_output=True, text=True, check=False).stdout.split()
    if len(packer_version) != 2 or not packer_version[1].startswith(PACKER_SERIES):
        raise PackageError(f"{command} reports {' '.join(packer_version) or 'no version'}; this script needs "
                           f"silicon-apps {PACKER_SERIES}x (`{PACKER_INSTALL}`)")
    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"{APP_ID}-{version}-{target}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="extend-package-") as directory:
        work = Path(directory)
        stage, apps_home = work / "package", work / "apps-home"
        (stage / "bin").mkdir(parents=True)
        (stage / "licences").mkdir()
        apps_home.mkdir()
        manifest = render_manifest(TEMPLATE.read_text(encoding="utf-8"), version, target)
        (stage / "apps.yaml").write_bytes(manifest.encode("utf-8"))
        staged_binary = stage / "bin" / binary_name(target)
        shutil.copyfile(binary, staged_binary)
        staged_binary.chmod(0o755)
        for name in LICENCES:
            shutil.copyfile(ROOT / name, stage / "licences" / name)
        validate(command, stage, apps_home)
        temporary = work / archive.name
        result = packer(command, ["pack", str(stage), "--output", str(temporary)], apps_home)
        if result.returncode != 0 or not temporary.is_file():
            raise PackageError(f"silicon-apps pack failed: {(result.stdout or result.stderr).strip()[:2000]}")
        verify_archive(command, temporary, stage, target, apps_home)
        shutil.move(str(temporary), str(archive))
    digest = sha256(archive)
    Path(f"{archive}.sha256").write_bytes(f"{digest}  {archive.name}\n".encode("utf-8"))
    print(f"packaged {archive} ({archive.stat().st_size} bytes)\nsha256 {digest}")
    return archive


def main(argv=None):
    parser = argparse.ArgumentParser(prog="scripts/package-apps.sh", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version", help="the CLI's version, equal to crates/extend-cli/Cargo.toml (for example 4.0.0)")
    parser.add_argument("target", help="one Silicon Apps target, for example linux-x86_64 or macos-aarch64")
    parser.add_argument("binary", type=Path, help="the built extend executable for that target")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist" / "apps", help="default: dist/apps")
    parser.add_argument("--discovery", default=os.environ.get("PACKAGE_DISCOVERY", "auto"),
                        help="auto (default) or require; also PACKAGE_DISCOVERY")
    parser.add_argument("--check-only", action="store_true",
                        help="verify the binary (header, glibc, discovery) and stop; needs no silicon-apps")
    parser.add_argument("--silicon-apps", help="the silicon-apps executable (default: SILICON_APPS, then PATH)")
    args = parser.parse_args(argv)
    if args.discovery not in ("auto", "require"):
        parser.error(f"--discovery (or PACKAGE_DISCOVERY) must be auto or require, not {args.discovery!r}")
    try:
        if args.check_only:
            check(args.version, args.target, args.binary, args.discovery)
        else:
            package(args.version, args.target, args.binary, args.output_dir.resolve(), args.discovery,
                    args.silicon_apps)
    except (PackageError, OSError, subprocess.SubprocessError) as error:
        print(f"package-apps: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
