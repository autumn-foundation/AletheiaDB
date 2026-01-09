# Profiling GallifreyDB with Tracy

This guide shows how to profile GallifreyDB to identify transaction commit bottlenecks using Tracy profiler via the observability framework.

## Context: Known Bottlenecks

Based on performance investigation, we know:
- **WAL overhead**: ~10-15% of transaction time (NOT the bottleneck)
- **Lock contention**: Timestamp + WAL locks (~90µs hold time)
- **Graph operations**: 85-90% of time spent in apply_changes()
- **Unknown**: Exact breakdown of apply_changes time

**Goal**: Use Tracy flame graphs to identify where apply_changes() spends its time.

## Architecture: Observability Framework

GallifreyDB uses a **layered observability approach**:

1. **Instrumentation Layer**: Code uses `tracing` spans for structured logging
2. **Backend Layer**: The `observability-tracy` feature bridges tracing spans to Tracy profiler
3. **Zero Overhead**: When observability features are disabled, all instrumentation compiles away

**Benefits**:
- Single instrumentation layer (tracing) serves multiple backends (logs, Tracy, Honeycomb, etc.)
- No direct Tracy coupling in application code
- Automatic span hierarchy for flame graphs
- Consistent with existing observability infrastructure

## Prerequisites

### 1. Install Tracy Profiler

Download Tracy from: https://github.com/wolfpld/tracy/releases

Extract and run the Tracy server/GUI.

### 2. Build with Observability-Tracy Support

```bash
cargo build --release --features observability-tracy
```

**Note**: Tracy adds ~5-10% overhead for detailed profiling. Always compare relative times, not absolute.

## Running Profiling Benchmarks

### Quick Start

```bash
# Terminal 1: Start Tracy profiler
./tracy-profiler

# Terminal 2: Run profiling benchmark
cargo bench --bench profiling_commit --features observability-tracy -- --profile-time 10
```

Tracy will automatically connect and start collecting data from the tracing spans.

### Available Benchmark Scenarios

1. **Sequential Commits** (baseline, no contention):
   ```bash
   cargo bench --bench profiling_commit --features observability-tracy -- sequential
   ```

2. **Concurrent Commits** (expose lock contention):
   ```bash
   cargo bench --bench profiling_commit --features observability-tracy -- concurrent
   ```

3. **Heavy Transactions** (large graph operations):
   ```bash
   cargo bench --bench profiling_commit --features observability-tracy -- heavy
   ```

4. **Mixed Workload** (realistic scenario):
   ```bash
   cargo bench --bench profiling_commit --features observability-tracy -- mixed
   ```

## Tracy Span Hierarchy

The tracing spans automatically map to Tracy zones with this hierarchy:

```
transaction_commit
├── commit_critical_section (LOCK HOLD TIME)
│   ├── wal_log_operations
│   └── wal_commit
├── group_commit_wait (GROUP COMMIT MODE)
└── apply_changes (OUTER SPAN)
    └── apply_changes_detailed (SUSPECTED BOTTLENECK)
        ├── apply_changes_setup
        ├── apply_create_node (per operation, trace level)
        ├── apply_create_edge (per operation, trace level)
        ├── apply_update_node (per operation, trace level)
        ├── apply_update_edge (per operation, trace level)
        ├── apply_delete_node (per operation, trace level)
        ├── apply_delete_edge (per operation, trace level)
        ├── add_node_version (historical storage, trace level)
        ├── add_edge_version (historical storage, trace level)
        ├── insert_node (current storage, trace level)
        ├── insert_edge (current storage, trace level)
        └── rebuild_adjacency_index (ADJACENCY REBUILD)
```

**Span Levels**:
- `info` - Top-level transaction operations (always visible)
- `debug` - Critical path sections (commit_critical_section, apply_changes, etc.)
- `trace` - Per-operation details (create_node, insert_edge, etc.)

**Note**: Tracy shows all span levels. The level distinction helps when viewing logs vs Tracy.

## Analyzing Results

### 1. Find the Bottleneck

In Tracy, look for the **widest spans** in the flame graph:

