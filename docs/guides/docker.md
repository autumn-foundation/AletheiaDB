# Docker Guide

Run AletheiaDB as a container — no Rust toolchain required. The image is the
zero-toolchain distribution channel for both serving modes.

- [Quickstart (compose)](#quickstart-compose)
- [What's in the image](#whats-in-the-image)
- [Serving modes](#serving-modes)
- [Configuration (environment variables)](#configuration-environment-variables)
- [Durability, volumes, and recovery](#durability-volumes-and-recovery)
- [Graceful shutdown](#graceful-shutdown)
- [Security](#security)
- [Measured footprint and startup](#measured-footprint-and-startup)
- [Backup / restore against a volume](#backup--restore-against-a-volume)
- [Publishing and tags](#publishing-and-tags)

---

## Quickstart (compose)

The repo ships a `docker-compose.yml` that brings up a durable server on
`localhost:1963` behind a named volume, in the same shape as `postgres:16`.

```bash
# Auth is ON by default — the server refuses to start without these two
# secrets. Generate real values (do NOT commit them):
export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"
export AUTUMN_SECURITY__SIGNING_SECRET="$(openssl rand -hex 32)"

docker compose up -d

# Health / first query (metrics-class endpoint; any role key works)
curl -H "x-api-key: $ALETHEIADB_BOOTSTRAP_ADMIN_KEY" http://localhost:1963/status
```

Stop and keep data: `docker compose down`. Wipe data: `docker compose down -v`.

---

## What's in the image

A multi-stage build compiles the workspace on `rust:1.92-slim-bookworm` and
ships **only the stripped binaries** on `debian:bookworm-slim`. Both base
images are pinned by tag **and** digest for reproducibility, and resolve
correctly on `linux/amd64` and `linux/arm64`.

Contents:

- `/usr/local/bin/aletheia-server` — HTTP server (default entrypoint)
- `/usr/local/bin/aletheia-mcp` — stdio MCP server
- `/usr/local/bin/aletheia` — local CLI (`backup` / `restore`)
- `tini` as PID 1 init (signal forwarding + zombie reaping)
- `ca-certificates`, `curl` (for the healthcheck)
- non-root user `aletheia` (uid/gid 1000); data dir owned by it

Exposed port: **1963**. Declared volume: **`/var/lib/aletheiadb`**.

---

## Serving modes

### HTTP server (default)

```bash
docker run --rm -p 1963:1963 \
  -e ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)" \
  -e AUTUMN_SECURITY__SIGNING_SECRET="$(openssl rand -hex 32)" \
  -v aletheiadb_data:/var/lib/aletheiadb \
  ghcr.io/autumn-foundation/aletheiadb:latest
```

### MCP server (stdio)

MCP clients that launch servers as containers exec the `aletheia-mcp` binary
and speak JSON-RPC over stdio. Override the command and keep stdin open
(`-i`):

```bash
docker run -i --rm \
  -e ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$YOUR_KEY" \
  -v aletheiadb_data:/var/lib/aletheiadb \
  ghcr.io/autumn-foundation/aletheiadb:latest aletheia-mcp
```

Example MCP client (`claude_desktop_config.json`-style) entry:

```json
{
  "mcpServers": {
    "aletheiadb": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "ALETHEIADB_BOOTSTRAP_ADMIN_KEY",
        "-v", "aletheiadb_data:/var/lib/aletheiadb",
        "ghcr.io/autumn-foundation/aletheiadb:latest", "aletheia-mcp"
      ],
      "env": { "ALETHEIADB_BOOTSTRAP_ADMIN_KEY": "your-admin-key" }
    }
  }
}
```

The MCP server authenticates with `ALETHEIADB_MCP_API_KEY` when set; a
bootstrap admin key is sufficient for local single-user use. See the
[Security Quickstart](security-quickstart.md).

---

## Configuration (environment variables)

The image maps onto the existing unified configuration via the server's
environment variables:

| Variable | Default (image) | Purpose |
|----------|-----------------|---------|
| `ALETHEIADB_PORT` | `1963` | HTTP listen port |
| `ALETHEIADB_HOST` | `0.0.0.0` | Bind address |
| `ALETHEIADB_DATA_DIR` | `/var/lib/aletheiadb` | Durable data dir (`wal/` + `indexes/`) |
| `ALETHEIADB_BOOTSTRAP_ADMIN_KEY` | *(required)* | Installs an admin key at boot |
| `ALETHEIADB_AUTH_MODE` | `required` | `required` or `anonymous` (opt-in) |
| `ALETHEIADB_CONFIG` | *(unset)* | Path to a mounted TOML config (takes precedence over `ALETHEIADB_DATA_DIR`) |
| `ALETHEIADB_CORS_PERMISSIVE` | `false` | Allow any CORS origin (dev only) |
| `ALETHEIADB_CORS_ORIGINS` | *(unset)* | Comma-separated allowed origins |
| `AUTUMN_SECURITY__SIGNING_SECRET` | *(required)* | Web-framework session/CSRF secret (release/prod profile) |

For full control, mount a TOML config and point `ALETHEIADB_CONFIG` at it:

```bash
docker run --rm -p 1963:1963 \
  -e ALETHEIADB_CONFIG=/etc/aletheiadb/config.toml \
  -e ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$KEY" \
  -e AUTUMN_SECURITY__SIGNING_SECRET="$SECRET" \
  -v $PWD/config.toml:/etc/aletheiadb/config.toml:ro \
  -v aletheiadb_data:/var/lib/aletheiadb \
  ghcr.io/autumn-foundation/aletheiadb:latest
```

---

## Durability, volumes, and recovery

Setting `ALETHEIADB_DATA_DIR` (the image default) selects the **durable**
path automatically: **GroupCommit** WAL durability plus index persistence
with load-on-startup. The data directory is laid out as the documented file
structure:

```
/var/lib/aletheiadb/
├── wal/        # write-ahead log (transaction durability)
├── indexes/    # index persistence (fast restarts)
└── auth/       # keys.json (0600) — auth state
```

`/var/lib/aletheiadb` is a **declared VOLUME**. A kill/restart of the
container with the volume attached recovers via the standard WAL/index
persistence path with **zero data loss**, honoring the < 5s recovery target
at the 10K-node/50K-edge reference scale.

- Named volume (compose default): survives `docker compose down`; removed by
  `docker compose down -v`.
- Bind mount: `-v $PWD/data:/var/lib/aletheiadb`. Ensure the host directory
  is writable by uid 1000 (the non-root `aletheia` user).

---

## Graceful shutdown

- **SIGTERM** (`docker stop`, `docker compose down`): `tini` forwards the
  signal; the server runs its on-shutdown hook (persists indexes / flushes
  per the configured durability mode) before exiting. Fits within the compose
  default 10s stop grace period at reference scale.
- **SIGKILL** (grace period exceeded, OOM, crash): no clean flush, but the
  next start replays the WAL — the durability contract still holds, you lose
  nothing that was acknowledged.

---

## Security

- **Auth is on by default (Issue #3350).** No credentials are baked into the
  image; it refuses to start without `ALETHEIADB_BOOTSTRAP_ADMIN_KEY` (and
  the framework's `AUTUMN_SECURITY__SIGNING_SECRET`). Anonymous mode is an
  explicit opt-in (`ALETHEIADB_AUTH_MODE=anonymous`) that grants every caller
  full access — never use it outside isolated local development.
- **Non-root by default.** The process runs as uid/gid 1000.
- **Minimal surface.** Only the two binaries plus `tini`, `curl`, and
  `ca-certificates`; no shell tooling or build deps in the runtime layer.
- Mint role-scoped keys over `POST /admin/keys` after boot; the bootstrap key
  is an admin key intended to create the keys you actually use. See the
  [Security Quickstart](security-quickstart.md).

---

## Measured footprint and startup

Measured on `linux/amd64`, Docker 29.3.1, from a clean build of this branch
(`docker build` → `docker run`; release binaries stripped). Reproduce with
the commands below.

| Metric | Value |
|--------|-------|
| Image size (`docker images`, uncompressed on-disk) | **183 MB** |
| Compressed size (`docker save … \| gzip -c \| wc -c`) | **45.3 MB** (47,482,010 bytes) |
| Cold start to healthy (`docker run` → healthcheck `healthy`) | **~5.4 s** |
| Graceful shutdown (`docker stop`, SIGTERM → exit) | **< 0.5 s** |
| First query after start (`/status`) | immediate once healthy — well inside the ≤ 60 s TTFQ target |

> The compressed-size budget in the spec is **≤ 150 MB**; the image ships at
> **45.3 MB** compressed. The uncompressed on-disk size reported by
> `docker images` (183 MB) is always larger than the compressed size a
> registry stores/transfers.

Reproduce:

```bash
docker build -t aletheiadb:local .
docker images aletheiadb:local                    # uncompressed on-disk size
docker save aletheiadb:local | gzip -c | wc -c    # compressed size
scripts/docker-smoke.sh aletheiadb:local          # boot + healthy timing + durability
```

---

## Backup / restore against a volume

The single-file `*.albk` backup captures the complete bi-temporal state. Run
the CLI against the same volume:

```bash
# Back up to a file on the host
docker run --rm \
  -v aletheiadb_data:/var/lib/aletheiadb \
  -v $PWD:/out \
  ghcr.io/autumn-foundation/aletheiadb:latest \
  aletheia backup /out/snapshot.albk

# Restore into the volume
docker run --rm \
  -v aletheiadb_data:/var/lib/aletheiadb \
  -v $PWD:/out \
  ghcr.io/autumn-foundation/aletheiadb:latest \
  aletheia restore /out/snapshot.albk
```

See the [Backup / Restore guide](backup-restore.md).

---

## Publishing and tags

CI publishes to GitHub Container Registry (`ghcr.io/autumn-foundation/aletheiadb`):

- **Release tag `vX.Y.Z`** → `X.Y.Z`, `X.Y`, `latest`, and `sha-<commit>`
  (multi-arch: `linux/amd64` + `linux/arm64`).
- **Push to `trunk`** → `trunk` and `sha-<commit>` (amd64, for smoke testing).
- **`workflow_dispatch`** → on-demand rebuild.

Per-release tags are immutable; the image digest is recorded in the release
notes. The build job runs on trunk pushes and tags only — it does **not** run
on the required PR path. A separate, path-filtered smoke job builds and
exercises the image (refuse-without-key, boot-with-key, create → query → AS
OF, kill/restart volume persistence) only when Docker files change.
