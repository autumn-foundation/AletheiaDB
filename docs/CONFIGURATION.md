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
| `data_dir` | PathBuf | "data/indexes" | Directory for index files |
| `load_on_startup` | bool | false | Load indexes on startup |
| `use_mmap` | bool | false | Use memory-mapped loading |
| `policies` | PersistencePolicies | Default | Automatic persistence policies |

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
aletheiadb = "0.1.0"  # config-toml enabled by default
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

## References

- [WAL Documentation](WAL.md) - Write-ahead log internals
- [Index Persistence Guide](guides/index-persistence-guide.md) - Index persistence details
- [Architecture Documentation](ARCHITECTURE.md) - System architecture
