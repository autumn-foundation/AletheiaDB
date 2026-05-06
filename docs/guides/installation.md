# Installation

## Prerequisites

- **Rust 1.92+** with edition 2024. Check your version: `rustup show`
- **[just](https://github.com/casey/just)** — optional command runner used by all `just *` examples in the docs

```bash
# Update Rust if needed
rustup update stable

# Install just (optional)
cargo install just
```

## As a Library Dependency

Add AletheiaDB to your `Cargo.toml`:

```toml
[dependencies]
aletheiadb = "0.1"
```

This gives you the core database with TOML config support. Feature flags are additive — enable only what you need.

### Feature Flags

| Flag | What it enables | When to use it |
|------|----------------|----------------|
| `config-toml` | Load config from `.toml` files | Default; remove to trim deps |
| `observability` | OpenTelemetry-compatible tracing spans and metrics | Production deployments |
| `metrics-rs` | Adapter from AletheiaDB metrics to the `metrics` facade | If your app already uses `metrics` |
| `embeddings` | Embedding generation system (OpenAI, HuggingFace, Ollama providers) | Semantic search with external providers |
| `embeddings-onnx` | ONNX local inference backend | Ultra-fast local embeddings (requires setup) |
| `mcp-server` | MCP server binary for LLM integration | Claude / MCP tool use |
| `sharding-rpc` | RPC client for distributed sharding | Horizontal scaling |
| `semantic-search` | Stable semantic modules (Fishing, Gestalt, etc.) | Production semantic search |
| `nova` | All experimental semantic modules | R&D / experimental use |

**Example: MCP server + observability + embeddings**

```toml
[dependencies]
aletheiadb = { version = "0.1", features = ["mcp-server", "observability", "embeddings"] }
```

**Example: Minimal — core database only, no extras**

```toml
[dependencies]
aletheiadb = { version = "0.1", default-features = false }
```

### Experimental Features

The `nova` umbrella enables every `semantic-*` experimental module. The stable
`semantic-search` flag is **not** included in `nova` — you need both if you want
all modules:

```toml
aletheiadb = { version = "0.1", features = ["nova", "semantic-search"] }
```

If you see a compiler error like `unresolved import` or `item is gated behind the 'nova' feature`, add `features = ["nova"]` to your dependency.

---

## Building from Source

```bash
git clone https://github.com/madmax983/AletheiaDB
cd AletheiaDB

# Build the library
cargo build

# Build with all features
cargo build --all-features

# Run tests
cargo test

# Or use just for the full workflow
just check-all
```

### Development Tools

```bash
# Coverage reports (required for CI)
cargo install cargo-llvm-cov

# Run coverage
just coverage          # HTML report
just coverage-check    # Verify ≥85% line coverage threshold
```

---

## Running the MCP Server

The MCP server exposes AletheiaDB tools to LLMs over stdio (compatible with Claude, Claude Code, and any MCP-capable host).

```bash
# Build and run
cargo run --bin aletheia-mcp --features mcp-server

# Or build first, then run the binary
cargo build --release --features mcp-server
./target/release/aletheia-mcp
```

The server communicates over stdin/stdout using the Model Context Protocol. Configure it in your MCP host as a stdio server pointing to the binary.

---

## Running the CLI

A local CLI mirrors MCP-style operations for shell workflows:

```bash
cargo run --bin aletheia -- --help

# Example operations
cargo run --bin aletheia -- node create Person --properties '{"name":"Alice"}'
cargo run --bin aletheia -- daemon start --host 127.0.0.1 --port 1963
cargo run --bin aletheia -- daemon status
```

---

## Verify Your Setup

Run the basic example to confirm everything works:

```bash
cargo run --example doctor_who_demo
```

Expected: no errors, some output about graph nodes and temporal queries.

---

## Next Step

→ [Getting Started](getting-started.md) — create your first database and run your first queries.
