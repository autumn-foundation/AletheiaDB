# Recovery Performance Benchmarks

This document provides performance benchmarks and analysis for GallifreyDB recovery operations.

## Overview

Database recovery is a critical operation that occurs during system startup or after a crash. GallifreyDB's recovery system is designed to restore database state quickly and reliably from persisted checkpoints and WAL (Write-Ahead Log) entries.

## Performance Targets

The following performance targets guide our recovery implementation:

| Scenario | Dataset Size | Target Recovery Time |
|----------|-------------|---------------------|
| Small Dataset | 100 nodes + 500 edges | < 100ms |
| Medium Dataset | 10,000 nodes + 50,000 edges | < 5 seconds |
| Large Dataset | 100,000 nodes + 500,000 edges | < 30 seconds |
| WAL Replay | 100,000 operations | < 10 seconds |
| Vector-Indexed Data | 10,000 nodes with 384-dim embeddings | < 10 seconds |

## Benchmark Results

### 1. Small Dataset Recovery

**Configuration:**
- Nodes: 100
- Edges: 500
- Recovery method: Checkpoint-based

**Results:**
```
Time: 2.11ms (mean), 2.07-2.15ms (range)
Status: ✓ PASS (well under 100ms target - 47x faster)
```

**Analysis:**
Small dataset recovery is extremely fast, completing in approximately 2 milliseconds. This demonstrates excellent performance for development, testing, and small production deployments.

### 2. Medium Dataset Recovery

**Configuration:**
- Nodes: 10,000
- Edges: 50,000
- Recovery method: Checkpoint-based

**Results:**
```
Time: 55.42ms (mean), 54.56-56.72ms (range)
Status: ✓ PASS (well under 5s target - 90x faster)
```

**Analysis:**
Medium dataset recovery completes in approximately 55 milliseconds, well under the 5-second target. This indicates strong scalability for typical production workloads.

### 3. Large Dataset Recovery

**Configuration:**
- Nodes: 100,000
- Edges: 500,000
- Recovery method: Checkpoint-based

**Results:**
```
Time: 828.96ms (mean), 819.63-839.10ms (range)
Status: ✓ PASS (well under 30s target - 36x faster)
```

**Analysis:**
Large dataset recovery completes in under 1 second, demonstrating excellent scalability for substantial production databases. Results are measured across 10 samples to ensure statistical reliability.

### 4. WAL Replay Recovery

**Configuration:**
- Operations: 100,000 (50% creates, 50% updates)
- Recovery method: WAL replay from empty checkpoint
- Batch size: 1,000 operations per flush

**Results:**
```
Time: 362.65ms (mean), 360.09-365.86ms (range)
Status: ✓ PASS (well under 10s target - 27x faster)
```

**Analysis:**
WAL replay recovery measures the overhead of replaying operations when no checkpoint is available or when recovering operations after the last checkpoint. Recovery completes in under 400ms for 100,000 operations (50,000 creates + 50,000 updates), demonstrating excellent throughput of approximately 275,000 ops/second. This ensures minimal data loss and fast recovery during crash scenarios.

### 5. Vector-Indexed Recovery

**Configuration:**
- Nodes: 10,000
- Vector dimensions: 384
- HNSW parameters: M=16, ef_construction=200
- Recovery method: Checkpoint-based with index rebuilding

**Results:**
```
Time: 52.76ms (mean), 52.38-53.17ms (range)
Status: ✓ PASS (well under 10s target - 189x faster)
```

**Analysis:**
Vector-indexed recovery completes in approximately 53 milliseconds for 10,000 nodes with 384-dimensional embeddings. This demonstrates that HNSW vector index recovery has minimal overhead compared to standard graph recovery, making it suitable for production deployments requiring semantic search capabilities.

## Recovery Architecture

### Checkpoint-Based Recovery

GallifreyDB uses a unified checkpoint system that persists:
- **Graph structure**: Nodes, edges, and adjacency information
- **Temporal data**: Historical versions and bi-temporal intervals
- **Vector indexes**: HNSW indexes for similarity search
- **String interner**: Optimized string storage

Recovery process:
1. Load latest checkpoint from disk
2. Restore graph, temporal, and vector indexes
3. Replay WAL entries from checkpoint LSN to current LSN
4. Initialize ID generators based on max existing IDs

### WAL Replay Recovery

When no checkpoint is available or for incremental recovery:
1. Start with empty or checkpointed state
2. Read WAL entries sequentially
3. Apply operations (CreateNode, UpdateNode, CreateEdge, etc.)
4. Build indexes incrementally
5. Flush final state

