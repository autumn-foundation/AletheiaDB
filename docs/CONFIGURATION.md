# AletheiaDB Configuration Guide

This document describes the unified configuration system for AletheiaDB, including WAL, historical storage, vector indexes, and index persistence.

## Table of Contents

- [Overview](#overview)
- [Programmatic Configuration](#programmatic-configuration)
- [TOML Configuration Files](#toml-configuration-files)
- [Configuration Parameters](#configuration-parameters)
- [Configuration Presets](#configuration-presets)
- [Feature Flags](#feature-flags)

## Overview

AletheiaDB provides a unified configuration system via `AletheiaDBConfig` that consolidates all settings:

- **WAL Configuration**: Write-ahead log durability, concurrency, performance
- **Historical Storage**: Version limits, reconstruction depth, caching
- **Vector Indexes**: k-NN query limits, HNSW parameters
- **Index Persistence**: Disk persistence, compression, loading strategies

## Programmatic Configuration

### Basic Example

```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};

// Use default configuration
let db = AletheiaDB::new();

// Or load from builder
let config = AletheiaDBConfig::builder().build();
let db = AletheiaDB::with_unified_config(config);
```

### Complete Example

```rust
use aletheiadb::{AletheiaDB, config::{AletheiaDBConfig, WalConfigBuilder, HistoricalConfigBuilder}};
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::storage::index_persistence::PersistenceConfig;

let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(32).unwrap()               // 32 concurrent append stripes
        .stripe_capacity(2048).unwrap()          // 2048 entries per stripe
        .write_buffer_size(128 * 1024).unwrap() // 128KB write buffer
        .segment_size(128 * 1024 * 1024).unwrap() // 128MB segments
        .durability_mode(DurabilityMode::group_commit_default())
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(5000).unwrap()
        .max_reconstruction_depth(200).unwrap()
        .reconstruction_cache_size(20000).unwrap()
        .build())
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/my-database".into(),
        load_on_startup: true,
        ..Default::default()
    })
    .build();

let db = AletheiaDB::with_unified_config(config);
```

## TOML Configuration Files

Configuration can be loaded from TOML files (requires default `config-toml` feature).

### Production Configuration Example

```toml
# config/production.toml

[wal]
num_stripes = 64
stripe_capacity = 4096
write_buffer_size = 262144    # 256KB
segment_size = 268435456      # 256MB
flush_interval_ms = 10
wal_dir = "data/wal"
segments_to_retain = 20

[historical]
max_versions_per_entity = 10000
max_reconstruction_depth = 200
reconstruction_cache_size = 100000

[vector]
max_k = 10000
max_layer = 16

[persistence]
enabled = true
data_dir = "data/production"
load_on_startup = true
use_mmap = true
max_interned_strings = 10000000
```

```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};

let config = AletheiaDBConfig::from_toml_file("config/production.toml")?;
let db = AletheiaDB::with_unified_config(config);
```

### Durability Mode Configuration

#### Synchronous Mode (Maximum Durability)

```toml
[wal]
[wal.durability_mode]
Synchronous = {}
```

**Characteristics:**
- Latency: ~1-5ms per write
- Throughput: ~600 writes/sec
- ACID: ✅ Full
- Use case: Financial transactions, critical data

#### Group Commit Mode (High Throughput ACID)

```toml
[wal]
[wal.durability_mode.GroupCommit]
max_delay_ms = 10
max_batch_size = 200
```

**Characteristics:**
- Latency: ~2-10ms per write
- Throughput: ~100K+ writes/sec
- ACID: ✅ Full
- Use case: Production workloads, high write rates

#### Async Mode (Highest Throughput)

```toml
[wal]
[wal.durability_mode.Async]
flush_interval_ms = 100
```

**Characteristics:**
- Latency: <100ns per write
- Throughput: ~500K+ writes/sec
- ACID: ❌ Eventual durability
- Use case: Analytics, non-critical data, batch imports

#### Async Batched Mode (Hybrid)

```toml
[wal]
[wal.durability_mode.AsyncBatched]
max_delay_ms = 50
max_batch_size = 1000
```

**Characteristics:**
- Latency: ~1-50ms per write
- Throughput: ~200K+ writes/sec
- ACID: ❌ Eventual durability (better than pure async)
- Use case: High-throughput with better durability than async

## Configuration Parameters

### WAL Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `num_stripes` | u32 | 16 | Number of concurrent append stripes (must be power of 2) |
| `stripe_capacity` | usize | 1024 | Ring buffer size per stripe |
| `write_buffer_size` | usize | 64KB | I/O buffer size in bytes |
| `segment_size` | u64 | 64MB | WAL segment file size (min: 1MB) |
| `wal_dir` | PathBuf | "data/wal" | Directory for WAL segments |
| `segments_to_retain` | usize | 10 | Number of old segments to keep |
| `flush_interval_ms` | u64 | 100 | Flush interval for async modes (ms) |
| `durability_mode` | DurabilityMode | Synchronous | Durability mode (see above) |

**Validation:**
- `num_stripes` must be > 0 and a power of 2
- `stripe_capacity` must be > 0
- `write_buffer_size` must be ≥ 1KB
- `segment_size` must be ≥ 1MB

### Historical Storage Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `max_versions_per_entity` | usize | 1000 | Maximum versions to keep per entity |
| `max_reconstruction_depth` | usize | 100 | Maximum anchor chain depth (max: 1000) |
| `reconstruction_cache_size` | usize | 10000 | LFU cache size for reconstructed versions |
| `max_schema_as_of_entities` | usize | 50000 | Per-kind (nodes/edges) cap on entities `AletheiaDB::schema_as_of` reconstructs in one call; truncation is disclosed via `GraphSchema::sampled` |

**Validation:**
- `max_versions_per_entity` must be > 0
- `max_reconstruction_depth` must be > 0 and ≤ 1000
- `reconstruction_cache_size` must be > 0

### Vector Index Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `max_k` | usize | 10000 | Maximum k for k-NN queries (DoS protection) |
| `max_layer` | usize | 16 | Maximum HNSW layers |

### Index Persistence Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `enabled` | bool | false | Enable index persistence |
| `data_dir` | PathBuf | "data" | Directory for index files (cwd-relative placeholder; always set explicitly when enabling) |
| `load_on_startup` | bool | true | Load indexes on startup (only applies when enabled) |
| `use_mmap` | bool | true | Use memory-mapped loading |
| `max_interned_strings` | usize | 10000000 | Max unique interned strings (DoS bound); read at `open()`, so changing it requires a restart |
| `policies` | PersistencePolicies | Default | Automatic persistence policies |

> **String interner cap (`max_interned_strings`).** AletheiaDB interns every
> distinct node/edge **label**, property **key**, and string property **value**
> into a process-global table mapping each unique string to a compact `u32` id.
> `max_interned_strings` bounds how many unique strings that table may hold —
> a DoS guard against unbounded memory growth from adversarial or runaway
> high-cardinality data.
>
> **It is a COUNT cap, not a memory cap.** It bounds the number of *entries*, not
> bytes. Each entry costs roughly **~100 bytes** of map/pointer overhead **plus
> the string's own bytes**, so for short identifiers the default of
> **10,000,000** sits around **~1–1.6 GB** — but that is a *typical-case*
> estimate, not a ceiling. Worst-case interner memory is
> `count × (per-entry overhead + string bytes)`, and the string bytes are bounded
> only by the per-string cap (`MAX_STRING_LENGTH`, 10 MB) and the persisted-file
> size cap — so an adversarial worst case is `count × 10 MB`, far above ~1 GB.
> The count cap is paired with those per-string and file-size caps; a precise
> total-**byte** budget is a deliberate future alternative (deferred: it would add
> a running-total atomic to the lock-free intern fast path).
>
> This one knob drives **both** the runtime intern cap and the persisted
> interner's load-validation cap. Note the load-validation bound is a **floor**:
> the effective load cap is `max(10M, max_interned_strings)`, so a configured cap
> only ever **raises** the load bound above the 10M floor — a configured value
> *below* 10M does not lower it (a grown database still reopens). It is read once
> at **`open()`**, so **changing it requires a restart** (there is no hot-reload).
> Valid range: at least **1** (a cap of `0` is rejected at `open()` — it would
> refuse all interning and brick the database); values at or above `u32::MAX` are
> clamped to `u32::MAX`, since interner ids are 32-bit and a higher cap is
> unreachable.
>
> **Precedence.** The `ALETHEIADB_MAX_INTERNED_STRINGS` environment variable only
> takes effect on the **embedded/ephemeral** path (`AletheiaDB::new()` or direct
> `GLOBAL_INTERNER` use *without* opening a database): it seeds the process-global
> interner at first access. On the `open()` / `with_unified_config` path the
> **config field is authoritative** and effectively overrides the env var,
> because the config field always carries a value (defaulting to 10M) and is
> applied at `open()`. So the practical precedence is: on `open()`, config wins;
> without `open()`, the env seed applies.
>
> **Multi-database caveat (process-global).** The interner and its cap are
> **process-global**. In a process that opens multiple databases, the cap is
> **last-open-wins**: a database opened *later* with a **lower** cap can refuse
> new interns on an **earlier**-opened database whose data pushed the interner
> past that lower bound. Existing ids are never evicted or renumbered (lowering
> the cap only refuses *new* interns), but a shared low cap can starve a busy
> earlier database. Prefer a single uniform cap across all databases in one
> process.
>
> **When the cap is hit**, the write that would exceed it fails immediately with
> a `FAILED_PRECONDITION` error (MCP and HTTP) whose message names
> `persistence.max_interned_strings`; a background index-persist that hits it
> logs one actionable line and **suspends** that index's background persistence
> until restart. **No data is lost** in either case — the WAL is the source of
> truth; only the on-disk index snapshot goes stale. To recover: raise
> `persistence.max_interned_strings` above the reported limit and **restart**.
>
> ```toml
> [persistence]
> enabled = true
> data_dir = "data/production"
> # Raise the interner cap for a very large, high-cardinality dataset.
> max_interned_strings = 50000000
> ```

> **Note (Issue #3388):** Index persistence is opt-in. Before this change,
> `PersistenceConfig::default()` had `enabled: true` with the cwd-relative
> `data_dir: "data"`, so any database built from a default/builder config
> silently persisted into (and loaded from) `./data`, letting unrelated
> instances that share a working directory observe each other's data. If you
> relied on that implicit default, opt in explicitly with `enabled: true`
> and an explicit `data_dir`, or use `AletheiaDB::open(path)` /
> `durable_config_for_data_dir(path)`. TOML configs must now set
> `enabled = true` under `[persistence]`; a `[persistence]` section that
> omits `enabled` (even one that sets `data_dir` or `load_on_startup`) is
> treated as disabled.

#### Persistence Policies

```rust
PersistenceConfig {
    enabled: true,
    data_dir: "data/my-db".into(),
    load_on_startup: true,
    policies: PersistencePolicies {
        graph: GraphPersistencePolicy {
            on_adjacency_rebuild: true,  // Save after rebuilding CSR
            mutation_threshold: 10000,   // Or after 10K mutations
            time_interval_secs: 300,     // Or every 5 minutes
        },
        vector: VectorPersistencePolicy {
            mutation_threshold: 5000,
            time_interval_secs: 300,
        },
        temporal: TemporalPersistencePolicy {
            version_threshold: 10000,
            anchor_threshold: 100,
            time_interval_secs: 600,
        },
    },
    use_mmap: true,
}
```

### Cold Storage (Redb) Configuration

The optional on-disk cold tier (`RedbColdStorage`, configured via `RedbConfig`)
holds unlimited bi-temporal history. It is constructed directly rather than
through the unified config (see the tiered-storage guide).

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `compression` | CompressionAlgorithm | Zstd | Compression algorithm for stored values |
| `enable_checksums` | bool | true | Application-level CRC32 checksums on compressed payloads (on top of Redb's own) |
| `cache_size_bytes` | usize | 0 | Redb internal cache size in bytes (0 = Redb default) |
| `reencrypt_batch_size` | usize | 4096 | Cold-tier rotation re-encrypt batch size: `(key, value)` pairs re-encrypted per redb write transaction during a key-rotation bulk pass (Issue #3617 PR3) |

**`reencrypt_batch_size` trade-off:** the bulk re-encrypt pass that rewraps
every cold value under a rotated key runs in bounded, cursor-resumable
transactions of this many values each. A **larger** value amortizes commit cost
(fewer, larger transactions → faster) at the price of longer write-transaction
holds, more per-transaction memory, and a longer crash-replay window; a
**smaller** value resumes at finer granularity and uses less memory but commits
more often. A value of `0` would make no forward progress, so it is floored to
`1` (both at the `RedbConfig::with_reencrypt_batch_size` setter and the read
site). This is a **runtime knob only** — it does not affect the on-disk format.

```rust
use aletheiadb::storage::redb_cold_storage::RedbConfig;

// Smaller batches: lower memory + finer-grained rotation resume.
let config = RedbConfig::new().with_reencrypt_batch_size(512);
```

## Configuration Presets

### Development (Default)

Balanced for local development:

```rust
let db = AletheiaDB::new();  // Uses defaults
```

**Characteristics:**
- Moderate memory usage
- Reasonable performance
- Synchronous durability (safe)

### Embedded Systems (Minimal Memory)

Optimized for memory-constrained environments:

```rust
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(4).unwrap()
        .stripe_capacity(256).unwrap()
        .write_buffer_size(16 * 1024).unwrap()
        .segment_size(16 * 1024 * 1024).unwrap()
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(100).unwrap()
        .reconstruction_cache_size(1000).unwrap()
        .build())
    .build();
```

**Characteristics:**
- Memory usage: ~50MB baseline
- Limited history retention
- Suitable for IoT devices, embedded systems

### Cloud Deployment (High Throughput)

Optimized for cloud VMs with ample resources:

```rust
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(64).unwrap()
        .stripe_capacity(4096).unwrap()
        .write_buffer_size(256 * 1024).unwrap()
        .segment_size(256 * 1024 * 1024).unwrap()
        .durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 500,
        })
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(10000).unwrap()
        .reconstruction_cache_size(100000).unwrap()
        .build())
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/production".into(),
        load_on_startup: true,
        use_mmap: true,
        ..Default::default()
    })
    .build();
```

**Characteristics:**
- Memory usage: ~2-4GB baseline
- High concurrency (64 stripes)
- Group commit for throughput
- Index persistence for fast restarts
- Suitable for production workloads

### Analytics (Maximum Throughput)

Optimized for batch data imports:

```rust
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(64).unwrap()
        .stripe_capacity(8192).unwrap()
        .write_buffer_size(512 * 1024).unwrap()
        .segment_size(512 * 1024 * 1024).unwrap()
        .durability_mode(DurabilityMode::AsyncBatched {
            max_delay_ms: 100,
            max_batch_size: 5000,
        })
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(1000).unwrap()
        .reconstruction_cache_size(50000).unwrap()
        .build())
    .build();
```

**Characteristics:**
- Memory usage: ~4-8GB baseline
- Eventual durability (trade durability for speed)
- Massive write throughput (>500K writes/sec)
- Suitable for data warehousing, ETL pipelines

## Feature Flags

### config-toml (Default)

Enable TOML configuration file support:

```toml
[dependencies]
aletheiadb = "0.3"  # config-toml enabled by default
```

**Adds:**
- `from_toml_file()` - Load config from TOML file
- `from_toml_str()` - Parse config from TOML string
- `to_toml_file()` - Save config to TOML file
- `to_toml_string()` - Serialize config to TOML string

**Dependencies Added:**
- `serde` - Serialization framework
- `toml` - TOML parser

**Disable if not needed:**

```toml
[dependencies]
aletheiadb = { version = "0.1.0", default-features = false }
```

This reduces compile time and binary size when only using programmatic configuration.

## Builder Validation

All builder methods validate inputs and return `Result<Self, ConfigError>`:

```rust
use aletheiadb::config::{WalConfigBuilder, ConfigError};

// This will error with ConfigError::InvalidValue
let result = WalConfigBuilder::new()
    .num_stripes(0);  // Error: must be > 0

assert!(matches!(result, Err(ConfigError::InvalidValue(_))));

// This will error - num_stripes must be power of 2
let result = WalConfigBuilder::new()
    .num_stripes(7);  // Error: not a power of 2

assert!(matches!(result, Err(ConfigError::InvalidValue(_))));
```

**Common Validation Errors:**
- `ConfigError::InvalidValue` - Parameter out of valid range
- `ConfigError::ParseError` - TOML parsing failed
- `ConfigError::IoError` - File I/O error

## Performance Tuning Guide

### Tuning for Write-Heavy Workloads

```rust
// Increase concurrency and buffer sizes
WalConfigBuilder::new()
    .num_stripes(64).unwrap()        // More concurrent writers
    .stripe_capacity(4096).unwrap()  // Larger ring buffers
    .write_buffer_size(512 * 1024).unwrap()  // Larger I/O buffer
    .durability_mode(DurabilityMode::group_commit_default())
```

### Tuning for Read-Heavy Workloads

```rust
// Increase cache sizes
HistoricalConfigBuilder::new()
    .reconstruction_cache_size(100000).unwrap()  // Cache more reconstructed versions
    .max_reconstruction_depth(200).unwrap()      // Allow deeper anchor chains

// Enable index persistence for fast cold starts
PersistenceConfig {
    enabled: true,
    data_dir: "/var/lib/my-app/indexes".into(),  // Always set explicitly when enabling
    load_on_startup: true,
    use_mmap: true,  // Memory-map large indexes
    ..Default::default()
}
```

### Tuning for Memory-Constrained Environments

```rust
// Reduce memory usage
WalConfigBuilder::new()
    .num_stripes(4).unwrap()
    .stripe_capacity(256).unwrap()
    .write_buffer_size(16 * 1024).unwrap()

HistoricalConfigBuilder::new()
    .max_versions_per_entity(100).unwrap()
    .reconstruction_cache_size(1000).unwrap()
```

## MCP Server Resource Limits

The MCP server (`AletheiaMcpServer`) exposes builder-style knobs that bound
per-call resource usage, guarding the surface against denial-of-service from
untrusted callers. All have safe defaults and are optional.

| Knob | Default | Purpose |
|------|---------|---------|
| `with_max_batch_operations(n)` | 1000 | Max operations accepted by one `apply_batch` call (Issue #3231). |
| `with_max_designate_targets(n)` | 1000 | Max targets accepted by one `designate_subject` call (Issue #3701). |
| `with_cursor_config(ttl, max_live_cursors)` | 5 min, 128 | Continuation-cursor TTL and per-connection live-cursor cap (Issue #3360). |
| `with_max_priority_properties(n)` | 1024 | Max entries in a token-budget `priority_properties` array (Issue #3583). |

### `with_max_priority_properties` (Issue #3583)

The token-budget parameter `priority_properties` (Issue #3353) names the
property keys a budgeted read protects from elision. It is consulted for every
property of every returned entity while the response is shaped. Left unbounded,
a caller could pass an array with hundreds of thousands of entries, turning
response shaping into multi-second blocking CPU. The per-key lookup is O(1)
(backed by a `HashSet`), and an array longer than this cap is rejected up front
with a structured `INVALID_ARGUMENT` error naming the cap and the given length
(so re-issuing under the cap succeeds), keeping the one-time validation cost
bounded as well.

```rust
use std::sync::Arc;
use aletheiadb::AletheiaDB;
use aletheiadb::mcp::AletheiaMcpServer;

let db = Arc::new(AletheiaDB::new()?);
// Tighten the priority_properties cap from the default 1024 to 64.
let server = AletheiaMcpServer::new(db).with_max_priority_properties(64);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## References

- [WAL Documentation](WAL.md) - Write-ahead log internals
- [Index Persistence Guide](guides/index-persistence-guide.md) - Index persistence details
- [Architecture Documentation](ARCHITECTURE.md) - System architecture
- [MCP Query Tool Guide](guides/mcp-query-tool.md) - Token budgets, cursors, and MCP resource limits
