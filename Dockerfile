# The Silicon Extend service. Optionally serves the built website with EXTEND_WEB_DIR=/srv/web.
FROM rust:1.98.0-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --locked --release -p extend-service --bin extend-service

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/extend --shell /usr/sbin/nologin extend
COPY --from=builder /build/target/release/extend-service /usr/local/bin/extend-service
COPY LICENSE THIRD_PARTY_NOTICES.md THIRD_PARTY_LICENSES.txt /usr/share/doc/silicon-extend/
# Production unless someone explicitly asks otherwise: production refuses the local Silicon Accounts,
# Briefcase and Ting stand-ins, and requires ACCOUNTS_URL, EXTEND_APP_SECRET and the Silicon
# Accounts webhook secret.
ENV EXTEND_ENVIRONMENT=production \
    EXTEND_BIND=0.0.0.0:8080 \
    EXTEND_DATA_DIR=/var/lib/extend/data \
    EXTEND_LOG_FORMAT=json
USER extend
WORKDIR /var/lib/extend
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s CMD curl -fsS http://127.0.0.1:8080/ready || exit 1
ENTRYPOINT ["/usr/local/bin/extend-service"]
CMD ["serve"]
