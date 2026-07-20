# AletheiaDB

[![CI](https://github.com/madmax983/AletheiaDB/actions/workflows/ci.yml/badge.svg)](https://github.com/madmax983/AletheiaDB/actions/workflows/ci.yml) [![codecov](https://codecov.io/gh/madmax983/AletheiaDB/branch/trunk/graph/badge.svg)](https://codecov.io/gh/madmax983/AletheiaDB) [![crates.io](https://img.shields.io/crates/v/aletheiadb.svg)](https://crates.io/crates/aletheiadb) [![docs.rs](https://docs.rs/aletheiadb/badge.svg)](https://docs.rs/aletheiadb) [![Security Policy](https://img.shields.io/badge/security-policy-blue.svg)](SECURITY.md)

A high-performance **bi-temporal graph database** in Rust, combining graph
traversal, vector similarity search, and full temporal history in a single
consistent query — with no external dependencies.

Most databases answer *what is true now*. AletheiaDB also answers *what was
true at any point in the past* and *what is semantically related*, all from
one query. [Why that matters →](docs/guides/why-aletheiadb.md)

---

## 60-Second Quickstart

One command seeds a small, story-driven **bi-temporal** dataset and walks you
through a guided sequence of queries — including a **time-travel (`AS OF`)**
query whose answer visibly differs from the current state. It is ephemeral
(nothing written to disk) and needs no server and no API key:

```bash
cargo run --example demo
```

Time-to-first-query is a few seconds (the one-time `cargo build` is separate).
The three entry channels, each with its exact commands:

| Channel | Command | Server? | API key? |
|---------|---------|---------|----------|
| **Embedded (Rust)** | `cargo run --example demo` | No | No |
| **MCP (agent-issued)** | `export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"`<br>`cargo run --bin aletheia-mcp --features mcp-server` | Yes (stdio) | Yes |
| **HTTP server** | `export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"`<br>`cargo run --bin aletheia-server --features http-server` | Yes | Yes |

Authentication is **on by default** for both servers — see the
[Security Quickstart](docs/guides/security-quickstart.md) for key setup and the
explicit anonymous opt-in. Full walkthrough, sample output, and measured
timings: **[60-Second Quickstart guide →](docs/guides/quickstart.md)**

---

## Install

```toml
[dependencies]
aletheiadb = "0.2"
```

Requires Rust 1.92+.

---

## Quick Start

```rust
use aletheiadb::prelude::*;

fn main() -> Result<()> {
    // Durable: persists across restarts at this path, so rerunning this
    // example against the same `./mydb` directory adds more nodes each time
    // (delete `./mydb` to start fresh). For an ephemeral, in-memory-only
    // database (tests, scratch sessions), use `AletheiaDB::new()`.
    let db = AletheiaDB::open("./mydb")?;

    // Create nodes and a relationship
    let alice = db.create_node("Person", properties! { "name" => "Alice" })?;
    let bob   = db.create_node("Person", properties! { "name" => "Bob"   })?;
    db.create_edge(alice, bob, "KNOWS", properties! {})?;

    // Snapshot the time before an update
    let before = aletheiadb::time::now();

    // Update Alice
    db.write(|tx| tx.update_node(alice, properties! { "role" => "engineer" }))?;

    // Time-travel: what did Alice look like before the update?
    let past = db.get_node_at_time(alice, before, before)?;
    assert!(past.properties.get("role").is_none());

    Ok(())
}
```

See the [Getting Started guide](docs/guides/getting-started.md) for a full walkthrough.

---

## Run with Docker

No Rust toolchain required. The official image ships both the HTTP server
(default) and the stdio MCP server.

```bash
# One command brings up a durable server on localhost:1963 with a persistent
# named volume. Both secrets are required — the server refuses to start
# without them (auth is on by default; see the Security Quickstart).
export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"
export AUTUMN_SECURITY__SIGNING_SECRET="$(openssl rand -hex 32)"
docker compose up -d

# First query
curl -H "x-api-key: $ALETHEIADB_BOOTSTRAP_ADMIN_KEY" http://localhost:1963/status
```

Data lives under a declared volume at `/var/lib/aletheiadb` (`wal/` +
`indexes/`); kill-and-restart with the volume attached recovers via WAL
replay with zero data loss. See the
[Docker guide](docs/guides/docker.md) for image details, MCP mode, env vars,
graceful shutdown, and measured footprint/startup.

---

## Shell completions

The `aletheia` CLI can print a tab-completion script for your shell. The script
reflects the subcommands compiled into your binary.

```bash
aletheia completions <bash|zsh|fish|powershell|elvish>
```

Install it once so `aletheia <TAB>` completes subcommands and flags:

```bash
# bash — system-wide (needs write access to the completion dir)
aletheia completions bash | sudo tee /etc/bash_completion.d/aletheia > /dev/null

# bash — per user (the file-write approach requires the `bash-completion` package)
mkdir -p ~/.local/share/bash-completion/completions
aletheia completions bash > ~/.local/share/bash-completion/completions/aletheia
# ...or source it directly (add this line to ~/.bashrc):
eval "$(aletheia completions bash)"
```

```zsh
# zsh — write into a user-owned completions dir and ensure compinit runs
# Ensure ~/.zshrc initializes completions:  autoload -Uz compinit && compinit
mkdir -p ~/.zsh/completions
aletheia completions zsh > ~/.zsh/completions/_aletheia
# add to ~/.zshrc BEFORE compinit:  fpath=(~/.zsh/completions $fpath)
# then reload: rm -f ~/.zcompdump; compinit   (or restart the shell)
```

```fish
# fish
mkdir -p ~/.config/fish/completions
aletheia completions fish > ~/.config/fish/completions/aletheia.fish
```

---

## Hybrid Queries

Graph traversal, vector ranking, and temporal snapshots compose into a single
query with a consistent view of the data:

```rust
use aletheiadb::prelude::*;
use aletheiadb::HnswConfig;

fn main() -> Result<()> {
    let db = AletheiaDB::new()?;
    db.vector_index("embedding").hnsw(HnswConfig { dimensions: 2, ..Default::default() }).enable()?;

    let alice = db.create_node("Person", properties! { "name" => "Alice" })?;
    let query_embedding = vec![0.1, 0.2];
    let valid_time = aletheiadb::time::now();
    let tx_time = aletheiadb::time::now();

    let _results = db.query()
        .as_of(valid_time, tx_time)              // temporal: point-in-time snapshot
        .start(alice)                            // graph: starting node
        .traverse("KNOWS")                       // graph: follow edges
        .rank_by_similarity(&query_embedding, 10) // vector: re-rank by similarity
        .execute(&db)?;

    Ok(())
}
```

See the [Hybrid Query guide](docs/guides/hybrid-query-guide.md).

---

## Performance

Benchmarks run on every push to trunk.
[📊 Latest results](https://madmax983.github.io/AletheiaDB/benchmarks/)

Averages across 30–212 datapoints of continuous CI runs:

| Operation | Historical avg | Target |
|-----------|---------------|--------|
| Node lookup (current state) | 25.7 ns | < 1 µs |
| Edge lookup (current state) | 25.4 ns | < 1 µs |
| Single-hop traversal | 185.8 ns | < 1 µs |
| 3-hop traversal | 24.0 µs | < 100 µs |
| Time-travel reconstruction | 82.8 ns | < 10 ms |
| k-NN search (k=10, 1K vectors) | 55.3 µs | < 10 ms |
| k-NN search (k=10, 10K vectors) | 127.2 µs | < 10 ms |
| Graph + vector hybrid (k=10) | 22.5 µs | < 20 ms |
| WAL throughput (GroupCommit) | ~100K ops/sec | — |

---

## Feature Flags

All features are off by default except `config-toml`.

| Flag | What it enables |
|------|----------------|
| `config-toml` | Load config from `.toml` files *(default)* |
| `observability` | OpenTelemetry-compatible tracing spans and metrics |
| `metrics-rs` | Adapter from AletheiaDB metrics to the `metrics` facade |
| `embeddings` | Embedding generation (OpenAI, HuggingFace, Ollama providers) |
| `embeddings-onnx` | ONNX local inference backend |
| `mcp-server` | MCP server binary for LLM/Claude integration |
| `sharding-rpc` | RPC client for distributed graph sharding |
| `semantic-search` | Stable semantic modules: associative retrieval, clustering, entity resolution, traversal |
| `nova` | Experimental semantic modules: reasoning, temporal analysis, diagnostics, characterization |

`nova` does **not** include `semantic-search` — use both flags if you want everything:
```toml
aletheiadb = { version = "0.2", features = ["nova", "semantic-search"] }
```

---

## Documentation

| Guide | Description |
|-------|-------------|
| [60-Second Quickstart](docs/guides/quickstart.md) | Fastest path to your first (time-travel) query, across all channels |
| [Why AletheiaDB](docs/guides/why-aletheiadb.md) | The problem it solves; when to use it |
| [Core Concepts](docs/guides/core-concepts.md) | Bi-temporal model, nodes, edges, WAL, vector search |
| [Installation](docs/guides/installation.md) | Prerequisites, feature flags, building from source |
| [0.1 → 0.2 Migration](docs/guides/migration-0.1-to-0.2.md) | Upgrading an embedded 0.1.x deployment to 0.2.0 |
| [Getting Started](docs/guides/getting-started.md) | First database, CRUD, time-travel, hybrid queries |
| [Docker](docs/guides/docker.md) | Container image, compose quickstart, MCP mode, volumes |
| [Persistence Guide](docs/guides/PERSISTENCE.md) | WAL, index persistence, cold storage |
| [Tiered Storage](docs/guides/tiered-storage-guide.md) | Unlimited history with hot/warm/cold tiers |
| [Hybrid Query Guide](docs/guides/hybrid-query-guide.md) | Graph + vector + temporal query API |
| [Vector Search](docs/guides/vector-search-integration.md) | HNSW indexing, k-NN, semantic drift |
| [Sharding Guide](docs/guides/sharding-guide.md) | Horizontal scaling with 2PC transactions |
| [Security Quickstart](docs/guides/security-quickstart.md) | Authentication, RBAC roles, API-key lifecycle |
| [Configuration](docs/CONFIGURATION.md) | All configuration options and presets |
| [Architecture](docs/ARCHITECTURE.md) | System design and internals |

---

## MCP Server (Claude / LLM Integration)

```bash
cargo run --bin aletheia-mcp --features mcp-server
```

Exposes AletheiaDB as a set of MCP tools over stdio: node/edge CRUD,
multi-hop traversal, vector search, temporal queries, and hybrid queries.
Compatible with Claude, Claude Code, and any MCP-capable host.

**Authentication is on by default**: the server refuses to start without a
credential (`ALETHEIADB_BOOTSTRAP_ADMIN_KEY`; session key via
`ALETHEIADB_MCP_API_KEY`) — see the
[Security Quickstart](docs/guides/security-quickstart.md) for key setup,
roles, and the explicit anonymous opt-in.

**Vector elision by default**: read tools (`get_node`, `list_nodes`,
`get_edge`, `list_edges`, `get_outgoing_edges`, `get_incoming_edges`,
`traverse`, `find_similar`, `hybrid_query`) replace vector/embedding
properties with a small `{type, dim, elided: true}` descriptor instead of
the raw float array, since a single embedding can cost thousands of tokens
of context an LLM can't reason over. Pass `include_vectors: true` on the
request to get the full array back. Similarity scores (`score` /
`similarity_score`) are always returned in full regardless of this flag.

---

## Contributing

```bash
git clone https://github.com/madmax983/AletheiaDB
cargo build
just check-all   # format + lint + test + coverage
```

See [DEVELOPMENT_WORKFLOW.md](docs/DEVELOPMENT_WORKFLOW.md). All PRs must
maintain ≥ 85% line coverage and pass `cargo clippy -- -D warnings`.

---

## Known Limitations

**Orphaned edges**: `delete_node` removes only the node; edges connecting to
it remain. Use `delete_node_cascade` to atomically remove the node and all
its edges.

---

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE) at your option.
