#!/usr/bin/env python3
"""Writes app/src/main/assets/open_source_licences.txt, the notices shown in the app.

Run from apps/android after changing dependencies:

    JAVA_HOME=... python3 tools/notices/generate_notices.py

It lists every artifact on the release runtime classpath with the licence its POM declares (from
the local Gradle cache), adds the vendored libadb and the native code inside dependencies, and
appends the full text of each licence. It fails when a dependency has a licence it doesn't know,
so a new dependency can't ship without its notice.
"""
import argparse
import glob
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
TEXTS = os.path.join(HERE, "texts")
LIBADB = os.path.join(ROOT, "vendor", "libadb")
OUT = os.path.join(ROOT, "app", "src", "main", "assets", "open_source_licences.txt")
CACHE = os.path.expanduser("~/.gradle/caches/modules-2/files-2.1")

APACHE = "Apache-2.0"
KNOWN = {
    APACHE: re.compile(r"apache", re.I),
    "Bouncy Castle": re.compile(r"bouncy castle", re.I),
    "LGPL-3.0": re.compile(r"lesser general public license v3|lgpl-3", re.I),
}
# Artifacts that are not shipped code (a BOM only aligns versions).
NOT_SHIPPED = {"androidx.compose:compose-bom"}


def runtime_coordinates(deps_file):
    if deps_file:
        text = open(deps_file, encoding="utf-8").read()
    else:
        text = subprocess.run(
            ["./gradlew", ":app:dependencies", "--configuration", "releaseRuntimeClasspath", "-q"],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
    coords = set()
    for m in re.finditer(r"([\w.\-]+):([\w.\-]+):([\w.\-]+)(?: -> ([\w.\-]+))?", text):
        group, artifact, version, resolved = m.groups()
        if f"{group}:{artifact}" in NOT_SHIPPED:
            continue
        coords.add((group, artifact, resolved or version))
    return sorted(coords)


def pom(group, artifact, version):
    found = glob.glob(f"{CACHE}/{group}/{artifact}/{version}/*/{artifact}-{version}.pom")
    return found[0] if found else None


def licences(path, depth=0):
    if not path or depth > 5:
        return []
    text = open(path, encoding="utf-8", errors="replace").read()
    text = re.sub(r'xmlns="[^"]+"', "", text, count=1)
    root = ET.fromstring(text)
    names = [(l.findtext("name") or "").strip() for l in root.findall("./licenses/license")]
    if names:
        return names
    parent = root.find("parent")
    if parent is not None:
        return licences(pom(parent.findtext("groupId"), parent.findtext("artifactId"), parent.findtext("version")), depth + 1)
    return []


def classify(names):
    for key, pattern in KNOWN.items():
        if any(pattern.search(n) for n in names):
            return key
    return None


def licence_text(name):
    if name == APACHE:
        return spdx_body(os.path.join(LIBADB, "LICENSES", "Apache-2.0"))
    if name == "GPL-3.0":
        return spdx_body(os.path.join(LIBADB, "LICENSES", "GPL-3.0-or-later"))
    files = {"LGPL-3.0": "LGPL-3.0.txt", "LGPL-2.1": "LGPL-2.1.txt", "Bouncy Castle": "BouncyCastle.txt", "ISC": "ISC.txt", "OFL-1.1": "OFL-1.1.txt"}
    return open(os.path.join(TEXTS, files[name]), encoding="utf-8").read().strip("\n")


def spdx_body(path):
    text = open(path, encoding="utf-8").read()
    return text.split("License-Text:", 1)[-1].strip("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deps", help="output of `gradlew :app:dependencies --configuration releaseRuntimeClasspath`")
    parser.add_argument("--check", action="store_true", help="fail if the committed file is out of date")
    args = parser.parse_args()

    by_licence = defaultdict(list)
    unknown = []
    for group, artifact, version in runtime_coordinates(args.deps):
        names = licences(pom(group, artifact, version))
        kind = classify(names)
        if kind is None:
            unknown.append(f"{group}:{artifact}:{version} {names}")
        else:
            by_licence[kind].append(f"{group}:{artifact}:{version}")
    if unknown:
        sys.exit("No known licence for:\n  " + "\n  ".join(unknown) + "\nAdd its licence to tools/notices/generate_notices.py.")

    spake2 = next(c for c in by_licence["LGPL-3.0"] if c.startswith("com.github.MuntashirAkon.spake2-java:"))
    out = []
    w = out.append
    w("OPEN-SOURCE LICENCES")
    w("")
    w("Silicon Extend for Android includes the open-source software below. Each part is used under")
    w("the licence named for it; the full licence texts follow the list.")
    w("")
    w("=" * 72)
    w("libadb-android 3.1.1 (vendored and modified)")
    w("=" * 72)
    w("Source: https://github.com/MuntashirAkon/libadb-android, tag 3.1.1")
    w("(commit c849886ebc6d48e7b46d967e78a6bb65c90c3b74).")
    w("Copyright (C) Muntashir Al-Islam. libadb-android is offered under")
    w("\"GPL-3.0-or-later OR Apache-2.0\"; Silicon Extend uses it under the Apache License 2.0.")
    w("Parts of it are also under these licences (texts below):")
    w("  BSD-3-Clause: Copyright 2013 Cameron Gutman (AdbConnection, AdbProtocol, AdbStream);")
    w("                Copyright 2020 Sam Palmer (AdbAuthenticationFailedException).")
    w("  MIT:          Copyright 2013 Google Inc. (PRNGFixes).")
    w("Silicon Extend changed AdbConnection.java, AdbStream.java, AdbProtocol.java,")
    w("AbsAdbConnectionManager.java and PairingConnectionCtx.java; each says so at its top.")
    w("")
    w("=" * 72)
    w(f"spake2-android {spake2.rsplit(':', 1)[1]} (SPAKE2 for Wireless debugging pairing)")
    w("=" * 72)
    w("Copyright 2021 Muntashir Al-Islam. Licensed under the GNU Lesser General Public License")
    w("version 3 (LGPL-3.0), which adds permissions to the GNU General Public License version 3.")
    w("Its native library is built from spake2-c (https://github.com/MuntashirAkon/spake2-c),")
    w("LGPL-3.0 or later; its README says portions are under the Apache License 2.0, LGPL-2.1")
    w("(its SHA-512 code) and MIT. All these texts are below.")
    w("Source: https://github.com/MuntashirAkon/spake2-java, tag v2.2.1, published by JitPack as")
    w(f"{spake2}. Silicon Extend uses it unmodified.")
    w("How it is included:")
    w("  - Its native code is a separate shared library in the APK, lib/<abi>/libspake2.so,")
    w("    which Android loads at run time (System.loadLibrary(\"spake2\")).")
    w("  - Its Java classes, io.github.muntashirakon.crypto.spake2.Spake2Context and Spake2Role,")
    w("    are in the APK's classes.dex without obfuscation.")
    w("  - You may replace either with a modified version that keeps the same interface: unpack")
    w("    the APK, replace lib/<abi>/libspake2.so and/or those classes, sign the APK with your own")
    w("    key and install it (Android installs a re-signed app after the original is removed).")
    w("  - LGPL-3.0 section 4 lets you modify these portions and reverse engineer them to debug")
    w("    such modifications.")
    w("")
    w("=" * 72)
    w("Conscrypt 2.5.3, including BoringSSL")
    w("=" * 72)
    w("Conscrypt (https://github.com/google/conscrypt) is licensed under the Apache License 2.0.")
    w("Its native library, lib/<abi>/libconscrypt_jni.so, includes BoringSSL")
    w("(https://boringssl.googlesource.com/boringssl): code from the OpenSSL project, Copyright (c)")
    w("1998-2011 The OpenSSL Project and Copyright (c) 1995-1998 Eric Young, now under the Apache")
    w("License 2.0, and code Copyright (c) 2014-2024 Google Inc. under the ISC licence (text below).")
    w("")
    w("=" * 72)
    w("Bouncy Castle")
    w("=" * 72)
    for c in sorted(by_licence["Bouncy Castle"]):
        w(f"  {c}")
    w("Licensed under the Bouncy Castle Licence (text below).")
    w("")
    w("=" * 72)
    w("Fonts (app/src/main/res/font, unmodified)")
    w("=" * 72)
    w("Each is licensed under the SIL Open Font License, Version 1.1 (text below).")
    w("  IBM Plex Sans 3.201 (variable), ibm_plex_sans.ttf, from Google Fonts")
    w("    (https://github.com/google/fonts/tree/main/ofl/ibmplexsans).")
    w("  IBM Plex Mono 2.3, Regular and Medium, ibm_plex_mono_regular.ttf and")
    w("    ibm_plex_mono_medium.ttf, from Google Fonts")
    w("    (https://github.com/google/fonts/tree/main/ofl/ibmplexmono).")
    w("    IBM Plex: Copyright © 2017 IBM Corp. with Reserved Font Name \"Plex\".")
    w("  Source Serif 4 4.005, Regular, source_serif_4_regular.ttf, from Adobe")
    w("    (https://github.com/adobe-fonts/source-serif, release branch, TTF/).")
    w("    Copyright 2014 - 2023 Adobe (http://www.adobe.com/), with Reserved Font Name")
    w("    ‘Source’. All Rights Reserved. Source is a trademark of Adobe in the United States")
    w("    and/or other countries.")
    w("")
    w("=" * 72)
    w("Licensed under the Apache License 2.0")
    w("=" * 72)
    w("AndroidX and Jetpack Compose (Copyright The Android Open Source Project), Kotlin and")
    w("kotlinx (Copyright JetBrains s.r.o. and Kotlin Programming Language contributors),")
    w("OkHttp and Okio (Copyright Square, Inc.), Guava (Copyright The Guava Authors), JSpecify")
    w("(Copyright The JSpecify Authors) and JetBrains annotations. Artifacts:")
    for c in sorted(by_licence[APACHE]):
        w(f"  {c}")
    w("OkHttp includes publicsuffixes.gz, compiled from the Public Suffix List")
    w("(https://publicsuffix.org/list/public_suffix_list.dat, its source form). It is subject to")
    w("the Mozilla Public License 2.0: https://mozilla.org/MPL/2.0/")
    w("")
    for name, title in [
        (APACHE, "Apache License 2.0"),
        ("BSD-3-Clause", "BSD 3-Clause License (for the libadb parts named above; <year> <owner> as listed there)"),
        ("MIT", "MIT License (PRNGFixes in libadb: Copyright 2013 Google Inc.; portions of spake2-c)"),
        ("ISC", "ISC License (for BoringSSL code in Conscrypt: Copyright (c) 2014-2024 Google Inc.)"),
        ("Bouncy Castle", "Bouncy Castle Licence"),
        ("LGPL-3.0", "GNU Lesser General Public License version 3 (spake2-android)"),
        ("GPL-3.0", "GNU General Public License version 3 (the base of LGPL-3.0)"),
        ("LGPL-2.1", "GNU Lesser General Public License version 2.1 (portions of spake2-c)"),
        ("OFL-1.1", "SIL Open Font License 1.1 (IBM Plex Sans, IBM Plex Mono, Source Serif 4)"),
    ]:
        w("")
        w("#" * 72)
        w(title)
        w("#" * 72)
        w("")
        if name in ("BSD-3-Clause", "MIT"):
            w(spdx_body(os.path.join(LIBADB, "LICENSES", name)))
        else:
            w(licence_text(name))
    text = "\n".join(out).rstrip() + "\n"
    if args.check:
        current = open(OUT, encoding="utf-8").read() if os.path.exists(OUT) else ""
        if current != text:
            sys.exit(f"{OUT} is out of date; run tools/notices/generate_notices.py")
        return
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    open(OUT, "w", encoding="utf-8").write(text)
    print(f"Wrote {OUT}: {sum(len(v) for v in by_licence.values())} artifacts")


if __name__ == "__main__":
    main()
