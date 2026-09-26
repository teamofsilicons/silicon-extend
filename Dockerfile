# The Silicon Bridge service. Optionally serves the built website with BRIDGE_WEB_DIR=/srv/web.
FROM rust:1.98.0-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY vendor/silicon-iam-client ./vendor/silicon-iam-client
RUN cargo build --locked --release -p bridge-service --bin bridge-service

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/bridge --shell /usr/sbin/nologin bridge
COPY --from=builder /build/target/release/bridge-service /usr/local/bin/bridge-service
ENV BRIDGE_BIND=0.0.0.0:8080 \
    BRIDGE_DATA_DIR=/var/lib/bridge/data \
    BRIDGE_LOG_FORMAT=json
USER bridge
WORKDIR /var/lib/bridge
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s CMD curl -fsS http://127.0.0.1:8080/ready || exit 1
ENTRYPOINT ["/usr/local/bin/bridge-service"]
CMD ["serve"]
