# Daemon Mode — One Database Owner for Every Local Client

**Issue**: [#2905](https://github.com/autumn-foundation/AletheiaDB/issues/2905)

AletheiaDB is an embedded, single-writer store. MCP clients, however, launch
**one server process per client session**. Left alone, that produces exactly two
bad outcomes:

| Setup | What actually happens |
|---|---|
| No `ALETHEIADB_DATA_DIR` / `ALETHEIADB_CONFIG` | Every MCP session silently gets its **own ephemeral database**. Two agents disagree about reality, and nothing is persisted. |
| A shared `ALETHEIADB_DATA_DIR` | Several processes open the **same** WAL and index directory. Concurrent multi-process writers are not a supported configuration. |

Daemon mode replaces both with an explicit ownership boundary:

```
                      ┌──────────────────────────────────────────┐
    aletheia CLI ────► │  aletheia-daemon   (the ONE DB owner)    │
    curl / SDK ──────► │                                          │
    MCP over HTTP ───► │   /nodes /edges /traverse …   (REST)     │
                       │   /mcp                        (MCP)      │
                       │   /openapi.json /status /metrics         │
                       └──────────────────────────────────────────┘
                              ▲                    ▲
                              │ JSON-RPC over HTTP │
                  ┌───────────┘                    └───────────┐
          ┌───────┴────────┐                        ┌──────────┴─────┐
          │ aletheia-mcp   │  … N client sessions   │ aletheia-mcp   │
          │ stdio ⇄ HTTP   │                        │ stdio ⇄ HTTP   │
          │ no storage     │                        │ no storage     │
          └────────────────┘                        └────────────────┘
```

One process opens the files. Everything else is a client.

---

## 1. Install and start the daemon

The daemon is a **separate binary** in the `aletheia-server` workspace member, so
`cargo install --path .` (which builds `aletheia`, `aletheia-mcp`, and
`aletheia-server`) does not produce it:

```bash
# The CLI + the stdio proxy:
cargo install --path . --features mcp-server
# The daemon itself:
cargo install --path crates/aletheia-server        # installs `aletheia-daemon`
```

`ALETHEIADB_DATA_DIR` is **required** for a durable daemon. Without it the daemon
runs in-memory, takes no ownership lock, and loses everything on stop:

```bash
export ALETHEIADB_DATA_DIR="$HOME/.aletheiadb"
export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"

aletheia daemon start
```

```
daemon started (pid=48211, surface=unified, exe=/usr/local/bin/aletheia-daemon, log=.aletheia/daemon.log)
  HTTP http://127.0.0.1:1963
  MCP  http://127.0.0.1:1963/mcp
  Point MCP clients at this daemon with ALETHEIADB_DAEMON_URL=http://127.0.0.1:1963
```

| Flag | Meaning | Default |
|---|---|---|
| `--surface unified\|legacy` | `unified` = `aletheia-daemon` (REST + `/mcp` + OpenAPI). `legacy` = `aletheia-server`, the older HTTP-only surface. **Both serve `POST /query`, with different contracts**: on the legacy surface it is the polymorphic JSON operations API; on the unified daemon it is the read-only Cypher/AQL query tool. | `unified` |
| `--host`, `--port` | Bind address | `127.0.0.1:1963` |
| `--pid-file`, `--log-file` | Bookkeeping locations | `.aletheia/daemon.pid`, `.aletheia/daemon.log` |

The daemon binds **loopback** by default. It holds unencrypted graph state; set
`--host 0.0.0.0` only when you intend to publish it and have configured
authentication accordingly.

Authentication is required by default. Supply a bootstrap admin key as above (it
is memory-only — re-supply it on each start), then mint durable keys:

```bash
curl -s -X POST http://127.0.0.1:1963/admin/keys \
  -H "x-api-key: $ALETHEIADB_BOOTSTRAP_ADMIN_KEY" \
  -H 'content-type: application/json' \
  -d '{"name":"my-agent","role":"writer"}'
```

Keys are persisted under `{data_dir}/auth/keys.json` and are shared by every
surface. For local experiments only, `ALETHEIADB_AUTH_MODE=anonymous` disables
authentication entirely (with a loud warning).

### Single ownership is enforced

The daemon claims `{data_dir}/daemon.lock`, recording its pid. A second daemon
started against the same directory **refuses to boot**:

```
aletheia-daemon failed to start: data directory '/home/me/.aletheiadb' is already
owned by a running daemon (pid 48211). Stop it with `aletheia daemon stop`, or
point this daemon at another ALETHEIADB_DATA_DIR.
```

A lock left behind by a crashed daemon is reclaimed automatically — liveness is
checked, not mere file existence.

The claim is honored by every AletheiaDB process that would open storage: the
`aletheia` CLI, `aletheia-mcp` in embedded mode, and `aletheia-server` all refuse
a directory a live daemon owns, naming the owning pid. It is an **advisory**
claim, not an OS-enforced mutual exclusion: it prevents the accidents (a second
daemon, a CLI command run while the daemon is up), not a process that ignores it
outright.

Ownership is resolved from `ALETHEIADB_DATA_DIR` **or** from the
`persistence.data_dir` of an `ALETHEIADB_CONFIG` TOML — both durable
configurations are locked.

---

## 2. Point MCP clients at it

### Option A — the client speaks HTTP (no proxy)

Clients that support remote MCP servers connect straight to the endpoint:

```
http://127.0.0.1:1963/mcp
```

with `Authorization: Bearer <your-api-key>`. Nothing else is needed: no local
process, no data directory, no storage.

### Option B — the client spawns a command (stdio)

Most desktop MCP clients spawn a command. Run `aletheia-mcp` in **daemon-client
mode** by setting `ALETHEIADB_DAEMON_URL`. In that mode the process is a *relay*:
it forwards JSON-RPC frames to the daemon's `/mcp` endpoint and writes the
replies back. It **never opens local storage** and never constructs a database,
so N client sessions cost N thin relays and still share one database.

Ask the daemon for the exact configuration block:

```bash
aletheia daemon status --json
```

```json
{
  "running": true,
  "pid": 48211,
  "pid_file": ".aletheia/daemon.pid",
  "executable": "/usr/local/bin/aletheia-daemon",
  "surface": "unified",
  "url": "http://127.0.0.1:1963",
  "mcp_endpoint": "http://127.0.0.1:1963/mcp",
  "mcp_client_config": {
    "mcpServers": {
      "aletheiadb": {
        "command": "/usr/local/bin/aletheia-mcp",
        "args": [],
        "env": {
          "ALETHEIADB_DAEMON_URL": "http://127.0.0.1:1963",
          "ALETHEIADB_MCP_API_KEY": "<your-api-key>"
        }
      }
    }
  }
}
```

Paste `mcp_client_config` into your client's configuration and replace
`<your-api-key>` with a minted key.

`--daemon-url <URL>` on the command line is equivalent to the environment
variable and takes precedence over it. The URL may be given as a base
(`http://127.0.0.1:1963`), with the path (`.../mcp`), or as a bare authority
(`127.0.0.1:1963`) — all resolve to the same endpoint.

---

## 3. Windows and Codex setup

The Windows story is what motivated this feature. Every command-based MCP client
session used to spawn an `aletheia-mcp.exe` that **held the installed executable
open**, so upgrading failed at the final rename:

```
error: failed to link or copy `...\aletheia-mcp.exe`
Caused by: Access is denied. (os error 5)
```

Daemon mode does not eliminate the relay processes, but it makes them
disposable: they exit as soon as their client disconnects, and the long-lived
process is the daemon — a single executable you stop deliberately before
upgrading.

### Start the daemon (PowerShell)

```powershell
$env:ALETHEIADB_DATA_DIR = "$env:USERPROFILE\.aletheiadb"
$env:ALETHEIADB_BOOTSTRAP_ADMIN_KEY = [Convert]::ToBase64String((1..32 | ForEach-Object { Get-Random -Max 256 }))

aletheia daemon start
aletheia daemon status
```

### Configure Codex

Codex reads `~/.codex/config.toml`. Point it at the **proxy**, with the daemon
URL in the environment:

```toml
[mcp_servers.aletheiadb]
command = "C:\\Users\\you\\.cargo\\bin\\aletheia-mcp.exe"
args = []

[mcp_servers.aletheiadb.env]
ALETHEIADB_DAEMON_URL = "http://127.0.0.1:1963"
ALETHEIADB_MCP_API_KEY = "your-minted-key"
```

### Configure Claude Desktop / Cursor

Both use the `mcpServers` JSON shape — exactly what `aletheia daemon status
--json` prints:

```json
{
  "mcpServers": {
    "aletheiadb": {
      "command": "C:\\Users\\you\\.cargo\\bin\\aletheia-mcp.exe",
      "args": [],
      "env": {
        "ALETHEIADB_DAEMON_URL": "http://127.0.0.1:1963",
        "ALETHEIADB_MCP_API_KEY": "your-minted-key"
      }
    }
  }
}
```

Note what these configurations do **not** contain: no `ALETHEIADB_DATA_DIR`.
That is the point — a daemon-mode client has no database to configure.

### Upgrading on Windows

Relay processes are owned by the **MCP client**, not by the daemon, so stopping
the daemon does not close them. Close the client (or its AletheiaDB sessions)
first — each relay exits on stdin EOF — then:

```powershell
# 1. Quit / disconnect the MCP client so every aletheia-mcp.exe exits.
# 2. Stop the daemon:
aletheia daemon stop
# 3. Reinstall both binaries:
cargo install --path . --features mcp-server --force
cargo install --path crates/aletheia-server --force
# 4. Restart:
aletheia daemon start
```

What daemon mode changes here is *lifetime*: relays are short-lived and tied to a
client session instead of being long-running database owners, so there is a
well-defined moment at which nothing holds the executable.

`aletheia daemon stop` works on Windows: liveness is resolved through `tasklist`
(filtered on pid *and* image name, and parsed by the pid column so a memory
figure can never be mistaken for a pid) and termination through `taskkill`.

---

## 4. Shutdown semantics

Explicitly, because "which process am I supposed to stop?" is the question this
design has to answer:

| Process | Lifetime | How it ends |
|---|---|---|
| **`aletheia-daemon`** | **Long-lived.** It owns the database. | `aletheia daemon stop` (SIGTERM on Unix, `taskkill` on Windows). Indexes are flushed on graceful shutdown, so stop/start is lossless. The data-directory lock is released. |
| **`aletheia-mcp` (daemon-client mode)** | Disposable, one per client session. | Exits — status 0 — as soon as **stdin reaches EOF**, i.e. when the MCP client disconnects. Killing a relay never affects the daemon or the data. |
| **`aletheia-mcp` (embedded mode)** | Owns its own database. | Same stdio lifetime, but its storage goes with it. |

If the daemon is down, a relay does **not** silently fall back to an embedded
database — that would recreate the divergent-state bug this feature exists to
prevent. Instead each request is answered with a JSON-RPC error naming the
endpoint and suggesting `aletheia daemon status`.

---

## 5. Embedded mode (the fallback)

Running `aletheia-mcp` **without** `ALETHEIADB_DAEMON_URL` keeps the original
behavior: the process opens the database selected by `ALETHEIADB_CONFIG` /
`ALETHEIADB_DATA_DIR` (or an ephemeral one if neither is set) and serves MCP over
stdio itself. This remains fully supported and is the right choice for demos,
tests, and single-session embedding.

Use daemon mode when **more than one** client session must see the same state.

---

## 6. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Every tool call returns `UNAUTHENTICATED` | No/invalid `ALETHEIADB_MCP_API_KEY` | Mint a key via `POST /admin/keys` and set it in the client config. |
| `PERMISSION_DENIED` on writes | The key's role is `reader` | Mint a `writer` key. See [access-control-matrix.md](access-control-matrix.md). |
| Relay reports "cannot reach the AletheiaDB daemon" | Daemon down, or wrong port | `aletheia daemon status`; check `.aletheia/daemon.log`. |
| `data directory … is already owned by a running daemon` | A daemon is already running there | `aletheia daemon stop`, or use a different data directory. |
| Agents see different data | A client is still in embedded mode | Confirm `ALETHEIADB_DAEMON_URL` is set for **every** client; a daemon-mode relay logs `daemon-client mode` to stderr at startup. |
| `daemon status` shows no URL | The pid file predates this feature | `aletheia daemon stop && aletheia daemon start` rewrites it. |
| Everything vanished after `daemon stop` | Started without `ALETHEIADB_DATA_DIR` — the daemon was in-memory | Export the variable and restart; `daemon start` reports the owned directory. |
| `could not find 'aletheia-daemon'` | The daemon binary is not installed | `cargo install --path crates/aletheia-server` (see §1). |
| A CLI command reports "owned by a running AletheiaDB daemon" | Working as designed — the daemon owns the directory | Use the HTTP/MCP surface, or `aletheia daemon stop` first. |

---

## See also

- [security-quickstart.md](security-quickstart.md) — authentication, roles, API-key lifecycle
- [access-control-matrix.md](access-control-matrix.md) — which role may call which tool
- [mcp-query-tool.md](mcp-query-tool.md) — the MCP tool surface the daemon serves
- [installation.md](installation.md) — installing the binaries