- **If `commit_critical_section` is wide**: Lock contention issue
  - Check surrounding threads for lock wait times
  - Look for threads stacked waiting for locks
  - **Solution**: Reduce critical section or use lock-free structures

- **If `apply_changes_detailed` is wide** (expected): Graph operations bottleneck
  - Drill down into sub-spans to see which operations are slow
  - Look for: `add_node_version`, `add_edge_version`, `rebuild_adjacency_index`
  - **Solution**: Optimize the specific operation identified

- **If `wal_commit` is wide**: Disk I/O issue (unlikely in temp directories)
  - Check fsync duration
  - **Solution**: Use AsyncBatched mode or faster disk

### 2. Compare Scenarios

- **Sequential vs Concurrent**: Shows impact of lock contention
- **Small vs Heavy**: Shows scaling behavior with transaction size
- **Different durability modes**: Shows WAL overhead

### 3. Key Metrics

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Lock hold time | <90µs | Width of `commit_critical_section` |
| Lock wait time | <10µs | Gap before lock acquire (visible in concurrent) |
| apply_changes | <500µs | Width of `apply_changes_detailed` span |
| Adjacency rebuild | <100µs | Width of `rebuild_adjacency_index` |

### 4. Timing Data from Observability

In addition to Tracy flame graphs, the observability framework logs detailed timing:

```rust
ts_lock_wait_us: 5
wal_lock_wait_us: 3
wal_log_us: 12
wal_commit_us: 45
total_locked_us: 65
total_commit_us: 523
operations_count: 10
```

This data is emitted at `info` level and can be sent to Honeycomb or other logging backends for aggregate analysis.

## Troubleshooting

**Tracy shows no data:**
- Start Tracy GUI BEFORE running benchmark
- Verify `--features observability-tracy` is specified
- Check that binary was built with observability-tracy feature
- Verify Tracy is listening on the correct port

**Benchmark runs slowly:**
- Expected - Tracy adds 5-10% overhead
- Compare relative times between operations, not absolute

**Missing spans:**
- Ensure observability feature is enabled
- Check that tracing spans use `.entered()` to activate
- Verify span variables start with `_` to avoid being dropped immediately

**Too much detail (trace-level spans):**
- This is normal - Tracy shows all spans regardless of level
- Focus on the wider spans for bottleneck identification
- Trace-level spans (per-operation) help drill down once you've identified the area

## Advanced: Adding More Instrumentation

If you need to add more profiling spans:

1. **Use tracing, not Tracy directly**:
   ```rust
   #[cfg(feature = "observability")]
   let _span = tracing::debug_span!("my_operation").entered();
   ```

2. **Choose appropriate level**:
   - `info_span!` - Top-level operations
   - `debug_span!` - Critical path sections
   - `trace_span!` - Per-operation details

3. **Naming convention**:
   - Use `snake_case` for consistency
   - Be descriptive: `rebuild_adjacency_index` not `rebuild`
   - Avoid dynamic strings (Tracy limitation): use string literals

4. **Zero overhead**:
   - Guard with `#[cfg(feature = "observability")]`
   - Compiler removes all instrumentation when feature disabled

## Alternative: Direct Tracing Logs

You can also use the observability feature without Tracy for structured logging:

```bash
# View timing logs without Tracy
cargo bench --bench profiling_commit --features observability
```

This will emit tracing events to stdout with timing breakdowns, useful for:
- Quick performance checks without Tracy GUI
- CI/CD pipeline performance monitoring
- Aggregate analysis with log aggregation tools

## Next Steps After Profiling

1. **Identify the slowest span** in apply_changes_detailed
2. **Add more granular instrumentation** to that specific area if needed
3. **Optimize** the identified bottleneck
4. **Re-profile** to validate optimization impact
5. **Document findings** in performance investigation docs

## References

- Tracy Profiler: https://github.com/wolfpld/tracy
- tracing crate: https://docs.rs/tracing
- tracing-tracy: https://docs.rs/tracing-tracy
- Existing observability: `src/api/transaction/write_tx.rs` (lines 169-394)
