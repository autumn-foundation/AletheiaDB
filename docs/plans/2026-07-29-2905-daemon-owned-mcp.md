# Daemon-owned AletheiaDB for MCP clients (Issue #2905)

**Status**: implementation plan (TDD: red → green → refactor)
**Issue**: [#2905](https://github.com/autumn-foundation/AletheiaDB/issues/2905)
**Date**: 2026-07-29

---

## 1. Problem restated

Command-based MCP clients (Claude Desktop, Codex, Cursor, …) spawn **one
`aletheia-mcp` process per client session**. Each of those processes calls
`AletheiaDB::open_from_env()` and therefore *owns storage*:

| Env configuration | Failure mode |
|---|---|
| Neither `ALETHEIADB_CONFIG` nor `ALETHEIADB_DATA_DIR` | Every session silently gets its own **ephemeral** database. Agents disagree about reality. |
| Shared `ALETHEIADB_DATA_DIR` | N processes open the **same** WAL + index directory. AletheiaDB is a single-writer embedded store; concurrent multi-process writers are not a supported (or tested) configuration. |
| Windows | Many live `aletheia-mcp.exe` handles pin `~/.cargo/bin/aletheia-mcp.exe`, so `cargo install --force` fails at the final rename with `os error 5`. |

The fix is an **ownership boundary**: exactly one process owns the files; every
agent session is a *client* of that process.

## 2. What already exists (survey before design)

| Asset | Location | Relevance |
|---|---|---|
| `aletheia daemon start/stop/status` | `src/bin/aletheia.rs` | Spawns `aletheia-server`, writes `.aletheia/daemon.pid`. Unix-only liveness (`/proc`) and stop (`kill`). |
| `aletheia-server` (autumn-web 0.4) | `src/bin/server.rs`, `src/http/` | Production HTTP surface: `/status`, `POST /query`, `/admin/keys*`. **No MCP.** |
| `aletheia-server` crate (autumn-web **0.6**) | `crates/aletheia-server/` | ~90 handlers projected to HTTP **and** `/mcp` via autumn's `mount_mcp` (Streamable-HTTP MCP), OpenAPI, RBAC gate, rate limits, cursors. Parity-tested against the legacy surfaces. **But it only builds a `TestApp` — no `run_server`, no binary.** |
| `aletheia-mcp` | `src/bin/mcp_server.rs` | stdio MCP server; calls `AletheiaDB::open_from_env()` unconditionally. |
| `AletheiaMcpServer::dispatch_tool_json` | `src/mcp/server.rs` | Public name+args → JSON seam already used by the 0.6 crate. |

The decisive find: **autumn-web 0.6's `mount_mcp("/mcp")` already serves a real
Streamable-HTTP MCP endpoint**, and `crates/aletheia-server` already calls it.
The missing piece is not a protocol — it is a *process*.

## 3. Brainstorming (divergent — options considered)

1. **Bespoke REST shim.** Daemon exposes `POST /mcp/tools/list` + `/mcp/tools/call`;
   proxy translates MCP↔REST.
2. **Autumn `mount_mcp` daemon + dumb JSON-RPC relay.** Daemon serves real MCP over
   HTTP; `aletheia-mcp` relays stdin/stdout frames verbatim.
3. **Unix socket / named pipe RPC.** Custom framing, no HTTP.
4. **File lock only.** Keep N processes, add an advisory lock so only one opens the dir.
5. **Make storage multi-process safe.** Explicitly out of scope per the issue.
6. **Client-side dedup.** Tell users to configure exactly one MCP client. Not a fix.
7. **Daemon spawns the stdio proxies itself** (supervisor model).
8. **Embed the MCP server in the CLI** (`aletheia mcp --daemon-url …`) rather than a separate binary.

**Chosen: (2)**, with (4) as a *complementary safety net* and (8) rejected to keep
`aletheia-mcp` the stable client-config entry point.

Why (2) beats (1): the relay carries `initialize`, capability negotiation, protocol
version, `tools/list`, `tools/call`, and future methods with **zero per-method code**;
a REST shim re-implements — and will drift from — the protocol. Why it beats (3):
HTTP is what MCP clients that *do* support remote transports already speak, so the
same daemon serves both proxy-based and native-HTTP clients (issue's "ideally
HTTP/SSE if supported by the client").

## 4. Reverse brainstorming (how could this fail?)

Deliberately enumerating how to make this *worse*, then designing the counter:

| # | How to break it | Counter-measure in this design |
|---|---|---|
| R1 | Proxy silently falls back to an embedded DB when the daemon is down → the exact "silent divergent database" bug the issue is about. | **Never fall back.** Daemon mode is explicit (`ALETHEIADB_DAEMON_URL`), and a connect failure is a hard, loud error. Mode selection is decided once at startup and logged to stderr. |
| R2 | Proxy opens local storage "just for a moment" (e.g. to read config). | Assert in code *and* in a test: in daemon mode `open_from_env()` is never called and the data dir stays empty (no `wal/`, no `indexes/`). |
| R3 | Daemon dies; proxies hang forever holding client sessions. | Bounded connect + request timeouts; transport errors become MCP JSON-RPC errors, not hangs. |
| R4 | Client disconnects; proxy lingers → same Windows file-lock problem, new binary. | Proxy exits when stdin reaches EOF. Test asserts process exit; the daemon must survive it. |
| R5 | Credential sprawl: every proxy needs its own key; or worse, none. | Proxy forwards `ALETHEIADB_MCP_API_KEY` as `Authorization: Bearer`; the daemon is the single RBAC decision point. Anonymous stays an explicit opt-in. |
| R6 | Two daemons started against one data dir (e.g. stale pid file). | pid-file check + **advisory lock file** owned by the daemon; a second daemon refuses to start. |
| R7 | "One daemon" claim is untested — reviewers can't tell if two sessions share state. | Test writes through session A and reads through session B, and asserts the daemon reports a single DB identity/instance. |
| R8 | Windows users can't stop the daemon (`/proc`, `kill`). | cfg-gated liveness/stop via `tasklist`/`taskkill`; CI already runs `windows-latest`. |
| R9 | Adding a second server binary confuses operators about which is "the" daemon. | One documented command (`aletheia daemon start`), one guide, `daemon status` prints the URL and the MCP endpoint. |
| R10 | Response streaming (SSE) breaks the relay. | Relay handles both `application/json` and `text/event-stream` responses; SSE frames are unwrapped to JSON-RPC messages on stdout. |

## 5. Six Thinking Hats

**⚪ White (facts).** autumn-web 0.6 `mount_mcp` = Streamable HTTP, protocol
versions `2025-06-18 / 2025-03-26 / 2024-11-05`. The 0.6 crate exposes ~90
`#[api_doc(mcp)]` handlers and already has a parity suite. The 0.4 server owns
`POST /query` and is what `aletheia daemon start` launches today. `rmcp` 1.7 is
already a dependency (`server`, `transport-io`).

**🔴 Red (instinct).** The stdio proxy must feel *boring*: no translation table, no
per-tool code, no surprises. If a reviewer has to ask "does the proxy support tool
X?", the design is wrong. A relay has no such question.

**⚫ Black (risks / caution).**
- Promoting the 0.6 crate to a production binary is the biggest risk in this change:
  its REST paths are **not** a superset of the 0.4 surface (`POST /query` means
  something different). → Mitigation: the new daemon binary is **additive**;
  `aletheia-server` is untouched, and `aletheia daemon start --surface legacy`
  preserves today's behavior exactly.
- Auth: the daemon is a network listener. Default must stay auth-required, bound to
  loopback by default for the daemon path.
- Locking: an advisory lock must not brick startup after an unclean kill (stale lock
  detection must be based on liveness, not file existence alone).

**🟡 Yellow (upside).** One process = one WAL, one recovery, one checkpoint cadence,
one cache. Upgrades stop fighting file handles. HTTP-capable clients need no proxy at
all. The daemon also gets OpenAPI + `/metrics` for free. The 0.6 migration lane gets
its first production consumer without disturbing the 0.4 contract.

**🟢 Green (creative).** `aletheia daemon status --json` emitting the exact MCP client
config block a user must paste (Codex/Claude Desktop shape) — turning a docs problem
into a command.

**🔵 Blue (process).** TDD, four slices, each red→green:
S1 daemon binary + `/mcp`; S2 proxy mode; S3 CLI/lock/Windows; S4 docs.
Then multi-angle review, then the AC evidence table.

## 6. Design

```
                    ┌────────────────────────────────────────────┐
  aletheia CLI ───► │  aletheia-daemon        (ONE DB owner)      │
  HTTP clients ───► │  autumn-web 0.6 app                         │
  MCP over HTTP ──► │    ├─ REST routes (/nodes, /edges, …)       │
                    │    ├─ /mcp   ← autumn mount_mcp (Streamable)│
                    │    ├─ /openapi.json, /metrics, /status      │
                    │    └─ ServerState { Arc<AletheiaDB> }       │
                    └────────────────────────────────────────────┘
                                    ▲            ▲
              JSON-RPC over HTTP    │            │  JSON-RPC over HTTP
                    ┌───────────────┘            └───────────────┐
        ┌───────────┴───────────┐                    ┌───────────┴───────────┐
        │ aletheia-mcp (proxy)  │  ... N sessions    │ aletheia-mcp (proxy)  │
        │ stdio ⇄ HTTP relay    │                    │ stdio ⇄ HTTP relay    │
        │ NO local storage      │                    │ NO local storage      │
        └───────────────────────┘                    └───────────────────────┘
```

### 6.1 Daemon (`aletheia-daemon`, `crates/aletheia-server`)
- `run_server(DaemonConfig)` lifts the existing `try_build_server_testapp` assembly
  onto autumn's production `AppBuilder` (`routes` → `openapi` → `mount_mcp("/mcp")`
  → security → `state_initializer` → `on_shutdown(persist_indexes)`).
- Route list extracted to `app::all_routes()` so the `TestApp` proving ground and the
  production builder can never diverge.
- `security::apply_security` generalized over both app types by a tiny local trait.
- Env: `ALETHEIADB_HOST` (default `127.0.0.1` — loopback, unlike the 0.4 server's
  `0.0.0.0`), `ALETHEIADB_PORT` (1963), `ALETHEIADB_DATA_DIR` / `ALETHEIADB_CONFIG`,
  `ALETHEIADB_AUTH_MODE`, `ALETHEIADB_BOOTSTRAP_ADMIN_KEY`.
- **Single-owner lock**: `{data_dir}/daemon.lock` holds the owning pid; startup
  refuses if a *live* process already holds it (stale locks are reclaimed).

### 6.2 Proxy (`aletheia-mcp` daemon-client mode)
- Selected by `--daemon-url <URL>` or `ALETHEIADB_DAEMON_URL`; otherwise embedded
  mode (unchanged fallback).
- In daemon mode: a raw newline-delimited stdio ⇄ HTTP relay. **As built, this
  does not go through `rmcp`** — the plan assumed an `rmcp` `ServerHandler`, but
  implementing each protocol method would have reintroduced exactly the
  per-method surface a relay avoids. Frames are forwarded verbatim, so any method
  a future protocol revision adds passes through with no code. No `AletheiaDB` is
  constructed.
- Credential: `ALETHEIADB_MCP_API_KEY` → `Authorization: Bearer`.
- Session affinity: `Mcp-Session-Id` echoed back on subsequent calls.
- Shutdown: stdin EOF → service ends → process exits. Daemon unaffected.

### 6.3 CLI
- `aletheia daemon start` launches `aletheia-daemon` (`--surface legacy` keeps
  `aletheia-server`), records host/port/surface in the pid file, prints the base URL
  and the `/mcp` endpoint.
- `daemon status [--json]` prints liveness + URLs + a ready-to-paste MCP client block.
- Liveness/stop work on Windows (`tasklist`/`taskkill`) and Unix (`/proc`, `kill`).

## 7. Test plan (written first)

| ID | Test | Proves |
|---|---|---|
| T1 | `initialize` + `tools/list` over `/mcp` returns the tool inventory | daemon is MCP-capable |
| T2 | `tools/call` create_node then `tools/call` get_node **on two separate client sessions** returns the same node | AC "multiple sessions, one DB" |
| T3 | `/mcp` without credential → uniform 401 envelope; reader key + write tool → 403 | RBAC at the daemon |
| T4 | proxy-mode `aletheia-mcp` with `ALETHEIADB_DATA_DIR` set writes **nothing** into that dir | AC "does not open local storage" |
| T5 | proxy relays `initialize`/`tools/list`/`tools/call` to a stub daemon; forwards bearer token | relay correctness |
| T6 | proxy exits ≤ N s after stdin EOF; daemon still answers afterwards | AC "shutdown behavior explicit" |
| T7 | no daemon URL → embedded mode still works | AC "fallback" |
| T8 | second daemon on the same data dir refuses to start | R6 |
| T9 | daemon status/stop liveness helper is cfg-correct per platform | R8 |
| T10 | docs contain a Windows/Codex setup block (documentation-validation style test) | AC "documentation" |

## 8. As-built deviations from this plan

| Planned | As built | Why |
|---|---|---|
| `rmcp` `ServerHandler` proxy | Raw newline-delimited JSON-RPC relay | Protocol-complete with no per-method code; see §6.2. |
| Sequential frame relaying | Concurrent, bounded at 32 in flight | A sequential relay lets one `await_changes` long-poll stall an entire agent session. |
| Lock = pid file + liveness | Same, but **exclusively created** (`O_CREAT|O_EXCL`), ownership-checked on release, shared in `aletheiadb::daemon_lock` | Check-then-create raced; an unconditional release could delete a successor's claim; and every storage-opening binary needs the same check, not just the daemon. |
| Lock keyed on `ALETHEIADB_DATA_DIR` | Keyed on the **resolved** data dir (env var *or* `ALETHEIADB_CONFIG` TOML) | The TOML path opens equally durable storage; locking only the env-var path left it unprotected. |

## 9. Out of scope (restated from the issue)

Clustering/consensus, multi-process writers, removal of embedded stdio mode.
