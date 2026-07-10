# syntax=docker/dockerfile:1.7
#
# Multi-stage Dockerfile for AletheiaDB.
#
# Builds three binaries and ships them on a slim Debian runtime:
#   * aletheia-server (default entrypoint) — the HTTP server, listens on :1963
#   * aletheia-mcp                          — the stdio MCP server, for MCP
#                                             clients that exec a container
#   * aletheia                              — the local CLI (backup / restore
#                                             against the mounted volume)
#
# Intended for the "docker run aletheiadb" workflow (analogous to
# `postgres:16`): persistent state under a mounted volume at
# /var/lib/aletheiadb, health probe on /status.
#
# Base images are pinned by tag AND digest for reproducibility. The digests
# are multi-arch manifest-list digests, so the same reference resolves
# correctly on linux/amd64 and linux/arm64. Refresh them with:
#   docker buildx imagetools inspect rust:1.92-slim-bookworm  --format '{{.Manifest.Digest}}'
#   docker buildx imagetools inspect debian:bookworm-slim     --format '{{.Manifest.Digest}}'
#
# The builder tag MUST satisfy the workspace `rust-version` in Cargo.toml
# (currently 1.92) or `cargo build --locked` fails the MSRV check.

# ── Stage 1: build ──────────────────────────────────────────────────────
FROM rust:1.92-slim-bookworm@sha256:f1f73538ebe623fd3673a35aff3df358ae1084c64c55646516e5b17b321b6c9b AS builder

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

# Build the serving-mode binaries plus the local CLI in one pass so they
# share dependency compilation. `--locked` keeps the image reproducible
# against Cargo.lock. `aletheia` has no required-features, so the
# http-server,mcp-server feature set is a superset that still builds it.
RUN cargo build --release --locked \
        --features http-server,mcp-server \
        --bin aletheia-server \
        --bin aletheia-mcp \
        --bin aletheia \
    # Strip debug info to keep the runtime image small (AC #6: <= 150MB).
    && strip target/release/aletheia-server target/release/aletheia-mcp target/release/aletheia

# ── Stage 2: runtime ────────────────────────────────────────────────────
FROM debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df AS runtime

# ca-certificates for any outbound TLS; curl so HEALTHCHECK works out of the
# box without adding a second tool; tini as a minimal init so SIGTERM is
# forwarded to the server (clean flush/fsync) and no zombies accumulate when
# the process runs as PID 1. All three are small enough that the "image just
# works" UX is worth pulling them in.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 aletheia \
    && useradd --system --uid 1000 --gid aletheia --home /var/lib/aletheiadb aletheia \
    && mkdir -p /var/lib/aletheiadb \
    && chown -R aletheia:aletheia /var/lib/aletheiadb

COPY --from=builder /build/target/release/aletheia-server /usr/local/bin/aletheia-server
COPY --from=builder /build/target/release/aletheia-mcp /usr/local/bin/aletheia-mcp
COPY --from=builder /build/target/release/aletheia /usr/local/bin/aletheia

# OCI image metadata (AC #1: provenance recorded alongside the digest in
# release notes). ARG values are injected by CI (docker/metadata-action).
ARG ALETHEIADB_VERSION=dev
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="AletheiaDB" \
      org.opencontainers.image.description="High-performance bi-temporal graph database (HTTP + MCP server)" \
      org.opencontainers.image.source="https://github.com/madmax983/AletheiaDB" \
      org.opencontainers.image.documentation="https://github.com/madmax983/AletheiaDB/blob/trunk/docs/guides/docker.md" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${ALETHEIADB_VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}"

USER aletheia
WORKDIR /var/lib/aletheiadb

ENV ALETHEIADB_HOST=0.0.0.0 \
    ALETHEIADB_PORT=1963 \
    ALETHEIADB_DATA_DIR=/var/lib/aletheiadb

# Authentication is REQUIRED by default (Issue #3350): the server refuses to
# start with zero credentials. Supply ALETHEIADB_BOOTSTRAP_ADMIN_KEY at run
# time (docker run -e / compose), then mint role-scoped keys over
# POST /admin/keys — see docs/guides/security-quickstart.md. Anonymous mode
# is an explicit opt-in (ALETHEIADB_AUTH_MODE=anonymous) that grants every
# caller full access; do not use it outside isolated local development.
#
# This is a release build, which runs under the web framework's `prod`
# profile — and that profile also refuses to start without
# AUTUMN_SECURITY__SIGNING_SECRET (>=32 bytes, not a demo value; generate
# with `openssl rand -hex 32`). The secret drives the framework's
# session/CSRF machinery, not AletheiaDB's token-based API, but it must be
# supplied at run time all the same (docker run -e / compose).
#
# Setting ALETHEIADB_DATA_DIR (above) selects the durable path: GroupCommit
# WAL durability + index persistence with load-on-startup, laid out as
# {data_dir}/wal and {data_dir}/indexes — the documented file structure. A
# kill/restart with the volume attached recovers with zero data loss via WAL
# replay.

EXPOSE 1963
VOLUME ["/var/lib/aletheiadb"]

# /status is metrics-class: in required-auth mode the probe needs a
# credential (any role). The x-api-key header is ignored in anonymous mode,
# so this works in both.
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD curl --fail --silent --show-error --header "x-api-key: ${ALETHEIADB_BOOTSTRAP_ADMIN_KEY}" http://127.0.0.1:${ALETHEIADB_PORT}/status || exit 1

# tini reaps zombies and forwards SIGTERM to the server for a clean,
# durability-honoring shutdown. Override the CMD to run the MCP server
# instead, e.g. `docker run -i --rm aletheiadb aletheia-mcp`.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["aletheia-server"]
