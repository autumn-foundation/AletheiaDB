# Profiling GallifreyDB with Tracy

This guide shows how to profile GallifreyDB to identify transaction commit bottlenecks using Tracy profiler.

## Context: Known Bottlenecks

Based on performance investigation, we know:
- **WAL overhead**: ~10-15% of transaction time (NOT the bottleneck)
- **Lock contention**: Timestamp + WAL locks (~90µs hold time)
- **Graph operations**: 85-90% of time spent in apply_changes()
- **Unknown**: Exact breakdown of apply_changes time

**Goal**: Use Tracy flame graphs to identify where apply_changes() spends its time.

## Prerequisites

### 1. Install Tracy Profiler

Download Tracy from: https://github.com/wolfpld/tracy/releases

Extract and run the Tracy server/GUI.

### 2. Build with Tracy Support

```bash
cargo build --release --features tracy
```

**Note**: Tracy adds ~5-10% overhead for detailed profiling. Always compare relative times, not absolute.

## Running Profiling Benchmarks

### Quick Start

```bash
# Terminal 1: Start Tracy profiler
./tracy-profiler

# Terminal 2: Run profiling benchmark
cargo bench --bench profiling_commit --features tracy -- --profile-time 10
```

Tracy will automatically connect and start collecting data.

### Available Benchmark Scenarios

1. **Sequential Commits** (baseline, no contention):
   ```bash
   cargo bench --bench profiling_commit --features tracy -- sequential
   ```

2. **Concurrent Commits** (expose lock contention):
   ```bash
   cargo bench --bench profiling_commit --features tracy -- concurrent
   ```

3. **Heavy Transactions** (large graph operations):
   ```bash
   cargo bench --bench profiling_commit --features tracy -- heavy
   ```

4. **Mixed Workload** (realistic scenario):
   ```bash
   cargo bench --bench profiling_commit --features tracy -- mixed
   ```

## Tracy Span Hierarchy

The instrumentation provides this span structure:

```
benchmark_iteration
└── WriteTransaction::commit
    ├── commit_critical_section
    │   ├── acquire_timestamp_lock (LOCK CONTENTION)
    │   ├── acquire_wal_lock (LOCK CONTENTION)
    │   ├── wal_log_operations
    │   └── wal_commit
    ├── group_commit_wait (GROUP COMMIT MODE)
    └── apply_changes (SUSPECTED BOTTLENECK)
        ├── apply_changes_setup
        ├── apply_create_node (per operation)
        ├── apply_create_edge (per operation)
        ├── apply_update_node (per operation)
        ├── apply_update_edge (per operation)
        ├── apply_delete_node (per operation)
        ├── apply_delete_edge (per operation)
        ├── HistoricalStorage::add_node_version
        │   ├── version_chain_lookup
        │   ├── version_chain_add
        │   └── temporal_index_update
        ├── CurrentStorage::insert_node
        ├── CurrentStorage::insert_edge
        └── rebuild_adjacency_index
```

## Analyzing Results

### 1. Find the Bottleneck

In Tracy, look for the **widest spans** in the flame graph:

- **If `commit_critical_section` is wide**: Lock contention issue
  - Check `acquire_timestamp_lock` and `acquire_wal_lock` for wait times
  - Look for threads stacked waiting for locks
  - **Solution**: Reduce critical section or use lock-free structures

- **If `apply_changes` is wide** (expected): Graph operations bottleneck
  - Drill down into sub-spans to see which operations are slow
  - Look for: `apply_create_node`, `apply_create_edge`, `rebuild_adjacency_index`
  - Check `HistoricalStorage::add_node_version` for version chain overhead
  - **Solution**: Optimize the specific operation identified

- **If `wal_commit` is wide**: Disk I/O issue (unlikely in temp directories)
  - Check fsync duration
  - **Solution**: Use AsyncBatched mode or faster disk

### 2. Compare Scenarios

- **Sequential vs Concurrent**: Shows impact of lock contention
  - Sequential baseline: No waiting threads
  - Concurrent: Multiple threads waiting for locks = contention

- **Small vs Heavy**: Shows scaling behavior with transaction size
  - Small (1-10 ops): Lock overhead may dominate
  - Heavy (100+ ops): apply_changes overhead dominates

- **Different durability modes**: Shows WAL overhead
  - Synchronous: Includes fsync time in critical section
  - Async/AsyncBatched: Non-blocking, faster commit

### 3. Key Metrics

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Lock hold time | <90µs | Width of `commit_critical_section` |
| Lock wait time | <10µs | Gap before lock acquire spans |
| apply_changes | <500µs | Width of `apply_changes` span |
| Adjacency rebuild | <100µs | Width of `rebuild_adjacency_index` |
| Per-operation cost | <10µs | Width of `apply_create_node` etc. |

### 4. Identifying Lock Contention

**Visual indicators in Tracy:**
- Multiple threads shown stacked vertically
- Threads waiting at `acquire_timestamp_lock` or `acquire_wal_lock`
- Long gaps between thread execution

**What to look for:**
- High concurrent workload: 8-16 threads competing for locks
- Lock wait time > lock hold time: Indicates severe contention
- Uneven thread utilization: Some threads idle while one holds lock

### 5. Identifying Graph Operation Bottlenecks

