# A Linux desktop for exercising Silicon Extend's Linux agent for real: Xvfb, a light window
# manager, the AT-SPI accessibility bus, xdotool, screenshot and clipboard tools, a GTK app
# (GNOME Calculator), Node 22 for agent-device, and the Rust toolchain to build extend-agent.
#
# The repository is mounted at /src at run time (see run.sh); nothing is baked in, so one image
# serves every run and cargo's caches live in named volumes.
#
#   docker build -t silicon-extend-linux-e2e apps/desktop/linux-e2e
#   apps/desktop/linux-e2e/run.sh
FROM debian:trixie

ARG DEBIAN_FRONTEND=noninteractive
ARG RUST_TOOLCHAIN=1.98.0
ARG NODE_MAJOR=22

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils git build-essential pkg-config cmake clang libssl-dev \
      libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev libayatana-appindicator3-dev \
      xvfb x11-utils openbox dbus dbus-x11 at-spi2-core \
      python3 python3-gi gir1.2-atspi-2.0 gir1.2-gtk-3.0 \
      xdotool imagemagick scrot xclip xdg-utils wmctrl \
      gnome-calculator fonts-dejavu-core procps jq ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# Node 22 LTS (agent-device needs 22.12 or newer; Debian ships 20).
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; case "$arch" in amd64) n=x64;; arm64) n=arm64;; *) echo "unsupported $arch"; exit 1;; esac; \
    file="$(curl -fsSL "https://nodejs.org/dist/latest-v${NODE_MAJOR}.x/" | grep -o "node-v${NODE_MAJOR}[.0-9]*-linux-${n}.tar.xz" | head -1)"; \
    curl -fsSL "https://nodejs.org/dist/latest-v${NODE_MAJOR}.x/${file}" | tar -xJ -C /usr/local --strip-components=1; \
    node --version

# Rust, pinned like rust-toolchain.toml.
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_TOOLCHAIN} -c clippy \
    && rustc --version

# A Carbon's desktop session runs as a normal user.
RUN useradd -m -s /bin/bash carbon
ENV DISPLAY=:99 NO_AT_EXTEND=0 GTK_A11Y=atspi
WORKDIR /home/carbon
