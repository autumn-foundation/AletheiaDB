# 60-Second Quickstart

Go from zero to your first **time-travel query** in under a minute. This is the
top of the funnel — the goal is a successful current-state query *and* a
successful `AS OF` query within 60 seconds of deciding to try AletheiaDB
(Issue #3380).

There are three entry channels. Pick the one that matches how you'll use
AletheiaDB:

| Channel | For | Needs a server? | Needs an API key? |
|---------|-----|-----------------|-------------------|
| [Embedded (Rust)](#embedded-rust--fastest-path) | Rust apps, evaluation | No | No |
| [MCP (agent-issued)](#mcp--let-an-agent-issue-the-first-query) | LLM / Claude tooling | Yes (stdio) | Yes (bootstrap in seconds) |
| [HTTP server](#http-server) | Language-agnostic clients | Yes | Yes |

> **What counts toward the 60 seconds?** Time-to-first-query (TTFQ) is
> measured from invoking the command to the first successful query response.
> The **one-time `cargo build`** (or download) is a separate, up-front cost and
> is *not* counted — a cold build takes minutes; the demo run itself is a
> few seconds. Both numbers are reported honestly below.

---

## Embedded (Rust) — fastest path

One command seeds a small, story-driven **bi-temporal** dataset and walks you
through a guided sequence of queries. It is ephemeral (in-memory; nothing is
written to your working directory) and needs no server and no API key:

```bash
cargo run --example demo
```

If you have the `aletheia` CLI binary built (or installed), the same guided
tour is available as a first-class subcommand — no `--example` flag, no data
directory, no server:

```bash
aletheia demo
```

`aletheia demo` seeds the identical ephemeral, story-driven graph in-memory and
prints the same guided sequence (current-state lookup, `AS OF` time-travel,
traversal, and version history). It mirrors `cargo run --example demo` and is
covered by the same CI behavior guard, so the two never drift.

You'll see four guided queries, each with a one-line "what you just saw":

```text
Seeded 204 nodes and 404 edges with genuine version history.

── Query 1 of 4: Current-state lookup ──
  let alice = db.get_node(alice_id)?;
  → Alice is currently: CTO

── Query 2 of 4: Traversal (who does Alice know?) ──
  → Alice KNOWS Bob (Engineer)

── Query 3 of 4: Time-travel — AS OF the founding day ──
  let past = db.get_node_at_time(alice_id, t_founding, t_founding)?;
  → On founding day, Alice was: Engineer
  what you just saw: the SAME node, a different answer — "Engineer" then vs "CTO" now.

── Query 4 of 4: History — every version of a fact ──
  → v1: title = Engineer
  → v2: title = Staff Engineer
  → v3: title = CTO
```

Query 3 is the differentiator: the *same* node returns a *different* answer as
of the founding day than it does now — a time-travel result no current-state
graph database can show you.

### Measured timing (reference: this repo's CI-class runner)

| Phase | Wall-clock |
|-------|-----------|
| One-time `cargo build --example demo` (cold) | ~135 s |
| Demo run — seed + first query (TTFQ) | ~3.1 s |
| Demo run — full 4-query guided sequence | ~3.1 s |

TTFQ is **~3 seconds**, comfortably inside the 60-second budget. The demo's
run-time and zero-error path are enforced on every push by
[`.github/workflows/quickstart.yml`](../../.github/workflows/quickstart.yml)
and by `tests/quickstart_demo.rs`, so this page can never silently drift.

### Drop it into your own project

The demo is ordinary AletheiaDB API. Here is the whole story in ~20 lines:

```rust
use aletheiadb::prelude::*;
use aletheiadb::time;

fn main() -> Result<()> {
    // Ephemeral (in-memory). Use AletheiaDB::open("./mydb") to persist.
    let db = AletheiaDB::new()?;

    // Hire Alice as an Engineer, then snapshot "the founding day".
    let alice = db.create_node("Person", properties! { "name" => "Alice", "title" => "Engineer" })?;
    let bob   = db.create_node("Person", properties! { "name" => "Bob",   "title" => "Engineer" })?;
    db.create_edge(alice, bob, "KNOWS", properties! {})?;
    let t_founding = time::now();

    // Facts change over time: Alice is promoted twice (two new versions).
    db.write(|tx| tx.update_node(alice, properties! { "name" => "Alice", "title" => "Staff Engineer" }))?;
    db.write(|tx| tx.update_node(alice, properties! { "name" => "Alice", "title" => "CTO" }))?;

    // 1) Current-state lookup.
    let now = db.get_node(alice)?;
    assert_eq!(now.properties.get("title").and_then(|v| v.as_str()), Some("CTO"));

    // 2) Time-travel: who was Alice on the founding day?
    let past = db.get_node_at_time(alice, t_founding, t_founding)?;
    assert_eq!(past.properties.get("title").and_then(|v| v.as_str()), Some("Engineer"));

    Ok(())
}
```

For a fuller walkthrough (CRUD, edges, hybrid queries), see
[Getting Started](getting-started.md).

---

## MCP — let an agent issue the first query

An LLM-tooling evaluator's "first query" should be issued *by their agent*.
AletheiaDB speaks the Model Context Protocol over stdio, exposing `get_schema`,
`traverse`, `get_node_at_time`, and more as MCP tools.

**Authentication is on by default** — the server refuses to start without a
credential. Bootstrap one in seconds (this is the whole ceremony):

```bash
# One high-entropy admin key, memory-only (never written to disk).
export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"

cargo run --bin aletheia-mcp --features mcp-server
```

Point an MCP client (Claude Desktop, Claude Code, or any MCP host) at it. A
Claude Desktop `mcpServers` entry looks like:

```json
{
  "mcpServers": {
    "aletheiadb": {
      "command": "cargo",
      "args": ["run", "--bin", "aletheia-mcp", "--features", "mcp-server"],
      "env": {
        "ALETHEIADB_MCP_API_KEY": "aletheia_sk_...your_key..."
      }
    }
  }
}
```

The session key (`ALETHEIADB_MCP_API_KEY`) is re-verified on every tool call.
For minting role-scoped keys, the anonymous-mode opt-in for local development
(`ALETHEIADB_AUTH_MODE=anonymous`), and the full RBAC model, see the
[Security Quickstart](security-quickstart.md).

> **Seeding data for an agent session:** the guided dataset above is created by
> the embedded Rust demo. The `aletheia demo` subcommand runs that same seed +
> guided-query tour in one command against an ephemeral database. To let an
> agent explore a *seeded, persistent* graph, run the demo's seed logic against
> a durable `AletheiaDB::open(path)` database and start the MCP server with the
> same `ALETHEIADB_DATA_DIR` (a one-command seeded, agent-reachable *server* is
> tracked as follow-up — see [Known gaps](#known-gaps--follow-ups)).

---

## HTTP server

For language-agnostic clients, run the HTTP server. It is also authenticated by
default:

```bash
export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"
ALETHEIADB_DATA_DIR=/var/lib/aletheiadb \
  cargo run --bin aletheia-server --features http-server
```

Then create a node and issue queries with `curl`. The complete key lifecycle
(mint writer/reader/metrics keys, use them, audit, revoke) is documented in the
[Security Quickstart](security-quickstart.md#step-2--mint-role-scoped-keys-over-the-admin-api).

---

## Known gaps / follow-ups

- **Non-Rust binary / container channel.** The `aletheia demo` CLI subcommand
  now runs the seeded guided demo from the compiled binary (no `--example`
  flag). The remaining gap is the **container packaging** (owned by the
  deployment spec) that ships that binary so a non-Rust user can run the demo
  with **no toolchain at all**. Until the container lands, the Rust-native
  `cargo run --example demo` / `aletheia demo` are the canonical guided demos,
  and the MCP/HTTP sections above cover the server paths.
- **Seeded agent session in one command.** See the note in the MCP section.

---

## Safety notes

- The embedded demo is **ephemeral** and clearly labeled as demo data — it
  writes nothing to your working directory and needs no cleanup.
- The MCP and HTTP servers never start unauthenticated by accident; anonymous
  mode is an explicit, loudly-warned opt-in.
