# Third-party notices

Silicon Extend's own code is licensed under the MIT licence in [`LICENSE`](LICENSE) (Copyright
Team of Silicons), which is also the licence `Cargo.toml` declares for the Rust crates. This file
lists the third-party software in this repository and in what Extend ships, and the licence each
part is used under. It was last generated on 2026-09-27 from the working tree; see
[Keeping this file current](#keeping-this-file-current).

What ships where:

| Artifact | Third-party parts it carries | Where its notices are |
|---|---|---|
| `extend` CLI (Honeycomb archive, six targets) | Rust crates | `licences/` in every target of the archive (`targets/<target>/licences/`): `LICENSE`, this file, and `THIRD_PARTY_LICENSES.txt` (every Rust crate's licence text) |
| Extend service (container image) | Rust crates, including `silicon-iam-client` | `/usr/share/doc/silicon-extend/`: `LICENSE`, this file, `THIRD_PARTY_LICENSES.txt` |
| Mac app (`Silicon Extend.app`) | Rust crates, the agent-device fork and its bundled JavaScript packages, Node.js 22.23.3, fonts inlined in the window | `Contents/Resources/`: `LICENSE`, this file, `THIRD_PARTY_LICENSES.txt`, `agent-device/LICENSE`, `node/LICENSE` |
| Linux tarball and `.deb` | Rust crates, the agent-device fork (including its `linux/` Python workers) and its bundled JavaScript packages, Node.js 22.23.3, fonts inlined in the window | `share/doc/silicon-extend/` (`LICENSE`, this file, `THIRD_PARTY_LICENSES.txt`), `lib/silicon-extend/agent-device/LICENSE`, `lib/silicon-extend/node/LICENSE` |
| Windows zip | Rust crates, fonts inlined in the window (no Node.js, no agent-device) | `LICENSE`, this file and `THIRD_PARTY_LICENSES.txt` beside the executable |
| Android APK | libadb-android, spake2-android, Conscrypt/BoringSSL, Bouncy Castle, AndroidX/Compose, Kotlin, OkHttp/Okio, fonts | The app's **Open-source licences** screen: `apps/android/app/src/main/assets/open_source_licences.txt`, with the full licence texts |
| Configuration website | SolidJS, Lucide icons, fonts | [`/licences.txt`](https://extend.teamofsilicons.com/licences.txt): Extend's licence and the full text of each package the site ships (`web/scripts/gen-licences.mjs`, run by every build) |

## Forked and vendored components

### agent-device (fork)

- Upstream: https://github.com/callstack/agent-device, forked at `bce6f52` (v0.21.15).
- Licence: **MIT**, Copyright (c) 2026 Callstack. Text: [`vendor/agent-device/LICENSE`](vendor/agent-device/LICENSE),
  shipped next to the runtime in the Mac and Linux packages.
- Modified by Silicon Extend. Every change is listed, newest first, in
  [`vendor/agent-device/FORK.md`](vendor/agent-device/FORK.md).
- Its build (`dist/`) inlines these npm packages (the fork's `tsdown.config.ts`, `deps.onlyBundle`),
  so they ship inside the Mac and Linux packages:

  | Package | Version | Licence |
  |---|---|---|
  | `@limrun/api` | 0.49.3 | Apache-2.0 |
  | `agent-base` | 7.1.4 | MIT |
  | `b4a` | 1.8.1 | Apache-2.0 |
  | `debug` | 4.4.3 | MIT |
  | `events-universal` | 1.0.1 | Apache-2.0 |
  | `eventsource-client` | 1.2.0 | MIT |
  | `eventsource-parser` | 3.1.1 | MIT |
  | `fast-fifo` | 1.3.2 | MIT |
  | `has-flag` | 4.0.0 | MIT |
  | `https-proxy-agent` | 7.0.6 | MIT |
  | `ignore` | 5.3.2, 7.0.6 | MIT |
  | `ipaddr.js` | 2.5.0 | MIT |
  | `jpeg-js` | 0.4.4 | BSD-3-Clause |
  | `ms` | 2.1.3 | MIT |
  | `pend` | 1.2.0 | MIT |
  | `pngjs` | 7.0.0 | MIT |
  | `proxy-from-env` | 2.1.0 | MIT |
  | `streamx` | 2.28.0 | MIT |
  | `supports-color` | 7.2.0 | MIT |
  | `tar-stream` | 3.2.0 | MIT |
  | `text-decoder` | 1.2.7 | Apache-2.0 |
  | `undici` | 7.29.0 | MIT |
  | `undici-types` | 6.21.0, 8.3.0 | MIT |
  | `ws` | 8.21.3 | MIT |
  | `yaml` | 2.9.0 | ISC |
  | `yauzl` | 3.4.0 | MIT |

  Versions are the ones installed in `vendor/agent-device/node_modules` on 2026-09-27; `pnpm-lock.yaml`
  is the authority. The fork's other workspace packages (`@agent-device/*`) are part of the fork itself.

### Node.js 22.23.3 (bundled in the Mac and Linux packages)

- Official release binary from https://nodejs.org/dist/v22.23.3/, checked against the pinned
  SHA-256 in `apps/desktop/macos/node-sha256.txt` and `apps/desktop/linux/node-sha256.txt`.
- Licence: **MIT** for Node.js itself. The `node` binary also contains third-party code (among it
  V8, libuv, OpenSSL, ICU, zlib, c-ares, llhttp, nghttp2 and Brotli) under the licences its
  `LICENSE` file lists. Packaging copies that `LICENSE` next to the binary (`node/LICENSE`) and
  ships nothing else from the Node.js distribution (no npm).

### libadb-android 3.1.1 (vendored and modified, Android app)

- Upstream: https://github.com/MuntashirAkon/libadb-android, tag 3.1.1, commit
  `c849886ebc6d48e7b46d967e78a6bb65c90c3b74`. Copyright (C) Muntashir Al-Islam.
- Offered under **GPL-3.0-or-later OR Apache-2.0**; Silicon Extend uses it under the
  **Apache License 2.0**. Parts are also under BSD-3-Clause (Copyright 2013 Cameron Gutman;
  Copyright 2020 Sam Palmer) and MIT (Copyright 2013 Google Inc.).
- Texts: [`apps/android/vendor/libadb/COPYING`](apps/android/vendor/libadb/COPYING) and
  [`apps/android/vendor/libadb/LICENSES/`](apps/android/vendor/libadb/LICENSES).
- Modified by Silicon Extend (`AdbConnection`, `AdbStream`, `AdbProtocol`,
  `AbsAdbConnectionManager`, `PairingConnectionCtx`); each changed file says so at its top, and
  [`apps/android/vendor/libadb/README.extend.md`](apps/android/vendor/libadb/README.extend.md)
  lists the changes.

### spake2-android 2.2.1 (Android app)

- `com.github.MuntashirAkon.spake2-java:spake2-android:2.2.1` from JitPack, built from
  https://github.com/MuntashirAkon/spake2-java tag v2.2.1; its native library comes from
  https://github.com/MuntashirAkon/spake2-c. Copyright 2021 Muntashir Al-Islam.
- Licence: **LGPL-3.0** (spake2-c: LGPL-3.0 or later, with portions under Apache-2.0, LGPL-2.1
  and MIT). Used unmodified.
- How LGPL-3.0 section 4 is met: `libspake2.so` stays a separate shared library loaded at run
  time, its Java classes are not obfuscated (the app is not minified), the in-app notices name the
  source, and they explain how to replace either part and re-sign the APK. Its checksum is pinned
  in `apps/android/gradle/verification-metadata.xml`. Whether this approach is sufficient is
  listed for the Carbon's confirmation in `docs/completion-work.md`.

### silicon-iam-client 4.0.0 (Extend service)

- Team of Silicons' own Silicon IAM client crate (https://github.com/teamofsilicons/silicon-iam),
  used from crates.io (https://crates.io/crates/silicon-iam-client).
- Licence: **Apache-2.0**; its text is in `THIRD_PARTY_LICENSES.txt`.

## Fonts

All three families are licensed under the **SIL Open Font License, Version 1.1**. They are used
unmodified (the website and the desktop window use latin subsets).

| Family | Copyright | Used in |
|---|---|---|
| IBM Plex Sans, IBM Plex Mono | Copyright © 2017 IBM Corp. with Reserved Font Name "Plex" | Website (`@fontsource/ibm-plex-sans` and `@fontsource/ibm-plex-mono` 5.3.0), desktop window (`crates/extend-agent/src/ui/page.html`, inlined from the same packages), Android (`apps/android/app/src/main/res/font/`) |
| Source Serif 4 | Copyright 2014 - 2023 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source' | Website (`@fontsource/source-serif-4` 5.3.0), desktop window (inlined), Android (`source_serif_4_regular.ttf`) |

The OFL 1.1 text is in the Android notices file (`apps/android/app/src/main/assets/open_source_licences.txt`)
and at https://openfontlicense.org. The website ships it in `/licences.txt`. The desktop window does
not yet ship the licence text next to the fonts (open gate in `docs/completion-work.md`).

## Android dependencies

The APK's full list, with versions and licence texts, is generated by
`apps/android/tools/notices/generate_notices.py` into
`apps/android/app/src/main/assets/open_source_licences.txt` and shown in the app. In summary:

| Component | Licence |
|---|---|
| libadb-android 3.1.1 (modified) | Apache-2.0 (elected from GPL-3.0-or-later OR Apache-2.0), with BSD-3-Clause and MIT parts |
| spake2-android 2.2.1 | LGPL-3.0 |
| Conscrypt 2.5.3, including BoringSSL | Apache-2.0; BoringSSL's OpenSSL-derived code under Apache-2.0 and Google's code under ISC |
| Bouncy Castle `bcprov`/`bcpkix` 1.81 | Bouncy Castle Licence (MIT-style) |
| AndroidX and Jetpack Compose, Kotlin and kotlinx, OkHttp and Okio, Guava `listenablefuture`, JSpecify, JetBrains annotations | Apache-2.0 |
| OkHttp's bundled Public Suffix List data | MPL-2.0 |
| IBM Plex Sans, IBM Plex Mono, Source Serif 4 | OFL-1.1 |

## Website runtime dependencies

| Package | Version | Licence |
|---|---|---|
| `solid-js` | 1.9.15 | MIT |
| `lucide-solid` | 1.48.0 | ISC |
| `@fontsource/ibm-plex-sans`, `@fontsource/ibm-plex-mono`, `@fontsource/source-serif-4` | 5.3.0 | OFL-1.1 |

Everything else in `web/package.json` is a build or test tool and is not shipped.

## Rust crates

The crates below are reachable through normal or build dependencies from the workspace crates
(`silicon-extend-cli`, `extend-service`, `extend-agent`, `extend-hosted`, `extend-driver`,
`silicon-extend-protocol` and `silicon-extend-client`) on any
platform: 582 crates. It is an over-approximation
of any one binary: it includes Windows-, macOS- and Linux-only crates and build-time crates (such as
`cc` and `cmake`) that are not linked in. Per binary: `extend` (the CLI) 220, `extend-service` 334,
`extend-agent` 527. Where a crate offers a choice (`A OR B`), Extend uses it under the permissive
option (MIT or Apache-2.0 where offered).

Crates to note:

- **MPL-2.0** (file-level copyleft): `cssparser`, `cssparser-macros`, `dtoa-short`, `selectors`
  and `option-ext`, all through `wry` (the desktop window's webview crate) in `extend-agent`.
  Used unmodified; their source is on crates.io.
- **Unicode-3.0**: the ICU4X crates (`icu_*`, `zerovec`, `yoke`, `tinystr`, …), through URL and
  IDNA handling.
- **CDLA-Permissive-2.0**: `webpki-roots` and `webpki-root-certs` (Mozilla's root certificate data).
- **OpenSSL/ISC/BSD-derived crypto**: `ring`, `aws-lc-rs`/`aws-lc-sys`, `rustls-webpki`,
  `untrusted`, `curve25519-dalek`, `ed25519-dalek`, `x25519-dalek`, `subtle`.

By licence expression (as `cargo metadata` reports it, with `A/B` spelled `A OR B`):

**Apache-2.0 OR MIT** (363): aead 0.5.2, aes 0.9.3, allocator-api2 0.2.21, android_system_properties 0.1.6, anstream 1.0.0, anstyle 1.0.14, anstyle-parse 1.0.0, anstyle-query 1.1.5, anstyle-wincon 3.0.11, anyhow 1.0.104, apple-native-keyring-store 1.0.2, async-broadcast 0.7.2, async-channel 2.5.0, async-executor 1.14.0, async-io 2.6.0, async-lock 3.4.2, async-process 2.5.0, async-recursion 1.1.1, async-signal 0.2.14, async-task 4.7.1, async-trait 0.1.92, atomic-waker 1.1.2, autocfg 1.5.1, base64 0.22.1, base64 0.23.1, base64ct 1.8.3, bit-set 0.8.0, bit-vec 0.8.0, bitflags 1.3.2, bitflags 2.13.2, block-buffer 0.10.4, block-buffer 0.12.1, block-padding 0.4.2, blocking 1.7.0, bumpalo 3.20.3, cbc 0.2.1, cc 1.5.1, cesu8 1.1.0, cfg-expr 0.15.8, cfg-if 1.0.5, chacha20 0.10.2, chacha20 0.9.1, chacha20poly1305 0.10.1, cipher 0.4.4, cipher 0.5.2, clap 4.6.7, clap_builder 4.6.7, clap_derive 4.6.7, clap_lex 1.1.1, cmake 0.1.58, cmov 0.5.4, colorchoice 1.0.5, concurrent-queue 2.5.0, const-oid 0.10.2, const-oid 0.9.6, cookie 0.18.2, cookie_store 0.22.1, core-foundation 0.10.1, core-foundation-sys 0.8.7, core-graphics 0.25.0, core-graphics-types 0.2.0, cpubits 0.1.1, cpufeatures 0.2.17, cpufeatures 0.3.1, crc 3.4.0, crc-catalog 2.5.0, crc32fast 1.5.2, crossbeam-channel 0.5.17, crossbeam-queue 0.3.14, crossbeam-utils 0.8.23, crypto-common 0.1.7, crypto-common 0.2.2, ctutils 0.4.2, curve25519-dalek-derive 0.1.1, dbus 0.9.12, der 0.7.10, deranged 0.5.8, digest 0.10.7, digest 0.11.3, dirs 7.0.0, dirs-sys 0.5.0, displaydoc 0.2.7, document-features 0.2.12, dtoa 1.0.11, ed25519 2.2.3, either 1.18.0, enumflags2 0.7.12, enumflags2_derive 0.7.12, equivalent 1.0.2, errno 0.3.14, etcetera 0.11.0, event-listener 5.4.2, event-listener-strategy 0.5.4, fastrand 2.5.0, fdeflate 0.3.7, field-offset 0.3.6, find-msvc-tools 0.1.14, flate2 1.1.10, flume 0.11.1, flume 0.12.0, fnv 1.0.7, foreign-types 0.3.2, foreign-types 0.5.0, foreign-types-macros 0.2.4, foreign-types-shared 0.1.1, foreign-types-shared 0.3.1, form_urlencoded 1.2.2, futures 0.3.34, futures-channel 0.3.34, futures-core 0.3.34, futures-executor 0.3.34, futures-intrusive 0.5.0, futures-io 0.3.34, futures-lite 2.6.1, futures-macro 0.3.34, futures-sink 0.3.34, futures-task 0.3.34, futures-util 0.3.34, getrandom 0.2.17, getrandom 0.3.4, getrandom 0.4.3, hashbrown 0.16.1, hashbrown 0.17.1, hashlink 0.11.1, heck 0.4.1, heck 0.5.0, hermit-abi 0.5.3, hex 0.4.3, hkdf 0.12.4, hkdf 0.13.0, hmac 0.12.1, hmac 0.13.0, html5ever 0.39.0, http 1.5.0, httparse 1.10.1, httpdate 1.0.3, hybrid-array 0.4.15, hyper-tls 0.6.0, idna 1.1.0, idna_adapter 1.2.2, indexmap 2.14.2, inout 0.1.4, inout 0.2.2, ipnet 2.12.2, is_terminal_polyfill 1.70.2, itoa 1.0.18, jni 0.21.1, jni 0.22.4, jni-macros 0.22.4, jni-sys 0.3.1, jni-sys 0.4.1, jni-sys-macros 0.4.1, jobserver 0.1.35, js-sys 0.3.106, keyboard-types 0.8.3, keyring 4.2.0, keyring-core 1.0.0, lazy_static 1.5.0, libappindicator 0.9.0, libappindicator-sys 0.9.0, libc 0.2.189, libdbus-sys 0.2.7, litrs 1.0.0, lock_api 0.4.14, log 0.4.34, markup5ever 0.39.0, md-5 0.11.0, mdns-sd 0.13.11, mime 0.3.17, muda 0.20.0, native-tls 0.2.18, ndk 0.9.0, ndk-context 0.1.1, ndk-sys 0.6.0+11769913, ntapi 0.4.3, num 0.4.3, num-bigint 0.4.8, num-complex 0.4.6, num-conv 0.2.2, num-integer 0.1.47, num-iter 0.1.46, num-rational 0.4.2, num-traits 0.2.19, once_cell 1.21.4, once_cell_polyfill 1.70.2, opaque-debug 0.3.1, openssl-macros 0.1.1, openssl-probe 0.2.1, ordered-stream 0.2.0, parking 2.2.1, parking_lot 0.12.5, parking_lot_core 0.9.12, percent-encoding 2.3.2, pin-project-lite 0.2.17, piper 0.2.5, pkcs8 0.10.2, pkg-config 0.3.34, png 0.18.1, polling 3.11.0, poly1305 0.8.0, powerfmt 0.2.0, ppv-lite86 0.2.21, proc-macro-crate 1.3.1, proc-macro-crate 2.0.2, proc-macro-crate 3.5.0, proc-macro-error 1.0.4, proc-macro-error-attr 1.0.4, proc-macro2 1.0.107, quinn 0.11.12, quinn-proto 0.11.18, quinn-udp 0.5.15, quote 1.0.47, rand 0.10.3, rand 0.9.5, rand_chacha 0.9.0, rand_core 0.10.1, rand_core 0.6.4, rand_core 0.9.5, rand_pcg 0.10.2, regex 1.13.1, regex-automata 0.4.18, regex-syntax 0.8.11, reqwest 0.13.5, rustc-hash 2.1.3, rustc_version 0.4.1, rustls-pki-types 1.15.1, rustls-platform-verifier 0.7.1, rustls-platform-verifier-android 0.2.0, rustversion 1.0.23, scopeguard 1.2.0, secrecy 0.10.3, secret-service 5.2.0, security-framework 3.7.0, security-framework-sys 2.17.0, semver 1.0.28, serde 1.0.229, serde_core 1.0.229, serde_derive 1.0.229, serde_json 1.0.151, serde_path_to_error 0.1.20, serde_repr 0.1.21, serde_spanned 0.6.9, serde_urlencoded 0.7.1, serde_with 3.23.0, servo_arc 0.4.3, sha1 0.10.7, sha1 0.11.0, sha2 0.10.9, sha2 0.11.0, shlex 2.0.1, signal-hook-registry 1.4.8, signature 2.2.0, simd_cesu8 1.2.0, simdutf8 0.1.5, siphasher 1.0.4, smallvec 1.16.2, socket2 0.5.10, socket2 0.6.5, spki 0.7.3, sqlx 0.9.0, sqlx-core 0.9.0, sqlx-macros 0.9.0, sqlx-macros-core 0.9.0, sqlx-mysql 0.9.0, sqlx-postgres 0.9.0, sqlx-sqlite 0.9.0, stable_deref_trait 1.2.1, string_cache 0.9.0, string_cache_codegen 0.6.1, stringprep 0.1.5, syn 1.0.109, syn 2.0.119, syn 3.0.6, system-deps 6.2.2, tao-macros 0.1.4, tempfile 3.27.0, tendril 0.5.1, terminal_size 0.4.4, thiserror 1.0.69, thiserror 2.0.21, thiserror-impl 1.0.69, thiserror-impl 2.0.21, thread_local 1.1.10, time 0.3.55, time-core 0.1.9, time-macros 0.2.32, tokio-rustls 0.26.5, toml 0.8.2, toml_datetime 0.6.3, toml_datetime 1.1.1+spec-1.1.0, toml_edit 0.19.15, toml_edit 0.20.2, toml_edit 0.25.15+spec-1.1.0, toml_parser 1.1.3+spec-1.1.0, tray-icon 0.25.1, tungstenite 0.29.0, tungstenite 0.30.0, typenum 1.20.1, unicase 2.9.0, unicode-bidi 0.3.18, unicode-normalization 0.1.25, unicode-properties 0.1.4, unicode-segmentation 1.13.3, universal-hash 0.5.1, ureq 3.4.2, ureq-proto 0.6.4, url 2.5.8, utf8-zero 0.8.1, utf8_iter 1.0.4, utf8parse 0.2.2, uuid 1.26.1, vcpkg 0.2.15, version_check 0.9.5, wasm-bindgen 0.2.129, wasm-bindgen-futures 0.4.79, wasm-bindgen-macro 0.2.129, wasm-bindgen-macro-support 0.2.129, wasm-bindgen-shared 0.2.129, wasm-streams 0.5.0, web-sys 0.3.106, web-time 1.1.0, web_atoms 0.2.6, winapi 0.3.9, winapi-i686-pc-windows-gnu 0.4.0, winapi-x86_64-pc-windows-gnu 0.4.0, windows 0.62.2, windows-collections 0.3.2, windows-core 0.62.2, windows-future 0.3.2, windows-implement 0.60.2, windows-interface 0.59.3, windows-link 0.2.1, windows-native-keyring-store 1.1.0, windows-numerics 0.3.1, windows-result 0.4.1, windows-strings 0.5.1, windows-sys 0.45.0, windows-sys 0.52.0, windows-sys 0.59.0, windows-sys 0.61.2, windows-targets 0.42.2, windows-targets 0.52.6, windows-threading 0.2.1, windows-version 0.1.7, windows_aarch64_gnullvm 0.42.2, windows_aarch64_gnullvm 0.52.6, windows_aarch64_msvc 0.42.2, windows_aarch64_msvc 0.52.6, windows_i686_gnu 0.42.2, windows_i686_gnu 0.52.6, windows_i686_gnullvm 0.52.6, windows_i686_msvc 0.42.2, windows_i686_msvc 0.52.6, windows_x86_64_gnu 0.42.2, windows_x86_64_gnu 0.52.6, windows_x86_64_gnullvm 0.42.2, windows_x86_64_gnullvm 0.52.6, windows_x86_64_msvc 0.42.2, windows_x86_64_msvc 0.52.6, wry 0.57.0, zbus-secret-service-keyring-store 1.0.1, zeroize 1.9.0, zeroize_derive 1.5.0.

**MIT** (125): atk 0.18.2, atk-sys 0.18.2, atoi 2.0.0, axum 0.8.9, axum-core 0.5.6, axum-macros 0.5.1, block2 0.6.2, bytes 1.12.1, cairo-rs 0.18.5, cairo-sys-rs 0.18.2, cfg_aliases 0.2.2, combine 4.6.8, data-encoding 2.11.1, derive_more 2.1.1, derive_more-impl 2.1.1, dlopen2 0.8.2, dlopen2_derive 0.4.3, dom_query 0.28.0, dotenvy 0.15.7, endi 1.1.1, fs_extra 1.3.0, gdk 0.18.2, gdk-pixbuf 0.18.5, gdk-pixbuf-sys 0.18.0, gdk-sys 0.18.2, gdkwayland-sys 0.18.2, gdkx11 0.18.2, gdkx11-sys 0.18.2, generic-array 0.14.7, gio 0.18.4, gio-sys 0.18.1, glib 0.18.5, glib-macros 0.18.5, glib-sys 0.18.1, gobject-sys 0.18.0, gtk 0.18.2, gtk-sys 0.18.2, gtk3-macros 0.18.2, h2 0.4.19, http-body 1.1.0, http-body-util 0.1.5, http-range-header 0.4.2, hyper 1.11.1, hyper-util 0.1.21, javascriptcore-rs 1.1.2, javascriptcore-rs-sys 1.1.1, libredox 0.1.25, libsqlite3-sys 0.37.0, libxdo 0.6.0, libxdo-sys 0.11.0, matchers 0.2.0, memoffset 0.9.1, mime_guess 2.0.5, mio 1.2.3, new_debug_unreachable 1.0.6, nix 0.31.3, nu-ansi-term 0.50.3, objc2 0.6.4, objc2-encode 4.1.0, objc2-foundation 0.3.2, openssl-sys 0.9.117, os_info 3.15.0, pango 0.18.3, pango-sys 0.18.0, phf 0.13.1, phf_codegen 0.13.1, phf_generator 0.13.1, phf_macros 0.13.1, phf_shared 0.13.1, plist 1.10.1, precomputed-hash 0.1.1, quick-xml 0.42.0, redox_syscall 0.5.18, redox_users 0.5.3, schannel 0.1.29, sharded-slab 0.1.7, simd-adler32 0.3.10, slab 0.4.12, soup3 0.5.0, soup3-sys 0.5.0, space-station 0.1.1, space-station-shared 0.1.1, spin 0.9.9, strsim 0.11.1, synstructure 0.14.0, sysinfo 0.38.4, tokio 1.53.1, tokio-macros 2.7.2, tokio-native-tls 0.3.1, tokio-stream 0.1.19, tokio-tungstenite 0.29.0, tokio-util 0.7.19, tower 0.5.3, tower-http 0.6.11, tower-http 0.7.1, tower-layer 0.3.3, tower-service 0.3.3, tracing 0.1.44, tracing-attributes 0.1.31, tracing-core 0.1.36, tracing-log 0.2.0, tracing-serde 0.2.0, tracing-subscriber 0.3.23, try-lock 0.2.5, uds_windows 1.2.1, valuable 0.1.1, version-compare 0.2.1, want 0.3.1, webkit2gtk 2.0.2, webkit2gtk-sys 2.0.2, webview2-com 0.39.1, webview2-com-macros 0.8.1, webview2-com-sys 0.39.1, winnow 0.5.40, winnow 1.0.4, x11 2.21.0, x11-dl 2.21.0, zbus 5.19.0, zbus_macros 5.19.0, zbus_names 4.3.4, zcheapstr 1.1.0, zmij 1.0.23, zvariant 5.15.0, zvariant_derive 5.15.0, zvariant_utils 4.2.0.

**Apache-2.0 OR MIT OR Zlib** (21): dispatch2 0.3.1, lru-slab 0.1.3, miniz_oxide 0.8.9, miniz_oxide 0.9.1, objc2-app-kit 0.3.2, objc2-cloud-kit 0.3.2, objc2-core-data 0.3.2, objc2-core-foundation 0.3.2, objc2-core-graphics 0.3.2, objc2-core-image 0.3.2, objc2-core-location 0.3.2, objc2-core-text 0.3.2, objc2-exception-helper 0.1.1, objc2-io-kit 0.3.2, objc2-io-surface 0.3.2, objc2-quartz-core 0.3.2, objc2-ui-kit 0.3.2, objc2-user-notifications 0.3.2, objc2-web-kit 0.3.2, raw-window-handle 0.6.2, tinyvec 1.13.3.

**Unicode-3.0** (18): icu_collections 2.3.0, icu_locale_core 2.3.0, icu_normalizer 2.3.0, icu_normalizer_data 2.3.0, icu_properties 2.3.0, icu_properties_data 2.3.0, icu_provider 2.3.1, litemap 0.8.3, potential_utf 0.1.6, tinystr 0.8.4, writeable 0.6.4, yoke 0.8.3, yoke-derive 0.8.3, zerofrom 0.1.8, zerofrom-derive 0.1.8, zerotrie 0.2.5, zerovec 0.11.8, zerovec-derive 0.11.6.

**MIT OR Unlicense** (6): aho-corasick 1.1.5, byteorder 1.5.0, memchr 2.8.3, same-file 1.0.6, walkdir 2.5.0, winapi-util 0.1.11.

**Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT** (5): linux-raw-sys 0.12.1, rustix 1.1.5, wasi 0.11.1+wasi-snapshot-preview1, wasip2 1.0.4+wasi-0.2.12, wit-bindgen 0.57.1.

**MPL-2.0** (5): cssparser 0.37.0, cssparser-macros 0.7.1, dtoa-short 0.3.5, option-ext 0.2.0, selectors 0.38.0.

**Apache-2.0** (5): gethostname 1.1.0, openssl 0.10.81, silicon-iam-client 4.0.0, sync_wrapper 1.0.2, tao 0.37.0.

**BSD-3-Clause** (4): curve25519-dalek 4.1.3, ed25519-dalek 2.2.0, subtle 2.6.1, x25519-dalek 2.0.1.

**Apache-2.0 OR ISC OR MIT** (3): hyper-rustls 0.27.10, rustls 0.23.45, rustls-native-certs 0.8.4.

**CDLA-Permissive-2.0** (3): webpki-root-certs 1.0.9, webpki-roots 0.26.11, webpki-roots 1.0.9.

**ISC** (3): libloading 0.7.4, rustls-webpki 0.103.15, untrusted 0.9.0.

**Apache-2.0 OR BSD-2-Clause OR MIT** (2): zerocopy 0.8.59, zerocopy-derive 0.8.59.

**Apache-2.0 OR BSD-3-Clause OR MIT** (2): num_enum 0.7.6, num_enum_derive 0.7.6.

**Apache-2.0 OR LGPL-2.1-or-later OR MIT** (2): r-efi 5.3.0, r-efi 6.0.0.

**Zlib** (2): foldhash 0.2.0, zlib-rs 0.6.8.

**(MIT OR Apache-2.0) AND Unicode-3.0** (1): unicode-ident 1.0.26.

**0BSD OR Apache-2.0 OR MIT** (1): adler2 2.0.1.

**Apache-2.0 AND ISC** (1): ring 0.17.14.

**Apache-2.0 AND MIT** (1): dpi 0.1.2.

**Apache-2.0 OR BSD-1-Clause OR MIT** (1): fiat-crypto 0.2.9.

**Apache-2.0 OR BSL-1.0** (1): ryu 1.0.23.

**Apache-2.0 OR BSL-1.0 OR MIT** (1): whoami 2.1.3.

**Apache-2.0 OR CC0-1.0 OR MIT-0** (1): dunce 1.0.5.

**Apache-2.0 WITH LLVM-exception** (1): target-lexicon 0.12.16.

**BSD-3-Clause OR MIT** (1): if-addrs 0.13.4.

**ISC AND (Apache-2.0 OR ISC)** (1): aws-lc-rs 1.18.1.

**ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)** (1): aws-lc-sys 0.45.0.

**MIT AND BSD-3-Clause** (1): matchit 0.8.4.

## Keeping this file current

- **Rust crates:** regenerate the list from `cargo metadata --format-version 1 --locked`: walk the
  resolve graph from the workspace members through dependencies of kind normal or build, and group
  the `license` field of every non-workspace package. Check any new licence expression before
  shipping it. Then regenerate the licence texts that ship with the binaries:
  `cargo about generate about.hbs -o THIRD_PARTY_LICENSES.txt` (`about.toml` lists the accepted
  licences; a crate under any other licence stops the generation). The CLI archive, the service
  image and the desktop packages copy `LICENSE`, this file and `THIRD_PARTY_LICENSES.txt`
  (`scripts/package-cli.py`, `Dockerfile`, `apps/desktop/*/build-*`).
- **agent-device:** after rebuilding the fork, re-read `deps.onlyBundle` in
  `vendor/agent-device/tsdown.config.ts` and the licences of those packages; record fork changes in
  `vendor/agent-device/FORK.md`.
- **Android:** run `python3 tools/notices/generate_notices.py` in `apps/android` after any
  dependency change (it fails on a licence it does not know), and refresh
  `gradle/verification-metadata.xml` as `apps/android/README.md` describes.
- **Node.js:** a new pinned version brings a new `LICENSE`; packaging copies it automatically.