## Optimization Techniques

### 1. Memory-Mapped Loading
Large checkpoint files use memory-mapped I/O for efficient loading without excessive memory allocation.

### 2. Parallel Index Loading
Graph, temporal, and vector indexes load concurrently on multi-core systems.

### 3. Zstd Compression
Checkpoints use Zstd compression (level 3) for 60-75% size reduction while maintaining fast decompression.

### 4. Incremental WAL Replay
WAL operations are batched during replay to amortize index update costs.

### 5. Pre-allocated Data Structures
Recovery pre-allocates buffers based on checkpoint metadata to minimize allocations.

## Running Benchmarks

### Quick Test (CI/Development)
```bash
BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=2 BENCH_WARMUP_TIME=1 \
  cargo bench --bench recovery_benchmarks
```

### Production Benchmarks
```bash
BENCH_SAMPLE_SIZE=100 BENCH_MEASUREMENT_TIME=10 BENCH_WARMUP_TIME=5 \
  cargo bench --bench recovery_benchmarks
```

### Individual Scenarios
```bash
# Run only small dataset benchmark
cargo bench --bench recovery_benchmarks recovery_small_dataset

# Run only vector-indexed benchmark
cargo bench --bench recovery_benchmarks recovery_vector_indexed
```

## Benchmark Configuration

The benchmarks use Criterion for statistical analysis:
- **Sample size**: 10-20 samples (configurable via `BENCH_SAMPLE_SIZE`)
- **Measurement time**: 2-60 seconds depending on scenario
- **Warmup time**: 1 second (configurable via `BENCH_WARMUP_TIME`)
- **Statistical analysis**: Mean, median, standard deviation, outliers

## Interpreting Results

### Performance Indicators

**Green (PASS)**: Recovery time well under target (< 50% of target)
- Small dataset: < 50ms
- Medium dataset: < 2.5s
- Large dataset: < 15s
- WAL replay: < 5s
- Vector-indexed: < 5s

**Yellow (ACCEPTABLE)**: Recovery time near target (50-100% of target)
- Meeting targets but may need optimization for future growth

**Red (FAIL)**: Recovery time exceeds target
- Requires optimization before release
- May indicate architectural issues

### Variance and Outliers

Criterion reports outliers which may indicate:
- **High mild outliers**: Occasional GC pauses or I/O spikes (acceptable)
- **High severe outliers**: Systematic performance issues (investigate)
- **Low variance**: Consistent, predictable recovery (ideal)

## Troubleshooting Slow Recovery

If recovery times exceed targets:

### 1. Check Disk I/O
```bash
# Monitor I/O during recovery
iostat -x 1
```

Recovery is I/O-bound. SSDs provide 5-10x faster recovery than HDDs.

### 2. Profile with Tracy
```bash
# Build with profiling
cargo build --release --features tracy

# Run with Tracy profiler
./target/release/gallifreydb
```

### 3. Adjust Checkpoint Frequency
More frequent checkpoints reduce WAL replay time:
```rust
let config = GallifreyDBConfig::builder()
    .checkpoint_interval(Duration::from_secs(300)) // 5 minutes
    .min_wal_entries(10_000)
    .build();
```

### 4. Tune Compression
Balance compression ratio vs. decompression speed:
```rust
// Faster decompression (lower compression)
let config = UnifiedCheckpointConfig {
    compression_level: 1, // Zstd level 1 (default: 3)
    ..Default::default()
};
```

## Future Improvements

### Planned Optimizations

1. **Incremental Checkpoints**: Delta checkpoints for faster saves and loads
2. **Parallel WAL Replay**: Multi-threaded operation replay
3. **Background Recovery**: Load checkpoint asynchronously during startup
4. **Lazy Vector Index Rebuild**: Defer HNSW reconstruction until first query

### Performance Goals (Next Release)

- Small dataset: < 1ms
- Medium dataset: < 50ms
- Large dataset: < 10s
- WAL replay: < 5s (for 100k ops)
- Vector-indexed: < 5s (with lazy rebuild)

## References

- [Checkpoint Manager Implementation](../../src/storage/checkpoint.rs)
- [WAL System Documentation](../WAL.md)
- [Index Persistence Guide](../guides/index-persistence-guide.md)
- [Benchmark Source Code](../../benches/recovery_benchmarks.rs)

## Changelog

### 2026-01-24 (Issue #296)
- Initial recovery benchmarks implementation
- All 5 benchmark scenarios implemented
- Performance targets established and validated
- Documentation created