**Visual indicators:**
- `apply_changes` span much wider than `commit_critical_section`
- Specific operation types dominating (e.g., all time in `apply_create_edge`)
- `rebuild_adjacency_index` taking significant portion of apply_changes

**What to look for:**
- Heavy workload: 100+ nodes/edges in single transaction
- Per-operation cost: Should be <10µs each
- Adjacency rebuild: Should be O(E log E) but efficient
- Historical storage: Version chain add should be fast

## Troubleshooting

**Tracy shows no data:**
- Start Tracy GUI BEFORE running benchmark
- Verify `--features tracy` is specified
- Check that binary was built with tracy feature

**Benchmark runs slowly:**
- Expected - Tracy adds 5-10% overhead
- Compare relative times between operations, not absolute

**Missing spans:**
- Ensure `#[cfg(feature = "tracy")]` guards are present
- Check that span variables start with `_` to avoid being dropped immediately

**Spans not appearing in flame graph:**
- Spans may be too short (<1µs) to visualize
- Tracy may aggregate very short spans
- Use Tracy's zoom feature to see fine-grained detail

## Alternative: observability-tracy Feature

You can also use the tracing-based observability with Tracy:

```bash
cargo bench --bench profiling_commit --features observability-tracy
```

This bridges the existing tracing spans (lines 165-349 in write_tx.rs) to Tracy, giving you the existing timing breakdowns in Tracy format.

**Difference:**
- `--features tracy`: Direct Tracy spans (minimal overhead, focused on bottlenecks)
- `--features observability-tracy`: Full observability with Tracy backend (more detail, higher overhead)

## Next Steps After Profiling

1. **Identify the slowest span** in apply_changes
   - Is it `rebuild_adjacency_index`?
   - Is it per-operation overhead (`apply_create_node` etc.)?
   - Is it historical storage (`HistoricalStorage::add_node_version`)?

2. **Add more granular instrumentation** to that specific area
   - Add spans inside the identified bottleneck function
   - Re-profile to validate the hypothesis

3. **Optimize the bottleneck**
   - Reduce allocations
   - Use more efficient data structures
   - Parallelize if possible

4. **Re-profile to validate improvement**
   - Run same benchmark with optimization
   - Compare flame graphs before/after
   - Measure speedup

5. **Document findings** in performance investigation docs
   - Update performance summary
   - Add optimization notes
   - Update performance targets if achieved

## Example Profiling Session

### Step 1: Identify the Problem

```bash
# Run concurrent benchmark to see lock contention
cargo bench --bench profiling_commit --features tracy -- concurrent/threads/8
```

**Observation in Tracy**: Threads spending time waiting in `acquire_timestamp_lock`

### Step 2: Isolate the Component

```bash
# Run sequential benchmark to measure without contention
cargo bench --bench profiling_commit --features tracy -- sequential/ops/10
```

**Observation**: Even without contention, `apply_changes` takes 500µs

### Step 3: Drill Down

Look at flame graph for `apply_changes`:
- 40% in `rebuild_adjacency_index`
- 30% in `apply_create_edge`
- 20% in `HistoricalStorage::add_node_version`
- 10% in other operations

**Conclusion**: Focus optimization on adjacency rebuild (40% of time)

### Step 4: Optimize and Verify

After implementing incremental adjacency rebuild:

```bash
# Re-run benchmark
cargo bench --bench profiling_commit --features tracy -- heavy
```

**Result**: `rebuild_adjacency_index` now 10µs instead of 200µs (20x improvement)

## Performance Targets

From CLAUDE.md, our targets are:

| Operation | Target | Current | Status |
|-----------|--------|---------|--------|
| Single-hop traversal | <1µs | TBD | 🔍 Measure |
| 3-hop traversal | <100µs | TBD | 🔍 Measure |
| Temporal reconstruction | <10ms | TBD | 🔍 Measure |
| Batch insertion | >100k/sec | ~700/sec | ❌ Below target |
| Transaction commit | <2ms | ~1.5-2ms | ⚠️ At limit |

**Use Tracy profiling to identify why batch insertion is below target.**

## References

- Tracy Profiler: https://github.com/wolfpld/tracy
- tracy-client docs: https://docs.rs/tracy-client
- Existing observability: `src/api/transaction/write_tx.rs:265-305`
- Performance investigation: See performance summary document
- Criterion benchmarks: `benches/` directory

## Tips and Best Practices

### Do's

✅ **Do** run Tracy GUI before starting benchmark
✅ **Do** compare relative times between scenarios
✅ **Do** focus on widest spans in flame graph
✅ **Do** zoom in on specific areas of interest
✅ **Do** run multiple iterations for consistent results
✅ **Do** profile both sequential and concurrent workloads
✅ **Do** document findings and optimizations

### Don'ts

❌ **Don't** trust absolute timing with Tracy (5-10% overhead)
❌ **Don't** profile in debug mode (use --release)
❌ **Don't** forget to disable Tracy for production builds
❌ **Don't** ignore lock contention in concurrent tests
❌ **Don't** optimize without profiling first
❌ **Don't** make changes without re-profiling to verify

## Conclusion

Tracy profiling is essential for identifying performance bottlenecks in GallifreyDB. Use it to:

1. Understand where transaction commit time is spent
2. Identify lock contention under concurrent load
3. Find expensive operations in apply_changes
4. Validate optimization efforts
5. Achieve performance targets

With Tracy, you can make data-driven optimization decisions instead of guessing.
