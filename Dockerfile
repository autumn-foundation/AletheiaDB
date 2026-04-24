# syntax=docker/dockerfile:1.7
#
# Multi-stage Dockerfile for the AletheiaDB HTTP server.
#
# Builds `aletheia-server` with the `http-server` feature and ships it on a
# slim Debian runtime. Intended for the "docker run aletheiadb" workflow
# (analogous to `postgres:16`): persistent state under a mounted volume at
# /var/lib/aletheiadb, health probe on /status.

# ── Stage 1: build ──────────────────────────────────────────────────────
FROM rust:1.86-slim-bookworm AS builder

# usearch + C++ codegen need a C toolchain; pkg-config for native deps.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libssl-dev \
        cmake \
        git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the full workspace. .dockerignore keeps target/, .git/, agents/, etc.
# out of the build context so this stays cheap to re-run on source changes.
COPY . .

RUN cargo build --release --locked \
    --features http-server \
    --bin aletheia-server

# ── Stage 2: runtime ────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# ca-certificates for any outbound TLS; curl so HEALTHCHECK works out of the
# box without adding a second tool. Both are small enough that pulling them
# in is worth the "image just works" UX.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 aletheia \
    && useradd --system --uid 1000 --gid aletheia --home /var/lib/aletheiadb aletheia \
    && mkdir -p /var/lib/aletheiadb \
    && chown -R aletheia:aletheia /var/lib/aletheiadb

COPY --from=builder /build/target/release/aletheia-server /usr/local/bin/aletheia-server

USER aletheia
WORKDIR /var/lib/aletheiadb

ENV ALETHEIADB_HOST=0.0.0.0 \
    ALETHEIADB_PORT=1963 \
    ALETHEIADB_DATA_DIR=/var/lib/aletheiadb

EXPOSE 1963
VOLUME ["/var/lib/aletheiadb"]

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:${ALETHEIADB_PORT}/status || exit 1

CMD ["aletheia-server"]
